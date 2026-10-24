//! Reedline REPL loop

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};
use std::{borrow::Cow, io::Write};

use crate::autosuggest::HistoryAutosuggestHinter;
use crate::completion::NiubashCompleter;
use crate::config::{
    CompletionStyle, EditorMode, MenuConfig, NativeWidgetBinding, NativeWidgetConfig,
};
use crate::history::LiveFileBackedHistory;
use crate::shell::Shell;
use crate::syntax_highlighting::NiubashSyntaxHighlighter;
use reedline::{
    default_emacs_keybindings, default_vi_insert_keybindings, default_vi_normal_keybindings,
    default_vi_visual_keybindings, ColumnarMenu, EditCommand, EditMode, Emacs, KeyCode,
    KeyModifiers, Keybindings, ListMenu, MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch,
    PromptViMode, Reedline, ReedlineEvent, ReedlineMenu, ReedlineRawEvent, Signal,
    ValidationResult, Validator, Vi,
};

const COMPLETION_MENU: &str = "completion_menu";
const HISTORY_MENU: &str = "history_menu";

/// Two crossterm key events arriving closer together than this belong to one
/// input burst. Within a burst reedline parses events back-to-back, but the
/// threshold must also absorb the SLOWEST slice gap of a terminal paste
/// feed: winpty/ConPTY delivers a paste in ~10 ms slices, and each repaint
/// reedline would perform for a partially-inserted chunk takes tens of ms
/// once the buffer grows — a small threshold here feeds back into those
/// very repaints (flush → repaint → gap → flush). 50 ms rides out the
/// driver's slice jitter, so a pasted chunk buffers through the slices and
/// is committed as ONE insert on the gap instead of degenerating into
/// per-line repaints.
///
/// Since niubash#284 the gap NO LONGER decides submits: whether a bare
/// Enter is paste data is structural (its queued LF partner — see
/// [`PASTE_LF_PROBE`]), so a fast typist's "text + Enter" burst submits
/// however hot the burst is. The gap's only job is marking the burst
/// boundary where a pending chunk flushes. Tuning knob: raise it if a
/// slower terminal driver is ever detected mid-paste; lower it only with
/// benchmark evidence (scripts/perf/paste_multiline_bench.py).
const PASTE_CHUNK_GAP: Duration = Duration::from_millis(50);

/// Paste-chunk detection is a Windows-console-path replacement for bracketed
/// paste; on Unix the terminal wraps pastes in ESC[200~/ESC[201~ and
/// crossterm delivers one `Event::Paste` per chunk (niubash#234), so no
/// heuristic is wanted there.
const PASTE_CHUNK_DETECTION: bool = cfg!(windows);

/// Budget for the paste-signature lookahead at a bare Enter (see
/// [`PasteChunkEditMode`]). Two machine-fed shapes exist on this path:
///
/// * the ConPTY text-feed shape (winpty, test drivers, expect scripts): a
///   paste arrives as console writes where every line break is a CRLF pair
///   in ONE write — the pair's LF (`Enter + CONTROL`) is already in the
///   input queue when the CR is dispatched — reedline's read loop itself
///   stops every batch at a bare Enter (`engine.rs` `read_line_helper`), so
///   the LF cannot have been consumed ahead of it.
/// * the Windows Terminal paste shape (niubash#341): the terminal
///   synthesizes a KEY_EVENT record per pasted character and every line
///   break is a lone CR — no LF ever exists, and an empty line is two
///   consecutive CRs. A CR behind a CR is therefore paste data too (see
///   [`PasteChunkEditMode::cr_paste_partner`]).
///
/// The lookahead therefore only has to cover the console driver's
/// synthesis jitter, not any human decision gap: 10 ms is an order of
/// magnitude above observed ConPTY handoff latency and an order of
/// magnitude below a perceptible submit delay. It decides the chunk-open
/// Enter (niubash#284) and, identically, a bare CR that arrives while a
/// chunk is pending (niubash#308). Tuning knob: a larger budget trades
/// submit latency for slow-driver paste robustness; lower it only with
/// driver traces showing the pair split wider than the budget.
const PASTE_LF_PROBE: Duration = Duration::from_millis(10);

/// `ExecuteHostCommand` payload prefix that identifies a shell-function
/// widget trigger. The host intercepts this before treating the signal as
/// submitted input.
pub const WIDGET_HOST_COMMAND_PREFIX: &str = "__niu_widget ";

/// `ExecuteHostCommand` payload prefix that identifies an engine `bind -x`
/// trigger (niubash#185). The payload is the index into
/// `Shell::engine_bindings`; the host runs the command through
/// [`Shell::run_bind_x_command`] with the current edit buffer and applies
/// the READLINE_* read-back to the editor.
pub const BINDX_HOST_COMMAND_PREFIX: &str = "__niu_bindx ";

/// A widget trigger parsed from a `WIDGET_HOST_COMMAND_PREFIX` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetInvocation {
    pub function: String,
}

impl WidgetInvocation {
    /// Parse a submitted line as a widget invocation. Lines that do not carry
    /// the sentinel, have no name, or contain whitespace after the name are
    /// not widget invocations and stay ordinary user input.
    pub fn parse(line: &str) -> Option<Self> {
        let name = line.strip_prefix(WIDGET_HOST_COMMAND_PREFIX)?.trim();
        if name.is_empty() || name.chars().any(char::is_whitespace) {
            return None;
        }
        Some(Self {
            function: name.to_string(),
        })
    }
}

/// A bind -x trigger parsed from a `BINDX_HOST_COMMAND_PREFIX` payload:
/// the index into the shell's engine-binding snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindXInvocation {
    pub index: usize,
}

impl BindXInvocation {
    /// Only exact `__niu_bindx <number>` lines are bind -x triggers;
    /// anything else stays ordinary user input.
    pub fn parse(line: &str) -> Option<Self> {
        let payload = line.strip_prefix(BINDX_HOST_COMMAND_PREFIX)?.trim();
        payload.parse::<usize>().ok().map(|index| Self { index })
    }
}

/// Parse a `NIU_BINDKEYS` value into widget bindings. Entries are separated
/// by newlines and shaped `keyspec:widget`, e.g. `Ctrl+X f:niu_fzf_file`.
/// Blank lines and `#` comments are skipped; malformed entries are dropped.
/// Known native widget names map to editor events; any other name becomes a
/// shell-function widget resolved at trigger time.
pub fn parse_user_bindkeys(value: &str) -> Vec<NativeWidgetBinding> {
    value
        .lines()
        .map(str::trim)
        .filter(|entry| !entry.is_empty() && !entry.starts_with('#'))
        .filter_map(|entry| {
            let (key, widget) = entry.split_once(':')?;
            let key = key.trim();
            let widget = widget.trim();
            if key.is_empty() || widget.is_empty() {
                return None;
            }
            Some(NativeWidgetBinding {
                widget: widget.to_string(),
                function: None,
                key: Some(key.to_string()),
                keymap: None,
                source_file: None,
                line: None,
                origin: "user".to_string(),
            })
        })
        .collect()
}

/// First words the interactive REPL resolves itself, before engine command
/// resolution: `exit`/`logout` (exit trap) and `self-update`/`update-niubash`
/// (self-update hand-off). None of them exist on PATH or in the engine builtin
/// table, so the syntax highlighter consults this predicate to avoid painting
/// valid REPL commands as unknown tokens (niu#260).
pub fn is_repl_reserved_command(word: &str) -> bool {
    is_exit_word(word) || matches!(word, "self-update" | "update-niubash")
}

/// The session-terminating REPL words. Shared by the highlighter predicate
/// above and the `exit`/`logout` line parser so both agree on the vocabulary.
fn is_exit_word(word: &str) -> bool {
    matches!(word, "exit" | "logout")
}

/// Parse an interactive `exit`/`logout` line into the status the session must
/// end with (niubash D1: `exit 3` used to print only the builtin's output and
/// leave the REPL running, because the matcher required the whole line to be
/// exactly `exit`).
///
/// Returns `None` for every line the REPL must hand to the engine instead:
/// non-exit words, and argument shapes the engine diagnoses itself. The
/// engine follows bash 5.3, where `exit abc` reports `numeric argument
/// required` and `exit 3 4` reports `too many arguments`; both leave the
/// session alive, so they must not be intercepted here.
fn repl_exit_status(line: &str) -> Option<i32> {
    let line = line.trim();
    // `exit 3;` is ordinary interactive punctuation, not a command list.
    let line = line.strip_suffix(';').unwrap_or(line).trim_end();
    let mut words = line.split_whitespace();
    if !is_exit_word(words.next()?) {
        return None;
    }
    let mut args: Vec<&str> = words.collect();
    // GNU get_exitstat skips one leading `--` before the status word.
    if args.first() == Some(&"--") {
        args.remove(0);
    }
    match args.as_slice() {
        [] => Some(0),
        [status] => status
            .parse::<i128>()
            .ok()
            .map(|value| value.rem_euclid(256) as i32),
        _ => None,
    }
}

/// End the interactive session with `status`: run the EXIT trap (GNU
/// `exit_shell`), persist session history (a hard exit skips the history
/// provider's `Drop`), and leave with the trap's status as the process code.
fn end_repl_session(shell: &mut Shell, status: i32) -> ! {
    let code = shell.finish_with_exit_trap(status).unwrap_or(status);
    shell.flush_session_history();
    flush_repl_output();
    std::process::exit(code);
}

/// Extract the arguments of a `self-update` / `update-niubash` REPL command,
/// or `None` when the line is not one.
pub fn self_update_command_args(line: &str) -> Option<Vec<String>> {
    let mut parts = line.split_whitespace();
    match parts.next()? {
        "self-update" | "update-niubash" => Some(parts.map(str::to_string).collect()),
        _ => None,
    }
}

/// Hand `self-update` off to a child process of the current executable with
/// `--self-update`, then exit this shell so the installer can replace the
/// binary. Returns `None` when spawning is not possible; otherwise the child
/// exit code.
pub fn spawn_self_update(args: &[String]) -> Option<i32> {
    let exe = std::env::current_exe().ok()?;
    let mut command = std::process::Command::new(exe);
    command.arg("--self-update").args(args);
    let mut child = command.spawn().ok()?;
    crate::console_guard::adopt_orphan_console(child.id());
    let status = child.wait().ok()?;
    Some(status.code().unwrap_or(0))
}

/// Build a `Reedline` instance for the shell.
///
/// The shell is shared through the same `Rc<RefCell<Shell>>` bridge the REPL
/// uses, so the completer can run shell-function completions in the engine.
pub fn build_line_editor(shell: &Rc<RefCell<Shell>>) -> anyhow::Result<Reedline> {
    let shell_ref = shell.borrow();
    // Infallible since niubash#134: a history file that cannot be opened
    // (sandboxed restricted token, os error 5) degrades to an in-memory
    // history instead of aborting the interactive session.
    let mut history = LiveFileBackedHistory::with_mode(
        shell_ref.history_max_size,
        shell_ref.history_path.clone(),
        shell_ref.history_mode,
    );
    // Explicitly rebuild the editor history from disk at construction so a
    // fresh process starts with persisted cross-session entries.
    let _ = history.reload_from_file();

    let completer = NiubashCompleter::new(shell_ref.completion_state.clone());
    // Shell-function completions reach the engine through the main-thread
    // bridge installed here; reedline completers must stay `Send`.
    crate::shell::install_completion_bridge(shell);
    let menu_config = shell_ref.menu_config;

    // The GNU completion style redraws the prompt unchanged while its list
    // is up; reedline replaces the prompt indicator with the active menu's
    // marker, so the GNU menu carries the shell's own indicator (the same
    // one the fresh prompt will render for this edit mode) — the redrawn
    // line is pixel-identical, as GNU readline's is.
    let gnu_indicator = shell_ref
        .prompt
        .render_prompt_indicator(match shell_ref.editor_mode {
            EditorMode::Emacs => PromptEditMode::Emacs,
            EditorMode::Vi => PromptEditMode::Vi(PromptViMode::Insert),
        })
        .into_owned();

    let completion_menu = ReedlineMenu::WithCompleter {
        menu: configured_completion_menu(COMPLETION_MENU, menu_config, &gnu_indicator),
        completer: Box::new(completer),
    };
    let history_menu = ReedlineMenu::HistoryMenu(Box::new(configured_list_menu(
        HISTORY_MENU,
        menu_config.history_page_size,
        menu_config,
    )));

    let mut editor = Reedline::create()
        .with_validator(Box::new(ReplValidator))
        .with_history(Box::new(history))
        .with_history_exclusion_prefix(history_exclusion_prefix(
            shell_ref.history_ignore_space_prefixed,
        ))
        .with_menu(completion_menu)
        .with_menu(history_menu)
        // niubash#373 sub-item 2 (niubash#355 finding 4): a completion
        // candidate must land in the line, not hover as a menu preview.
        // With the engine default (`partial_completions = false`) the menu
        // activation splices NOTHING into the buffer, so `cat uniqfi<TAB>`
        // left the line at `cat uniqfi` with the completed text drawn only
        // as the menu's highlighted entry — a subsequent Enter executed the
        // uncompleted path. `partial_completions` gives every TAB the GNU
        // readline commit shape: the longest common prefix of the candidate
        // set is spliced into the line (a lone candidate commits outright —
        // bash's unique-match behavior), the grid still shows for an
        // ambiguous set, and Enter submits whatever is committed. The
        // history menu opts out upstream (its `can_partially_complete` is
        // `false`), so history search is untouched; `quick_completions`
        // stays OFF deliberately — its lone-candidate path simulates Enter
        // (accept AND execute), which is nushell's completion cell, not a
        // bash-like shell's TAB.
        .with_partial_completions(true)
        // niubash#202: bracketed paste (mode ?2004). Without it a terminal
        // paste of multiline text behaves as if the user typed each line
        // followed by Enter: every line runs its own prompt cycle (each
        // with a syntax-highlighted repaint, pre-prompt hooks and a
        // history flush — measured at ~130 ms/line for a 200-line paste)
        // and each line EXECUTES as it arrives, so a pasted heredoc or
        // multi-line construct never survives the paste. With bracketed
        // paste the terminal wraps the whole chunk in ESC[200~...ESC[201~,
        // crossterm delivers it as one Event::Paste, and reedline inserts
        // it into the edit buffer as a single unit (GNU bash paste
        // semantics: nothing executes until the user submits the buffer;
        // one repaint for the whole paste).
        //
        // Windows stays off: stock crossterm reads console input through
        // the Win32 console API and has no ANSI input parser, so ESC[200~
        // can never surface as crossterm's Event::Paste there (upstream:
        // crossterm-rs/crossterm#737; ConPTY consumes the mode markers
        // before any parser would see them — verified empirically while
        // triaging this issue). Instead the Windows path gets PSReadLine-
        // style paste-chunk detection ([`PasteChunkEditMode`] below): a
        // paste is recognized by its CRLF line-break signature (the queued
        // LF partner of every fed break) and folded into the edit buffer
        // without executing, the same buffered-until-Enter semantics the
        // bracketed-paste path gives Unix, while typed lines — a human's
        // Enter however fast, a driver's bare-CR send_line — submit
        // normally (niubash#284). Ctrl+V (PasteSystem, bound below)
        // remains the one-shot clipboard path.
        .use_bracketed_paste(cfg!(not(target_os = "windows")))
        .with_edit_mode(Box::new(PasteChunkEditMode::new(
            build_edit_mode(
                shell_ref.editor_mode,
                &shell_ref.native_widgets,
                &shell_ref.native_widget_bindings,
                &shell_ref.user_widget_bindings,
                &shell_ref.engine_bindings,
            ),
            menu_config.completion_style,
        )));

    if shell_ref.autosuggest.history_strategy_enabled() {
        editor = editor.with_hinter(Box::new(HistoryAutosuggestHinter::new(
            &shell_ref.autosuggest,
        )));
    }
    if shell_ref.syntax_highlighting.main_highlighter_enabled() {
        let functions = shell_ref.executor.functions_snapshot();
        let commands = shell_ref
            .aliases
            .keys()
            .map(String::as_str)
            .chain(functions.iter().map(String::as_str));
        editor = editor.with_highlighter(Box::new(
            NiubashSyntaxHighlighter::new_with_commands_and_state(
                &shell_ref.syntax_highlighting,
                commands,
                shell_ref.completion_state.clone(),
            ),
        ));
    }

    Ok(editor)
}

fn configured_list_menu(name: &str, page_size: usize, config: MenuConfig) -> ListMenu {
    ListMenu::default()
        .with_name(name)
        .with_page_size(page_size)
        .with_max_entry_lines(config.max_entry_lines)
        .with_only_buffer_difference(false)
}

/// Build the completion menu according to the configured style.
///
/// `gnu_indicator` is the shell's own prompt indicator, used as the GNU
/// menu's marker so the redrawn prompt stays identical while its list is up
/// (see the comment at the call site).
fn configured_completion_menu(
    name: &str,
    config: MenuConfig,
    gnu_indicator: &str,
) -> Box<dyn reedline::Menu> {
    match config.completion_style {
        CompletionStyle::Gnu => {
            // GNU readline literal semantics (opt-in, `completion.style gnu`):
            // TAB #1 completes the common prefix, TAB #2 on an unchanged line
            // prints the candidates columnar and the prompt is repainted — a
            // printed list, no menu navigation, no arrow-key capture, no
            // popup. See `completion::gnu_menu` for the state machine.
            let menu = crate::completion::gnu_menu::GnuMenu::default()
                .with_name(name)
                .with_marker(gnu_indicator);
            Box::new(menu)
        }
        CompletionStyle::Ide => {
            // Multi-column popup with descriptions (VS Code style). Column
            // count adapts to the terminal width; descriptions render to the
            // right of (or under) the value column.
            let menu = reedline::IdeMenu::default()
                .with_name(name)
                .with_min_completion_width(20)
                .with_max_completion_width(48)
                .with_max_completion_height(config.completion_page_size.min(20) as u16)
                .with_padding(1)
                .with_description_mode(reedline::DescriptionMode::PreferRight);
            Box::new(menu)
        }
        CompletionStyle::Column => {
            let page_cols = if config.completion_page_size <= 4 {
                2
            } else if config.completion_page_size <= 9 {
                3
            } else {
                4
            };
            Box::new(
                ColumnarMenu::default()
                    .with_name(name)
                    .with_columns(page_cols)
                    .with_only_buffer_difference(false),
            )
        }
        CompletionStyle::List => Box::new(configured_list_menu(
            name,
            config.completion_page_size,
            config,
        )),
        CompletionStyle::Inline => {
            // Inline mode: use ListMenu with page_size 1 to highlight one at a time
            Box::new(
                ListMenu::default()
                    .with_name(name)
                    .with_page_size(1)
                    .with_max_entry_lines(config.max_entry_lines)
                    .with_only_buffer_difference(false),
            )
        }
    }
}

fn history_exclusion_prefix(ignore_space_prefixed: bool) -> Option<String> {
    ignore_space_prefixed.then(|| " ".to_string())
}

/// Apply a shell-function widget's editor outcome to the suspended editor.
/// The editor resumes with the new buffer on the next `read_line` call.
fn apply_widget_outcome(line_editor: &mut Reedline, outcome: &crate::shell::WidgetOutcome) {
    if let Some(buffer) = &outcome.buffer {
        line_editor.run_edit_commands(&[
            EditCommand::Clear,
            EditCommand::InsertString(buffer.clone()),
        ]);
    }
    if let Some(position) = outcome.cursor {
        line_editor.run_edit_commands(&[EditCommand::MoveToPosition {
            position,
            select: false,
        }]);
    }
}

/// What a host-command sentinel (`__niu_widget` / `__niu_bindx`) wants
/// from the REPL after running in the engine.
enum SentinelOutcome {
    /// Payload was not one of ours: fall through to ordinary handling.
    NotSentinel,
    /// The widget ran and resumed editing (bind -x always lands here —
    /// GNU returns to the edit line, bashline.c:4701-4706).
    Resumed,
    /// The widget produced a buffer to submit (`NIU_WIDGET_ACCEPT=1`);
    /// `None` keeps the buffer as it stood.
    Accept(Option<String>),
}

/// Run a widget or engine bind -x sentinel payload against the engine,
/// applying the editor outcome to the suspended line editor.
fn run_host_sentinel(
    shell: &Rc<RefCell<Shell>>,
    line_editor: &mut Reedline,
    payload: &str,
) -> SentinelOutcome {
    if let Some(bindx) = BindXInvocation::parse(payload) {
        let editor_buffer = line_editor.current_buffer_contents().to_string();
        let editor_cursor = line_editor.current_insertion_point();
        let outcome =
            shell
                .borrow_mut()
                .run_bind_x_command(bindx.index, &editor_buffer, editor_cursor);
        apply_widget_outcome(line_editor, &outcome);
        return SentinelOutcome::Resumed;
    }
    let Some(widget) = WidgetInvocation::parse(payload) else {
        return SentinelOutcome::NotSentinel;
    };
    if !shell.borrow().widget_function_available(&widget.function) {
        return SentinelOutcome::NotSentinel;
    }
    let editor_buffer = line_editor.current_buffer_contents().to_string();
    let editor_cursor = line_editor.current_insertion_point();
    let outcome =
        shell
            .borrow_mut()
            .run_widget_function(&widget.function, &editor_buffer, editor_cursor);
    if outcome.accept {
        SentinelOutcome::Accept(outcome.buffer)
    } else {
        apply_widget_outcome(line_editor, &outcome);
        SentinelOutcome::Resumed
    }
}

fn build_edit_mode(
    mode: EditorMode,
    native_widgets: &NativeWidgetConfig,
    native_widget_bindings: &[NativeWidgetBinding],
    user_widget_bindings: &[NativeWidgetBinding],
    engine_bindings: &[rubash::shell::bind_registry::BindEntry],
) -> Box<dyn EditMode> {
    match mode {
        EditorMode::Emacs => {
            let mut keybindings = default_emacs_keybindings();
            add_menu_keybindings(&mut keybindings);
            add_system_clipboard_keybindings(&mut keybindings);
            add_native_widget_keybindings(
                &mut keybindings,
                NativeKeymapTarget::Emacs,
                native_widgets,
                native_widget_bindings,
            );
            add_bundle_widget_keybindings(
                &mut keybindings,
                NativeKeymapTarget::Emacs,
                native_widgets,
                native_widget_bindings,
            );
            add_user_widget_keybindings(&mut keybindings, user_widget_bindings);
            warn_unusable_engine_bindings(add_engine_bind_keybindings(
                &mut keybindings,
                NativeKeymapTarget::Emacs,
                engine_bindings,
            ));
            Box::new(Emacs::new(keybindings))
        }
        EditorMode::Vi => {
            let mut insert_keybindings = default_vi_insert_keybindings();
            let mut normal_keybindings = default_vi_normal_keybindings();
            add_menu_keybindings(&mut insert_keybindings);
            add_menu_keybindings(&mut normal_keybindings);
            add_system_clipboard_keybindings(&mut insert_keybindings);
            add_system_clipboard_keybindings(&mut normal_keybindings);
            add_native_widget_keybindings(
                &mut insert_keybindings,
                NativeKeymapTarget::ViInsert,
                native_widgets,
                native_widget_bindings,
            );
            add_native_widget_keybindings(
                &mut normal_keybindings,
                NativeKeymapTarget::ViNormal,
                native_widgets,
                native_widget_bindings,
            );
            add_bundle_widget_keybindings(
                &mut insert_keybindings,
                NativeKeymapTarget::ViInsert,
                native_widgets,
                native_widget_bindings,
            );
            add_bundle_widget_keybindings(
                &mut normal_keybindings,
                NativeKeymapTarget::ViNormal,
                native_widgets,
                native_widget_bindings,
            );
            add_user_widget_keybindings(&mut insert_keybindings, user_widget_bindings);
            add_user_widget_keybindings(&mut normal_keybindings, user_widget_bindings);
            warn_unusable_engine_bindings(add_engine_bind_keybindings(
                &mut insert_keybindings,
                NativeKeymapTarget::ViInsert,
                engine_bindings,
            ));
            warn_unusable_engine_bindings(add_engine_bind_keybindings(
                &mut normal_keybindings,
                NativeKeymapTarget::ViNormal,
                engine_bindings,
            ));
            Box::new(Vi::new(
                insert_keybindings,
                normal_keybindings,
                // reedline 0.52 added visual mode with its own keybinding
                // set; niubash has never bound visual keys of its own, so
                // the upstream defaults are the faithful choice.
                default_vi_visual_keybindings(),
            ))
        }
    }
}

/// PSReadLine-style paste-chunk detection for the Windows console input path
/// (niubash#202).
///
/// On Windows, crossterm reads console input through the Win32 console API
/// and has no ANSI input parser, while ConPTY swallows the bracketed-paste
/// markers before any parser could see them (crossterm-rs/crossterm#737 —
/// see the builder comment in [`build_line_editor`]). A terminal paste
/// therefore arrives as per-line `KeyCode::Char` / `KeyCode::Enter` key
/// events, and reedline's read loop (`read_line_helper`) stops draining at
/// each bare `Enter` — so each pasted line lands as its own event batch
/// ending in `Enter`, which reedline treats exactly like a typed submit:
/// every line repaints a full prompt cycle and EXECUTES as it arrives.
///
/// PSReadLine solves the same problem on the same constrained Console API
/// with an arrival-burst heuristic: keystrokes that arrive faster than any
/// human types — especially with newlines among them — are PASTE DATA, and
/// only a newline that arrives on its own submits. This wrapper reproduces
/// that at the `EditMode::parse_event` seam, the one place reedline hands
/// every raw crossterm event to host code before dispatch — with one
/// decisive refinement (niubash#284): the submit-versus-paste decision at a
/// bare `Enter` is STRUCTURAL, not timing-based.
///
/// * A machine-fed paste announces itself by its line-break signature. Two
///   shapes reach this path (see [`PASTE_LF_PROBE`]): the ConPTY text-feed
///   shape delivers every break as a CRLF pair — the CR arrives as a bare
///   `Enter`, its LF as `Enter + CONTROL` (the console reports Ctrl+J for
///   LF) — while the Windows Terminal paste shape (niubash#341) delivers a
///   KEY_EVENT record per pasted character with a lone bare CR per break
///   and `\r\r` for an empty line, no LF ever. A typed submit — human,
///   ConPTY driver `send_line`, expect script, `send-keys` — is a lone bare
///   CR with NOTHING behind it. So when a bare Enter that would OPEN a
///   chunk is dispatched, the wrapper looks ahead into the input queue
///   ([`PASTE_LF_PROBE`] budget): an `Enter + CONTROL` or a bare CR queued
///   behind the CR proves paste data — the break(s) fold into the pending
///   chunk and the Enter never reaches reedline. Any other queued key
///   (typeahead text, a chord) or silence proves a typed submit: the Enter
///   passes through and the lookahead's read parks in-process, so reedline
///   sees it, in order, on the next dispatch — and at the submit itself the
///   parked keys are written back to the console queue's head, so the
///   accepted command's own `read` sees them too (niubash#417). The LF of a
///   CRLF is decisive
///   by itself, too: a bare `Enter + CONTROL` with no chunk pending opens
///   one directly (humans do not submit with Ctrl+J; a slow paste whose CR
///   already slipped through still gets absorbed from its first LF on).
///   This is what rescues the fast typist the pure 50 ms heuristic
///   swallowed: "last character, then Enter" within one burst is the
///   canonical TYPING shape, and its queue is empty — it submits, every
///   time.
/// * While a chunk is pending the paste is quiet: further text keys and
///   pasted line breaks of the burst are appended to the pending buffer and
///   translated to `ReedlineEvent::None`, so reedline does not repaint (or
///   execute) per pasted line — PSReadLine's paste-silence behavior. A
///   pending chunk does NOT swallow Enters blindly (niubash#308): a bare CR
///   whose queue holds no partner — silence or typeahead behind it —
///   proves the pasted block complete and the Enter the user's submit, so
///   the chunk flushes and submits in one batch. Only the breaks still
///   INSIDE the block (CRLF pairs, bare LFs, the CRs of `\r\r` empty
///   lines) stay data.
/// * When a gap of [`PASTE_CHUNK_GAP`] marks the end of the burst, the
///   whole buffer is emitted as ONE `EditCommand::InsertString` (one
///   repaint for the entire paste), and a subsequent bare Enter submits the
///   buffered chunk exactly once — the paste's trailing newline never
///   executes by itself.
/// * Non-key events (the focus flicker ConPTY emits between pasted lines,
///   mouse, ...) translate to `None` upstream; the lookahead skips over
///   them while hunting for the LF partner and drops them (they dispatch
///   to nothing), and they neither break a pending burst nor flush it.
///
/// The 50 ms gap keeps one job: marking burst boundaries for the
/// flush-on-gap commit. It no longer decides submits — a decision that
/// misfired on every "text + Enter" burst (niubash#284: the first Enter of
/// every machine-fed line, script, and fast human line was silently
/// absorbed and the command needed a second Enter).
///
/// Every other event is delegated unchanged to the wrapped
/// [`EditMode`](Emacs)/[`EditMode`](Vi), so keybindings, widgets and the
/// vi/emacs state machines behave identically. Only the Windows console
/// path enables the rewrite ([`PASTE_CHUNK_DETECTION`]); Unix keeps the
/// real bracketed paste of niubash#234, whose `Event::Paste` never emits a
/// bare `Enter` and is untouched by this logic. The Ctrl+V
/// (`EditCommand::PasteSystem`) path never produces Enter events either, so
/// there is no double-insert interaction between the two clipboard routes.
struct PasteChunkEditMode {
    inner: Box<dyn EditMode>,
    /// Text buffered for the paste burst in flight. `Some(_)` means a
    /// burst was detected and its events are being absorbed quietly.
    pending: Option<String>,
    /// When the previous key event was parsed; the burst clock. `None`
    /// means "no burst in progress".
    last_event: Option<Instant>,
    /// Events the Enter lookahead read but did not consume (typeahead keys
    /// that proved the Enter a typed submit). Dispatched FIFO before the
    /// next raw event, so reedline never loses or reorders them. They
    /// cannot ride along on the Enter's own `Multiple`: reedline drops
    /// `Multiple` sub-events after the first `Exits` (engine.rs
    /// `handle_editor_event`), and an Enter exits. At a SUBMIT — a batch
    /// whose dispatch hands the shell a line — whatever remains parked goes
    /// back to the input stream's head ([`Unread`], niubash#417): the
    /// accepted command's engine `read` reads the kernel queue next, and
    /// events left in-process would be unreachable to it until the next
    /// prompt.
    pushback: VecDeque<crossterm29::event::Event>,
    /// Where the lookahead reads the next queued event: the live console
    /// queue (`crossterm::event::poll` + `read`) in production, a scripted
    /// queue in tests. Same thread as reedline's own read loop — on the
    /// Windows console path crossterm has no reader thread, so the probe
    /// cannot race a producer inside the process.
    lookahead: Lookahead,
    /// niubash#373: whether a completion menu may still be alive — a TAB
    /// opened the completion flow and no submit/chord has ended that
    /// window. Gates the GNU style's Enter dismissal prefix (see
    /// [`Self::delegate`]): the prefix must never ride an Enter inside an
    /// active history search, where reedline routes whole `Multiple`
    /// batches to the search handler that drops them (`Multiple(_)` →
    /// `Inapplicable`) — that was the Ctrl+R dead key. A CONTROL chord
    /// always ends the window: the verb it invokes (search, cancel, EOF)
    /// owns the next Enter.
    menu_possible: bool,
    /// niubash#486: whether an Enter inside the TAB window keeps the #373
    /// dismissal prefix. Only the GNU printed list needs it: it reports
    /// `is_active` while its candidate list is up but has no selectable
    /// candidate (its `replace_in_buffer` is a deliberate no-op), so
    /// reedline's `MenuAccept` would take the Enter and swallow it — a
    /// dead key where GNU readline runs the line as typed. The
    /// interactive menus (column/list/ide/inline) take reedline's menu
    /// first-refusal natively: Enter commits the highlighted candidate
    /// into the editable line — the 1.1.0 behavior niubash#486 restores —
    /// and a second Enter submits.
    enter_dismisses_menu: bool,
    /// niubash#420: put probe-consumed events BACK into the input stream.
    /// The live console cannot unread, so the events are written back into
    /// the kernel input queue (`WriteConsoleInputW`, the #417 reinjection
    /// face). MSDN pins WriteConsoleInput as APPENDING behind the pending
    /// events — a tail write it cannot do head placement with on its own.
    /// The face therefore compensates (`requeue_console_input`): it drains
    /// the pending tail first and rewrites it BEHIND the re-queued records,
    /// so the events land at the head slot, the position the probe took
    /// them from. That ordering is load-bearing for the #417 submit
    /// writeback (a plain append would let line 2's `ead...` run before
    /// the parked `r` of `read` and shred the burst); #420's shapes were
    /// benign under the plain append too (the canonical wedge requeues
    /// onto an empty queue, tail = head) and stay exact under the
    /// compensated one. The scripted test queue pushes FRONT: it models
    /// the head slot the production face now guarantees.
    unread: Unread,
}

/// The lookahead source of [`PasteChunkEditMode`]: yield the next queued
/// input event within `budget`, or `None` on silence.
type Lookahead = Box<dyn FnMut(Duration) -> Option<crossterm29::event::Event> + Send>;

/// The requeue source of [`PasteChunkEditMode`]: hand events the probe
/// consumed but the submit decision did NOT need back into the
/// input stream, verbatim and in order — #420's partner CR and
/// #417's submit-parked typeahead both ride it. Raw `WriteConsoleInput`
/// appends at the queue tail (MSDN semantics — see the `unread` field
/// doc); the production face wraps that primitive with a drain-and-rewrite
/// so the re-queued events land at the head slot, the position the probe
/// took them from. The scripted test queue models the head slot directly.
type Unread = Box<dyn FnMut(&[crossterm29::event::Event]) + Send>;

/// The production lookahead: wait up to `budget` for the next console input
/// event and read it. A non-console stdin (pipes, tests) makes `poll` fail —
/// that is "no paste partner" for every caller here, so map it to silence.
fn live_lookahead(budget: Duration) -> Option<crossterm29::event::Event> {
    if crossterm29::event::poll(budget).unwrap_or(false) {
        crossterm29::event::read().ok()
    } else {
        None
    }
}

/// The production requeue: write the events back into the kernel console
/// input queue, so the NEXT read (the accepted command's engine `read`, or
/// reedline's own) sees them in order. Two callers feed it: #420's partner-CR
/// requeue at chunk-open (a bare Enter the probe consumed) and #417's submit
/// writeback (the typeahead keys the probe parked through a submitted
/// batch). A non-console stdin or a failed write drops the events: the queue
/// they came from does not exist (the probe only ever runs against one) or
/// refused them.
fn live_unread(events: &[crossterm29::event::Event]) {
    #[cfg(windows)]
    win_unread(events);
    #[cfg(not(windows))]
    let _ = events;
}

/// One synthesized console key-down record's payload — platform-pure so the
/// crossterm→`INPUT_RECORD` mapping is unit-testable off-Windows. Mirrors
/// what conhost synthesizes for the same keystroke, so every reader on this
/// path (reedline's Windows parser, the engine's console reader) decodes the
/// re-injected record back into the event that was consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(any(windows, test))]
struct KeyRecordFields {
    /// `wVirtualKeyCode`: the key identity for the keys every reader takes
    /// from the VK (arrows, Backspace, Tab, ...), the base letter of a
    /// control chord (Ctrl+R arrives as `uChar` 0x12 / VK 'R'), and the
    /// letter/digit of printable keys; 0 where the readers decode from
    /// `uChar` directly (non-ASCII, symbols).
    virtual_key_code: u16,
    /// `uChar.UnicodeChar`: one record per UTF-16 unit, so a non-BMP
    /// character round-trips through crossterm's surrogate assembly exactly
    /// as the terminal delivered it.
    unicode_char: u16,
    /// `dwControlKeyState`: SHIFT/LEFT_CTRL/LEFT_ALT — the subset the
    /// console parser itself produces.
    control_key_state: u32,
}

#[cfg(any(windows, test))]
const SHIFT_PRESSED_FLAG: u32 = 0x0010;
#[cfg(any(windows, test))]
const LEFT_ALT_PRESSED_FLAG: u32 = 0x0002;
#[cfg(any(windows, test))]
const LEFT_CTRL_PRESSED_FLAG: u32 = 0x0008;

/// Map a key event the probe consumed into the console key-down records that
/// decode back to it. `wVirtualScanCode` is omitted (always 0): every reader
/// on this path decodes from VK + `uChar` — crossterm's `parse_key_event_record`
/// and the engine's `translate_console_key` never read the scan. Events the
/// console parser cannot produce (releases never park in the pushback, and
/// media/modifier keys never parse) map to nothing — they were never
/// consumable here to begin with.
#[cfg(any(windows, test))]
fn key_press_records(key: &crossterm29::event::KeyEvent) -> Vec<KeyRecordFields> {
    use crossterm29::event::{KeyCode, KeyEventKind, KeyModifiers};

    // Releases carry no text and no action upstream (the detector delegates
    // them to `None`); the probe only ever parks Presses.
    if key.kind != KeyEventKind::Press {
        return Vec::new();
    }
    let mut state = 0u32;
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        state |= SHIFT_PRESSED_FLAG;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        state |= LEFT_CTRL_PRESSED_FLAG;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        state |= LEFT_ALT_PRESSED_FLAG;
    }
    // VK codes mirror crossterm 0.29's `parse_key_event_record` table (the
    // inverse mapping): exactly the VKs that parser matches on.
    let record = |vk: u16, ch: u16| KeyRecordFields {
        virtual_key_code: vk,
        unicode_char: ch,
        control_key_state: state,
    };
    match key.code {
        KeyCode::Backspace => vec![record(0x08, 0x08)],
        KeyCode::Esc => vec![record(0x1B, 0x1B)],
        KeyCode::Enter => {
            // The LF partner shape (`Enter + CONTROL`) rides VK_RETURN with
            // the Ctrl state and the LF char, the record ConPTY synthesizes
            // for a fed 0x0A byte. A BARE CR never reaches this mapping
            // while parked (the #341 pushback invariant) — the #420 partner
            // path requeues its own record — but the arm stays total.
            vec![record(
                0x0D,
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    0x0A
                } else {
                    0x0D
                },
            )]
        }
        KeyCode::Tab => vec![record(0x09, 0x09)],
        KeyCode::BackTab => {
            // Shift+Tab: the parser decides BackTab from VK_TAB + SHIFT.
            state |= SHIFT_PRESSED_FLAG;
            vec![KeyRecordFields {
                virtual_key_code: 0x09,
                unicode_char: 0x09,
                control_key_state: state,
            }]
        }
        KeyCode::Left => vec![record(0x25, 0)],
        KeyCode::Right => vec![record(0x27, 0)],
        KeyCode::Up => vec![record(0x26, 0)],
        KeyCode::Down => vec![record(0x28, 0)],
        KeyCode::Home => vec![record(0x24, 0)],
        KeyCode::End => vec![record(0x23, 0)],
        KeyCode::PageUp => vec![record(0x21, 0)],
        KeyCode::PageDown => vec![record(0x22, 0)],
        KeyCode::Insert => vec![record(0x2D, 0)],
        KeyCode::Delete => vec![record(0x2E, 0)],
        KeyCode::F(n) if (1..=24).contains(&n) => vec![record(0x70 + u16::from(n - 1), 0)],
        KeyCode::Char(ch) => {
            // Control chords: conhost encodes the control code in uChar and
            // the base letter in the VK (Ctrl+R → 0x12 / 'R' /
            // LEFT_CTRL_PRESSED) — crossterm's parser takes the 0x01..=0x1F
            // uChar arm and re-derives the char from the VK. What the probe
            // parks is that parse's OUTPUT: crossterm 0.29 hands the chord
            // over as Char(letter) + CONTROL (its `get_char_for_key` runs a
            // zeroed key state through ToUnicode, so the ctrl state never
            // taints the letter), which is why an arm keyed on the
            // control-code char was dead code on Windows (niubash#434): the
            // chord fell through to the printable arm and the writeback
            // carried uChar 'r' — a raw-byte reader (the accepted command's
            // engine console `read`, the #417 submit writeback's consumer)
            // saw 'r' where the user's keystroke was 0x12. Both delivery
            // shapes map to the one conhost record: event-level readers
            // re-derive the chord from VK + state, byte-level readers get
            // the original 0x01..=0x1A control byte.
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let control_code = match ch {
                    'a'..='z' | 'A'..='Z' => {
                        u16::from(ch.to_ascii_lowercase() as u8) - u16::from(b'a') + 1
                    }
                    '\u{1}'..='\u{1a}' => ch as u16,
                    _ => 0,
                };
                if control_code != 0 {
                    return vec![record(control_code + u16::from(b'A') - 1, control_code)];
                }
            }
            // Printable characters: the readers decode from uChar; the VK
            // carries the letter/digit identity where one exists (cosmetic
            // for the parser, exact for a layout-aware reader).
            let vk = match ch {
                'a'..='z' => ch.to_ascii_uppercase() as u16,
                'A'..='Z' | '0'..='9' => ch as u16,
                _ => 0,
            };
            // One char is at most two UTF-16 units (a surrogate pair); one
            // record per unit round-trips through crossterm's surrogate
            // assembly exactly as the terminal delivered it.
            let mut units = [0u16; 2];
            ch.encode_utf16(&mut units)
                .iter()
                .map(|unit| record(vk, *unit))
                .collect()
        }
        // KeyCode::Null and the media/modifier keys never parse from console
        // records (crossterm's Windows table maps their VKs to None), so
        // they can never be parked here. Map to nothing rather than guess.
        _ => Vec::new(),
    }
}

#[cfg(windows)]
fn win_unread(events: &[crossterm29::event::Event]) {
    use windows_sys::Win32::System::Console::{
        INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0,
    };

    let records: Vec<INPUT_RECORD> = events
        .iter()
        .filter_map(|event| match event {
            // Non-key events (ConPTY focus flicker, mouse) are inert
            // upstream — dropping them is observably identical to
            // dispatching them (the probe's own skip, same justification).
            crossterm29::event::Event::Key(key) => Some(key),
            _ => None,
        })
        .flat_map(key_press_records)
        .map(|fields| INPUT_RECORD {
            EventType: KEY_EVENT as u16,
            Event: INPUT_RECORD_0 {
                KeyEvent: KEY_EVENT_RECORD {
                    bKeyDown: 1,
                    wRepeatCount: 1,
                    wVirtualKeyCode: fields.virtual_key_code,
                    wVirtualScanCode: 0,
                    uChar: KEY_EVENT_RECORD_0 {
                        UnicodeChar: fields.unicode_char,
                    },
                    dwControlKeyState: fields.control_key_state,
                },
            },
        })
        .collect();
    // The console handle only exists while stdin is the interactive console
    // — the only shape that ever reaches the probe.
    crate::typeahead_guard::requeue_console_input(&records);
}

impl PasteChunkEditMode {
    fn new(inner: Box<dyn EditMode>, completion_style: CompletionStyle) -> Self {
        Self::with_lookahead(
            inner,
            Box::new(live_lookahead),
            Box::new(live_unread),
            completion_style,
        )
    }

    /// The test constructor: a scripted lookahead queue in place of the
    /// live console queue. The requeue pushes FRONT — the same head slot
    /// the production face guarantees by draining the pending tail and
    /// rewriting it behind the re-queued records (see the `unread` field
    /// doc); the scripted queue models the slot directly, without the
    /// drain-rewrite dance.
    #[cfg(test)]
    fn with_scripted_queue(
        inner: Box<dyn EditMode>,
        queue: std::sync::Arc<std::sync::Mutex<VecDeque<crossterm29::event::Event>>>,
        completion_style: CompletionStyle,
    ) -> Self {
        let unread_queue = std::sync::Arc::clone(&queue);
        Self::with_lookahead(
            inner,
            Box::new(move |budget| {
                let _ = budget;
                queue.lock().expect("test queue").pop_front()
            }),
            Box::new(move |events| {
                let mut queue = unread_queue.lock().expect("test queue");
                for event in events.iter().rev() {
                    queue.push_front(event.clone());
                }
            }),
            completion_style,
        )
    }

    fn with_lookahead(
        inner: Box<dyn EditMode>,
        lookahead: Lookahead,
        unread: Unread,
        completion_style: CompletionStyle,
    ) -> Self {
        Self {
            inner,
            pending: None,
            last_event: None,
            pushback: VecDeque::new(),
            lookahead,
            menu_possible: false,
            enter_dismisses_menu: matches!(completion_style, CompletionStyle::Gnu),
            unread,
        }
    }

    /// The pending chunk, if any, as the `InsertString` that lands it in
    /// the edit buffer (and triggers the paste's single repaint).
    fn take_pending_event(&mut self) -> Option<ReedlineEvent> {
        self.pending
            .take()
            .map(|chunk| edit_event(EditCommand::InsertString(chunk)))
    }

    /// The next queued input event, drained from the pushback first so a
    /// lookahead never reads past what it already holds.
    fn next_queued(&mut self, budget: Duration) -> Option<crossterm29::event::Event> {
        if let Some(event) = self.pushback.pop_front() {
            return Some(event);
        }
        (self.lookahead)(budget)
    }

    /// Decide a bare Enter that would OPEN a paste chunk: does it carry the
    /// paste signature? Returns `true` when the Enter is paste data (the
    /// chunk is opened with its line break; an LF pair partner is consumed
    /// with it), `false` when it is a typed submit.
    ///
    /// niubash#420: a bare CR behind a bare CR is NO LONGER paste evidence
    /// here. The `\r\r` pair is indistinguishable, at the queue level,
    /// between the WT KEY_EVENT paste's empty line (niubash#341) and TWO
    /// TYPEAHEAD SUBMITS in one pty write — a driver's `send("cmd\r\r")`,
    /// an expect script's back-to-back Enters, the Ctrl+R accept-Enter
    /// followed by the submit-Enter. Folding it swallowed the first submit:
    /// the line echoed and parked in the continuation buffer forever (the
    /// shared-driver wedge of PR #418's Ctrl+R E2E). GNU typeahead semantics
    /// win: the first CR SUBMITS, and the partner CR — already consumed by
    /// the probe — goes back into the input stream (the #417 requeue face;
    /// the drain-and-rewrite behind it lands the CR at the queue's head
    /// slot, the next submit nothing has to drain past) to submit the
    /// empty line, or to accept+submit the Ctrl+R match, on its own
    /// dispatch. A chunk only opens on paste evidence now: the LF partner
    /// (humans never send Ctrl+J), or an already-pending chunk (see the
    /// mid-chunk fold below).
    fn enter_opens_chunk(&mut self, enter: &crossterm29::event::KeyEvent) -> bool {
        // A bare LF (Enter + CONTROL) with no chunk pending is a paste
        // newline by itself: humans submit with Enter, never Ctrl+J, while
        // every machine-fed line break is a CR, an LF, or the pair — and
        // the LF also catches the slow paste whose CR already slipped
        // through as a (correct) submit.
        if matches!(enter.modifiers, crossterm29::event::KeyModifiers::CONTROL) {
            self.pending = Some(String::from("\n"));
            return true;
        }
        // A bare CR: paste only when an LF partner is queued right behind
        // it (the probe consumes the partner, so the pair is complete
        // here). A CR partner is typeahead: requeue it (verbatim, ordered)
        // and submit.
        match self.cr_paste_partner() {
            PastePartner::LfPartner => {
                self.pending = Some(String::from("\n"));
                true
            }
            PastePartner::CrPartner(partner) => {
                (self.unread)(&[partner]);
                false
            }
            PastePartner::None => false,
        }
    }

    /// The structural paste test for a bare CR (niubash#284/#308): what is
    /// queued behind it? The probe consumes a partner when found (an LF
    /// pair partner, or the CR of a `\r\r` empty line); any other key event
    /// — typeahead text, a chord — is parked in the pushback (and at the
    /// submit written back to the stream's head, niubash#417) so nothing is
    /// lost or reordered, and the CR is a typed submit; so is
    /// silence. Inert non-key events (ConPTY focus flicker) are skipped
    /// while hunting: they dispatch to nothing upstream. The probe is SOUND
    /// because reedline's own batch loop stops every batch at a bare Enter
    /// (reedline 0.50.0 `src/engine.rs:1052-1067`, the `completed()`
    /// helper), so everything behind a dispatched CR is still queued here —
    /// the CR's partner cannot have been consumed ahead of it.
    ///
    /// THE PUSHBACK INVARIANT (niubash#341): the probe never pushes a bare
    /// CR back. A bare CR left in the pushback crosses reedline's batch
    /// boundary: the next batch converts the pushback first
    /// (`process_input_batch`), the CR becomes a batch-leading
    /// `ReedlineEvent::Enter`, and reedline's dispatch returns at the first
    /// `Exits` — dropping the very text events reedline queued after it
    /// (engine.rs:1246-1258). Those raw events are already consumed from
    /// the console: the next line's first bytes would vanish (`if is_even
    /// 10; then` executing as `_even 10; then`). A CR behind a CR is paste
    /// data — the WT empty line — never "typed submit evidence".
    fn cr_paste_partner(&mut self) -> PastePartner {
        let deadline = Instant::now() + PASTE_LF_PROBE;
        loop {
            let budget = deadline.saturating_duration_since(Instant::now());
            let Some(event) = self.next_queued(budget) else {
                return PastePartner::None;
            };
            match event {
                crossterm29::event::Event::Key(key)
                    if key.kind == crossterm29::event::KeyEventKind::Press =>
                {
                    let is_lf_partner = matches!(
                        key,
                        crossterm29::event::KeyEvent {
                            code: crossterm29::event::KeyCode::Enter,
                            modifiers: crossterm29::event::KeyModifiers::CONTROL,
                            kind: crossterm29::event::KeyEventKind::Press,
                            ..
                        }
                    );
                    if is_lf_partner {
                        return PastePartner::LfPartner;
                    }
                    if is_bare_enter(&key) {
                        // niubash#341: a bare CR behind a bare CR is the
                        // KEY_EVENT paste's empty line. Consumed here — but
                        // niubash#420: the SAME bytes are two typeahead
                        // submits, so the caller decides by paste evidence:
                        // mid-chunk the pair folds as data, at chunk-open
                        // the partner is requeued verbatim and this CR is a
                        // typed submit. (Pushing it into the pushback would
                        // plant the batch-crossing Enter described above —
                        // the requeue goes to the input stream's head
                        // instead, outside reedline's batch.)
                        return PastePartner::CrPartner(crossterm29::event::Event::Key(key));
                    }
                    self.pushback.push_back(crossterm29::event::Event::Key(key));
                    // Parked only until this batch's boundary: a submit
                    // dispatches it back to the input stream's head
                    // (niubash#417), any other terminal hands it to the
                    // next batch's drain — both without loss or reorder.
                    return PastePartner::None;
                }
                // Non-key events are inert upstream; dropping them here is
                // observably identical to dispatching them.
                _ => {}
            }
            if Instant::now() >= deadline {
                return PastePartner::None;
            }
        }
    }
}

/// What the paste-signature probe ([`PasteChunkEditMode::cr_paste_partner`])
/// found queued behind a bare CR.
enum PastePartner {
    /// `Enter + CONTROL`: the CR's LF pair partner (the ConPTY CRLF feed
    /// shape — the fed LF surfaces as Ctrl+J). The pair folds ONE line
    /// break.
    LfPartner,
    /// A bare CR: the Windows Terminal KEY_EVENT empty-line shape
    /// (niubash#341) OR two typeahead submits in one write (niubash#420 —
    /// indistinguishable at the queue level). The probe consumed the
    /// partner CR and hands it back here; the caller decides: mid-chunk
    /// (paste already proven) it folds as a second line break, at
    /// chunk-open it is requeued verbatim and the CR is a typed submit.
    CrPartner(crossterm29::event::Event),
    /// Any other queued Press key (typeahead text, a chord) or silence:
    /// typed-submit evidence. The key is parked in the pushback and rides
    /// the submit back to the input stream's head (niubash#417), in order,
    /// for the accepted command's `read` — or the next dispatch if no
    /// submit follows.
    None,
}

/// A bare text key: the only thing a paste burst is made of.
fn is_plain_text_key(key: &crossterm29::event::KeyEvent) -> bool {
    matches!(
        key,
        crossterm29::event::KeyEvent {
            code: crossterm29::event::KeyCode::Char(_),
            modifiers: crossterm29::event::KeyModifiers::NONE
                | crossterm29::event::KeyModifiers::SHIFT,
            kind: crossterm29::event::KeyEventKind::Press,
            ..
        }
    )
}

/// A newline key — what both a typed submit and a pasted line break look
/// like on the Windows console path. The CR of a pasted CRLF arrives as a
/// bare Enter; its LF arrives as `Enter + CONTROL` (the console reports
/// Ctrl+J for LF), which must be paste data too — reedline's default
/// keymaps bind no Ctrl+Enter/Ctrl+J action, so folding it during a burst
/// costs nothing. All other Enter modifiers stay chords.
fn is_bare_enter(key: &crossterm29::event::KeyEvent) -> bool {
    matches!(
        key,
        crossterm29::event::KeyEvent {
            code: crossterm29::event::KeyCode::Enter,
            modifiers: crossterm29::event::KeyModifiers::NONE
                | crossterm29::event::KeyModifiers::CONTROL,
            kind: crossterm29::event::KeyEventKind::Press,
            ..
        }
    )
}

/// A bare Enter key press with no chord — the Ctrl+J (LF) shape is paste
/// data on the chunk paths and must never dismiss a menu.
fn is_bare_enter_press(raw: &crossterm29::event::Event) -> bool {
    matches!(
        raw,
        crossterm29::event::Event::Key(crossterm29::event::KeyEvent {
            code: crossterm29::event::KeyCode::Enter,
            modifiers: crossterm29::event::KeyModifiers::NONE,
            kind: crossterm29::event::KeyEventKind::Press,
            ..
        })
    )
}

impl EditMode for PasteChunkEditMode {
    fn parse_event(&mut self, event: ReedlineRawEvent) -> ReedlineEvent {
        let raw = crossterm29::event::Event::from(event);
        // Events the Enter lookahead read but did not consume entered the
        // queue before this one, so they are dispatched first — reedline
        // sees the original order, nothing is lost. (A pushed-back key can
        // itself be a bare Enter; its own lookahead then runs when it is
        // translated here, with the pushback and scripted queue consulted
        // first.) The drain stops at a TERMINAL translation (an Enter,
        // Ctrl+C, ...): reedline drops `Multiple` sub-events after the
        // first exiting one (engine.rs `handle_editor_event`), so whatever
        // trails stays queued here and dispatches on the next parse_event
        // call instead of being dropped.
        self.pushback.push_back(raw);
        let mut events = Vec::new();
        while let Some(pushed) = self.pushback.pop_front() {
            let translated = self.translate_event(pushed);
            let terminal = !matches!(
                translated,
                ReedlineEvent::None | ReedlineEvent::Edit(_) | ReedlineEvent::Resize(..)
            );
            // A wrapped submit arrives as its own `Multiple` (the flush
            // event, then the dismissal+submit pair — niubash#373, the GNU
            // style's shape); splice the parts in, so a flush+submit batch
            // stays one flat `Multiple` instead of nesting.
            match translated {
                ReedlineEvent::Multiple(inner) => events.extend(inner),
                other => events.push(other),
            }
            if terminal {
                break;
            }
        }
        // niubash#417: a batch ending in an Enter is (or leads to) a SUBMIT
        // — reedline stops reading at it and hands the shell the line, and
        // the accepted command's engine `read` then reads the KERNEL queue,
        // which cannot see this in-process deque. Whatever the Enter probe
        // parked (the typeahead that proved the submit) goes back to the
        // input stream's head now, so a command that reads gets it and a
        // command that does not finds it queued for the next prompt, in
        // order. A validator-held Enter (PS2 continuation) re-reads the
        // records through the same queue — same order, no observable
        // change. Batches that do NOT end in a submit keep their park
        // until the next dispatch (nothing leaves the read loop between).
        if matches!(events.last(), Some(ReedlineEvent::Enter)) && !self.pushback.is_empty() {
            let parked: Vec<crossterm29::event::Event> = self.pushback.drain(..).collect();
            (self.unread)(&parked);
        }
        if events.len() == 1 {
            events.pop().expect("len checked")
        } else {
            ReedlineEvent::Multiple(events)
        }
    }

    fn edit_mode(&self) -> PromptEditMode {
        self.inner.edit_mode()
    }
}

impl PasteChunkEditMode {
    /// Translate one raw event through the paste-chunk state machine.
    fn translate_event(&mut self, raw: crossterm29::event::Event) -> ReedlineEvent {
        if !PASTE_CHUNK_DETECTION {
            // Unix: bracketed paste (niubash#234) delivers `Event::Paste`
            // chunks; the arrival heuristic is a Windows-console fallback
            // only and must never rewrite Unix input.
            return self.delegate(raw);
        }
        // Key releases carry no text and no submit; everything else goes
        // through the burst state machine.
        let key = match &raw {
            crossterm29::event::Event::Key(key)
                if key.kind != crossterm29::event::KeyEventKind::Release =>
            {
                key
            }
            // Non-key events (the focus flicker ConPTY emits between
            // pasted lines, mouse, ...) translate to `None` upstream and
            // must neither break a pending burst nor flush it.
            _ => return self.delegate(raw),
        };

        let now = Instant::now();
        let new_burst = self
            .last_event
            .is_none_or(|t| now.duration_since(t) >= PASTE_CHUNK_GAP);
        self.last_event = Some(now);
        // Events to dispatch for this raw key: a pending-chunk flush
        // (burst end) first, then the translation of the key itself.
        let mut events = Vec::new();
        if new_burst {
            // A gap ≥ PASTE_CHUNK_GAP ends any burst: whatever the burst
            // buffered quietly is committed to the edit buffer now — one
            // insert, one repaint for the whole chunk. A gap does NOT by
            // itself decide submits anymore (niubash#284): the Enter
            // decision below is structural (queued LF partner), not
            // timing-based, so a stale burst can never swallow one.
            if let Some(flushed) = self.take_pending_event() {
                events.push(flushed);
            }
        }

        let translated = if is_plain_text_key(key) {
            match &mut self.pending {
                // Mid-burst text: absorb quietly, no repaint, no execute.
                Some(chunk) => {
                    if let crossterm29::event::KeyCode::Char(ch) = key.code {
                        chunk.push(ch);
                    }
                    ReedlineEvent::None
                }
                // Typed character (no burst in flight): normal editing.
                None => self.delegate(raw),
            }
        } else if is_bare_enter(key) {
            // niubash#308: the structural submit test (niubash#288) extends
            // to a bare CR arriving while a chunk is pending. Upstream
            // bracketed paste folds a paste's line breaks into `\n` DATA of
            // one insert (reedline 0.50.0 `src/edit_mode/mod.rs:44-47`,
            // `Event::Paste` → `InsertString`), and the buffer "can then be
            // submitted with a separate enter" (reedline 0.50.0
            // `src/engine.rs:408-416`, `use_bracketed_paste`): once the
            // paste body is complete, the next Enter IS the submit — never
            // more data. On the ConPTY CRLF path the body is complete
            // exactly when a bare CR carries no partner; on the Windows
            // Terminal KEY_EVENT path (niubash#341) a queued bare CR is
            // still data — the empty line's own break. Only silence or a
            // pushed-back non-Enter key decides the submit, mid-burst or
            // not.
            let is_lf = matches!(key.modifiers, crossterm29::event::KeyModifiers::CONTROL);
            let partner = if !is_lf && self.pending.is_some() {
                self.cr_paste_partner()
            } else {
                PastePartner::None
            };
            if let Some(chunk) = &mut self.pending {
                if is_lf {
                    // A dispatched Ctrl+J during a burst is a real line
                    // break of the paste (lone-LF feed, or the LF of a pair
                    // whose CR flushed in an earlier chunk): data, never a
                    // submit — humans do not press Ctrl+J (the chunk-open
                    // path above pins the same judgment).
                    chunk.push('\n');
                    ReedlineEvent::None
                } else {
                    match partner {
                        // The CR is the head of another pasted break — a
                        // break INSIDE the block, so data. An LF partner
                        // folds the pair's one line break; a CR partner is
                        // the `\r\r` empty line's own break and folds a
                        // second one. The probe consumed the partner, so it
                        // can never arrive as its own event — no separate
                        // pairing state needed. (At chunk-OPEN a CR partner
                        // is typeahead, not data — niubash#420.)
                        PastePartner::LfPartner => {
                            chunk.push('\n');
                            ReedlineEvent::None
                        }
                        PastePartner::CrPartner(_) => {
                            chunk.push_str("\n\n");
                            ReedlineEvent::None
                        }
                        // A bare CR with no partner behind it — silence or
                        // a queued non-Enter key (niubash#308): the pasted
                        // block is complete and this is the user's submit.
                        // Flush the chunk and submit in one batch, so the
                        // FIRST Enter after a paste executes it — no second
                        // Enter. Same flush-then-submit shape as the
                        // settled-burst path above.
                        PastePartner::None => {
                            events.push(
                                self.take_pending_event()
                                    .expect("pending chunk checked above"),
                            );
                            self.delegate(raw)
                        }
                    }
                }
            } else if self.enter_opens_chunk(key) {
                // Paste signature confirmed (queued LF partner, or the LF
                // itself): start absorbing the chunk quietly instead of
                // submitting the line. The Enter never reaches reedline.
                ReedlineEvent::None
            } else {
                // No paste signature: the user's own submit — a human's
                // Enter however fast the burst before it, a ConPTY driver's
                // send_line, an expect script, a `send-keys` (niubash#284).
                // It lands after any chunk flush above, so a settled paste
                // is inserted and submitted in one batch.
                self.delegate(raw)
            }
        } else {
            // Any chord or navigation key breaks the burst and must not
            // sit behind an unflushed chunk.
            if let Some(flushed) = self.take_pending_event() {
                events.push(flushed);
            }
            self.delegate(raw)
        };
        // A wrapped submit arrives as its own `Multiple` (dismissal Esc,
        // then the submit Enter — niubash#373, the GNU style's shape);
        // splice the parts in, so a flush+submit batch stays one flat
        // `Multiple` instead of nesting.
        match translated {
            ReedlineEvent::Multiple(inner) => events.extend(inner),
            other => events.push(other),
        }
        if events.len() == 1 {
            events.pop().expect("len checked")
        } else {
            ReedlineEvent::Multiple(events)
        }
    }

    /// Hand the raw event to the wrapped edit mode unchanged.
    fn delegate(&mut self, raw: crossterm29::event::Event) -> ReedlineEvent {
        // Track the menu window (niubash#373) for every key that actually
        // dispatches — a TAB may leave the completion menu alive, a CONTROL
        // chord hands the next Enter to a verb (history search, cancel,
        // EOF) whose handling must never see the dismissal prefix below.
        // Plain text keys cannot open or close a menu and stay out of it.
        match &raw {
            crossterm29::event::Event::Key(key)
                if key.kind == crossterm29::event::KeyEventKind::Press =>
            {
                match (key.code, key.modifiers) {
                    (crossterm29::event::KeyCode::Tab, crossterm29::event::KeyModifiers::NONE) => {
                        self.menu_possible = true;
                    }
                    (_, modifiers)
                        if modifiers.contains(crossterm29::event::KeyModifiers::CONTROL) =>
                    {
                        self.menu_possible = false;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        // Release events are rejected upstream too; the conversion can
        // only fail on one, so the fallback keeps the event inert either
        // way.
        let enter_press = is_bare_enter_press(&raw);
        let translated = match ReedlineRawEvent::try_from(raw) {
            Ok(event) => self.inner.parse_event(event),
            Err(_) => ReedlineEvent::None,
        };
        // niubash#373: reedline 0.52 offers an open menu first refusal at
        // every submit event (`menu_took_the_key`) — nushell's completion
        // cell, where Enter accepts the menu's selection and only a SECOND
        // Enter submits. GNU bash has no menu state at accept-line: Enter
        // runs the line as typed, listing or not.
        //
        // niubash#486: the interactive menus (column/list/ide/inline) want
        // the first refusal — the 1.1.0 behavior, where Enter commits the
        // highlighted candidate into the editable line — so their Enter
        // stays bare and the engine's `MenuAccept` decides with the exact
        // menu state. Only the GNU printed list keeps the dismissal prefix:
        // it reports `is_active` while its list is up but has no candidate
        // to accept, so `MenuAccept` would claim the Enter, splice nothing,
        // and swallow the submit. Inside the TAB window the Esc prefix
        // dismisses it so the very same Enter submits — one Enter, GNU's
        // shape, with the committed text from the prefix completion.
        //
        // The gate is load-bearing beyond the menu: reedline 0.52 routes a
        // WHOLE `Multiple` batch to the history-search handler while a
        // Ctrl+R search is active, and that handler drops `Multiple(_)` as
        // `Inapplicable` — an unconditionally wrapped Enter made Ctrl+R's
        // accepting Enter a dead key (the match never loaded). Outside the
        // TAB window the Enter stays bare, so search mode receives the
        // plain event its handler implements.
        //
        // Chord carve-outs: Ctrl+J (Enter + CONTROL) never wraps — it is
        // paste data on the chunk paths; the console reports Ctrl+M as the
        // bare Enter key itself, so it wraps exactly like Enter (harmless:
        // no menu outside the window means the Esc is a no-op).
        let submit_carries_enter = match &translated {
            ReedlineEvent::Enter => true,
            ReedlineEvent::Multiple(events) => {
                matches!(events.last(), Some(ReedlineEvent::Enter))
            }
            _ => false,
        };
        if submit_carries_enter && enter_press && self.menu_possible && self.enter_dismisses_menu {
            self.menu_possible = false;
            let mut events = vec![ReedlineEvent::Esc];
            match translated {
                ReedlineEvent::Enter => events.push(ReedlineEvent::Enter),
                ReedlineEvent::Multiple(rest) => events.extend(rest),
                _ => unreachable!("checked above"),
            }
            return ReedlineEvent::Multiple(events);
        }
        translated
    }
}

/// One warning per sequence the editor cannot mirror (multi-key
/// sequences are a reedline limitation — see the wt100/i185 matrix).
fn warn_unusable_engine_bindings(unusable: Vec<String>) {
    for keyseq in unusable {
        eprintln!(
            "niubash: bind: key sequence '{}' cannot be mirrored to the line editor",
            keyseq
        );
    }
}

/// Apply user-declared widget bindings from `NIU_BINDKEYS`.
///
/// Unlike bundle bindkeys these bypass the native-widget pack gate: writing
/// the variable is the explicit opt-in. Bindings apply to every keymap.
fn add_user_widget_keybindings(keybindings: &mut Keybindings, bindings: &[NativeWidgetBinding]) {
    for binding in bindings {
        let Some(key) = binding.key.as_deref().and_then(parse_key_sequence) else {
            eprintln!(
                "niubash: NIU_BINDKEYS: cannot parse key '{}' for widget '{}'",
                binding.key.as_deref().unwrap_or_default(),
                binding.widget
            );
            continue;
        };
        let Some(event) = native_widget_event(&binding.widget) else {
            continue;
        };
        keybindings.add_binding(key.0, key.1, event);
    }
}

/// Mirror engine-registered `bind` entries (niubash#185) into a reedline
/// keymap. `bind -x` entries become `__niu_bindx <index>` host-command
/// triggers; function entries map through the native-widget table
/// (unknown readline function names are skipped — GNU's
/// rl_parse_and_bind silently drops them, wt100 probe); macro entries
/// insert their translated text. Sequences reedline cannot represent
/// (multi-key `\C-x\C-f`, unrecognized escapes) are reported back for a
/// one-time warning.
fn add_engine_bind_keybindings(
    keybindings: &mut Keybindings,
    target: NativeKeymapTarget,
    entries: &[rubash::shell::bind_registry::BindEntry],
) -> Vec<String> {
    let mut unusable = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if !engine_keymap_applies(entry.keymap.as_deref(), target) {
            continue;
        }
        let Some((modifiers, key_code)) = parse_key_sequence(&entry.keyseq) else {
            unusable.push(entry.keyseq.clone());
            continue;
        };
        let event = match &entry.kind {
            rubash::shell::bind_registry::BindKind::Execute { .. } => {
                ReedlineEvent::ExecuteHostCommand(format!("{BINDX_HOST_COMMAND_PREFIX}{index}"))
            }
            rubash::shell::bind_registry::BindKind::Macro { text } => {
                edit_event(EditCommand::InsertString(text.clone()))
            }
            // A readline function name the editor cannot deliver is
            // skipped; the sentinel fallthrough of native_widget_event is
            // for zsh-style shell-function widgets, which `bind` to a
            // readline function position has no meaning for.
            rubash::shell::bind_registry::BindKind::Function { name } => {
                match native_widget_event(name) {
                    // native_widget_event's sentinel fallthrough means
                    // "unknown readline function": skipped, like GNU's
                    // silent rl_parse_and_bind drop (wt100 probe).
                    Some(ReedlineEvent::ExecuteHostCommand(payload))
                        if payload.starts_with(WIDGET_HOST_COMMAND_PREFIX) =>
                    {
                        unusable.push(entry.keyseq.clone());
                        continue;
                    }
                    Some(event) => event,
                    None => {
                        unusable.push(entry.keyseq.clone());
                        continue;
                    }
                }
            }
        };
        keybindings.add_binding(modifiers, key_code, event);
    }
    unusable
}

/// Keymap targeting for engine bind entries: GNU's emacs maps land on the
/// single emacs keymap (the sequence text carries any \e or \C-x prefix);
/// GNU's vi-movement aliases land on vi normal, vi-insert on vi insert
/// (bashline.c:4771 get_cmd_xmap_from_keymap).
fn engine_keymap_applies(keymap: Option<&str>, target: NativeKeymapTarget) -> bool {
    let Some(keymap) = keymap else {
        return target == NativeKeymapTarget::Emacs;
    };
    match keymap {
        "emacs" | "emacs-standard" | "emacs-meta" | "emacs-ctlx" => {
            target == NativeKeymapTarget::Emacs
        }
        "vi" | "vi-move" | "vi-command" | "vi-movement" => target == NativeKeymapTarget::ViNormal,
        "vi-insert" => target == NativeKeymapTarget::ViInsert,
        _ => false,
    }
}

/// Convert a CHARACTER offset (GNU READLINE_POINT unit,
/// bashline.c:4540 readline_get_char_offset MB_STRLEN) into a reedline
/// insertion point (byte offset), clamped to the buffer.
pub(crate) fn char_offset_to_byte(buffer: &str, chars: usize) -> usize {
    buffer
        .char_indices()
        .nth(chars)
        .map(|(offset, _)| offset)
        .unwrap_or(buffer.len())
}

fn add_menu_keybindings(keybindings: &mut Keybindings) {
    keybindings.add_binding(
        KeyModifiers::NONE,
        KeyCode::Tab,
        ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu(COMPLETION_MENU.to_string()),
            ReedlineEvent::Edit(vec![EditCommand::Complete]),
        ]),
    );
    keybindings.add_binding(
        KeyModifiers::SHIFT,
        KeyCode::BackTab,
        ReedlineEvent::MenuPrevious,
    );
}

/// Ctrl+V -> system clipboard paste. Only wired when reedline carries the
/// `system_clipboard` feature (gated to non-android targets in Cargo.toml,
/// niubash#207): the PasteSystem edit command only exists under that feature,
/// and android has no system clipboard to paste from.
#[cfg(not(target_os = "android"))]
fn add_system_clipboard_keybindings(keybindings: &mut Keybindings) {
    keybindings.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('v'),
        ReedlineEvent::Edit(vec![EditCommand::PasteSystem]),
    );
}

#[cfg(target_os = "android")]
fn add_system_clipboard_keybindings(_keybindings: &mut Keybindings) {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeKeymapTarget {
    Emacs,
    ViInsert,
    ViNormal,
}

fn add_native_widget_keybindings(
    keybindings: &mut Keybindings,
    _target: NativeKeymapTarget,
    config: &NativeWidgetConfig,
    _bindings: &[NativeWidgetBinding],
) {
    if !config.enabled {
        return;
    }

    add_native_widget_preset_keybindings(keybindings, &config.presets);
}

/// Apply bundle-declared widget bindings (from pack `keybindings/*.toml`
/// assets). Gated only by `import_bindkeys`; the pack-level decision gate
/// happens when the bindings are loaded into the shell.
fn add_bundle_widget_keybindings(
    keybindings: &mut Keybindings,
    target: NativeKeymapTarget,
    config: &NativeWidgetConfig,
    bindings: &[NativeWidgetBinding],
) {
    if !config.import_bindkeys {
        return;
    }

    for binding in bindings {
        if !native_widget_keymap_applies(binding.keymap.as_deref(), target) {
            continue;
        }
        let Some(key) = binding.key.as_deref().and_then(parse_key_sequence) else {
            continue;
        };
        let Some(event) = native_widget_event(&binding.widget) else {
            continue;
        };
        keybindings.add_binding(key.0, key.1, event);
    }
}

fn add_native_widget_preset_keybindings(keybindings: &mut Keybindings, presets: &[String]) {
    if presets
        .iter()
        .any(|preset| preset.eq_ignore_ascii_case("autosuggestions"))
    {
        keybindings.add_binding(
            KeyModifiers::CONTROL,
            KeyCode::Char(' '),
            ReedlineEvent::HistoryHintComplete,
        );
    }
}

fn native_widget_keymap_applies(keymap: Option<&str>, target: NativeKeymapTarget) -> bool {
    let Some(keymap) = keymap else {
        return true;
    };
    matches!(
        (keymap, target),
        ("main" | "all", _)
            | ("emacs", NativeKeymapTarget::Emacs)
            | ("viins", NativeKeymapTarget::ViInsert)
            | ("vicmd", NativeKeymapTarget::ViNormal)
    )
}

fn native_widget_event(widget: &str) -> Option<ReedlineEvent> {
    match widget {
        "autosuggest-accept" => Some(ReedlineEvent::HistoryHintComplete),
        "autosuggest-execute" => Some(ReedlineEvent::Multiple(vec![
            ReedlineEvent::HistoryHintComplete,
            ReedlineEvent::Enter,
        ])),
        "autosuggest-partial-accept" => Some(ReedlineEvent::HistoryHintWordComplete),
        "history-substring-search-up" | "history-search-backward" | "up-line-or-history" => {
            Some(ReedlineEvent::Up)
        }
        "history-substring-search-down" | "history-search-forward" | "down-line-or-history" => {
            Some(ReedlineEvent::Down)
        }
        "accept-line" => Some(ReedlineEvent::Enter),
        "beginning-of-line" => Some(edit_event(EditCommand::MoveToLineStart { select: false })),
        "end-of-line" => Some(edit_event(EditCommand::MoveToLineEnd { select: false })),
        "beginning-of-buffer-or-history" | "beginning-of-buffer" => {
            Some(edit_event(EditCommand::MoveToStart { select: false }))
        }
        "end-of-buffer-or-history" | "end-of-buffer" => {
            Some(edit_event(EditCommand::MoveToEnd { select: false }))
        }
        "backward-char" => Some(edit_event(EditCommand::MoveLeft { select: false })),
        "forward-char" => Some(edit_event(EditCommand::MoveRight { select: false })),
        "backward-word" => Some(edit_event(EditCommand::MoveWordLeft { select: false })),
        "forward-word" => Some(edit_event(EditCommand::MoveWordRight { select: false })),
        "backward-delete-char" => Some(edit_event(EditCommand::Backspace)),
        "delete-char" => Some(edit_event(EditCommand::Delete)),
        "backward-kill-word" => Some(edit_event(EditCommand::CutWordLeft)),
        "kill-word" => Some(edit_event(EditCommand::CutWordRight)),
        "kill-line" => Some(edit_event(EditCommand::CutToLineEnd)),
        "backward-kill-line" | "unix-line-discard" => {
            Some(edit_event(EditCommand::CutFromLineStart))
        }
        "kill-whole-line" => Some(edit_event(EditCommand::CutCurrentLine)),
        "yank" => Some(edit_event(EditCommand::PasteCutBufferBefore)),
        "undo" => Some(edit_event(EditCommand::Undo)),
        "redo" => Some(edit_event(EditCommand::Redo)),
        "clear-screen" => Some(ReedlineEvent::ClearScreen),
        "redisplay" => Some(ReedlineEvent::Repaint),
        "expand-or-complete" | "complete-word" => Some(completion_event()),
        "menu-previous" => Some(ReedlineEvent::MenuPrevious),
        "history-incremental-search-backward" => Some(ReedlineEvent::SearchHistory),
        // Unknown widget names become shell-function widgets: the host
        // intercepts the sentinel payload, runs the named function with the
        // editor state, and applies its outcome before resuming the editor.
        _ => Some(ReedlineEvent::ExecuteHostCommand(format!(
            "{}{}",
            WIDGET_HOST_COMMAND_PREFIX, widget
        ))),
    }
}

fn edit_event(command: EditCommand) -> ReedlineEvent {
    ReedlineEvent::Edit(vec![command])
}

fn completion_event() -> ReedlineEvent {
    ReedlineEvent::UntilFound(vec![
        ReedlineEvent::Menu(COMPLETION_MENU.to_string()),
        edit_event(EditCommand::Complete),
    ])
}

fn parse_key_sequence(value: &str) -> Option<(KeyModifiers, KeyCode)> {
    if let Some(key) = parse_named_key_sequence(value) {
        return Some(key);
    }
    match value {
        "^[[A" | "\\e[A" | "\\eOA" => Some((KeyModifiers::NONE, KeyCode::Up)),
        "^[[B" | "\\e[B" | "\\eOB" => Some((KeyModifiers::NONE, KeyCode::Down)),
        "^[[C" | "\\e[C" | "\\eOC" => Some((KeyModifiers::NONE, KeyCode::Right)),
        "^[[D" | "\\e[D" | "\\eOD" => Some((KeyModifiers::NONE, KeyCode::Left)),
        "^?" => Some((KeyModifiers::NONE, KeyCode::Backspace)),
        _ => parse_alt_key_sequence(value)
            .or_else(|| parse_meta_key_sequence(value))
            .or_else(|| parse_control_key_sequence(value))
            .or_else(|| parse_plain_key_sequence(value)),
    }
}

/// GNU bind/inputrc `\C-x` / `\M-x` forms (readline rl_translate_keyseq).
/// Multi-key tails (`\C-x\C-f`) are not representable as a single
/// reedline binding and return None (the bridge warns once).
fn parse_meta_control_sequence(
    value: &str,
    prefix: &str,
    base: KeyModifiers,
) -> Option<(KeyModifiers, KeyCode)> {
    let rest = value.strip_prefix(prefix)?;
    let mut chars = rest.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some((base, KeyCode::Char(ch.to_ascii_lowercase())))
}

fn parse_meta_key_sequence(value: &str) -> Option<(KeyModifiers, KeyCode)> {
    parse_meta_control_sequence(value, "\\C-", KeyModifiers::CONTROL)
        .or_else(|| parse_meta_control_sequence(value, "\\M-", KeyModifiers::ALT))
}

fn parse_named_key_sequence(value: &str) -> Option<(KeyModifiers, KeyCode)> {
    let normalized = value
        .trim()
        .to_ascii_lowercase()
        .replace("control+", "ctrl+")
        .replace("option+", "alt+");
    match normalized.as_str() {
        "tab" => return Some((KeyModifiers::NONE, KeyCode::Tab)),
        "shift+tab" | "backtab" => return Some((KeyModifiers::SHIFT, KeyCode::BackTab)),
        "esc" | "escape" => return Some((KeyModifiers::NONE, KeyCode::Esc)),
        "enter" | "return" => return Some((KeyModifiers::NONE, KeyCode::Enter)),
        "space" => return Some((KeyModifiers::NONE, KeyCode::Char(' '))),
        "backspace" => return Some((KeyModifiers::NONE, KeyCode::Backspace)),
        "delete" | "del" => return Some((KeyModifiers::NONE, KeyCode::Delete)),
        "up" => return Some((KeyModifiers::NONE, KeyCode::Up)),
        "down" => return Some((KeyModifiers::NONE, KeyCode::Down)),
        "left" => return Some((KeyModifiers::NONE, KeyCode::Left)),
        "right" => return Some((KeyModifiers::NONE, KeyCode::Right)),
        _ => {}
    }
    parse_modified_named_key(&normalized, "ctrl+", KeyModifiers::CONTROL)
        .or_else(|| parse_modified_named_key(&normalized, "alt+", KeyModifiers::ALT))
}

fn parse_modified_named_key(
    value: &str,
    prefix: &str,
    modifiers: KeyModifiers,
) -> Option<(KeyModifiers, KeyCode)> {
    let rest = value.strip_prefix(prefix)?;
    if rest == "space" {
        return Some((modifiers, KeyCode::Char(' ')));
    }
    let mut chars = rest.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some((modifiers, KeyCode::Char(ch)))
}

fn parse_alt_key_sequence(value: &str) -> Option<(KeyModifiers, KeyCode)> {
    let rest = value
        .strip_prefix("^[")
        .or_else(|| value.strip_prefix("\\e"))?;
    let mut chars = rest.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some((KeyModifiers::ALT, KeyCode::Char(ch.to_ascii_lowercase())))
}

fn parse_control_key_sequence(value: &str) -> Option<(KeyModifiers, KeyCode)> {
    let rest = value.strip_prefix('^')?;
    let mut chars = rest.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    match ch {
        'I' | 'i' => Some((KeyModifiers::NONE, KeyCode::Tab)),
        'J' | 'j' | 'M' | 'm' => Some((KeyModifiers::NONE, KeyCode::Enter)),
        'H' | 'h' => Some((KeyModifiers::NONE, KeyCode::Backspace)),
        ' ' => Some((KeyModifiers::CONTROL, KeyCode::Char(' '))),
        '[' => Some((KeyModifiers::NONE, KeyCode::Esc)),
        ch if ch.is_ascii_alphabetic() => Some((
            KeyModifiers::CONTROL,
            KeyCode::Char(ch.to_ascii_lowercase()),
        )),
        ch => Some((KeyModifiers::CONTROL, KeyCode::Char(ch))),
    }
}

fn parse_plain_key_sequence(value: &str) -> Option<(KeyModifiers, KeyCode)> {
    let mut chars = value.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some((KeyModifiers::NONE, KeyCode::Char(ch)))
}

/// Lets Enter grow an unfinished command into a multi-line buffer instead of
/// submitting it. Reedline submits only once `validate` reports Complete, so
/// a pasted or typed `cmd \` / unclosed quote / open `if..fi` block stays
/// editable — arrows move between lines — until the last line closes it.
/// The REPL-side `PendingReplInput` collector remains as a fallback.
struct ReplValidator;

impl Validator for ReplValidator {
    fn validate(&self, line: &str) -> ValidationResult {
        if is_repl_input_complete(line) {
            ValidationResult::Complete
        } else {
            ValidationResult::Incomplete
        }
    }
}

#[derive(Debug, Default)]
struct PendingReplInput {
    lines: Vec<String>,
}

impl PendingReplInput {
    fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    fn push(&mut self, line: &str) {
        self.lines.push(line.to_string());
    }

    fn clear(&mut self) {
        self.lines.clear();
    }

    fn take(&mut self) -> String {
        let script = self.script();
        self.clear();
        script
    }

    fn script(&self) -> String {
        self.lines.join("\n")
    }

    fn is_complete(&self) -> bool {
        is_repl_input_complete(&self.script())
    }

    fn is_multiline(&self) -> bool {
        // With the reedline validator a complete multi-line buffer arrives as
        // a single entry containing '\n'; the fallback collector still joins
        // per-line reads into multiple entries.
        self.lines.len() > 1 || self.script().contains('\n')
    }
}

pub(crate) struct ContinuationPrompt {
    indicator: String,
    /// The vi-mode indicator strings carried over from the prompt backend
    /// (niubash#184). The PromptEditMode itself arrives live from reedline
    /// on every repaint, so ESC toggling insert/normal inside a multi-line
    /// edit re-renders the indicator without extra plumbing.
    vi_insert_indicator: String,
    vi_normal_indicator: String,
}

impl ContinuationPrompt {
    pub(crate) fn new(prompt: &dyn Prompt, vi_indicators: (String, String)) -> Self {
        Self {
            indicator: prompt.render_prompt_multiline_indicator().into_owned(),
            vi_insert_indicator: vi_indicators.0,
            vi_normal_indicator: vi_indicators.1,
        }
    }
}

impl Prompt for ContinuationPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Owned(self.indicator.clone())
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, prompt_mode: PromptEditMode) -> Cow<'_, str> {
        // niubash#184: continuation reads carry the same minimal vi-mode
        // indicator as the main prompt (GNU's PS2 has no mode display, but
        // knowing insert vs normal before a dd on a multi-line buffer is
        // exactly where it matters).
        Cow::Owned(crate::prompt::mode_indicator_for(
            prompt_mode,
            &self.vi_insert_indicator,
            &self.vi_normal_indicator,
        ))
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Owned(self.indicator.clone())
    }

    fn render_prompt_history_search_indicator(
        &self,
        _history_search: PromptHistorySearch,
    ) -> Cow<'_, str> {
        Cow::Borrowed("(history search) ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReplToken {
    Word(String),
    Operator(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockClose {
    Fi,
    Done,
    Brace,
    Paren,
    FunctionBody,
    /// niubash#482: an open `case` split by phase so the gather knows when
    /// a `)` is a legal pattern-list terminator versus a stray close —
    /// GNU reports `syntax error near unexpected token `)'' for the
    /// latter the moment it is read, instead of waiting for the opener.
    /// `case` seen, operand word pending.
    CaseOperand,
    /// Operand read, `in` pending.
    CaseExpectIn,
    /// Inside the pattern region, no pattern word yet (`)` here is a
    /// syntax error in GNU).
    CasePatternOpen,
    /// Inside the pattern region after at least one pattern word — `)`
    /// legally ends the pattern list.
    CasePattern,
    /// Inside a clause body; `;;`/`;&`/`;;;&` return to CasePatternOpen.
    CaseBody,
}

impl BlockClose {
    /// Any open-`case` frame variant — the `esac` closer matches all of
    /// them (GNU's `esac` closes from pattern or body position).
    fn is_case(self) -> bool {
        matches!(
            self,
            BlockClose::CaseOperand
                | BlockClose::CaseExpectIn
                | BlockClose::CasePatternOpen
                | BlockClose::CasePattern
                | BlockClose::CaseBody
        )
    }
}

#[derive(Debug, Default)]
struct ReplInputScan {
    tokens: Vec<ReplToken>,
    open_quote: Option<char>,
    trailing_backslash: bool,
}

pub fn is_script_input_complete(input: &str) -> bool {
    is_repl_input_complete(input)
}

fn is_repl_input_complete(input: &str) -> bool {
    let scan = scan_repl_input(input);
    if scan.open_quote.is_some() || scan.trailing_backslash {
        return false;
    }
    if !heredoc_input_complete(input) {
        return false;
    }

    let mut stack = Vec::new();
    let mut command_position = true;
    let mut trailing_list_operator = false;
    let mut index = 0;

    while index < scan.tokens.len() {
        match &scan.tokens[index] {
            ReplToken::Operator(operator) => {
                match operator.as_str() {
                    ";" | "\n" => {
                        command_position = true;
                        trailing_list_operator = false;
                    }
                    "|" => {
                        // Inside a case pattern list `|` separates
                        // alternatives rather than opening a pipeline.
                        if matches!(
                            stack.last(),
                            Some(BlockClose::CasePatternOpen | BlockClose::CasePattern)
                        ) {
                            *stack.last_mut().expect("top checked") = BlockClose::CasePatternOpen;
                        } else {
                            command_position = true;
                            trailing_list_operator = true;
                        }
                    }
                    "&&" | "||" => {
                        command_position = true;
                        trailing_list_operator = true;
                    }
                    "(" => {
                        stack.push(BlockClose::Paren);
                        command_position = true;
                        trailing_list_operator = false;
                    }
                    ")" => {
                        match stack.last() {
                            Some(BlockClose::Paren) => {
                                stack.pop();
                                // A `(a|b)` pattern list close also ends
                                // the case pattern — the clause body
                                // follows.
                                if matches!(
                                    stack.last(),
                                    Some(BlockClose::CasePatternOpen | BlockClose::CasePattern)
                                ) {
                                    *stack.last_mut().expect("top checked") = BlockClose::CaseBody;
                                }
                            }
                            // `a)` legally ends a case pattern list.
                            Some(BlockClose::CasePattern) => {
                                *stack.last_mut().expect("top checked") = BlockClose::CaseBody;
                            }
                            _ => {
                                // niubash#482: a `)` closing nothing is
                                // GNU's `syntax error near unexpected
                                // token `)'' — the group is abandoned NOW,
                                // not when the opener eventually closes
                                // (GNU never waits: `{ echo c\n)' errors
                                // and drops back to PS1). Reporting the
                                // buffer complete submits it so the
                                // engine's armed parse prints the
                                // diagnostic instead of PS2-wedging.
                                return true;
                            }
                        }
                        command_position = false;
                        trailing_list_operator = false;
                    }
                    ";;" | ";&" | ";;;&" => {
                        // Legal after a case clause body (the next pattern
                        // follows) or inside an arithmetic `( (` frame;
                        // anywhere else GNU reports a syntax error at
                        // the token — same terminal verdict as `)`.
                        match stack.last() {
                            Some(BlockClose::CaseBody) => {
                                *stack.last_mut().expect("top checked") =
                                    BlockClose::CasePatternOpen;
                            }
                            Some(BlockClose::Paren) => {}
                            _ => return true,
                        }
                        command_position = true;
                        trailing_list_operator = false;
                    }
                    _ => {}
                }
                index += 1;
            }
            ReplToken::Word(word) => {
                trailing_list_operator = false;

                // Words in case-pattern position are pattern text, never
                // closers (`fi)`, `esac)`, `in` all name patterns).
                // `esac` straight after `in`/`;;` is GNU's empty-case
                // close (is_case_end_keyword), not a pattern.
                match stack.last() {
                    Some(BlockClose::CasePatternOpen) if word != "esac" => {
                        *stack.last_mut().expect("top checked") = BlockClose::CasePattern;
                        command_position = false;
                        index += 1;
                        continue;
                    }
                    Some(BlockClose::CasePattern) => {
                        command_position = false;
                        index += 1;
                        continue;
                    }
                    Some(BlockClose::CaseOperand) => {
                        *stack.last_mut().expect("top checked") = BlockClose::CaseExpectIn;
                        // The operand is consumed below as an ordinary
                        // word (`in` is legal operand text too).
                    }
                    _ => {}
                }

                if command_position && is_function_header(&scan.tokens, index) {
                    stack.push(BlockClose::FunctionBody);
                    command_position = false;
                    index += 3;
                    continue;
                }

                match word.as_str() {
                    "if" if command_position => {
                        stack.push(BlockClose::Fi);
                        command_position = false;
                    }
                    "for" | "while" | "until" | "select" if command_position => {
                        stack.push(BlockClose::Done);
                        command_position = false;
                    }
                    "case" if command_position => {
                        stack.push(BlockClose::CaseOperand);
                        command_position = false;
                    }
                    "in" if stack.last() == Some(&BlockClose::CaseExpectIn) => {
                        *stack.last_mut().expect("top checked") = BlockClose::CasePatternOpen;
                        command_position = false;
                    }
                    "fi" | "done" if command_position => {
                        let want = if word == "fi" {
                            BlockClose::Fi
                        } else {
                            BlockClose::Done
                        };
                        if stack.last() == Some(&want) {
                            stack.pop();
                        } else if matches!(
                            stack.last(),
                            Some(
                                BlockClose::Fi
                                    | BlockClose::Done
                                    | BlockClose::Brace
                                    | BlockClose::FunctionBody
                            )
                        ) {
                            // Same foreign-closer family as `)`: GNU's
                            // reader abandons the group at the token.
                            // Under `(`/case frames these are legal words
                            // (arithmetic text, patterns) — leave them be.
                            return true;
                        }
                        command_position = false;
                    }
                    "esac" if command_position => {
                        if stack.last().is_some_and(|top| top.is_case()) {
                            stack.pop();
                        } else if matches!(
                            stack.last(),
                            Some(
                                BlockClose::Fi
                                    | BlockClose::Done
                                    | BlockClose::Brace
                                    | BlockClose::FunctionBody
                            )
                        ) {
                            return true;
                        }
                        command_position = false;
                    }
                    "{" => {
                        if stack.last() == Some(&BlockClose::FunctionBody) {
                            stack.pop();
                        }
                        stack.push(BlockClose::Brace);
                        command_position = true;
                    }
                    "}" => {
                        if stack.last() == Some(&BlockClose::Brace) {
                            stack.pop();
                        } else if matches!(
                            stack.last(),
                            Some(
                                BlockClose::Fi
                                    | BlockClose::Done
                                    | BlockClose::Brace
                                    | BlockClose::FunctionBody
                            )
                        ) {
                            return true;
                        }
                        command_position = false;
                    }
                    "then" | "do" | "else" => {
                        command_position = true;
                    }
                    "elif" => {
                        command_position = false;
                    }
                    _ => {
                        command_position = false;
                    }
                }
                index += 1;
            }
        }
    }

    stack.is_empty() && !trailing_list_operator
}

fn heredoc_input_complete(input: &str) -> bool {
    if !input.contains("<<") {
        return true;
    }
    rubash::lexer::tokenize(input)
        .into_iter()
        .all(|token| !is_unterminated_heredoc_body_token(&token))
}

// Whether a token is a here-doc body that did not yet reach its delimiter.
// rubash marks such bodies with a leading \x1f (and, for quoted here-docs,
// the __RUBASH_HD1__ marker before it). This mirrors rubash's own
// command_has_unterminated_heredoc check without depending on a private
// lexer helper that is not part of the published API.
fn is_unterminated_heredoc_body_token(token: &rubash::Token) -> bool {
    use rubash::TokenKind;
    if token.kind != TokenKind::HereDocBody {
        return false;
    }
    const QUOTED_HEREDOC_MARKER: &str = "__RUBASH_HD1__";
    let body = token
        .value
        .strip_prefix(QUOTED_HEREDOC_MARKER)
        .unwrap_or(&token.value);
    body.starts_with('')
}

/// Pending here-doc body skip state for the REPL input scanner.
///
/// GNU bash gathers here-doc bodies at line granularity while lexing: the
/// body starts after the newline of the operator line and everything up to
/// the delimiter line is opaque data. The completeness scanner must mirror
/// that, otherwise body text poisons the quote and block tracking
/// (`x; if y` inside a body would push a phantom `fi` block and the pasted
/// script would never submit).
struct ReplHeredocSkip {
    delimiter: String,
    strip_tabs: bool,
    /// False while still scanning the operator line (delimiters, comments,
    /// and quotes on that line are ordinary syntax); true inside the body.
    in_body: bool,
}

fn scan_repl_input(input: &str) -> ReplInputScan {
    let chars: Vec<char> = input.chars().collect();
    let mut scan = ReplInputScan {
        trailing_backslash: has_unescaped_trailing_backslash(input),
        ..ReplInputScan::default()
    };
    let mut word = String::new();
    let mut quote = None;
    let mut index = 0;
    let mut heredoc_skip: Option<ReplHeredocSkip> = None;
    let mut heredoc_line = String::new();

    while index < chars.len() {
        let ch = chars[index];

        if let Some(skip) = &mut heredoc_skip {
            if skip.in_body {
                if ch == '\n' {
                    let candidate = heredoc_line.trim_end_matches('\r');
                    let stripped = if skip.strip_tabs {
                        candidate.trim_start_matches('\t')
                    } else {
                        candidate
                    };
                    if stripped == skip.delimiter {
                        heredoc_skip = None;
                    }
                    heredoc_line.clear();
                    index += 1;
                    continue;
                }
                heredoc_line.push(ch);
                index += 1;
                continue;
            }
            // Still on the operator line: fall through to normal scanning.
            // The first newline switches the state into the body.
            if ch == '\n' {
                skip.in_body = true;
                heredoc_line.clear();
                index += 1;
                continue;
            }
        }

        if let Some(quote_char) = quote {
            word.push(ch);
            if ch == '\\' && quote_char != '\'' {
                if let Some(next) = chars.get(index + 1) {
                    word.push(*next);
                    index += 2;
                    continue;
                }
            }
            if ch == quote_char {
                quote = None;
            }
            index += 1;
            continue;
        }

        match ch {
            '#' if word.is_empty() => {
                index = skip_repl_comment(&chars, index);
            }
            '<' => {
                flush_repl_word(&mut scan.tokens, &mut word);
                if chars.get(index + 1) == Some(&'<') {
                    if chars.get(index + 2) == Some(&'<') {
                        // Here-string: `<<< word` takes its payload from the
                        // following word, not from subsequent lines.
                        scan.tokens.push(ReplToken::Operator("<<<".to_string()));
                        index += 3;
                        continue;
                    }
                    let mut cursor = index + 2;
                    let strip_tabs = chars.get(cursor) == Some(&'-');
                    if strip_tabs {
                        cursor += 1;
                    }
                    while matches!(chars.get(cursor), Some(c) if *c == ' ' || *c == '\t') {
                        cursor += 1;
                    }
                    let mut delimiter = String::new();
                    match chars.get(cursor) {
                        Some(q @ ('"' | '\'')) => {
                            let quote_char = *q;
                            cursor += 1;
                            while let Some(&c) = chars.get(cursor) {
                                if c == quote_char {
                                    cursor += 1;
                                    break;
                                }
                                if c == '\\' && quote_char == '"' {
                                    cursor += 1;
                                    if let Some(&escaped) = chars.get(cursor) {
                                        delimiter.push(escaped);
                                        cursor += 1;
                                    }
                                    continue;
                                }
                                delimiter.push(c);
                                cursor += 1;
                            }
                        }
                        _ => {
                            while let Some(&c) = chars.get(cursor) {
                                if c.is_ascii_whitespace()
                                    || matches!(c, ';' | '&' | '|' | '<' | '>' | '(' | ')')
                                {
                                    break;
                                }
                                if c == '\\' {
                                    cursor += 1;
                                    if let Some(&escaped) = chars.get(cursor) {
                                        delimiter.push(escaped);
                                        cursor += 1;
                                    }
                                    continue;
                                }
                                delimiter.push(c);
                                cursor += 1;
                            }
                        }
                    }
                    // Sequential gathering, like bash's lexer: bodies of
                    // multiple heredocs on one operator line are read in
                    // operator order, so the last operator's delimiter ends
                    // the combined skip.
                    heredoc_skip = Some(ReplHeredocSkip {
                        delimiter,
                        strip_tabs,
                        in_body: false,
                    });
                    index = cursor;
                    continue;
                }
                word.push('<');
                index += 1;
            }
            '\'' | '"' | '`' => {
                quote = Some(ch);
                word.push(ch);
                index += 1;
            }
            '\\' => {
                word.push(ch);
                if let Some(next) = chars.get(index + 1) {
                    word.push(*next);
                    index += 2;
                } else {
                    index += 1;
                }
            }
            '\r' => {
                flush_repl_word(&mut scan.tokens, &mut word);
                index += 1;
            }
            '\n' => {
                flush_repl_word(&mut scan.tokens, &mut word);
                scan.tokens.push(ReplToken::Operator("\n".to_string()));
                index += 1;
            }
            ch if ch.is_ascii_whitespace() => {
                flush_repl_word(&mut scan.tokens, &mut word);
                index += 1;
            }
            ';' => {
                flush_repl_word(&mut scan.tokens, &mut word);
                // Case-clause terminators are single operators: only
                // adjacent bytes form them (`a ;;` is two `;`).
                if chars.get(index + 1) == Some(&';') {
                    if chars.get(index + 2) == Some(&'&') {
                        scan.tokens.push(ReplToken::Operator(";;&".to_string()));
                        index += 3;
                    } else {
                        scan.tokens.push(ReplToken::Operator(";;".to_string()));
                        index += 2;
                    }
                } else if chars.get(index + 1) == Some(&'&') {
                    scan.tokens.push(ReplToken::Operator(";&".to_string()));
                    index += 2;
                } else {
                    scan.tokens.push(ReplToken::Operator(";".to_string()));
                    index += 1;
                }
            }
            '(' | ')' => {
                flush_repl_word(&mut scan.tokens, &mut word);
                scan.tokens.push(ReplToken::Operator(ch.to_string()));
                index += 1;
            }
            '|' | '&' => {
                flush_repl_word(&mut scan.tokens, &mut word);
                if chars.get(index + 1) == Some(&ch) {
                    scan.tokens.push(ReplToken::Operator(format!("{ch}{ch}")));
                    index += 2;
                } else {
                    scan.tokens.push(ReplToken::Operator(ch.to_string()));
                    index += 1;
                }
            }
            _ => {
                word.push(ch);
                index += 1;
            }
        }
    }

    flush_repl_word(&mut scan.tokens, &mut word);
    scan.open_quote = quote;
    scan
}

fn flush_repl_word(tokens: &mut Vec<ReplToken>, word: &mut String) {
    if word.is_empty() {
        return;
    }
    tokens.push(ReplToken::Word(std::mem::take(word)));
}

fn skip_repl_comment(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() && chars[index] != '\n' {
        index += 1;
    }
    index
}

fn is_function_header(tokens: &[ReplToken], index: usize) -> bool {
    let Some(ReplToken::Word(name)) = tokens.get(index) else {
        return false;
    };
    is_shell_identifier(name)
        && matches!(tokens.get(index + 1), Some(ReplToken::Operator(op)) if op == "(")
        && matches!(tokens.get(index + 2), Some(ReplToken::Operator(op)) if op == ")")
}

fn is_shell_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn has_unescaped_trailing_backslash(input: &str) -> bool {
    let last_line = input
        .trim_end_matches(['\r', '\n'])
        .rsplit_once('\n')
        .map(|(_, line)| line)
        .unwrap_or_else(|| input.trim_end_matches(['\r', '\n']));
    let count = last_line.chars().rev().take_while(|ch| *ch == '\\').count();
    count % 2 == 1
}

/// Run the interactive REPL.
///
/// Notices produced off the startup path (e.g. the background update check)
/// wait here and print above the next prompt instead of blocking shell
/// startup or garbling an active line edit.
static PENDING_NOTICES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Queue a line to be printed before the next prompt draws. Callable from
/// any thread; used by the bin crate's background update check.
pub fn set_pending_notice(text: String) {
    if let Ok(mut queue) = PENDING_NOTICES.lock() {
        queue.push(text);
    }
}

fn drain_pending_notices() {
    if let Ok(mut queue) = PENDING_NOTICES.lock() {
        for line in queue.drain(..) {
            println!("{line}");
        }
    }
}

/// One dim orientation line shown only on the very first interactive start,
/// pointing at `about`, the in-shell tour.
/// Follows the wizard language so a Chinese first run reads Chinese.
fn print_first_run_hint() {
    let hint = if crate::setup_wizard::wizard_lang_is_chinese() {
        "\x1b[2m  输入 about 快速上手 · niu setup 重新配置 · 文档 github.com/unixwin/niubash\x1b[0m"
    } else {
        "\x1b[2m  Type `about` for a quick tour · `niu setup` to reconfigure · docs: github.com/unixwin/niubash\x1b[0m"
    };
    println!("{hint}");
}

/// Takes ownership of the shell and shares it with the completer through an
/// `Rc<RefCell<Shell>>` bridge so shell-function completions can execute in
/// the engine while the line editor is active. No borrow is held across
/// `read_line`: the prompt is cloned out each iteration.
pub fn run_repl(shell: Shell) -> anyhow::Result<()> {
    let shell = Rc::new(RefCell::new(shell));
    // Snapshot the console modes before anything (wizard, rc files, external
    // commands) can change them. Restored before every prompt: a child that
    // exits with a broken console (e.g. ssh.exe on a failed auth) must not
    // leave the REPL drawing literal escape sequences with a hidden cursor.
    let console_baseline = crate::console_guard::capture();
    let first_run = crate::setup_wizard::is_first_run();

    let welcome = format!("Niubash {}", env!("CARGO_PKG_VERSION"));
    println!("{}", welcome);
    if first_run && crate::terminal::stdio_is_interactive() {
        print_first_run_hint();
    }

    // Typeahead guard (niubash#167): from here until the first (and every)
    // `read_line`, console input typed while the shell runs prompt
    // machinery is swept out of the shared console input queue — probing
    // children spawned by the rc bootstrap or PROMPT_COMMAND would
    // otherwise eat the first typed key — and reinjected, in order, right
    // before the editor reads. Armed here so startup rc children are
    // covered too; disarmed while a foreground command executes (the
    // command owns the terminal) and re-armed at the top of every loop
    // iteration for the pre-prompt stretch.
    let mut typeahead = crate::typeahead_guard::TypeaheadGuard::disarmed();
    typeahead.arm();
    // niubash#399: bind the Option BEFORE the `if let` — the scrutinee's
    // `borrow_mut` temporary would otherwise live to the end of the `if
    // let` (edition 2021), and the inner `borrow_mut` for the EXIT trap
    // panicked with "RefCell already borrowed" on every rc/profile exit.
    let startup_exit = shell.borrow_mut().run_startup_rc();
    if let Some(status) = startup_exit {
        // niubash#402: the startup rc executed `exit` — end the session with
        // its status before the first prompt, the same shape as the
        // PROMPT_COMMAND jump handled in the loop below (GNU
        // jump_to_top_level unwinds reader_loop; no prompt is rendered).
        let code = shell
            .borrow_mut()
            .finish_with_exit_trap(status)
            .unwrap_or(status);
        flush_repl_output();
        std::process::exit(code);
    }
    let no_editing = shell.borrow().no_editing;
    if no_editing {
        typeahead.disarm_and_reinject();
        return run_repl_without_line_editor(&mut shell.borrow_mut());
    }
    // User widget bindings and completion functions come from the rc
    // (NIU_BINDKEYS / NIU_COMPDEFS); read them after sourcing and before
    // the line editor is built. Engine `bind` registrations (bind -x,
    // macros, function bindings) come from the same rc and snapshot at
    // the same point (niubash#185).
    shell.borrow_mut().load_user_widget_bindings();
    shell.borrow_mut().load_user_compdefs();
    shell.borrow_mut().load_engine_bindings();
    // niubash#184: the rc's `set -o vi` (the standard bashrc line) must land
    // in the editor built below. GNU bash resolves the editing mode the same
    // way after startup files: `set -o vi` runs set_edit_mode
    // (builtins/set.def:424) → rl_variable_bind("editing-mode") during rc
    // sourcing, before the first readline prompt. The engine option flags
    // are read and folded into `editor_mode` here; build_line_editor consumes
    // the field.
    shell.borrow_mut().refresh_edit_mode();
    let mut line_editor = build_line_editor(&shell)?;
    let mut engine_bind_generation = shell.borrow().engine_bind_generation;
    let mut pending = PendingReplInput::default();

    loop {
        crate::console_guard::restore(&console_baseline);
        typeahead.arm();
        // A command (rc script, PROMPT_COMMAND, or user command) may have
        // registered or removed bindings at runtime. GNU mutates the live
        // readline keymap immediately; reedline keymaps are baked at
        // editor build time, so a moved registry generation rebuilds the
        // editor before the next prompt. The buffer is empty at this
        // boundary (the previous line was submitted or interrupted) and
        // history writes through to the file, so the rebuild is lossless.
        if shell.borrow().executor.bind_registry_generation() != engine_bind_generation {
            shell.borrow_mut().load_engine_bindings();
            line_editor = build_line_editor(&shell)?;
            engine_bind_generation = shell.borrow().engine_bind_generation;
        }
        let signal = if pending.is_empty() {
            drain_pending_notices();
            // niubash#180: a finished `niu setup` run (a child process)
            // left its handoff marker — apply the new configuration before
            // the precmd hooks render the prompt, so the very next prompt
            // already shows the new theme.
            shell.borrow_mut().apply_setup_config_if_pending();
            // niubash#191: consume the engine's re-armed exit jump (a PC
            // that ran `exit`/an errexit unwind, rubash#433) and end the
            // session — no prompt is rendered and no further line is read,
            // like GNU's jump_to_top_level unwinding reader_loop. The
            // jump's status (last_exit_code, e.g. `exit 5`) is carried out
            // through the EXIT trap.
            if shell.borrow_mut().run_precmd_hooks() {
                let status = shell.borrow().executor.last_exit_code();
                let code = shell
                    .borrow_mut()
                    .finish_with_exit_trap(status)
                    .unwrap_or(status);
                // The jump's status IS the session status (GNU's
                // jump_to_top_level unwinds reader_loop and exit_shell
                // carries it); the REPL's Ok(()) return otherwise always
                // maps to rc 0 in main, so carry the status out directly —
                // the same shape as the engine's piped `-i` route
                // (src/main.rs run_interactive_stdin).
                flush_repl_output();
                std::process::exit(code);
            }
            // GNU readline starts every fresh line in insert mode, even in
            // vi editing mode (lib/readline/readline.c:1243-1249: "Each
            // line starts in insert mode (the default)" —
            // _rl_set_insert_mode(RL_IM_DEFAULT, 1) plus
            // _rl_vi_initialize_line() run per readline call). Reedline
            // 0.50 keeps ViMode across reads and exposes no reset, so a
            // line submitted in normal mode would leave the NEXT prompt in
            // normal mode, where typed letters are motions. When the
            // previous read ended in vi normal/visual, rebuild the editor:
            // a fresh Vi starts in insert.
            if matches!(
                line_editor.prompt_edit_mode(),
                PromptEditMode::Vi(PromptViMode::Normal) | PromptEditMode::Vi(PromptViMode::Visual)
            ) {
                line_editor = build_line_editor(&shell)?;
            }
            // niubash#184: `set -o vi` / `set -o emacs` must switch the LIVE
            // line editor, like GNU bash: the two options route to one
            // editing-mode state (set.def:200/235 → set_edit_mode,
            // set.def:424) whose `rl_variable_bind("editing-mode")` rebinds
            // the active keymap immediately (bind.c:2001 sv_editmode,
            // bind.c:2092/2104) — the last `set -o` wins at the next
            // prompt. Reedline 0.50 exposes no edit-mode setter on a live
            // engine (only the `with_edit_mode` builder), so a change is
            // applied by rebuilding through `build_line_editor` — the same
            // construction path as startup, so menus, hinter and bindings
            // come along.
            if shell.borrow_mut().refresh_edit_mode().is_some() {
                line_editor = build_line_editor(&shell)?;
            }
            let prompt = shell.borrow_mut().prompt_for_display();
            typeahead.disarm_and_reinject();
            line_editor.read_line(&prompt)
        } else {
            let prompt = shell.borrow().prompt.clone();
            let prompt = ContinuationPrompt::new(&prompt, prompt.vi_indicators());
            typeahead.disarm_and_reinject();
            line_editor.read_line(&prompt)
        };
        // The foreground command (or widget/exit path) below owns the
        // terminal: the guard stays disarmed until the next prompt
        // rebuild at the top of the loop.
        typeahead.disarm_and_reinject();

        match signal {
            Ok(Signal::HostCommand(payload)) => {
                // reedline 0.50 delivers ReedlineEvent::ExecuteHostCommand
                // as Signal::HostCommand (enums.rs:38-42 "passthrough
                // value... up to the caller to define the protocol"). The
                // widget (`__niu_widget`) and engine bind -x
                // (`__niu_bindx`) sentinels ride on it; an unknown payload
                // is ignored. The editor buffer is untouched by the
                // signal, so a pending multi-line continuation stays
                // intact around the widget run.
                match run_host_sentinel(&shell, &mut line_editor, &payload) {
                    SentinelOutcome::Resumed => flush_repl_output(),
                    SentinelOutcome::Accept(Some(buffer)) => {
                        // A widget that accepts from a host-command
                        // keypress submits its buffer as ordinary input.
                        flush_repl_output();
                        let _ = shell.borrow_mut().execute_interactive_line(buffer.trim());
                        flush_repl_output();
                        // niubash R1: the widget's line can carry a top-level
                        // `exit` too — end the session like any submission.
                        // Take into a `let` first: an `if let` scrutinee's
                        // `borrow_mut()` temporary would still be alive in the
                        // body, and `end_repl_session`'s own `borrow_mut()`
                        // would panic on the second mutable borrow.
                        let jump = shell.borrow_mut().take_session_exit_jump();
                        if let Some(status) = jump {
                            end_repl_session(&mut shell.borrow_mut(), status);
                        }
                    }
                    SentinelOutcome::Accept(None) => {}
                    SentinelOutcome::NotSentinel => {}
                }
            }
            Ok(Signal::Success(buffer)) => {
                let mut line = buffer.trim_end_matches(['\r', '\n']).to_string();
                if pending.is_empty() {
                    // Older reedline releases delivered ExecuteHostCommand
                    // as Success; keep the sentinel check on the submitted
                    // line for hosts pinned that way. Under 0.50 the
                    // sentinels arrive through Signal::HostCommand above.
                    match run_host_sentinel(&shell, &mut line_editor, &line) {
                        SentinelOutcome::Resumed => {
                            flush_repl_output();
                            continue;
                        }
                        SentinelOutcome::Accept(widget_buffer) => {
                            line = widget_buffer.unwrap_or_else(|| {
                                line_editor.current_buffer_contents().to_string()
                            });
                        }
                        SentinelOutcome::NotSentinel => {}
                    }
                }
                let line = line.as_str();
                if pending.is_empty() && line.trim().is_empty() {
                    continue;
                }
                if pending.is_empty() {
                    if let Some(status) = repl_exit_status(line) {
                        end_repl_session(&mut shell.borrow_mut(), status);
                    }
                }
                if pending.is_empty() {
                    if let Some(args) = self_update_command_args(line) {
                        if let Some(code) = spawn_self_update(&args) {
                            std::process::exit(code);
                        }
                    }
                }

                pending.push(line);
                if !pending.is_complete() {
                    continue;
                }

                let is_multiline = pending.is_multiline();
                let script = pending.take();
                if is_multiline {
                    let _ = shell.borrow_mut().execute_interactive_script(&script);
                } else {
                    let _ = shell.borrow_mut().execute_interactive_line(script.trim());
                }
                flush_repl_output();
                // niubash R1: the engine unwound this submission with a
                // top-level `exit` (`true; exit 3`, an alias or function body
                // running `exit`, errexit) — GNU's jump_to_top_level ends
                // reader_loop (evalstring.c:618-619), so end the session
                // with the jump's status instead of prompting again. A bare
                // `exit N` never reaches here: the D1 intercept above ends
                // the session before the engine runs. Taken into a `let` so
                // the scrutinee's `borrow_mut()` temporary is dropped before
                // end_repl_session borrows again (an `if let` would hold it
                // through the body and panic with `RefCell already borrowed`).
                let jump = shell.borrow_mut().take_session_exit_jump();
                if let Some(status) = jump {
                    end_repl_session(&mut shell.borrow_mut(), status);
                }
            }
            Ok(Signal::CtrlD) => {
                println!();
                if !pending.is_empty() {
                    pending.clear();
                    continue;
                }
                let _ = shell.borrow_mut().finish_with_exit_trap(0);
                break;
            }
            Ok(Signal::CtrlC) => {
                println!();
                if crate::ctrl_c::consume_ctrl_c() {
                    flush_repl_output();
                }
                pending.clear();
                continue;
            }
            Ok(_) => continue,
            Err(e) => {
                eprintln!("niubash: line editor error: {}", e);
                let _ = shell.borrow_mut().finish_with_exit_trap(1);
                break;
            }
        }
    }

    Ok(())
}

/// `--noediting` fallback: read plain input lines with no
/// readline-style editing, matching GNU bash when invoked with --noediting.
/// The prompt is rendered through the same PromptBackend so the visual style
/// is preserved; control characters and completion are unavailable.
fn run_repl_without_line_editor(shell: &mut Shell) -> anyhow::Result<()> {
    use std::io::BufRead;
    let console_baseline = crate::console_guard::capture();
    let stdin = std::io::stdin();
    let mut pending = PendingReplInput::default();
    loop {
        crate::console_guard::restore(&console_baseline);
        // niubash#180: same live-session apply as the line-editor loop.
        shell.apply_setup_config_if_pending();
        // niubash#191: same exit-jump consumption as the line-editor loop —
        // a PC `exit` ends the session before the next prompt is printed.
        if shell.run_precmd_hooks() {
            let status = shell.executor.last_exit_code();
            let code = shell.finish_with_exit_trap(status).unwrap_or(status);
            flush_repl_output();
            std::process::exit(code);
        }
        let prompt = if pending.is_empty() {
            shell.prompt.render_prompt_left().to_string()
        } else {
            "> ".to_string()
        };
        print!("{prompt}");
        let _ = std::io::stdout().flush();

        let mut line = String::new();
        let read = stdin.lock().read_line(&mut line)?;
        if read == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if pending.is_empty() && line.trim().is_empty() {
            continue;
        }
        if pending.is_empty() {
            if let Some(status) = repl_exit_status(line) {
                end_repl_session(shell, status);
            }
        }

        pending.push(line);
        if !pending.is_complete() {
            continue;
        }

        let is_multiline = pending.is_multiline();
        let script = pending.take();
        if is_multiline {
            let _ = shell.execute_interactive_script(&script);
        } else {
            let _ = shell.execute_interactive_line(script.trim());
        }
        flush_repl_output();
        // niubash R1: same exit-jump consumption as the line-editor loop.
        if let Some(status) = shell.take_session_exit_jump() {
            end_repl_session(shell, status);
        }
    }
    let _ = shell.finish_with_exit_trap(0);
    Ok(())
}

fn flush_repl_output() {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use reedline::Menu;

    #[test]
    fn emacs_keybindings_keep_ctrl_r_history_search_and_tab_completion() {
        let mut keybindings = default_emacs_keybindings();
        add_menu_keybindings(&mut keybindings);
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('r')),
            Some(ReedlineEvent::SearchHistory)
        );
        assert!(matches!(
            keybindings.find_binding(KeyModifiers::NONE, KeyCode::Tab),
            Some(ReedlineEvent::UntilFound(_))
        ));
    }

    #[test]
    fn history_exclusion_prefix_tracks_ignore_space_config() {
        assert_eq!(history_exclusion_prefix(false), None);
        assert_eq!(history_exclusion_prefix(true), Some(" ".to_string()));
    }

    #[test]
    fn repl_reserved_commands_cover_exit_logout_and_self_update_words() {
        for word in ["exit", "logout", "self-update", "update-niubash"] {
            assert!(is_repl_reserved_command(word), "{word} should be reserved");
        }
        // Case-sensitive, matching the REPL's own exact-match intercepts.
        assert!(!is_repl_reserved_command("Self-Update"));
        assert!(!is_repl_reserved_command("self_update"));
        assert!(!is_repl_reserved_command("ls"));
    }

    #[test]
    fn repl_exit_status_accepts_exit_and_logout_with_a_status_argument() {
        assert_eq!(repl_exit_status("exit"), Some(0));
        assert_eq!(repl_exit_status("  exit  "), Some(0));
        assert_eq!(repl_exit_status("exit 3"), Some(3));
        assert_eq!(repl_exit_status("exit 3;"), Some(3));
        assert_eq!(repl_exit_status("logout"), Some(0));
        assert_eq!(repl_exit_status("logout 7"), Some(7));
        // GNU get_exitstat skips one leading `--`.
        assert_eq!(repl_exit_status("exit --"), Some(0));
        assert_eq!(repl_exit_status("exit -- 9"), Some(9));
        // Eight-bit truncation, same as the engine's exit builtin.
        assert_eq!(repl_exit_status("exit 258"), Some(2));
        assert_eq!(repl_exit_status("exit -1"), Some(255));
    }

    #[test]
    fn repl_exit_status_leaves_diagnosed_shapes_to_the_engine() {
        // bash 5.3 reports these and keeps the session alive, so the REPL
        // must not terminate on them.
        assert_eq!(repl_exit_status("exit abc"), None);
        assert_eq!(repl_exit_status("exit 3 4"), None);
        assert_eq!(repl_exit_status("exit 3 4;"), None);
        // Not exit/logout at all.
        assert_eq!(repl_exit_status("echo exit 3"), None);
        assert_eq!(repl_exit_status("exit3"), None);
        assert_eq!(repl_exit_status(""), None);
    }

    #[test]
    fn configured_list_menu_preserves_menu_name() {
        let menu = configured_list_menu(
            "custom_menu",
            12,
            MenuConfig {
                completion_page_size: 12,
                history_page_size: 7,
                max_entry_lines: 3,
                completion_style: CompletionStyle::default(),
            },
        );

        assert_eq!(menu.name(), "custom_menu");
    }

    #[test]
    fn completion_menu_passes_full_buffer_not_only_difference() {
        // When FromStr calls configure the completion-list menu, only_buffer_difference
        // must be false so that the completer sees the entire input line including the
        // command word and any text before the cursor. Otherwise `cd repo<Tab>` would
        // only get `repo` (and worse, `cmak<Tab>` after menu activation would only get
        // `k`, producing irrelevant PATH suggestions like `kill` or `klist`).
        let completion_menu = configured_list_menu(COMPLETION_MENU, 10, MenuConfig::default());
        assert_eq!(completion_menu.name(), COMPLETION_MENU);
    }

    #[test]
    fn history_menu_uses_full_buffer_for_search() {
        let history_menu = configured_list_menu(HISTORY_MENU, 7, MenuConfig::default());
        assert_eq!(history_menu.name(), HISTORY_MENU);
    }

    #[test]
    fn repl_input_complete_tracks_if_blocks() {
        assert!(!is_repl_input_complete("if [ $HTTP_CODE -eq 200 ]; then"));
        assert!(!is_repl_input_complete(
            "if [ $HTTP_CODE -eq 200 ]; then\n  echo OK"
        ));
        assert!(is_repl_input_complete(
            "if [ $HTTP_CODE -eq 200 ]; then\n  echo OK\nfi"
        ));
        assert!(is_repl_input_complete(
            "if [ $HTTP_CODE -eq 200 ]; then echo OK; fi"
        ));
    }

    #[test]
    fn repl_input_complete_tracks_loop_and_case_blocks() {
        assert!(!is_repl_input_complete("for item in a b; do"));
        assert!(is_repl_input_complete(
            "for item in a b; do\n  echo $item\ndone"
        ));

        assert!(is_repl_input_complete(
            "for ((i=0; i<height; i++)); do\n  echo $i\ndone"
        ));
        assert!(!is_repl_input_complete(
            "for ((i=0; i<height; i++); do\n  echo $i\ndone"
        ));

        assert!(!is_repl_input_complete("while true; do"));
        assert!(is_repl_input_complete("while true; do\n  break\ndone"));

        assert!(!is_repl_input_complete("case $x in"));
        assert!(is_repl_input_complete(
            "case $x in\n  a) echo A ;;\n  *) echo other ;;\nesac"
        ));
    }

    #[test]
    fn repl_input_complete_tracks_functions_and_brace_groups() {
        assert!(!is_repl_input_complete("hello()"));
        assert!(!is_repl_input_complete("hello() {"));
        assert!(is_repl_input_complete("hello() {\n  echo hi\n}"));
        assert!(is_repl_input_complete("{ echo hi; }"));
    }

    #[test]
    fn repl_validator_holds_incomplete_input_in_the_buffer() {
        // Enter on an unfinished command must not submit: the validator keeps
        // the buffer open so multi-line input stays editable (arrows work
        // across lines) until the last line completes it.
        let validator = ReplValidator;
        assert!(matches!(
            validator.validate("echo \"unterminated"),
            ValidationResult::Incomplete
        ));
        assert!(matches!(
            validator.validate("echo one \\"),
            ValidationResult::Incomplete
        ));
        assert!(matches!(
            validator.validate("if true; then"),
            ValidationResult::Incomplete
        ));
        assert!(matches!(
            validator.validate("echo done"),
            ValidationResult::Complete
        ));
        assert!(matches!(
            validator.validate("echo one \\\n  two"),
            ValidationResult::Complete
        ));
    }

    #[test]
    fn repl_input_complete_tracks_quotes_and_list_continuations() {
        assert!(!is_repl_input_complete("echo \"unterminated"));
        assert!(is_repl_input_complete("echo \"terminated\""));
        assert!(!is_repl_input_complete("echo one |"));
        assert!(is_repl_input_complete("echo one |\n  grep one"));
        assert!(!is_repl_input_complete("echo one \\"));
        assert!(is_repl_input_complete("echo one \\\n  two"));
    }

    #[test]
    fn repl_input_complete_tracks_heredoc_delimiters() {
        // The engine treats a lone << operator line as an unterminated
        // here-doc body (bash parse.y gather_here_documents pulls the body
        // from the input stream before executing), so the REPL must keep
        // reading instead of submitting the line alone.
        assert!(!is_repl_input_complete("cat > out.txt <<'EOF'"));
        assert!(!is_repl_input_complete("cat > out.txt <<\"EOF\""));
        assert!(!is_repl_input_complete("cat > out.txt <<EOF"));
        assert!(!is_repl_input_complete("cat > out.txt <<-EOF"));
        assert!(!is_repl_input_complete("cat <<A <<'B'\nA-body\nA"));
        assert!(is_repl_input_complete("cat > out.txt <<'EOF'\nbody A\nEOF"));
        assert!(is_repl_input_complete("cat > out.txt <<EOF\nbody A\nEOF"));
        // A << inside quotes is not a here-doc operator.
        assert!(is_repl_input_complete("echo \"<<EOF\""));
    }

    #[test]
    fn repl_input_complete_ignores_heredoc_body_syntax() {
        // Block keywords inside a here-doc body are opaque data (git-summary's
        // usage() body contains `... repos; if omitted, ...`, which used to
        // push a phantom `fi` block and wedge the REPL in continuation mode).
        assert!(is_repl_input_complete("f() {\ncat <<EOF\nx; if y\nEOF\n}"));
        assert!(is_repl_input_complete("cat <<EOF\nx; if y\nEOF"));
        assert!(is_repl_input_complete(
            "cat <<'EOF'\nbody with \"quotes && (parens)\nEOF"
        ));
        // <<- strips leading tabs from the delimiter line only.
        assert!(is_repl_input_complete(
            "cat <<-EOF\n\tindented; if x\n\tEOF"
        ));
        // Sequential multi-heredoc gathering.
        assert!(is_repl_input_complete(
            "cat <<E1 <<E2\nA; if a\nB; fi b\nE1\nE2"
        ));
        // A here-string is not a heredoc; its payload stays code.
        assert!(!is_repl_input_complete("cat <<< \"unterminated"));
        // Unterminated bodies still keep the REPL reading.
        assert!(!is_repl_input_complete("cat <<EOF\nnever closed"));
        // A delimiter line with trailing text is body data, not a delimiter.
        assert!(!is_repl_input_complete(
            "cat <<EOF\nbody\nEOF trailing\nmore"
        ));
    }

    #[test]
    fn repl_input_complete_ignores_shell_comments() {
        assert!(is_repl_input_complete("# 11. 条件判断 (if)"));
        assert!(is_repl_input_complete(
            "# case/esac/function() are comments"
        ));
        assert!(is_repl_input_complete(
            "# 11. 条件判断 (if)\nprintf \"ok\\n\""
        ));
        assert!(is_repl_input_complete("echo foo#bar"));
        assert!(is_repl_input_complete("echo foo # if (comment)"));
        assert!(!is_repl_input_complete("if true; then\n  # fi in comment"));
        assert!(is_repl_input_complete(
            "if true; then\n  # fi in comment\n  echo ok\nfi"
        ));
    }

    #[test]
    fn vi_keybindings_keep_ctrl_r_history_search_and_tab_completion() {
        let mut insert = default_vi_insert_keybindings();
        let mut normal = default_vi_normal_keybindings();
        add_menu_keybindings(&mut insert);
        add_menu_keybindings(&mut normal);

        for keybindings in [insert, normal] {
            assert_eq!(
                keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('r')),
                Some(ReedlineEvent::SearchHistory)
            );
            assert!(matches!(
                keybindings.find_binding(KeyModifiers::NONE, KeyCode::Tab),
                Some(ReedlineEvent::UntilFound(_))
            ));
        }
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn default_keybindings_keep_ctrl_c_as_interrupt_and_add_ctrl_v_paste() {
        let mut emacs = default_emacs_keybindings();
        add_system_clipboard_keybindings(&mut emacs);
        assert_eq!(
            emacs.find_binding(KeyModifiers::CONTROL, KeyCode::Char('c')),
            Some(ReedlineEvent::CtrlC)
        );
        assert_eq!(
            emacs.find_binding(KeyModifiers::CONTROL, KeyCode::Char('v')),
            Some(ReedlineEvent::Edit(vec![EditCommand::PasteSystem]))
        );
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn vi_insert_keybindings_add_ctrl_v_paste() {
        let mut insert = default_vi_insert_keybindings();
        add_system_clipboard_keybindings(&mut insert);
        assert_eq!(
            insert.find_binding(KeyModifiers::CONTROL, KeyCode::Char('v')),
            Some(ReedlineEvent::Edit(vec![EditCommand::PasteSystem]))
        );
    }

    #[test]
    fn native_widget_preset_adds_autosuggest_accept_binding() {
        let mut keybindings = default_emacs_keybindings();
        let config = NativeWidgetConfig {
            enabled: true,
            presets: vec!["autosuggestions".to_string()],
            import_bindkeys: false,
        };

        add_native_widget_keybindings(&mut keybindings, NativeKeymapTarget::Emacs, &config, &[]);

        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char(' ')),
            Some(ReedlineEvent::HistoryHintComplete)
        );
    }

    #[test]
    fn native_widget_imports_recognized_bindkey_widgets() {
        let mut keybindings = default_emacs_keybindings();
        let config = NativeWidgetConfig {
            enabled: true,
            presets: Vec::new(),
            import_bindkeys: true,
        };
        let bindings = vec![native_widget_binding("^ ", None, "autosuggest-accept")];

        add_bundle_widget_keybindings(
            &mut keybindings,
            NativeKeymapTarget::Emacs,
            &config,
            &bindings,
        );

        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char(' ')),
            Some(ReedlineEvent::HistoryHintComplete)
        );
    }

    #[test]
    fn native_widget_imports_named_bundle_key_syntax() {
        let mut keybindings = default_emacs_keybindings();
        let config = NativeWidgetConfig {
            enabled: true,
            presets: Vec::new(),
            import_bindkeys: true,
        };
        let bindings = vec![
            native_widget_binding("Ctrl+A", Some("emacs"), "beginning-of-line"),
            native_widget_binding("Alt+B", Some("emacs"), "backward-word"),
            native_widget_binding("Shift+Tab", Some("emacs"), "menu-previous"),
        ];

        add_bundle_widget_keybindings(
            &mut keybindings,
            NativeKeymapTarget::Emacs,
            &config,
            &bindings,
        );

        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('a')),
            Some(edit_event(EditCommand::MoveToLineStart { select: false }))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('b')),
            Some(edit_event(EditCommand::MoveWordLeft { select: false }))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::SHIFT, KeyCode::BackTab),
            Some(ReedlineEvent::MenuPrevious)
        );
    }

    #[test]
    fn native_widget_bindkeys_respect_vi_keymaps() {
        let mut insert = default_vi_insert_keybindings();
        let mut normal = default_vi_normal_keybindings();
        let config = NativeWidgetConfig {
            enabled: true,
            presets: Vec::new(),
            import_bindkeys: true,
        };
        let bindings = vec![native_widget_binding(
            "^F",
            Some("viins"),
            "autosuggest-accept",
        )];

        add_bundle_widget_keybindings(
            &mut insert,
            NativeKeymapTarget::ViInsert,
            &config,
            &bindings,
        );
        add_bundle_widget_keybindings(
            &mut normal,
            NativeKeymapTarget::ViNormal,
            &config,
            &bindings,
        );

        assert_eq!(
            insert.find_binding(KeyModifiers::CONTROL, KeyCode::Char('f')),
            Some(ReedlineEvent::HistoryHintComplete)
        );
        assert_ne!(
            normal.find_binding(KeyModifiers::CONTROL, KeyCode::Char('f')),
            Some(ReedlineEvent::HistoryHintComplete)
        );
    }

    #[test]
    fn native_widget_maps_history_substring_arrows_to_history_navigation() {
        let mut keybindings = default_emacs_keybindings();
        let config = NativeWidgetConfig {
            enabled: true,
            presets: Vec::new(),
            import_bindkeys: true,
        };
        let bindings = vec![
            native_widget_binding("^[[A", None, "history-substring-search-up"),
            native_widget_binding("^[[B", None, "history-substring-search-down"),
        ];

        add_bundle_widget_keybindings(
            &mut keybindings,
            NativeKeymapTarget::Emacs,
            &config,
            &bindings,
        );

        assert_eq!(
            keybindings.find_binding(KeyModifiers::NONE, KeyCode::Up),
            Some(ReedlineEvent::Up)
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::NONE, KeyCode::Down),
            Some(ReedlineEvent::Down)
        );
    }

    #[test]
    fn native_widget_maps_standard_editor_widgets_to_reedline_events() {
        let mut keybindings = default_emacs_keybindings();
        let config = NativeWidgetConfig {
            enabled: true,
            presets: Vec::new(),
            import_bindkeys: true,
        };
        let bindings = vec![
            native_widget_binding("^A", None, "beginning-of-line"),
            native_widget_binding("^E", None, "end-of-line"),
            native_widget_binding("^[b", None, "backward-word"),
            native_widget_binding("\\ef", None, "forward-word"),
            native_widget_binding("^K", None, "kill-line"),
            native_widget_binding("^L", None, "clear-screen"),
            native_widget_binding("^M", None, "accept-line"),
            native_widget_binding("^I", None, "expand-or-complete"),
        ];

        add_bundle_widget_keybindings(
            &mut keybindings,
            NativeKeymapTarget::Emacs,
            &config,
            &bindings,
        );

        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('a')),
            Some(edit_event(EditCommand::MoveToLineStart { select: false }))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('e')),
            Some(edit_event(EditCommand::MoveToLineEnd { select: false }))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('b')),
            Some(edit_event(EditCommand::MoveWordLeft { select: false }))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('f')),
            Some(edit_event(EditCommand::MoveWordRight { select: false }))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('k')),
            Some(edit_event(EditCommand::CutToLineEnd))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('l')),
            Some(ReedlineEvent::ClearScreen)
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::NONE, KeyCode::Enter),
            Some(ReedlineEvent::Enter)
        );
        assert!(matches!(
            keybindings.find_binding(KeyModifiers::NONE, KeyCode::Tab),
            Some(ReedlineEvent::UntilFound(_))
        ));
    }

    #[test]
    fn native_widget_maps_bash_history_navigation_aliases() {
        for widget in [
            "history-substring-search-up",
            "history-search-backward",
            "up-line-or-history",
        ] {
            assert_eq!(native_widget_event(widget), Some(ReedlineEvent::Up));
        }
        for widget in [
            "history-substring-search-down",
            "history-search-forward",
            "down-line-or-history",
        ] {
            assert_eq!(native_widget_event(widget), Some(ReedlineEvent::Down));
        }
    }

    fn native_widget_binding(key: &str, keymap: Option<&str>, widget: &str) -> NativeWidgetBinding {
        NativeWidgetBinding {
            widget: widget.to_string(),
            function: None,
            key: Some(key.to_string()),
            keymap: keymap.map(str::to_string),
            source_file: None,
            line: None,
            origin: "test".to_string(),
        }
    }

    #[test]
    fn native_widget_unknown_widget_becomes_host_command_sentinel() {
        let mut keybindings = default_emacs_keybindings();
        let config = NativeWidgetConfig {
            enabled: true,
            presets: Vec::new(),
            import_bindkeys: true,
        };
        let bindings = vec![native_widget_binding("^X", None, "niu_fzf_file")];

        add_bundle_widget_keybindings(
            &mut keybindings,
            NativeKeymapTarget::Emacs,
            &config,
            &bindings,
        );

        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('x')),
            Some(ReedlineEvent::ExecuteHostCommand(format!(
                "{}niu_fzf_file",
                WIDGET_HOST_COMMAND_PREFIX
            )))
        );
    }

    #[test]
    fn bundle_widget_bindings_apply_even_when_feature_flag_disabled() {
        let mut keybindings = default_emacs_keybindings();
        let config = NativeWidgetConfig {
            enabled: false,
            presets: Vec::new(),
            import_bindkeys: true,
        };
        let bindings = vec![native_widget_binding(
            "Ctrl+R",
            None,
            "history-incremental-search-backward",
        )];

        add_bundle_widget_keybindings(
            &mut keybindings,
            NativeKeymapTarget::Emacs,
            &config,
            &bindings,
        );

        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('r')),
            Some(ReedlineEvent::SearchHistory)
        );
    }

    #[test]
    fn parse_user_bindkeys_parses_multiline_entries() {
        let value = "# comment line\nCtrl+X:niu_fzf_file\n\n   Alt+G : niu_git_status \nmalformed-no-colon\n:missing-key\nmissing-widget:\n";
        let bindings = parse_user_bindkeys(value);

        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].key.as_deref(), Some("Ctrl+X"));
        assert_eq!(bindings[0].widget, "niu_fzf_file");
        assert_eq!(bindings[0].keymap, None);
        assert_eq!(bindings[0].origin, "user");
        assert_eq!(bindings[1].key.as_deref(), Some("Alt+G"));
        assert_eq!(bindings[1].widget, "niu_git_status");
    }

    #[test]
    fn parse_user_bindkeys_accepts_empty_value() {
        assert!(parse_user_bindkeys("").is_empty());
    }

    #[test]
    fn user_widget_bindings_apply_without_pack_gate() {
        let mut keybindings = default_emacs_keybindings();
        let bindings = parse_user_bindkeys(
            "Ctrl+G:niu_git_status\nCtrl+R:history-incremental-search-backward",
        );

        add_user_widget_keybindings(&mut keybindings, &bindings);

        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('g')),
            Some(ReedlineEvent::ExecuteHostCommand(format!(
                "{}niu_git_status",
                WIDGET_HOST_COMMAND_PREFIX
            )))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('r')),
            Some(ReedlineEvent::SearchHistory)
        );
    }

    #[test]
    fn user_widget_bindings_apply_to_both_vi_keymaps() {
        let mut insert = default_vi_insert_keybindings();
        let mut normal = default_vi_normal_keybindings();
        let bindings = parse_user_bindkeys("Ctrl+G:niu_git_status");

        add_user_widget_keybindings(&mut insert, &bindings);
        add_user_widget_keybindings(&mut normal, &bindings);

        let expected = Some(ReedlineEvent::ExecuteHostCommand(format!(
            "{}niu_git_status",
            WIDGET_HOST_COMMAND_PREFIX
        )));
        assert_eq!(
            insert.find_binding(KeyModifiers::CONTROL, KeyCode::Char('g')),
            expected
        );
        assert_eq!(
            normal.find_binding(KeyModifiers::CONTROL, KeyCode::Char('g')),
            expected
        );
    }

    #[test]
    fn bindx_invocation_parses_indexed_sentinels() {
        assert_eq!(
            BindXInvocation::parse("__niu_bindx 0"),
            Some(BindXInvocation { index: 0 })
        );
        assert_eq!(
            BindXInvocation::parse("__niu_bindx 12"),
            Some(BindXInvocation { index: 12 })
        );
        assert_eq!(BindXInvocation::parse("__niu_bindx "), None);
        assert_eq!(BindXInvocation::parse("__niu_bindx x"), None);
        assert_eq!(BindXInvocation::parse("__niu_bindx 1 2"), None);
        assert_eq!(BindXInvocation::parse("echo __niu_bindx 1"), None);
        assert_eq!(BindXInvocation::parse("__niu_widget niu_fzf_file"), None);
    }

    #[test]
    fn parse_key_sequence_reads_gnu_bind_forms() {
        // The GNU bind/inputrc forms plugins actually write
        // (fzf key-bindings.bash: '\C-r', '\C-s', '\e[200~' family).
        assert_eq!(
            parse_key_sequence(r#"\C-r"#),
            Some((KeyModifiers::CONTROL, KeyCode::Char('r')))
        );
        assert_eq!(
            parse_key_sequence(r#"\C-o"#),
            Some((KeyModifiers::CONTROL, KeyCode::Char('o')))
        );
        assert_eq!(
            parse_key_sequence(r#"\M-b"#),
            Some((KeyModifiers::ALT, KeyCode::Char('b')))
        );
        // Multi-key sequences are not representable as a single reedline
        // binding: the bridge warns and skips them.
        assert_eq!(parse_key_sequence(r#"\C-x\C-f"#), None);
        assert_eq!(
            parse_key_sequence(r#"\e[A"#),
            Some((KeyModifiers::NONE, KeyCode::Up))
        );
    }

    #[test]
    fn char_offset_to_byte_walks_multibyte_boundaries() {
        assert_eq!(char_offset_to_byte("", 0), 0);
        assert_eq!(char_offset_to_byte("abc", 1), 1);
        assert_eq!(char_offset_to_byte("abc", 9), 3, "clamps past end");
        // READLINE_POINT is a CHARACTER offset (bashline.c:4540
        // readline_get_char_offset): two CJK chars in, byte 6 out.
        assert_eq!(char_offset_to_byte("中文x", 2), 6);
        assert_eq!(char_offset_to_byte("中文x", 3), 7);
    }

    #[test]
    fn engine_bind_entries_mirror_into_reedline_keybindings() {
        let mut keybindings = default_emacs_keybindings();
        let entries = vec![
            rubash::shell::bind_registry::BindEntry {
                keyseq: r#"\C-r"#.to_string(),
                kind: rubash::shell::bind_registry::BindKind::Execute {
                    command: "__fzf_history__".to_string(),
                },
                keymap: Some("emacs-standard".to_string()),
            },
            rubash::shell::bind_registry::BindEntry {
                keyseq: r#"\C-t"#.to_string(),
                kind: rubash::shell::bind_registry::BindKind::Macro {
                    text: "ins-macro".to_string(),
                },
                keymap: None,
            },
            rubash::shell::bind_registry::BindEntry {
                keyseq: r#"\C-o"#.to_string(),
                kind: rubash::shell::bind_registry::BindKind::Function {
                    name: "accept-line".to_string(),
                },
                keymap: None,
            },
            // Unknown readline function name: GNU silently drops it
            // (wt100 probe), so the bridge reports it unusable.
            rubash::shell::bind_registry::BindEntry {
                keyseq: r#"\C-g"#.to_string(),
                kind: rubash::shell::bind_registry::BindKind::Function {
                    name: "no-such-fn".to_string(),
                },
                keymap: None,
            },
            // Multi-key sequence: reedline cannot represent it.
            rubash::shell::bind_registry::BindEntry {
                keyseq: r#"\C-x\C-f"#.to_string(),
                kind: rubash::shell::bind_registry::BindKind::Execute {
                    command: "multi".to_string(),
                },
                keymap: None,
            },
            // vi-insert keymap entry must not land on the emacs keymap
            // (\C-6 is unbound in the emacs defaults, so a hit would be
            // ours and ours alone).
            rubash::shell::bind_registry::BindEntry {
                keyseq: r#"\C-6"#.to_string(),
                kind: rubash::shell::bind_registry::BindKind::Execute {
                    command: "vi-only".to_string(),
                },
                keymap: Some("vi-insert".to_string()),
            },
        ];

        let unusable =
            add_engine_bind_keybindings(&mut keybindings, NativeKeymapTarget::Emacs, &entries);

        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('r')),
            Some(ReedlineEvent::ExecuteHostCommand(
                "__niu_bindx 0".to_string()
            ))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('t')),
            Some(edit_event(EditCommand::InsertString(
                "ins-macro".to_string()
            )))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('o')),
            Some(ReedlineEvent::Enter)
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('6')),
            None,
            "vi-insert entry must not bind on the emacs keymap"
        );
        assert_eq!(
            unusable,
            vec![r#"\C-g"#.to_string(), r#"\C-x\C-f"#.to_string()]
        );
    }

    #[test]
    fn engine_keymap_targets_split_vi_insert_and_normal() {
        assert!(engine_keymap_applies(None, NativeKeymapTarget::Emacs));
        assert!(!engine_keymap_applies(None, NativeKeymapTarget::ViInsert));
        assert!(engine_keymap_applies(
            Some("vi-insert"),
            NativeKeymapTarget::ViInsert
        ));
        assert!(engine_keymap_applies(
            Some("vi"),
            NativeKeymapTarget::ViNormal
        ));
        assert!(engine_keymap_applies(
            Some("vi-command"),
            NativeKeymapTarget::ViNormal
        ));
        assert!(!engine_keymap_applies(
            Some("vi-insert"),
            NativeKeymapTarget::ViNormal
        ));
        assert!(engine_keymap_applies(
            Some("emacs-meta"),
            NativeKeymapTarget::Emacs
        ));
    }

    #[test]
    fn widget_invocation_parse_accepts_only_sentinel_lines() {
        assert_eq!(
            WidgetInvocation::parse("__niu_widget niu_fzf_file"),
            Some(WidgetInvocation {
                function: "niu_fzf_file".to_string()
            })
        );
        assert_eq!(WidgetInvocation::parse("__niu_widget "), None);
        assert_eq!(WidgetInvocation::parse("__niu_widget  spaced name"), None);
        assert_eq!(WidgetInvocation::parse("__niu_widget "), None);
        assert_eq!(WidgetInvocation::parse("echo __niu_widget foo"), None);
        assert_eq!(WidgetInvocation::parse("ls -la"), None);
    }

    #[test]
    fn widget_invocation_parse_handles_multibyte_names() {
        let name = "niu_中文widget";
        assert_eq!(
            WidgetInvocation::parse(&format!("{}{}", WIDGET_HOST_COMMAND_PREFIX, name)),
            Some(WidgetInvocation {
                function: name.to_string()
            })
        );
    }

    // ---- PasteChunkEditMode (Windows paste-chunk detection, niubash#202/#284) ----

    fn paste_key(
        code: crossterm29::event::KeyCode,
        modifiers: crossterm29::event::KeyModifiers,
    ) -> ReedlineRawEvent {
        ReedlineRawEvent::try_from(crossterm29::event::Event::Key(
            crossterm29::event::KeyEvent::new(code, modifiers),
        ))
        .expect("press key events convert")
    }

    /// A detector whose Enter lookahead reads a scripted queue instead of
    /// the live console. `events` model what the console input queue holds
    /// at the moment the detector decides a bare Enter. Built on the
    /// product-default column completion style (niubash#486: its Enter
    /// stays bare inside the TAB window — the engine's menu first-refusal
    /// commits the highlighted candidate).
    fn paste_detector_with_queue(events: Vec<crossterm29::event::Event>) -> PasteChunkEditMode {
        paste_detector_and_queue(events).0
    }

    /// [`paste_detector_with_queue`] plus the queue's own handle, so a test
    /// can feed events as the dispatch progresses — the real console queue
    /// keeps growing while the feed arrives (each pasted CRLF queues its LF
    /// partner behind its CR).
    fn paste_detector_and_queue(
        events: Vec<crossterm29::event::Event>,
    ) -> (
        PasteChunkEditMode,
        std::sync::Arc<std::sync::Mutex<VecDeque<crossterm29::event::Event>>>,
    ) {
        paste_detector_and_queue_for_style(events, CompletionStyle::Column)
    }

    /// [`paste_detector_and_queue`] under an explicit completion style —
    /// the niubash#373 dismissal prefix is a GNU-style shape, so the tests
    /// that pin it need a GNU detector (niubash#486).
    fn paste_detector_and_queue_for_style(
        events: Vec<crossterm29::event::Event>,
        completion_style: CompletionStyle,
    ) -> (
        PasteChunkEditMode,
        std::sync::Arc<std::sync::Mutex<VecDeque<crossterm29::event::Event>>>,
    ) {
        use std::sync::{Arc, Mutex};
        let queue = Arc::new(Mutex::new(VecDeque::from(events)));
        let detector = PasteChunkEditMode::with_scripted_queue(
            Box::new(Emacs::new(default_emacs_keybindings())),
            queue.clone(),
            completion_style,
        );
        (detector, queue)
    }

    /// The common case: the queue behind the Enter is empty (a human's
    /// submit, a ConPTY driver's `send_line`, an expect script).
    fn paste_detector() -> PasteChunkEditMode {
        paste_detector_with_queue(Vec::new())
    }

    /// [`paste_detector`] under the GNU completion style — the style whose
    /// Enter inside the TAB window still carries the niubash#373 dismissal
    /// prefix.
    fn gnu_paste_detector() -> PasteChunkEditMode {
        paste_detector_and_queue_for_style(Vec::new(), CompletionStyle::Gnu).0
    }

    fn cr() -> ReedlineRawEvent {
        paste_key(
            crossterm29::event::KeyCode::Enter,
            crossterm29::event::KeyModifiers::NONE,
        )
    }

    /// A TAB key press — the completion menu's opener and the start of the
    /// dismissal-prefix window (niubash#373).
    fn tab() -> ReedlineRawEvent {
        paste_key(
            crossterm29::event::KeyCode::Tab,
            crossterm29::event::KeyModifiers::NONE,
        )
    }

    /// A Ctrl+R chord — the history-search verb. The ENTER that follows it
    /// belongs to the search: the dismissal prefix must never ride it.
    fn ctrl_r() -> ReedlineRawEvent {
        paste_key(
            crossterm29::event::KeyCode::Char('r'),
            crossterm29::event::KeyModifiers::CONTROL,
        )
    }

    fn lf() -> ReedlineRawEvent {
        paste_key(
            crossterm29::event::KeyCode::Enter,
            crossterm29::event::KeyModifiers::CONTROL,
        )
    }

    /// The LF partner as the raw console event the scripted queue holds
    /// (`Event::Key`, not the reedline wrapper `lf()` produces).
    fn lf_key_event() -> crossterm29::event::Event {
        crossterm29::event::Event::Key(crossterm29::event::KeyEvent::new(
            crossterm29::event::KeyCode::Enter,
            crossterm29::event::KeyModifiers::CONTROL,
        ))
    }

    /// A bare CR as the raw console event the scripted queue holds — the
    /// KEY_EVENT record ConPTY synthesizes for a fed `\r` byte and the only
    /// line-break shape a Windows Terminal paste delivers (niubash#341).
    fn cr_key_event() -> crossterm29::event::Event {
        crossterm29::event::Event::Key(crossterm29::event::KeyEvent::new(
            crossterm29::event::KeyCode::Enter,
            crossterm29::event::KeyModifiers::NONE,
        ))
    }

    /// A bare text key press as the raw console event.
    fn char_key_event(ch: char) -> crossterm29::event::Event {
        crossterm29::event::Event::Key(crossterm29::event::KeyEvent::new(
            crossterm29::event::KeyCode::Char(ch),
            crossterm29::event::KeyModifiers::NONE,
        ))
    }

    /// Flatten a translated event tree the way reedline's
    /// `process_input_batch` flattens `Multiple` before dispatching.
    fn flatten_reedline_events(event: ReedlineEvent, out: &mut Vec<ReedlineEvent>) {
        match event {
            ReedlineEvent::Multiple(events) => {
                for event in events {
                    flatten_reedline_events(event, out);
                }
            }
            other => out.push(other),
        }
    }

    /// Drive the detector through reedline's read-loop protocol (reedline
    /// 0.50.0 `src/engine.rs`): (R1) batch-read console events and stop the
    /// batch at a bare Enter (`completed()`, engine.rs:1052-1067); (R2)
    /// convert the WHOLE batch through `parse_event` before dispatching
    /// anything (`process_input_batch`, engine.rs:1201-1246); (R3) dispatch
    /// until the first `Exits` — a submitted buffer — and drop the rest of
    /// the batch (engine.rs:1246-1258). `slices` refill the scripted
    /// console queue at batch boundaries, the way a ConPTY feed's write
    /// slices keep arriving while the shell processes the previous batch.
    /// Returns the submitted buffers in order plus the final unsubmitted
    /// buffer. The R1/R2/R3 interplay is where niubash#341 lost bytes: this
    /// harness is the protocol-level reproduction.
    fn run_reedline_batch_protocol(
        mode: &mut PasteChunkEditMode,
        queue: &std::sync::Arc<std::sync::Mutex<VecDeque<crossterm29::event::Event>>>,
        slices: &mut VecDeque<Vec<crossterm29::event::Event>>,
    ) -> (Vec<String>, String) {
        let mut submitted = Vec::new();
        let mut buffer = String::new();
        'session: loop {
            // R1, first event of the batch: reedline blocks here; a slice
            // refill models the feed's next write arriving.
            while queue.lock().expect("test queue").is_empty() {
                match slices.pop_front() {
                    Some(slice) => {
                        queue.lock().expect("test queue").extend(slice);
                    }
                    None => break 'session,
                }
            }
            let mut batch: Vec<crossterm29::event::Event> = Vec::new();
            while let Some(event) = queue.lock().expect("test queue").pop_front() {
                let stops_batch = match &event {
                    crossterm29::event::Event::Key(key) => is_bare_enter(key),
                    _ => false,
                };
                batch.push(event);
                if stops_batch {
                    break;
                }
            }
            // R2: the whole batch is converted before anything dispatches.
            let mut flat: Vec<ReedlineEvent> = Vec::new();
            for raw in batch {
                let Ok(raw) = ReedlineRawEvent::try_from(raw) else {
                    continue;
                };
                flatten_reedline_events(mode.parse_event(raw), &mut flat);
            }
            // R3: dispatch; the first Exits drops the rest of the batch.
            for event in flat {
                match event {
                    ReedlineEvent::Edit(commands) => {
                        for command in commands {
                            match command {
                                EditCommand::InsertChar(c) => buffer.push(c),
                                EditCommand::InsertString(s) => buffer.push_str(&s),
                                other => {
                                    panic!("sim: unexpected edit command {other:?}")
                                }
                            }
                        }
                    }
                    ReedlineEvent::Enter => {
                        submitted.push(std::mem::take(&mut buffer));
                        continue 'session;
                    }
                    _ => {}
                }
            }
        }
        (submitted, buffer)
    }

    /// The text the detector's dispatches deliver to the edit buffer across
    /// a whole WT KEY_EVENT paste: submitted buffers joined at their Enter
    /// boundaries, then the final unsubmitted buffer.
    fn reassemble_protocol_text(submitted: &[String], final_buffer: &str) -> String {
        let mut text = submitted.join("\n");
        if !final_buffer.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(final_buffer);
        }
        text
    }

    /// niubash#341, the owner's Windows Terminal corruption as a
    /// protocol-level regression. A WT paste reaches niubash as per-char
    /// KEY_EVENT records with a bare CR per line break — no LF partner
    /// ever exists, and an empty line is two consecutive CRs (`\r\r`).
    /// Sliced the way ConPTY's write slices cut a paste (here: after the
    /// empty line, then mid-line), the empty line's second CR used to be
    /// parked in the pushback by the Enter probe; reedline translated it
    /// into a batch-leading Enter on the next batch, and the first-Exits
    /// drop shredded the batch's queued text — `if is_even 10; then`
    /// executed as `_even 10; then`. Every byte must survive instead.
    #[test]
    fn wt_bare_cr_paste_survives_reedline_batch_protocol_341() {
        let script = "}\r\rif is_even 10; then\r  echo \"10 是偶数\"\rfi\r";
        let events: Vec<crossterm29::event::Event> = script
            .chars()
            .map(|ch| match ch {
                '\r' => cr_key_event(),
                other => char_key_event(other),
            })
            .collect();
        // Slice cuts matching the observed corruption: the `\r\r` adjacency
        // lands in one ConPTY write, the next line's prefix in the next.
        let slices: VecDeque<Vec<crossterm29::event::Event>> = VecDeque::from(vec![
            events[0..3].to_vec(),
            events[3..9].to_vec(),
            events[9..].to_vec(),
        ]);
        let (mut detector, queue) = paste_detector_and_queue(Vec::new());
        let mut slices = slices;
        let (submitted, final_buffer) =
            run_reedline_batch_protocol(&mut detector, &queue, &mut slices);
        let expected = script.replace('\r', "\n");
        let expected = expected.strip_suffix('\n').expect("script ends in a CR");
        let actual = reassemble_protocol_text(&submitted, &final_buffer);
        assert_eq!(
            actual, expected,
            "the WT KEY_EVENT paste lost or altered bytes across the batch boundary"
        );
        assert!(
            submitted
                .iter()
                .chain(std::iter::once(&final_buffer))
                .any(|line| line.contains("if is_even 10; then")),
            "the if line must reach the editor intact, not as `_even 10; then`"
        );
    }

    /// The same corruption shape with the ConPTY slice cut at EVERY event
    /// boundary: no matter where the feed's writes split the stream, the
    /// protocol delivers the script byte for byte.
    #[test]
    fn wt_bare_cr_paste_survives_every_slice_cut_341() {
        let script = "a=1\r\rif true; then\r\techo branch\rfi\r";
        let events: Vec<crossterm29::event::Event> = script
            .chars()
            .map(|ch| match ch {
                '\r' => cr_key_event(),
                other => char_key_event(other),
            })
            .collect();
        for cut in 0..events.len() {
            let (mut detector, queue) = paste_detector_and_queue(Vec::new());
            let mut slices = VecDeque::from(vec![events[..cut].to_vec(), events[cut..].to_vec()]);
            let (submitted, final_buffer) =
                run_reedline_batch_protocol(&mut detector, &queue, &mut slices);
            let expected = script.replace('\r', "\n");
            let expected = expected.strip_suffix('\n').expect("script ends in a CR");
            assert_eq!(
                reassemble_protocol_text(&submitted, &final_buffer),
                expected,
                "slice cut at event {cut} lost or altered bytes"
            );
        }
    }

    /// A `\r\r\r` run — a line break followed by TWO empty lines — folds
    /// correctly and survives the protocol byte for byte at every slice
    /// cut: the probe consumes the first pair as two breaks, and the third
    /// CR (no partner behind it) submits what preceded it. The boundary the
    /// pair-fold must not over- or under-count.
    #[test]
    fn wt_bare_cr_paste_survives_triple_cr_run_341() {
        let script = "a=1\r\r\recho ok\r";
        let events: Vec<crossterm29::event::Event> = script
            .chars()
            .map(|ch| match ch {
                '\r' => cr_key_event(),
                other => char_key_event(other),
            })
            .collect();
        for cut in 0..events.len() {
            let (mut detector, queue) = paste_detector_and_queue(Vec::new());
            let mut slices = VecDeque::from(vec![events[..cut].to_vec(), events[cut..].to_vec()]);
            let (submitted, final_buffer) =
                run_reedline_batch_protocol(&mut detector, &queue, &mut slices);
            let expected = script.replace('\r', "\n");
            let expected = expected.strip_suffix('\n').expect("script ends in a CR");
            assert_eq!(
                reassemble_protocol_text(&submitted, &final_buffer),
                expected,
                "slice cut at event {cut} lost or altered bytes across the \\r\\r\\r run"
            );
        }
    }

    /// The owner's shape under a THREE-write feed: every pair of independent
    /// slice cuts on a short script — the two-cut generalization of the
    /// sweep above. No delivery split may lose or reorder a byte.
    #[test]
    fn wt_bare_cr_paste_survives_every_two_cut_split_341() {
        let script = "x=1\r\rif true; then\r\techo branch\rfi\r";
        let events: Vec<crossterm29::event::Event> = script
            .chars()
            .map(|ch| match ch {
                '\r' => cr_key_event(),
                other => char_key_event(other),
            })
            .collect();
        let expected = script.replace('\r', "\n");
        let expected = expected.strip_suffix('\n').expect("script ends in a CR");
        for cut1 in 0..events.len() {
            for cut2 in cut1..events.len() {
                let (mut detector, queue) = paste_detector_and_queue(Vec::new());
                let mut slices = VecDeque::from(vec![
                    events[..cut1].to_vec(),
                    events[cut1..cut2].to_vec(),
                    events[cut2..].to_vec(),
                ]);
                let (submitted, final_buffer) =
                    run_reedline_batch_protocol(&mut detector, &queue, &mut slices);
                assert_eq!(
                    reassemble_protocol_text(&submitted, &final_buffer),
                    expected,
                    "slice cuts at events {cut1}/{cut2} lost or altered bytes"
                );
            }
        }
    }

    /// Age the burst clock so the next parsed event starts a new burst.
    fn expire_burst(detector: &mut PasteChunkEditMode) {
        detector.last_event = Some(Instant::now() - PASTE_CHUNK_GAP - Duration::from_millis(1));
    }

    #[test]
    fn paste_chunk_detector_absorbs_crlf_paste_and_flushes_on_gap() {
        // Typing before the paste is delegated normally.
        let mut detector = paste_detector();
        for ch in "abc".chars() {
            let event = detector.parse_event(paste_key(
                crossterm29::event::KeyCode::Char(ch),
                crossterm29::event::KeyModifiers::NONE,
            ));
            assert!(
                matches!(event, ReedlineEvent::Edit(_)),
                "typed char must reach the editor"
            );
        }
        assert!(detector.pending.is_none());

        // The paste's first line break arrives as the CRLF pair: the CR is
        // dispatched while its LF partner is still queued (reedline's batch
        // stops at the bare CR, so the LF is always behind it). The CR with
        // its partner queued is paste data: the chunk opens with the pair's
        // one line break, the LF is consumed with it, and reedline never
        // sees the Enter.
        let (mut detector, queue) = paste_detector_and_queue(vec![lf_key_event()]);
        assert!(matches!(detector.parse_event(cr()), ReedlineEvent::None));
        assert_eq!(detector.pending.as_deref(), Some("\n"));
        assert!(
            detector.pushback.is_empty(),
            "the LF partner is consumed, not pushed back"
        );

        // Further burst text is absorbed quietly.
        for ch in "de".chars() {
            assert!(matches!(
                detector.parse_event(paste_key(
                    crossterm29::event::KeyCode::Char(ch),
                    crossterm29::event::KeyModifiers::NONE
                )),
                ReedlineEvent::None
            ));
        }
        assert_eq!(detector.pending.as_deref(), Some("\nde"));

        // The next pasted CRLF: the feed queues its LF partner behind its
        // CR (one console write — the real queue shape), the CR folds the
        // pair's one newline, and the partner is consumed with it — the LF
        // never dispatches as its own event (niubash#308).
        queue.lock().expect("test queue").push_back(lf_key_event());
        assert!(matches!(detector.parse_event(cr()), ReedlineEvent::None));
        assert_eq!(
            detector.pending.as_deref(),
            Some("\nde\n"),
            "the CRLF pair is one line break"
        );
        assert!(
            detector.pushback.is_empty(),
            "the interior LF partner is consumed, not pushed back"
        );

        // A lone Ctrl+J dispatched mid-burst (no CR head: an LF-only feed,
        // or the second break of adjacent newlines) is a real line break.
        assert!(matches!(detector.parse_event(lf()), ReedlineEvent::None));
        assert_eq!(detector.pending.as_deref(), Some("\nde\n\n"));

        // After the burst settles, the chunk is flushed as ONE insert and
        // the user's own Enter (empty queue behind it) still submits the
        // buffer — a BARE Enter: no TAB preceded it, so the dismissal
        // window is closed (niubash#373) and history search or any other
        // flow keeps the plain event its handler implements.
        expire_burst(&mut detector);
        let event = detector.parse_event(cr());
        match event {
            ReedlineEvent::Multiple(events) => {
                assert_eq!(events.len(), 2, "flush insert followed by the submit");
                assert!(matches!(&events[0], ReedlineEvent::Edit(_)));
                assert!(matches!(&events[1], ReedlineEvent::Enter));
            }
            other => panic!("expected flush + submit, got {other:?}"),
        }
        assert!(detector.pending.is_none());
    }

    /// niubash#308, the regression itself: a paste whose body arrived
    /// complete (every break a CRLF pair), then the user's Enter lands
    /// within the burst window — `send(paste_crlf + "\r")`, one write. The
    /// trailing bare CR has NO LF partner behind it (silence in the queue),
    /// so it is the submit, not more paste data: the chunk flushes and
    /// submits in ONE batch on the FIRST Enter. The pre-#308 behavior
    /// pushed the Enter into the chunk as a newline and nothing executed
    /// until a second Enter.
    #[test]
    fn paste_chunk_detector_enter_in_paste_burst_submits_the_chunk() {
        let (mut detector, _queue) = paste_detector_and_queue(vec![lf_key_event()]);
        // The paste body: CRLF pair opens the chunk (partner consumed),
        // then its text is absorbed.
        assert!(matches!(detector.parse_event(cr()), ReedlineEvent::None));
        for ch in "echo hi".chars() {
            assert!(matches!(
                detector.parse_event(paste_key(
                    crossterm29::event::KeyCode::Char(ch),
                    crossterm29::event::KeyModifiers::NONE
                )),
                ReedlineEvent::None
            ));
        }
        assert_eq!(detector.pending.as_deref(), Some("\necho hi"));

        // The user's Enter, microseconds after the last paste key (one
        // burst, empty queue behind it — the scripted queue is drained).
        let event = detector.parse_event(cr());
        match event {
            ReedlineEvent::Multiple(events) => {
                assert_eq!(events.len(), 2, "chunk insert followed by the submit");
                assert!(
                    matches!(
                        &events[0],
                        ReedlineEvent::Edit(commands)
                            if matches!(
                                commands.as_slice(),
                                [EditCommand::InsertString(chunk)]
                                    if chunk == "\necho hi"
                            )
                    ),
                    "the buffered chunk lands whole, Enter never becomes data"
                );
                assert!(matches!(&events[1], ReedlineEvent::Enter));
            }
            other => panic!("expected flush + submit, got {other:?}"),
        }
        assert!(detector.pending.is_none());
        assert!(detector.pushback.is_empty());
    }

    /// The data half of niubash#308: an Enter INSIDE the pasted block — its
    /// CR with the LF partner queued right behind it, the real CRLF-feed
    /// queue shape — stays paste data mid-burst. Only the partner-less bare
    /// CR (block complete) submits.
    #[test]
    fn paste_chunk_detector_interior_crlf_pair_stays_data_mid_burst() {
        let (mut detector, queue) = paste_detector_and_queue(vec![lf_key_event()]);
        assert!(matches!(detector.parse_event(cr()), ReedlineEvent::None));
        for ch in "ab".chars() {
            detector.parse_event(paste_key(
                crossterm29::event::KeyCode::Char(ch),
                crossterm29::event::KeyModifiers::NONE,
            ));
        }
        // Interior break: the feed queues the LF partner behind its CR.
        queue.lock().expect("test queue").push_back(lf_key_event());
        assert!(
            matches!(detector.parse_event(cr()), ReedlineEvent::None),
            "an interior CR with its LF partner queued is data, not a submit"
        );
        assert_eq!(detector.pending.as_deref(), Some("\nab\n"));
        assert!(
            detector.pushback.is_empty(),
            "the partner is consumed with its CR"
        );
        assert!(detector.pending.is_some(), "the burst continues buffering");
    }

    /// The text that reaches reedline when the pasted block `input` is fed
    /// through the detector, reassembled from the translated events (typed
    /// edits + flushed chunk inserts). Models the console queue the way one
    /// WriteConsoleInput burst delivers it: every `\n` directly after a
    /// `\r` is that CR's LF partner, queued behind it in the same write and
    /// consumed by the CR's probe (niubash#308) — it never dispatches as
    /// its own event; a `\n` after a `\n` is a lone Ctrl+J.
    fn feed_paste_burst(input: &str) -> String {
        let (mut detector, queue) = paste_detector_and_queue(Vec::new());
        let mut reassembled = String::new();
        fn append_event_text(event: &ReedlineEvent, out: &mut String) {
            match event {
                ReedlineEvent::Edit(commands) => {
                    for command in commands {
                        match command {
                            EditCommand::InsertChar(c) => out.push(*c),
                            EditCommand::InsertString(s) => out.push_str(s),
                            other => {
                                panic!("unexpected edit command in paste reassembly: {other:?}")
                            }
                        }
                    }
                }
                ReedlineEvent::Multiple(events) => {
                    for event in events {
                        append_event_text(event, out);
                    }
                }
                // The user's submit Enter is not paste data.
                ReedlineEvent::Enter => {}
                // The menu-dismissal prefix of a wrapped submit
                // (niubash#373) — not paste data either.
                ReedlineEvent::Esc => {}
                ReedlineEvent::None => {}
                other => panic!("unexpected event in paste reassembly: {other:?}"),
            }
        }
        let mut skip_next_lf = false;
        for ch in input.chars() {
            let event = match ch {
                '\r' => {
                    queue.lock().expect("test queue").push_back(lf_key_event());
                    skip_next_lf = true;
                    detector.parse_event(cr())
                }
                '\n' => {
                    if skip_next_lf {
                        skip_next_lf = false;
                        continue;
                    }
                    detector.parse_event(lf())
                }
                _ => {
                    skip_next_lf = false;
                    detector.parse_event(paste_key(
                        crossterm29::event::KeyCode::Char(ch),
                        crossterm29::event::KeyModifiers::NONE,
                    ))
                }
            };
            append_event_text(&event, &mut reassembled);
        }
        // The burst settles; the user's Enter flushes the chunk and submits
        // — a BARE Enter: the paste flow never opened the TAB window
        // (niubash#373), so the submit stays the plain shape every
        // downstream handler (history search included) implements.
        expire_burst(&mut detector);
        let event = detector.parse_event(cr());
        match &event {
            ReedlineEvent::Multiple(events) => {
                assert_eq!(events.len(), 2, "flush insert then submit: {event:?}");
                assert!(matches!(&events[0], ReedlineEvent::Edit(_)));
                assert!(matches!(events[1], ReedlineEvent::Enter));
            }
            other => panic!("expected flush + submit, got {other:?}"),
        }
        append_event_text(&event, &mut reassembled);
        reassembled
    }

    /// niubash#291: the line after an EMPTY line in a pasted chunk keeps
    /// every byte. The exact reported burst — `echo one\r\n\r\necho two\r
    /// \n\n`, then a separate submit Enter — must reach reedline byte for
    /// byte: whatever the chunk's line handling does at the `\n\n`
    /// boundary, reassembly may not lose the `e` of `echo two`.
    #[test]
    fn paste_chunk_reassembles_empty_line_paste_byte_exact() {
        let input = "echo one\r\n\r\necho two\r\n\n";
        let reassembled = feed_paste_burst(input);
        assert_eq!(
            reassembled,
            input.replace("\r\n", "\n"),
            "the pasted block lost or altered bytes at the empty-line boundary"
        );
    }

    /// Two consecutive empty lines inside the chunk: both boundaries stay
    /// byte-exact (no break lost, none doubled).
    #[test]
    fn paste_chunk_reassembles_two_consecutive_empty_lines_byte_exact() {
        let input = "echo one\r\n\r\n\r\necho two\r\n\n";
        let reassembled = feed_paste_burst(input);
        assert_eq!(
            reassembled,
            input.replace("\r\n", "\n"),
            "the pasted block lost or altered bytes across two empty lines"
        );
    }

    /// An indented line right after an empty line: leading spaces are data,
    /// not a swallow-into-the-previous-line cue.
    #[test]
    fn paste_chunk_reassembles_indented_line_after_empty_line_byte_exact() {
        let input = "echo one\r\n\r\n  echo two\r\n\n";
        let reassembled = feed_paste_burst(input);
        assert_eq!(
            reassembled,
            input.replace("\r\n", "\n"),
            "the indented line after the empty line lost or altered bytes"
        );
    }

    /// Control: the same shape WITHOUT the empty line (pure CRLF block) —
    /// guards that whatever the empty-line handling does, the plain block
    /// stays byte-exact too.
    #[test]
    fn paste_chunk_reassembles_crlf_block_without_empty_lines_byte_exact() {
        let input = "echo one\r\necho two\r\n";
        let reassembled = feed_paste_burst(input);
        assert_eq!(
            reassembled,
            input.replace("\r\n", "\n"),
            "the plain CRLF block lost or altered bytes"
        );
    }

    /// niubash#308 adjacency: typeahead (not an LF partner) queued behind a
    /// bare CR that lands on a pending chunk. The chunk flushes and submits,
    /// and the typeahead key goes back to the input stream's head at the
    /// submit (niubash#417), so it lands, in order, in the NEXT line's
    /// buffer — GNU typeahead semantics, never absorption.
    #[test]
    fn paste_chunk_detector_pending_enter_with_typeahead_behind_submits_in_order() {
        let (mut detector, queue) = paste_detector_and_queue(vec![lf_key_event()]);
        assert!(matches!(detector.parse_event(cr()), ReedlineEvent::None));
        detector.parse_event(paste_key(
            crossterm29::event::KeyCode::Char('v'),
            crossterm29::event::KeyModifiers::NONE,
        ));
        // The next command's first key is already queued when the user's
        // Enter dispatches (back-to-back writes).
        queue
            .lock()
            .expect("test queue")
            .push_back(crossterm29::event::Event::Key(
                crossterm29::event::KeyEvent::new(
                    crossterm29::event::KeyCode::Char('z'),
                    crossterm29::event::KeyModifiers::NONE,
                ),
            ));
        let event = detector.parse_event(cr());
        match event {
            ReedlineEvent::Multiple(events) => {
                assert_eq!(events.len(), 2, "flush insert followed by the submit");
                assert!(matches!(&events[0], ReedlineEvent::Edit(_)));
                assert!(matches!(&events[1], ReedlineEvent::Enter));
            }
            other => panic!("expected flush + submit, got {other:?}"),
        }
        assert!(detector.pending.is_none());
        // The probe-parked 'z' went back to the input stream at the submit —
        // the accepted command's engine `read` must be able to reach it
        // (niubash#417); nothing stays parked in-process.
        assert!(
            detector.pushback.is_empty(),
            "the typeahead is requeued, not parked"
        );
        {
            let requeued = queue.lock().expect("test queue");
            assert_eq!(requeued.len(), 1, "'z' is back at the stream's head");
            assert!(matches!(
                requeued.front(),
                Some(crossterm29::event::Event::Key(key))
                    if matches!(key.code, crossterm29::event::KeyCode::Char('z'))
            ));
        }
        // reedline re-reads the requeued key as the next raw event: an edit
        // of the fresh line, not lost paste data.
        let event = detector.parse_event(paste_key(
            crossterm29::event::KeyCode::Char('z'),
            crossterm29::event::KeyModifiers::NONE,
        ));
        assert!(
            matches!(event, ReedlineEvent::Edit(_)),
            "the requeued 'z' edits the fresh line: {event:?}"
        );
        // And the key typed after it ('w') follows in order.
        let event = detector.parse_event(paste_key(
            crossterm29::event::KeyCode::Char('w'),
            crossterm29::event::KeyModifiers::NONE,
        ));
        assert!(matches!(event, ReedlineEvent::Edit(_)));
        assert!(detector.pushback.is_empty());
    }

    /// niubash#284, the regression itself: a fast "text + Enter" burst with
    /// NOTHING queued behind the Enter is a typed submit, not a paste —
    /// whatever the burst clock says. ConPTY drivers, expect scripts and
    /// fast human typists all feed exactly this shape.
    #[test]
    fn paste_chunk_detector_lets_a_fast_typed_enter_submit() {
        let mut detector = paste_detector();
        // Characters parsed microseconds apart (one burst, no gap), Enter
        // right behind the last one.
        for ch in "echo hi".chars() {
            if ch == ' ' {
                continue;
            }
            let event = detector.parse_event(paste_key(
                crossterm29::event::KeyCode::Char(ch),
                crossterm29::event::KeyModifiers::NONE,
            ));
            assert!(matches!(event, ReedlineEvent::Edit(_)));
        }
        let event = detector.parse_event(cr());
        assert!(
            matches!(event, ReedlineEvent::Enter),
            "the first Enter of a typed line must submit, not be absorbed: {event:?}"
        );
        assert!(detector.pending.is_none());
        assert!(detector.pushback.is_empty());
    }

    #[test]
    fn paste_chunk_detector_lets_a_lone_enter_submit() {
        let mut detector = paste_detector();
        let event = detector.parse_event(paste_key(
            crossterm29::event::KeyCode::Enter,
            crossterm29::event::KeyModifiers::NONE,
        ));
        assert!(
            matches!(event, ReedlineEvent::Enter),
            "typed submit must pass through"
        );
        assert!(detector.pending.is_none());
    }

    #[test]
    fn paste_chunk_detector_does_not_swallow_enter_after_a_stale_burst() {
        let mut detector = paste_detector();
        for ch in "abc".chars() {
            detector.parse_event(paste_key(
                crossterm29::event::KeyCode::Char(ch),
                crossterm29::event::KeyModifiers::NONE,
            ));
        }
        // The burst fully settles before the user presses Enter. Timing no
        // longer decides submits at all (niubash#284) — the structural rule
        // covers this shape too — but the shape stays pinned: silence
        // behind the Enter is a submit.
        expire_burst(&mut detector);
        let event = detector.parse_event(cr());
        assert!(
            matches!(event, ReedlineEvent::Enter),
            "silence behind the Enter is a submit: {event:?}"
        );
        assert!(detector.pending.is_none());
    }

    /// Typeahead behind a typed Enter (a driver's next send_line, a
    /// script's next line): the Enter submits, and the queued text goes
    /// back to the input stream's head at the submit (niubash#417), so it
    /// lands, in order, in the NEXT line's buffer — GNU typeahead
    /// semantics, never absorption into a paste chunk.
    #[test]
    fn paste_chunk_detector_submits_enter_with_typeahead_behind_it() {
        let (mut detector, queue) = paste_detector_and_queue(vec![char_key_event('c')]);
        for ch in "cmd1".chars() {
            detector.parse_event(paste_key(
                crossterm29::event::KeyCode::Char(ch),
                crossterm29::event::KeyModifiers::NONE,
            ));
        }
        let event = detector.parse_event(cr());
        assert!(
            matches!(event, ReedlineEvent::Enter),
            "a CR whose queued follower is not an LF is a typed submit: {event:?}"
        );
        // niubash#417: the probe-parked 'c' went back to the input stream
        // at the submit — the accepted command's engine `read` (the queue's
        // next reader) must be able to reach it.
        assert!(
            detector.pushback.is_empty(),
            "the typeahead is requeued, not parked in-process"
        );
        {
            let requeued = queue.lock().expect("test queue");
            assert_eq!(requeued.len(), 1, "'c' is back at the stream's head");
            assert!(
                matches!(
                    requeued.front(),
                    Some(crossterm29::event::Event::Key(key))
                        if matches!(key.code, crossterm29::event::KeyCode::Char('c'))
                ),
                "'c' is requeued verbatim"
            );
        }

        // reedline re-reads the requeued 'c' as the next raw event, then
        // 'm': both edits of the fresh line, in order, no paste absorption.
        for ch in ['c', 'm'] {
            let event = detector.parse_event(paste_key(
                crossterm29::event::KeyCode::Char(ch),
                crossterm29::event::KeyModifiers::NONE,
            ));
            assert!(
                matches!(event, ReedlineEvent::Edit(_)),
                "requeued '{ch}' edits the fresh line: {event:?}"
            );
        }
        assert!(detector.pushback.is_empty());
    }

    /// niubash#417, the head-vs-tail distinction this fix rides on: the
    /// probe parks the FIRST key queued behind the submit Enter, and any
    /// keys behind THAT stay in the queue. The requeue must land the parked
    /// key back in FRONT of them (the order the typist produced), never
    /// behind — a tail append would let line 2's `ead` run before the
    /// parked `r` of `read` and shred the burst.
    #[test]
    fn paste_chunk_detector_submit_requeues_parked_typeahead_at_stream_head_417() {
        let (mut detector, queue) =
            paste_detector_and_queue(vec![char_key_event('r'), char_key_event('!')]);
        detector.parse_event(paste_key(
            crossterm29::event::KeyCode::Char('e'),
            crossterm29::event::KeyModifiers::NONE,
        ));
        // The submit Enter: the probe parks 'r' (the next line's first
        // key); '!' is still queued behind it.
        let event = detector.parse_event(cr());
        assert!(
            matches!(event, ReedlineEvent::Enter),
            "typeahead behind the Enter is a typed submit: {event:?}"
        );
        assert!(detector.pushback.is_empty());
        // The queue reads [parked 'r', still-queued '!'] — the exact order
        // the typist produced.
        {
            let requeued = queue.lock().expect("test queue");
            let codes: Vec<char> = requeued
                .iter()
                .filter_map(|event| match event {
                    crossterm29::event::Event::Key(key) => match key.code {
                        crossterm29::event::KeyCode::Char(ch) => Some(ch),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            assert_eq!(
                codes,
                vec!['r', '!'],
                "the parked key is re-queued AHEAD of what was queued behind it"
            );
        }
    }

    /// A bare Ctrl+J (the console's shape for a fed LF) with no chunk
    /// pending is a paste newline by itself: it opens the chunk instead of
    /// reaching reedline (humans do not submit with Ctrl+J).
    #[test]
    fn paste_chunk_detector_opens_chunk_on_lf_alone() {
        let mut detector = paste_detector();
        assert!(matches!(detector.parse_event(lf()), ReedlineEvent::None));
        assert_eq!(detector.pending.as_deref(), Some("\n"));
        // Burst text continues to be absorbed.
        assert!(matches!(
            detector.parse_event(paste_key(
                crossterm29::event::KeyCode::Char('x'),
                crossterm29::event::KeyModifiers::NONE
            )),
            ReedlineEvent::None
        ));
        assert_eq!(detector.pending.as_deref(), Some("\nx"));
    }

    /// ConPTY focus flicker between pasted lines must not hide the LF
    /// partner from the lookahead: the CR still identifies as paste.
    #[test]
    fn paste_chunk_detector_skips_flicker_to_find_lf_partner() {
        let mut detector = paste_detector_with_queue(vec![
            crossterm29::event::Event::FocusGained,
            crossterm29::event::Event::FocusLost,
            crossterm29::event::Event::Key(crossterm29::event::KeyEvent::new(
                crossterm29::event::KeyCode::Enter,
                crossterm29::event::KeyModifiers::CONTROL,
            )),
        ]);
        for ch in "abc".chars() {
            detector.parse_event(paste_key(
                crossterm29::event::KeyCode::Char(ch),
                crossterm29::event::KeyModifiers::NONE,
            ));
        }
        assert!(matches!(detector.parse_event(cr()), ReedlineEvent::None));
        assert_eq!(detector.pending.as_deref(), Some("\n"));
        assert!(detector.pushback.is_empty(), "flicker is inert");
    }

    /// niubash#420: at chunk-OPEN a CR behind a CR is TWO TYPEAHEAD SUBMITS
    /// in one pty write — a driver's `send("cmd\r\r")`, an expect script's
    /// back-to-back Enters, the Ctrl+R accept-Enter followed by the
    /// submit-Enter. The bytes are IDENTICAL to the WT KEY_EVENT empty-line
    /// paste (niubash#341), so a CR partner alone is no longer paste
    /// evidence: the first CR SUBMITS and the probe-consumed partner goes
    /// back into the input stream (the #417 requeue face — a
    /// pushback would cross reedline's batch boundary: R2 translates it as
    /// a batch-leading Enter and R3's first-Exits drop shreds the batch's
    /// queued text, the #341 byte loss). The requeued CR then submits the
    /// empty line — or, in the Ctrl+R flow, submits the loaded match — on
    /// its own dispatch.
    #[test]
    fn paste_chunk_detector_open_cr_pair_submits_and_requeues_420() {
        let (mut detector, queue) = paste_detector_and_queue(vec![cr_key_event()]);
        detector.parse_event(paste_key(
            crossterm29::event::KeyCode::Char('c'),
            crossterm29::event::KeyModifiers::NONE,
        ));
        assert!(
            matches!(detector.parse_event(cr()), ReedlineEvent::Enter),
            "a CR with a bare CR queued behind it at chunk-open is a typed submit"
        );
        assert!(
            detector.pending.is_none(),
            "no chunk opens without paste evidence"
        );
        assert!(
            detector.pushback.is_empty(),
            "a bare CR must never be parked in the pushback"
        );
        // The partner CR went back to the HEAD of the input stream, not
        // into the void and not into the pushback.
        let requeued = queue.lock().expect("test queue");
        assert_eq!(requeued.len(), 1, "the partner CR is requeued verbatim");
        assert!(matches!(
            requeued.front(),
            Some(crossterm29::event::Event::Key(key)) if is_bare_enter(key)
        ));
    }

    /// The #341 fold survives where paste evidence exists: a chunk opened
    /// by an LF partner (the ConPTY CRLF shape), then a bare CR with a CR
    /// behind it MID-CHUNK — the WT empty line inside a live paste — still
    /// folds as two line breaks of data.
    #[test]
    fn paste_chunk_detector_midchunk_cr_pair_still_folds_341() {
        let (mut detector, _queue) = paste_detector_and_queue(vec![lf_key_event(), cr_key_event()]);
        // LF partner opens the chunk.
        assert!(matches!(detector.parse_event(cr()), ReedlineEvent::None));
        assert_eq!(detector.pending.as_deref(), Some("\n"));
        // Mid-chunk: the bare CR's partner CR folds the empty line as data —
        // the incoming CR's break plus the partner's, after the LF.
        assert!(
            matches!(detector.parse_event(cr()), ReedlineEvent::None),
            "mid-chunk the CR pair is paste data"
        );
        assert_eq!(
            detector.pending.as_deref(),
            Some("\n\n\n"),
            "all three breaks fold inside the live paste"
        );
        assert!(detector.pushback.is_empty());
    }

    /// The pushback invariant behind #341, stated directly: whatever the
    /// probe meets in the queue, the pushback may hold text keys and chords
    /// — never a bare CR. At a submit the whole park empties into the input
    /// stream's head (niubash#417), so what crosses a batch boundary is
    /// never an unexamined CR.
    #[test]
    fn paste_chunk_detector_pushback_never_holds_a_bare_cr_341() {
        // Text behind the CR: typed submit, the text key requeued to the
        // stream's head — never parked past the batch boundary.
        let (mut detector, queue) = paste_detector_and_queue(vec![char_key_event('z')]);
        let event = detector.parse_event(cr());
        assert!(
            matches!(event, ReedlineEvent::Enter),
            "a CR with typeahead behind it is a typed submit: {event:?}"
        );
        assert!(
            detector.pushback.is_empty(),
            "a submit leaves nothing parked in the pushback (niubash#417)"
        );
        assert_eq!(queue.lock().expect("test queue").len(), 1);
        // A CR behind the CR at chunk-open: typed submit, the partner
        // requeued to the input stream's head — never the pushback
        // (niubash#420).
        let (mut detector, queue) = paste_detector_and_queue(vec![cr_key_event()]);
        assert!(matches!(detector.parse_event(cr()), ReedlineEvent::Enter));
        assert!(detector.pushback.is_empty());
        assert_eq!(queue.lock().expect("test queue").len(), 1);
    }

    #[test]
    fn paste_chunk_detector_keeps_typing_out_of_the_chunk() {
        let mut detector = paste_detector();
        // Human-paced keystrokes (each after a full gap) never open a
        // pending chunk, even across many keystrokes.
        for ch in "echo hi".chars() {
            if ch == ' ' {
                continue;
            }
            expire_burst(&mut detector);
            detector.parse_event(paste_key(
                crossterm29::event::KeyCode::Char(ch),
                crossterm29::event::KeyModifiers::NONE,
            ));
            assert!(detector.pending.is_none());
        }
    }

    #[test]
    fn paste_chunk_newline_key_matches_cr_and_lf_variants() {
        assert!(is_bare_enter(&crossterm29::event::KeyEvent::new(
            crossterm29::event::KeyCode::Enter,
            crossterm29::event::KeyModifiers::NONE
        )));
        // The LF of a pasted CRLF surfaces as Enter + CONTROL on the
        // console input path.
        assert!(is_bare_enter(&crossterm29::event::KeyEvent::new(
            crossterm29::event::KeyCode::Enter,
            crossterm29::event::KeyModifiers::CONTROL
        )));
        assert!(!is_bare_enter(&crossterm29::event::KeyEvent::new(
            crossterm29::event::KeyCode::Enter,
            crossterm29::event::KeyModifiers::ALT
        )));
        assert!(!is_bare_enter(&crossterm29::event::KeyEvent::new(
            crossterm29::event::KeyCode::Char('m'),
            crossterm29::event::KeyModifiers::NONE
        )));
    }

    // ---- #417 writeback face: the event→console-record synthesis ----

    fn synth(
        code: crossterm29::event::KeyCode,
        modifiers: crossterm29::event::KeyModifiers,
    ) -> Vec<KeyRecordFields> {
        key_press_records(&crossterm29::event::KeyEvent::new(code, modifiers))
    }

    /// A plain character decodes from uChar; the VK carries the
    /// letter/digit identity. Uppercase rides the SHIFT state exactly like
    /// the record conhost synthesizes for shift+letter.
    #[test]
    fn key_records_synthesize_plain_characters() {
        assert_eq!(
            synth(
                crossterm29::event::KeyCode::Char('e'),
                crossterm29::event::KeyModifiers::NONE
            ),
            vec![KeyRecordFields {
                virtual_key_code: b'E' as u16,
                unicode_char: 'e' as u16,
                control_key_state: 0,
            }]
        );
        assert_eq!(
            synth(
                crossterm29::event::KeyCode::Char('Z'),
                crossterm29::event::KeyModifiers::SHIFT
            ),
            vec![KeyRecordFields {
                virtual_key_code: b'Z' as u16,
                unicode_char: 'Z' as u16,
                control_key_state: SHIFT_PRESSED_FLAG,
            }]
        );
        // A symbol has no layout-independent VK: the readers decode from
        // uChar (VK 0 falls through both parsers' special-key tables).
        assert_eq!(
            synth(
                crossterm29::event::KeyCode::Char('!'),
                crossterm29::event::KeyModifiers::SHIFT
            )
            .first()
            .map(|r| (r.virtual_key_code, r.unicode_char)),
            Some((0, '!' as u16))
        );
    }

    /// A control chord round-trips the conhost convention the console
    /// parser produced it with: control code in uChar, base letter in the
    /// VK, LEFT_CTRL_PRESSED in the state — so crossterm's parser re-derives
    /// the same event and the engine's reader decodes the same byte.
    ///
    /// niubash#434: the delivery shape crossterm 0.29 hands the probe is
    /// Char(letter) + CONTROL — its own parse of conhost's control-code
    /// record — and the mapping must key on it. Keyed on the control-code
    /// char alone the arm never fired on Windows and the writeback carried
    /// the letter, so a raw-byte reader saw 'r' instead of 0x12.
    #[test]
    fn key_records_synthesize_control_chords() {
        let expected = vec![KeyRecordFields {
            virtual_key_code: b'R' as u16,
            unicode_char: 0x12,
            control_key_state: LEFT_CTRL_PRESSED_FLAG,
        }];
        // The production shape: Char(letter) + CONTROL.
        assert_eq!(
            synth(
                crossterm29::event::KeyCode::Char('r'),
                crossterm29::event::KeyModifiers::CONTROL
            ),
            expected
        );
        // Case of the delivered letter is the keyboard's business, not the
        // chord's: the record is the base letter + the control code.
        assert_eq!(
            synth(
                crossterm29::event::KeyCode::Char('R'),
                crossterm29::event::KeyModifiers::CONTROL
            ),
            expected
        );
        // The parser's own uChar arm shape (a control-code char) maps to the
        // same record.
        assert_eq!(
            synth(
                crossterm29::event::KeyCode::Char('\u{12}'),
                crossterm29::event::KeyModifiers::CONTROL
            ),
            expected
        );
        // A chord letter with SHIFT rides the shift flag next to the ctrl
        // one — the re-derivation reads the case back from it.
        assert_eq!(
            synth(
                crossterm29::event::KeyCode::Char('R'),
                crossterm29::event::KeyModifiers::CONTROL | crossterm29::event::KeyModifiers::SHIFT
            ),
            vec![KeyRecordFields {
                virtual_key_code: b'R' as u16,
                unicode_char: 0x12,
                control_key_state: LEFT_CTRL_PRESSED_FLAG | SHIFT_PRESSED_FLAG,
            }]
        );
        // Every letter maps to its control code — the full 0x01..=0x1A span
        // a raw-byte reader can be handed back.
        for (letter, code) in ('a'..='z').zip(0x01u16..=0x1A) {
            let record = &synth(
                crossterm29::event::KeyCode::Char(letter),
                crossterm29::event::KeyModifiers::CONTROL,
            )[0];
            assert_eq!(record.virtual_key_code, u16::from(b'A') + code - 1);
            assert_eq!(record.unicode_char, code);
            assert_eq!(record.control_key_state, LEFT_CTRL_PRESSED_FLAG);
        }
    }

    /// The crossterm 0.29 Windows parse (`parse_key_event_record`) of the
    /// records the mapping emits, for the control-chord shape: the uChar
    /// lands in the parser's `0x00..=0x1f` arm, which re-derives the
    /// character from the VK (`get_char_for_key` — a zeroed key state
    /// through ToUnicode, so the letter never taints; the case comes from
    /// the record's SHIFT flag alone) and takes the modifiers from the
    /// state. The parser itself is `pub(crate)` in crossterm, so the
    /// event-level round trip is asserted through this transcription of its
    /// decision.
    #[cfg(test)]
    fn parse_back_control_chord(record: &KeyRecordFields) -> crossterm29::event::KeyEvent {
        use crossterm29::event::{KeyCode, KeyEvent, KeyModifiers};
        assert!(
            (0x01..=0x1F).contains(&record.unicode_char),
            "not a control-code uChar: {record:?}"
        );
        assert!(
            (record.virtual_key_code as u8).is_ascii_uppercase(),
            "not a letter VK: {record:?}"
        );
        let mut modifiers = KeyModifiers::empty();
        if record.control_key_state & SHIFT_PRESSED_FLAG != 0 {
            modifiers |= KeyModifiers::SHIFT;
        }
        if record.control_key_state & LEFT_CTRL_PRESSED_FLAG != 0 {
            modifiers |= KeyModifiers::CONTROL;
        }
        if record.control_key_state & LEFT_ALT_PRESSED_FLAG != 0 {
            modifiers |= KeyModifiers::ALT;
        }
        let mut ch = (record.virtual_key_code as u8).to_ascii_lowercase() as char;
        if record.control_key_state & SHIFT_PRESSED_FLAG != 0 {
            ch = ch.to_ascii_uppercase();
        }
        KeyEvent::new(KeyCode::Char(ch), modifiers)
    }

    /// Event-level round trip (niubash#434): the synthesized Ctrl+R record
    /// parses back to exactly the chord the probe consumed — the #417
    /// writeback can requeue a parked chord without reedline ever seeing a
    /// different event, while a byte-level reader gets the original
    /// control byte.
    #[test]
    fn control_chord_records_round_trip_through_the_console_parse() {
        for (letter, _code) in ('a'..='z').zip(0x01u16..=0x1A) {
            let delivered = crossterm29::event::KeyEvent::new(
                crossterm29::event::KeyCode::Char(letter),
                crossterm29::event::KeyModifiers::CONTROL,
            );
            let record = &key_press_records(&delivered)[0];
            assert_eq!(
                parse_back_control_chord(record),
                delivered,
                "Ctrl+{letter} did not round-trip"
            );
        }
        // The chord with SHIFT: the re-derived event keeps it.
        let shifted = crossterm29::event::KeyEvent::new(
            crossterm29::event::KeyCode::Char('R'),
            crossterm29::event::KeyModifiers::CONTROL | crossterm29::event::KeyModifiers::SHIFT,
        );
        assert_eq!(
            parse_back_control_chord(&key_press_records(&shifted)[0]),
            shifted
        );
    }

    /// Special keys ride the VKs of crossterm's own parse table (the exact
    /// inverse mapping); BackTab is decided from VK_TAB + SHIFT.
    #[test]
    fn key_records_synthesize_special_keys() {
        assert_eq!(
            synth(
                crossterm29::event::KeyCode::Left,
                crossterm29::event::KeyModifiers::NONE
            )
            .first()
            .map(|r| (r.virtual_key_code, r.unicode_char)),
            Some((0x25, 0))
        );
        assert_eq!(
            synth(
                crossterm29::event::KeyCode::F(12),
                crossterm29::event::KeyModifiers::NONE
            )
            .first()
            .map(|r| r.virtual_key_code),
            Some(0x7B)
        );
        let back_tab = synth(
            crossterm29::event::KeyCode::BackTab,
            crossterm29::event::KeyModifiers::SHIFT,
        );
        assert_eq!(back_tab.len(), 1);
        assert_eq!(back_tab[0].virtual_key_code, 0x09);
        assert_eq!(
            back_tab[0].control_key_state, SHIFT_PRESSED_FLAG,
            "BackTab round-trips as VK_TAB + SHIFT"
        );
    }

    /// A non-BMP character is one record per UTF-16 unit, so crossterm's
    /// surrogate assembly reproduces the original Char event.
    #[test]
    fn key_records_split_non_bmp_characters_into_surrogate_units() {
        let records = synth(
            crossterm29::event::KeyCode::Char('\u{10437}'),
            crossterm29::event::KeyModifiers::NONE,
        );
        let units: Vec<u16> = records.iter().map(|r| r.unicode_char).collect();
        assert_eq!(units, vec![0xD801, 0xDC37]);
    }

    /// Releases and shapes the console parser cannot produce never map —
    /// they could never be parked here to begin with.
    #[test]
    fn key_records_drop_releases_and_unparseable_codes() {
        let mut release = crossterm29::event::KeyEvent::new(
            crossterm29::event::KeyCode::Char('x'),
            crossterm29::event::KeyModifiers::NONE,
        );
        release.kind = crossterm29::event::KeyEventKind::Release;
        assert!(key_press_records(&release).is_empty());
        assert!(synth(
            crossterm29::event::KeyCode::Null,
            crossterm29::event::KeyModifiers::NONE
        )
        .is_empty());
    }

    /// niubash#373 CR round: the menu-dismissal Esc rides an Enter ONLY
    /// inside the TAB window, and (niubash#486) only under the GNU
    /// completion style. Outside the window the Enter stays a bare
    /// `ReedlineEvent::Enter` — reedline 0.52 routes whole `Multiple`
    /// batches to the history-search handler while Ctrl+R is active, and
    /// that handler drops `Multiple(_)` as `Inapplicable`, so an
    /// unconditionally wrapped Enter turned Ctrl+R's accepting Enter into a
    /// dead key (the search match never loaded). A CONTROL chord (the
    /// Ctrl+R itself) ends the window.
    #[test]
    fn paste_chunk_wrapper_dismissal_esc_rides_only_the_gnu_tab_window() {
        // No TAB since the last submit: the Enter stays bare — the shape
        // history search (and every non-completion flow) requires. True
        // under every completion style.
        for completion_style in [CompletionStyle::Column, CompletionStyle::Gnu] {
            let (mut detector, _queue) =
                paste_detector_and_queue_for_style(Vec::new(), completion_style);
            let event = detector.parse_event(cr());
            assert!(
                matches!(event, ReedlineEvent::Enter),
                "an Enter outside the TAB window must stay bare ({completion_style:?}): {event:?}"
            );
        }

        // TAB opens the window: the GNU style's follow-up Enter carries
        // the dismissal — its printed list reports `is_active` with no
        // selectable candidate, so a bare Enter would die in the engine's
        // no-op `MenuAccept`.
        let mut detector = gnu_paste_detector();
        detector.parse_event(tab());
        let event = detector.parse_event(cr());
        match &event {
            ReedlineEvent::Multiple(events) => {
                assert_eq!(events.len(), 2, "{event:?}");
                assert!(matches!(&events[0], ReedlineEvent::Esc));
                assert!(matches!(&events[1], ReedlineEvent::Enter));
            }
            other => panic!("expected dismissal + submit, got {other:?}"),
        }

        // The window closed with that submit: the next Enter is bare again.
        let event = detector.parse_event(cr());
        assert!(
            matches!(event, ReedlineEvent::Enter),
            "the window ended with the wrapped submit: {event:?}"
        );

        // A CONTROL chord ends the window without a submit: TAB, Ctrl+R,
        // Enter — the Enter belongs to the search and stays bare.
        let mut detector = gnu_paste_detector();
        detector.parse_event(tab());
        detector.parse_event(ctrl_r());
        let event = detector.parse_event(cr());
        assert!(
            matches!(event, ReedlineEvent::Enter),
            "a chord-cleared window must leave the search's Enter bare: {event:?}"
        );
    }

    /// niubash#486: under an interactive menu style the Enter inside the
    /// TAB window stays BARE — reedline's `menu_took_the_key` (`MenuAccept`)
    /// gives the open menu its first refusal and commits the highlighted
    /// candidate into the editable line (the 1.1.0 behavior); a second
    /// Enter then submits. The #373 wrap would dismiss the menu WITHOUT
    /// committing — the regression this issue reports.
    #[test]
    fn paste_chunk_wrapper_interactive_menu_enter_stays_bare_for_the_accept() {
        for completion_style in [
            CompletionStyle::Column,
            CompletionStyle::List,
            CompletionStyle::Ide,
            CompletionStyle::Inline,
        ] {
            let (mut detector, _queue) =
                paste_detector_and_queue_for_style(Vec::new(), completion_style);
            detector.parse_event(tab());
            let event = detector.parse_event(cr());
            assert!(
                matches!(event, ReedlineEvent::Enter),
                "inside the TAB window an interactive menu's Enter must stay bare \
                 ({completion_style:?}) so the engine can commit the candidate: {event:?}"
            );

            // The next Enter (the committed line's submit) is bare too.
            let event = detector.parse_event(cr());
            assert!(
                matches!(event, ReedlineEvent::Enter),
                "the follow-up submit stays bare ({completion_style:?}): {event:?}"
            );
        }
    }
}
