//! Reedline REPL loop

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::{Duration, Instant};
use std::{borrow::Cow, io::Write};

use crate::autosuggest::HistoryAutosuggestHinter;
use crate::completion::cycle_menu::SharedCycleSession;
use crate::completion::NiubashCompleter;
use crate::config::{
    CompletionInteraction, CompletionStyle, EditorMode, MenuConfig, NativeWidgetBinding,
    NativeWidgetConfig,
};
use crate::history::LiveFileBackedHistory;
use crate::shell::Shell;
use crate::syntax_highlighting::NiubashSyntaxHighlighter;
use reedline::{
    default_emacs_keybindings, default_vi_insert_keybindings, default_vi_normal_keybindings,
    default_vi_visual_keybindings, ColumnarMenu, Direction, EditCommand, EditMode, Emacs, KeyCode,
    KeyCombination, KeyModifiers, Keybindings, ListMenu, MenuBuilder, Prompt, PromptEditMode,
    PromptHistorySearch, PromptViMode, Reedline, ReedlineEvent, ReedlineMenu, ReedlineRawEvent,
    Signal, ValidationResult, Validator, Vi,
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

/// `ExecuteHostCommand` payload prefix for the host-implemented GNU
/// readline commands of [`crate::readline_commands`] (niubash#765,
/// #766, #767): the payload is the command name; the host performs
/// the command against the suspended editor and the live session
/// in [`run_readline_command`]. These names are readline functions,
/// never shell functions — the prefix keeps them out of the
/// shell-function widget path below.
pub const READLINE_HOST_COMMAND_PREFIX: &str = "__niu_readline ";

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
    // niubash#646: the session-end write consults the HIST* values as
    // the EXIT trap left them — GNU reads HISTFILESIZE/histappend at
    // the write itself (bashhist.c maybe_save_shell_history).
    shell.refresh_history_settings();
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
    // niubash#569/#773: the editor history instance and the provider
    // instance are distinct objects, but a session has ONE history
    // list (GNU: the builtin and readline read the same in-memory
    // list). When the provider is installed (every interactive
    // entry), the editor adopts its state wholesale — the same store,
    // not a second load of the file — plus its ledger (niubash#569:
    // so -c accounts for lines the editor recorded) and, inside the
    // state, its settings cell (niubash#646: so recorded lines are
    // filtered/stifled by the same HIST* snapshot the provider's
    // builtins consult). Edit-mode rebuilds re-adopt the same state,
    // so the sharing survives rebuilds. Without a provider, the
    // editor falls back to adopting just the shared cells, or to its
    // own file load.
    let shared_ledger = shell_ref.history_ledger();
    let shared_settings = shell_ref.history_settings();
    let shared_state = shell_ref.history_state();
    let mut history = match (shared_state, shared_ledger, shared_settings) {
        (Some(state), Some(ledger), _) => {
            LiveFileBackedHistory::with_shared_state(shell_ref.history_max_size, state, ledger)
        }
        (None, Some(ledger), Some(settings)) => {
            LiveFileBackedHistory::with_shared_ledger_and_settings(
                shell_ref.history_max_size,
                shell_ref.history_path.clone(),
                ledger,
                settings,
            )
        }
        (None, Some(ledger), None) => LiveFileBackedHistory::with_shared_ledger(
            shell_ref.history_max_size,
            shell_ref.history_path.clone(),
            ledger,
        ),
        _ => LiveFileBackedHistory::with_file(
            shell_ref.history_max_size,
            shell_ref.history_path.clone(),
        ),
    };
    // niubash#660: this instance is the one reedline records typed
    // lines into, so it carries the `history` option gate — its saves
    // are dropped while `set +o history` is in effect. The provider
    // instance is never gated, so `history -s` keeps adding, as in GNU.
    history.set_history_option_gate();
    // No construction-time reload (audit item H-01): the shared-state
    // constructor above adopts the provider's store as-is (niubash#773:
    // one list per session), and the fallback constructors load the
    // backing file exactly once, so a second full read here bought
    // nothing — and on an editor rebuild after `history -c` it silently
    // undid the ledger's view hide (#614) and cleared-base accounting
    // (#642) that the provider maintains.

    let completer = NiubashCompleter::new(shell_ref.completion_state.clone());
    // Shell-function completions reach the engine through the main-thread
    // bridge installed here; reedline completers must stay `Send`.
    crate::shell::install_completion_bridge(shell);
    let menu_config = shell_ref.menu_config;

    // niubash#631: the cycle interaction's shared session cell. The
    // CycleMenu publishes its session (pre-completion snapshot, "a
    // candidate is in the line") into it; the edit-mode wrapper
    // consumes it at key-translation time for the two keys a menu
    // cannot finish itself — Esc (roll the line back to the snapshot)
    // and Enter (accept the candidate and run). Only built when the
    // interaction applies: `cycle` on a non-GNU style (the GNU style's
    // literal readline interaction is inert to the axis by design).
    let cycle_session: Option<SharedCycleSession> = if (menu_config.completion_interaction
        == CompletionInteraction::Cycle
        || menu_config.completion_interaction == CompletionInteraction::Select)
        && menu_config.completion_style != CompletionStyle::Gnu
    {
        Some(Arc::new(std::sync::Mutex::new(
            crate::completion::cycle_menu::CycleSession::default(),
        )))
    } else {
        None
    };

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

    // Shared visibility flag between the completion grid and the history
    // autosuggest hinter: while the grid is on screen the ghost stands
    // down instead of painting over the same word (fish parity).
    // `InsertCompletions` (M-*) and other engine-level completion paths
    // read `Reedline`'s own completer, not the menu's. The menu owns a
    // separate instance over the same shared state, so give the engine one
    // too instead of leaving M-* without a candidate source.
    let engine_completer = NiubashCompleter::new(shell_ref.completion_state.clone());
    let menu_open = Arc::new(AtomicBool::new(false));
    let completion_menu = ReedlineMenu::WithCompleter {
        menu: configured_completion_menu(
            COMPLETION_MENU,
            menu_config,
            &gnu_indicator,
            menu_open.clone(),
            cycle_session.clone(),
        ),
        completer: Box::new(completer),
    };
    let history_menu = ReedlineMenu::HistoryMenu(Box::new(configured_list_menu(
        HISTORY_MENU,
        menu_config.history_page_size,
        menu_config,
    )));

    let build_args = (
        &shell_ref.native_widgets,
        &shell_ref.native_widget_bindings,
        &shell_ref.user_widget_bindings,
        &shell_ref.engine_bindings,
        shell_ref.engine_bind_generation,
        menu_config.completion_interaction,
    );
    let (edit_mode, keymap_registry, layer_parts) = build_edit_mode(
        shell_ref.editor_mode,
        build_args.0,
        build_args.1,
        build_args.2,
        build_args.3,
        build_args.4,
        build_args.5,
    );
    // niubash#630: the other machine, registered as a standby so a startup
    // `bind` of `vi-editing-mode` / `emacs-editing-mode` (GNU's two
    // editing-mode toggles) can actually reach it via
    // `ReedlineEvent::SwitchMode`. Without a standby the engine has nothing
    // to switch *to* and declines the event. Its keymap is built the same
    // way as the configured mode's — the same native widgets, engine
    // `bind -m` entries and user rules — so a flip keeps the user's
    // bindings in the target keymap. Its registry snapshot is discarded:
    // the published one describes the active mode.
    let (standby_edit_mode, _, standby_layer_parts) = build_edit_mode(
        match shell_ref.editor_mode {
            EditorMode::Emacs => EditorMode::Vi,
            EditorMode::Vi => EditorMode::Emacs,
        },
        build_args.0,
        build_args.1,
        build_args.2,
        build_args.3,
        build_args.4,
        build_args.5,
    );
    // Publish the unified keymap registry (niubash#630): this build's
    // effective table and dropped records become the one snapshot the
    // viewing faces read, and the reminder pass surfaces the binds
    // this build newly failed to mirror, with their fixes (DESIGN §8 —
    // it replaces the old per-build warning pass).
    for notice in crate::keymap_registry::publish(keymap_registry) {
        eprintln!("{notice}");
    }
    // readline's `keyseq-timeout` for the chord layer, resolved once per
    // editor build. The standby machine gets the same value so a mode flip
    // keeps the wait the user configured.
    // GNU's `set keyseq-timeout` (rc `bind 'set ...'` or ~/.inputrc) is the
    // only surface for this knob — no config duplication. rubash's table
    // already defaults to 500, so an unset variable keeps readline's value.
    let keyseq_timeout = engine_keyseq_timeout(&shell_ref)
        .unwrap_or(std::time::Duration::from_millis(DEFAULT_KEYSEQ_TIMEOUT_MS));

    let mut editor = Reedline::create()
        .with_validator(Box::new(ReplValidator))
        .with_completer(Box::new(engine_completer))
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
        .with_edit_mode(Box::new(
            ChordEditMode::new(
                Box::new(
                    PasteChunkEditMode::new(edit_mode, menu_config.completion_style)
                        .with_cycle_session(cycle_session.clone()),
                ),
                layer_parts.chords,
                layer_parts.alt_keys,
                Some(Box::new(live_lookahead)),
                keyseq_timeout,
            )
            .with_quote_sequences(layer_parts.quote_sequences),
        ))
        .with_additional_edit_mode(Box::new(
            ChordEditMode::new(
                Box::new(
                    PasteChunkEditMode::new(standby_edit_mode, menu_config.completion_style)
                        .with_cycle_session(cycle_session),
                ),
                standby_layer_parts.chords,
                standby_layer_parts.alt_keys,
                Some(Box::new(live_lookahead)),
                keyseq_timeout,
            )
            .with_quote_sequences(standby_layer_parts.quote_sequences),
        ));

    if shell_ref.autosuggest.history_strategy_enabled() {
        editor = editor.with_hinter(Box::new(HistoryAutosuggestHinter::new(
            &shell_ref.autosuggest,
            menu_open,
        )));
    }
    if shell_ref.syntax_highlighting.main_highlighter_enabled() {
        let functions = shell_ref.session().functions();
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

    // niubash#775: the keymap generation this editor was built from.  The
    // REPL compares it between prompts so a plugin that registered or
    // unbound a key at runtime gets a lossless rebuild before the next line.
    crate::readline_live::mark_installed();

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
    menu_open: Arc<AtomicBool>,
    cycle_session: Option<SharedCycleSession>,
) -> Box<dyn reedline::Menu> {
    // niubash#631: under the cycle interaction the CycleMenu serves
    // every non-GNU style — it renders its own candidate grid and
    // splices candidates into the line itself, which no stock menu
    // exposes the control for (see `completion::cycle_menu`). The
    // GNU style is exempt: its interaction stays the literal readline
    // shape whatever the axis says. Under `insert`, the per-style
    // menus below are exactly the pre-#631 behavior.
    if let Some(session) = cycle_session {
        if config.completion_style != CompletionStyle::Gnu {
            // One menu for both grid interactions: `cycle` and
            // `select` differ in ruling history, not in the menu —
            // since niubash#778 both open with the first candidate
            // spliced and highlighted (the #742 select form's
            // unselected open was the highlight regression).
            return Box::new(
                crate::completion::cycle_menu::CycleMenu::new(session)
                    .with_name(name)
                    .with_menu_open_flag(menu_open),
            );
        }
    }
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
            // Owner ruling 2026-10-26 (zsh/bash parity): TAB #1 splices the
            // common prefix and stops — the menu must not linger on the
            // re-queried set (e.g. auto-listing a just-completed directory's
            // children skips the window where the user may just want to
            // stop). `ColumnMenu` wraps the stock grid with that stop rule;
            // a TAB that splices nothing still shows the grid immediately.
            Box::new(crate::completion::column_menu::ColumnMenu::new(
                ColumnarMenu::default()
                    .with_name(name)
                    .with_columns(page_cols)
                    .with_only_buffer_difference(false),
                menu_open,
            ))
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
    if let Some(name) = payload.strip_prefix(READLINE_HOST_COMMAND_PREFIX) {
        return run_readline_command(shell, line_editor, name);
    }
    // niubash#775: a key a loadable builtin bound through the readline ABI.
    // Mirror the live editor into the exported globals first, run the
    // plugin's C function, and adopt whatever it left in rl_line_buffer.
    if let Some((root, path)) = crate::readline_live::parse_payload(payload) {
        let editor_buffer = line_editor.current_buffer_contents().to_string();
        let editor_cursor = line_editor.current_insertion_point();
        crate::readline_live::sync_editor_to_abi(&editor_buffer, editor_cursor, None);
        let before = crate::readline_live::snapshot_abi_line();
        crate::readline_live::dispatch_state_begin();
        let fired = crate::readline_live::dispatch(root, &path);
        crate::readline_live::dispatch_state_end();
        if fired && crate::readline_live::abi_line_changed(&before) {
            crate::readline_live::sync_abi_to_editor(line_editor);
        }
        return SentinelOutcome::Resumed;
    }
    if let Some(bindx) = BindXInvocation::parse(payload) {
        let editor_buffer = line_editor.current_buffer_contents().to_string();
        let editor_cursor = line_editor.current_insertion_point();
        // A `bind -x` command (and any plugin builtin it invokes) must see
        // the line being edited, not the last line readline accepted.
        crate::readline_live::sync_editor_to_abi(&editor_buffer, editor_cursor, None);
        let before = crate::readline_live::snapshot_abi_line();
        crate::readline_live::dispatch_state_begin();
        let outcome =
            shell
                .borrow_mut()
                .run_bind_x_command(bindx.index, &editor_buffer, editor_cursor);
        crate::readline_live::dispatch_state_end();
        apply_widget_outcome(line_editor, &outcome);
        // The host's READLINE_* read-back wins unless a plugin touched the
        // exported ABI too; then the plugin's mutation is the newer one.
        if crate::readline_live::abi_line_changed(&before) {
            crate::readline_live::sync_abi_to_editor(line_editor);
        }
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
    crate::readline_live::sync_editor_to_abi(&editor_buffer, editor_cursor, None);
    let before = crate::readline_live::snapshot_abi_line();
    let outcome =
        shell
            .borrow_mut()
            .run_widget_function(&widget.function, &editor_buffer, editor_cursor);
    if outcome.accept {
        SentinelOutcome::Accept(outcome.buffer)
    } else {
        apply_widget_outcome(line_editor, &outcome);
        if crate::readline_live::abi_line_changed(&before) {
            crate::readline_live::sync_abi_to_editor(line_editor);
        }
        SentinelOutcome::Resumed
    }
}

/// Print text to the terminal while the editor is suspended for a
/// host command (the dumps of niubash#765): the prompt redraws over
/// the next `read_line`, so the text lands above it exactly like a
/// `bind -x` command's output does.
fn print_suspended_output(text: &str) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(text.as_bytes());
    let _ = lock.flush();
}

/// Group keyseqs by function name, preserving first-seen order
/// (the construction the engine's own `bind -P` face uses).
fn push_keyseq(by_name: &mut Vec<(String, Vec<String>)>, name: &str, seq: String) {
    if let Some((_, seqs)) = by_name.iter_mut().find(|(n, _)| n == name) {
        if !seqs.contains(&seq) {
            seqs.push(seq);
        }
    } else {
        by_name.push((name.to_string(), vec![seq]));
    }
}

/// Remove one keyseq from every function's list (a later binding
/// of the same seq to something else retires the earlier claim).
fn remove_keyseq(by_name: &mut [(String, Vec<String>)], seq: &str) {
    for (_, seqs) in by_name.iter_mut() {
        seqs.retain(|s| s != seq);
    }
}

/// The `dump-functions` listing (lib/readline/bind.c:2820
/// `rl_dump_functions` → `rl_function_dumper(0)`): every funmap
/// name, with the keyseqs that invoke it. The binding state is the
/// emacs keymap's: the engine default table, this batch's GNU
/// default command bindings, and the engine registry's function
/// entries for the emacs keymap, later layers overriding earlier
/// ones per keyseq — the same sources `bind -P` answers from,
/// plus the host defaults. The readable form (GNU's explicit-
/// argument variant) does not travel the host-command channel,
/// which carries no prefix argument.
fn dump_function_lines(entries: &[crate::bind_surface::EngineBindEntry]) -> Vec<String> {
    let mut by_name: Vec<(String, Vec<String>)> = Vec::new();
    for (seq, name) in rubash::builtins::bind::DEFAULT_FUNCTION_BINDINGS {
        push_keyseq(&mut by_name, name, seq.to_string());
    }
    for (seq, name) in crate::readline_commands::GNU_DEFAULT_COMMAND_BINDINGS {
        push_keyseq(&mut by_name, name, seq.to_string());
    }
    for entry in entries {
        if !engine_keymap_applies(entry.keymap.as_deref(), NativeKeymapTarget::Emacs) {
            continue;
        }
        remove_keyseq(&mut by_name, &entry.keyseq);
        if let crate::bind_surface::EngineBindKind::Function { name } = &entry.kind {
            push_keyseq(&mut by_name, name, entry.keyseq.clone());
        }
    }
    let mut names: Vec<String> = crate::readline_commands::funmap_names()
        .into_iter()
        .map(str::to_string)
        .collect();
    for (name, _) in &by_name {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names.sort();
    names
        .iter()
        .map(|name| {
            let seqs = by_name
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, s)| s.as_slice())
                .unwrap_or(&[]);
            crate::readline_commands::format_dump_function_line(name, seqs)
        })
        .collect()
}

thread_local! {
    /// The (buffer, point) a dynamic-complete-history invocation
    /// left behind, so the next invocation at the identical spot
    /// reads as the second consecutive press and lists the matches
    /// (niubash#767).
    static LAST_HISTORY_COMPLETION: std::cell::RefCell<Option<(String, usize)>> =
        const { std::cell::RefCell::new(None) };
    /// The (command, buffer, point) the last context-specific
    /// completion press left behind (GNU's repeated-TAB rule: the
    /// second press at the same spot lists instead of editing).
    static LAST_SPECIFIC_COMPLETION: std::cell::RefCell<Option<(String, usize, String)>> =
        const { std::cell::RefCell::new(None) };
}

/// The interactive history as the editor holds it, oldest first —
/// the list shell-expand-line's history expansion and
/// dynamic-complete-history's tokenization read (niubash#767). The
/// engine-side history list is empty on the REPL path, so the
/// editor is the only honest source.
fn editor_history_entries(line_editor: &Reedline) -> Vec<String> {
    line_editor
        .history()
        .search(reedline::SearchQuery::everything(
            reedline::SearchDirection::Forward,
            None,
        ))
        .map(|items| items.into_iter().map(|item| item.command_line).collect())
        .unwrap_or_default()
}

/// `rl_forward_word` (lib/readline/text.c:482) from POSITION:
/// if the character there is not a word character, advance to
/// the next word character first; then advance to the end of
/// the word. Word characters are alphanumerics and `_`
/// (readline's `_rl_walphabetic`).
fn forward_word_end(text: &str, mut position: usize) -> usize {
    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }
    let mut rest = text[position..].chars().peekable();
    if matches!(rest.peek(), Some(&c) if !is_word_char(c)) {
        while let Some(&c) = rest.peek() {
            if is_word_char(c) {
                break;
            }
            position += c.len_utf8();
            rest.next();
        }
    }
    while let Some(&c) = rest.peek() {
        if !is_word_char(c) {
            break;
        }
        position += c.len_utf8();
        rest.next();
    }
    position
}

/// Replace the suspended editor's line with EXPANDED, placing the
/// point the way bash's `set_up_new_line` does (bashline.c:2845):
/// a point at the end stays at the new end; any other point keeps
/// its offset, and when the character there is not whitespace
/// `rl_forward_word` (text.c:482) advances it to the end of that
/// word. No change when EXPANDED equals the line.
fn replace_line_set_up_new_line(
    line_editor: &mut Reedline,
    buffer: &str,
    cursor: usize,
    expanded: String,
) {
    if expanded == buffer {
        return;
    }
    let position = if cursor >= buffer.len() {
        expanded.len()
    } else {
        let mut position = cursor.min(expanded.len());
        while !expanded.is_char_boundary(position) {
            position -= 1;
        }
        if position < expanded.len() && !matches!(expanded.as_bytes()[position], b' ' | b'\t') {
            position = forward_word_end(&expanded, position);
        }
        position
    };
    line_editor.run_edit_commands(&[
        EditCommand::Clear,
        EditCommand::InsertString(expanded),
        EditCommand::MoveToPosition {
            position,
            select: false,
        },
    ]);
}

/// Perform one host readline command (see
/// [`READLINE_HOST_COMMAND_PREFIX`]) against the suspended editor
/// and the live session.
fn run_readline_command(
    shell: &Rc<RefCell<Shell>>,
    line_editor: &mut Reedline,
    name: &str,
) -> SentinelOutcome {
    match name {
        // bind.c:2820 rl_dump_functions: the "\r\n" of the dispatch
        // plus the dumper's own leading "\n" — one blank line —
        // then one line per funmap name.
        "dump-functions" => {
            let (entries, _) = crate::bind_surface::load_engine_bindings(shell.borrow().executor());
            let lines = dump_function_lines(&entries);
            let mut out = String::from("\n\n");
            for line in lines {
                out.push_str(&line);
                out.push('\n');
            }
            print_suspended_output(&out);
        }
        // bind.c:2905 rl_dump_macros → rl_macro_dumper(0): the
        // registry's macro entries in GNU plain form.
        "dump-macros" => {
            let (entries, _) = crate::bind_surface::load_engine_bindings(shell.borrow().executor());
            let mut out = String::from("\n");
            for entry in &entries {
                if !engine_keymap_applies(entry.keymap.as_deref(), NativeKeymapTarget::Emacs) {
                    continue;
                }
                if let crate::bind_surface::EngineBindKind::Macro { text } = &entry.kind {
                    out.push_str(&rubash::builtins::bind::format_macro_plain(
                        &entry.keyseq,
                        text,
                    ));
                    out.push('\n');
                }
            }
            print_suspended_output(&out);
        }
        // bind.c:3076 rl_dump_variables → rl_variable_dumper(0):
        // the registry's variable table in GNU plain form.
        "dump-variables" => {
            let variables = crate::bind_surface::load_readline_variables(shell.borrow().executor());
            let mut out = String::from("\n");
            for (name, value) in &variables {
                out.push_str(&rubash::builtins::bind::format_variable_plain(name, value));
                out.push('\n');
            }
            print_suspended_output(&out);
        }
        // bind.c:1025 rl_re_read_init_file: re-read the rc's bind
        // state; the between-prompts staleness rebuild re-mirrors
        // any registry movement before the next prompt.
        "re-read-init-file" => {
            shell.borrow_mut().reread_init_file_bindings();
        }
        // lib/readline/util.c:181 rl_tilde_expand (niubash#766):
        // expand the tilde at the start of the whitespace-delimited
        // word at point; point lands just past the replacement, as
        // _rl_replace_text (text.c:194) leaves it.
        "tilde-expand" => {
            let buffer = line_editor.current_buffer_contents().to_string();
            let cursor = line_editor.current_insertion_point();
            if let Some((start, end)) = crate::readline_commands::tilde_word_span(&buffer, cursor) {
                let word = &buffer[start..end];
                let env: std::collections::HashMap<String, String> =
                    shell.borrow().session().environment().into_iter().collect();
                if let Some(expanded) = rubash::expand::tilde::tilde::expand_word_prefix(word, &env)
                {
                    if expanded != word {
                        let mut next =
                            String::with_capacity(buffer.len() - (end - start) + expanded.len());
                        next.push_str(&buffer[..start]);
                        next.push_str(&expanded);
                        next.push_str(&buffer[end..]);
                        let position = start + expanded.len();
                        line_editor.run_edit_commands(&[
                            EditCommand::Clear,
                            EditCommand::InsertString(next),
                            EditCommand::MoveToPosition {
                                position,
                                select: false,
                            },
                        ]);
                    }
                }
            }
        }
        // lib/readline/text.c:2358 rl_execute_named_command
        // (niubash#766): read a command name in the minibuffer and
        // dispatch it.
        "execute-named-command" => {
            return execute_named_command(shell, line_editor);
        }
        // bashline.c:2982 shell_expand_line (niubash#767): history
        // expansion, alias expansion, then word expansion of the
        // whole line, as one edit. GNU leaves the line untouched on
        // an expansion error. Point placement follows the C tail:
        // a point at the end of the old line goes to the end of
        // the new one; any other point keeps its offset, and when
        // the character there is not whitespace, rl_forward_word
        // (text.c:482) advances it to the end of that word.
        "shell-expand-line" => {
            let buffer = line_editor.current_buffer_contents().to_string();
            let cursor = line_editor.current_insertion_point();
            let entries = editor_history_entries(line_editor);
            if let Some(expanded) = shell.borrow_mut().shell_expand_line(&buffer, &entries) {
                replace_line_set_up_new_line(line_editor, &buffer, cursor, expanded);
            }
        }
        // bashline.c:2902 history_expand_line: only the history
        // expansion stage of shell_expand_line (pre_process_line,
        // bashhist.c:562). An expansion error leaves the line as it
        // was, like GNU's cleanup path.
        "history-expand-line" => {
            let buffer = line_editor.current_buffer_contents().to_string();
            let cursor = line_editor.current_insertion_point();
            let entries = editor_history_entries(line_editor);
            if let Some(expanded) = shell.borrow_mut().history_expand_line(&buffer, &entries) {
                replace_line_set_up_new_line(line_editor, &buffer, cursor, expanded);
            }
        }
        // util.c:99 _rl_abort_internal / util.c:122 rl_abort: clear
        // the pending command and ding. The line buffer is NOT
        // touched — C-g must not abandon what the user typed. The
        // suspended editor has already resolved and dropped any
        // chord/prefix state before this event; the remaining GNU
        // state (numeric argument, mark, keyboard macro) has no host
        // seam, so the observable delivery here is the bell.
        "abort" => {
            readline_ding();
        }
        // bashline.c:3896/3954/3966/3980/3890/3795
        // bash_complete_filename / bash_complete_variable /
        // bash_complete_command / bash_complete_hostname /
        // bash_complete_username / bash_brace_completion: the
        // ordinary completion machinery pinned to one candidate
        // source. The shared press rule is rl_complete_internal's:
        // a lone match is inserted, several insert the longest common
        // prefix, and a second press at the same spot lists.
        // bashline.c:4068 `bash_glob_complete_word` joins them with
        // `shell_glob_filename` as its candidate source.
        "complete-filename"
        | "complete-variable"
        | "complete-command"
        | "complete-hostname"
        | "complete-username"
        | "complete-into-braces"
        | "glob-complete-word" => {
            run_specific_completion(shell, line_editor, name);
        }
        // bashline.c:3810 dynamic_complete_history (niubash#767):
        // completion whose candidates are the tokenized history
        // (history_completion_generator, bashline.c:3782), sorted.
        // The first press completes to the longest common prefix (a
        // unique match completes fully); a second consecutive press
        // at the same buffer and point lists the matches, like
        // readline's TAB/TAB completion display.
        "dynamic-complete-history" => {
            let buffer = line_editor.current_buffer_contents().to_string();
            let cursor = line_editor.current_insertion_point();
            let (start, end) = crate::readline_commands::completion_word_span(&buffer, cursor);
            let word = &buffer[start..end];
            let entries = editor_history_entries(line_editor);
            let matches = crate::readline_commands::history_completion_matches(&entries, word);
            if matches.is_empty() {
                readline_ding();
                return SentinelOutcome::Resumed;
            }
            let repeated = LAST_HISTORY_COMPLETION.with(|last| {
                matches!(last.borrow().as_ref(), Some((b, c)) if b == &buffer && *c == cursor)
            });
            if repeated {
                let mut out = String::from("\n");
                out.push_str(&matches.join("  "));
                out.push('\n');
                print_suspended_output(&out);
                return SentinelOutcome::Resumed;
            }
            let replacement = if matches.len() == 1 {
                let mut only = matches[0].clone();
                if !buffer[end..].starts_with(' ') {
                    only.push(' ');
                }
                only
            } else {
                crate::readline_commands::common_prefix(&matches)
            };
            if replacement != word {
                let mut next =
                    String::with_capacity(buffer.len() - (end - start) + replacement.len());
                next.push_str(&buffer[..start]);
                next.push_str(&replacement);
                next.push_str(&buffer[end..]);
                let position = start + replacement.len();
                line_editor.run_edit_commands(&[
                    EditCommand::Clear,
                    EditCommand::InsertString(next.clone()),
                    EditCommand::MoveToPosition {
                        position,
                        select: false,
                    },
                ]);
                LAST_HISTORY_COMPLETION.with(|last| {
                    *last.borrow_mut() = Some((next, position));
                });
            } else {
                LAST_HISTORY_COMPLETION.with(|last| {
                    *last.borrow_mut() = Some((buffer.clone(), cursor));
                });
            }
        }
        _ => {}
    }
    SentinelOutcome::Resumed
}

/// One context-specific completion press (`complete-command` /
/// `complete-variable` / `complete-filename` / `complete-hostname` /
/// `complete-username` / `complete-into-braces`). Candidates come from
/// [`crate::completion::NiubashCompleter::complete_specific`], which
/// pins the GNU source while reusing the ordinary completion tables.
fn run_specific_completion(shell: &Rc<RefCell<Shell>>, line_editor: &mut Reedline, name: &str) {
    use crate::completion::SpecificCompletion;
    let mode = match name {
        "complete-command" => SpecificCompletion::Command,
        "complete-variable" => SpecificCompletion::Variable,
        "complete-hostname" => SpecificCompletion::Hostname,
        "complete-username" => SpecificCompletion::Username,
        "complete-into-braces" => SpecificCompletion::Brace,
        "glob-complete-word" => SpecificCompletion::Glob,
        _ => SpecificCompletion::Filename,
    };
    let buffer = line_editor.current_buffer_contents().to_string();
    let cursor = line_editor.current_insertion_point();
    let matches: Vec<String> = {
        let mut completer = NiubashCompleter::new(shell.borrow().completion_state.clone());
        completer
            .complete_specific(mode, &buffer, cursor)
            .into_iter()
            .map(|suggestion| suggestion.value)
            .collect()
    };
    if matches.is_empty() {
        readline_ding();
        return;
    }
    let repeated = LAST_SPECIFIC_COMPLETION.with(|last| {
        matches!(
            last.borrow().as_ref(),
            Some((b, c, n)) if b == &buffer && *c == cursor && n == name
        )
    });
    LAST_SPECIFIC_COMPLETION.with(|last| {
        *last.borrow_mut() = Some((buffer.clone(), cursor, name.to_string()));
    });
    if repeated && matches.len() > 1 {
        // Second press at the same spot: list, like the TAB/TAB
        // display in rl_complete_internal.
        let mut sorted = matches;
        sorted.sort();
        let mut out = String::from("\n");
        out.push_str(&sorted.join("  "));
        out.push('\n');
        print_suspended_output(&out);
        return;
    }
    let (start, end) = crate::completion::completion_span(&buffer, cursor);
    let word = &buffer[start..end];
    let Some(replacement) = crate::readline_commands::completion_replacement(&matches, word) else {
        readline_ding();
        return;
    };
    let mut next = String::with_capacity(buffer.len() - (end - start) + replacement.len());
    next.push_str(&buffer[..start]);
    next.push_str(&replacement);
    next.push_str(&buffer[end..]);
    let position = start + replacement.len();
    line_editor.run_edit_commands(&[
        EditCommand::Clear,
        EditCommand::InsertString(next),
        EditCommand::MoveToPosition {
            position,
            select: false,
        },
    ]);
}

/// GNU's rl_ding: ring the terminal bell.
fn readline_ding() {
    print_suspended_output("\u{7}");
}

/// `execute-named-command` (text.c:2358): read the name with
/// `_rl_read_command_name` (text.c:2309), then dispatch the named
/// function. An unknown name dings and returns 1 — nothing
/// dispatches. Dispatch fidelity: edit events apply to the
/// suspended editor, `Multiple` folds, nested host commands recurse
/// through the sentinel dispatcher, and accept-line submits the
/// current buffer; editor events that need the live editor (menus,
/// mode switches, suspension) cannot be injected into a suspended
/// editor and ding instead.
fn execute_named_command(
    shell: &Rc<RefCell<Shell>>,
    line_editor: &mut Reedline,
) -> SentinelOutcome {
    let Some(name) = read_command_name() else {
        return SentinelOutcome::Resumed;
    };
    if !crate::readline_commands::funmap_names().contains(&name.as_str()) {
        readline_ding();
        return SentinelOutcome::Resumed;
    }
    dispatch_named_event(shell, line_editor, native_widget_event(&name))
}

fn dispatch_named_event(
    shell: &Rc<RefCell<Shell>>,
    line_editor: &mut Reedline,
    event: Option<ReedlineEvent>,
) -> SentinelOutcome {
    match event {
        Some(ReedlineEvent::Edit(commands)) => {
            line_editor.run_edit_commands(&commands);
            SentinelOutcome::Resumed
        }
        Some(ReedlineEvent::Multiple(events)) => {
            for event in events {
                if let SentinelOutcome::Accept(line) =
                    dispatch_named_event(shell, line_editor, Some(event))
                {
                    return SentinelOutcome::Accept(line);
                }
            }
            SentinelOutcome::Resumed
        }
        Some(ReedlineEvent::Enter) => {
            SentinelOutcome::Accept(Some(line_editor.current_buffer_contents().to_string()))
        }
        Some(ReedlineEvent::ExecuteHostCommand(payload)) => {
            run_host_sentinel(shell, line_editor, &payload)
        }
        _ => {
            readline_ding();
            SentinelOutcome::Resumed
        }
    }
}

/// The minibuffer read behind execute-named-command:
/// `_rl_read_command_name` (text.c:2309) reads one line with
/// completion over the funmap names (`readcmd_completion_function`).
/// The host reads keys in raw mode while the editor is suspended —
/// the same posture as the interactive menu's raw-mode loop. TAB
/// completes to the longest common prefix; a second consecutive
/// TAB lists the matches, like readline's completion display.
/// Returns None when the read is aborted (ESC / C-g / C-c).
fn read_command_name() -> Option<String> {
    use crossterm29::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

    fn render(text: &str, cursor: usize) {
        let mut out = String::from("\r\u{1b}[K");
        out.push_str("M-x ");
        out.push_str(text);
        let tail = text[cursor..].chars().count();
        if tail > 0 {
            out.push_str(&format!("\u{1b}[{tail}D"));
        }
        print_suspended_output(&out);
    }

    let _ = crossterm29::terminal::enable_raw_mode();
    let mut text = String::new();
    let mut cursor: usize = 0;
    let mut last_was_tab = false;
    print_suspended_output("\r\n");
    render(&text, cursor);
    let result = loop {
        let Ok(Event::Key(key)) = crossterm29::event::read() else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => break Some(text.clone()),
            KeyCode::Char('j' | 'm') if ctrl => break Some(text.clone()),
            KeyCode::Esc => break None,
            KeyCode::Char('g' | 'c') if ctrl => break None,
            KeyCode::Tab => {
                let matches: Vec<String> = crate::readline_commands::funmap_names()
                    .iter()
                    .filter(|name| name.starts_with(text.as_str()))
                    .map(|name| name.to_string())
                    .collect();
                if matches.is_empty() {
                    readline_ding();
                } else {
                    let lcp = crate::readline_commands::common_prefix(&matches);
                    if lcp.len() > text.len() {
                        text = lcp;
                        cursor = text.len();
                        render(&text, cursor);
                    } else if last_was_tab {
                        let mut out = String::from("\r\n");
                        out.push_str(&matches.join("  "));
                        out.push_str("\r\n");
                        print_suspended_output(&out);
                        render(&text, cursor);
                    }
                }
                last_was_tab = true;
                continue;
            }
            KeyCode::Backspace => {
                if cursor > 0 {
                    let prev = text[..cursor]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    text.replace_range(prev..cursor, "");
                    cursor = prev;
                }
            }
            KeyCode::Delete => {
                if cursor < text.len() {
                    let next = text[cursor..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| cursor + i)
                        .unwrap_or(text.len());
                    text.replace_range(cursor..next, "");
                }
            }
            KeyCode::Left => {
                cursor = text[..cursor]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
            }
            KeyCode::Right => {
                if cursor < text.len() {
                    cursor = text[cursor..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| cursor + i)
                        .unwrap_or(text.len());
                }
            }
            KeyCode::Home => cursor = 0,
            KeyCode::End => cursor = text.len(),
            KeyCode::Char('a') if ctrl => cursor = 0,
            KeyCode::Char('e') if ctrl => cursor = text.len(),
            KeyCode::Char('b') if ctrl => {
                cursor = text[..cursor]
                    .char_indices()
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(0);
            }
            KeyCode::Char('f') if ctrl => {
                if cursor < text.len() {
                    cursor = text[cursor..]
                        .char_indices()
                        .nth(1)
                        .map(|(i, _)| cursor + i)
                        .unwrap_or(text.len());
                }
            }
            KeyCode::Char('u') if ctrl => {
                text.clear();
                cursor = 0;
            }
            KeyCode::Char('k') if ctrl => {
                text.truncate(cursor);
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                text.insert(cursor, c);
                cursor += c.len_utf8();
            }
            _ => {
                readline_ding();
                last_was_tab = false;
                continue;
            }
        }
        last_was_tab = false;
        render(&text, cursor);
    };
    let _ = crossterm29::terminal::disable_raw_mode();
    print_suspended_output("\r\n");
    result
}

/// One editor keymap under construction, remembering which registry
/// layer last wrote each slot (niubash#630). The layer closures are the
/// same `add_*_keybindings` passes the editor was always built with;
/// the wrapper only observes their effects, diffing the map around
/// each pass so the registry can attribute every slot's winner (and
/// the layer it shadowed) without the passes changing shape.
struct LayeredKeymap {
    keybindings: Keybindings,
    // Keyed by the (modifiers, key code) pair: reedline 0.52 does not
    // export its combination type name.
    stacks: std::collections::HashMap<
        (KeyModifiers, KeyCode),
        Vec<(crate::keymap_registry::KeymapSource, ReedlineEvent)>,
    >,
}

impl LayeredKeymap {
    fn new(defaults: Keybindings) -> Self {
        let stacks = defaults
            .bindings
            .iter()
            .map(|(combo, event)| {
                (
                    (combo.modifier, combo.key_code),
                    vec![(crate::keymap_registry::KeymapSource::Default, event.clone())],
                )
            })
            .collect();
        Self {
            keybindings: defaults,
            stacks,
        }
    }

    fn layer<R>(
        &mut self,
        source: crate::keymap_registry::KeymapSource,
        apply: impl FnOnce(&mut Keybindings) -> R,
    ) -> R {
        let before = self.keybindings.bindings.clone();
        let out = apply(&mut self.keybindings);
        for (combo, event) in &self.keybindings.bindings {
            if before.get(combo) != Some(event) {
                self.stacks
                    .entry((combo.modifier, combo.key_code))
                    .or_default()
                    .push((source.clone(), event.clone()));
            }
        }
        out
    }

    fn finish(self) -> (Keybindings, crate::keymap_registry::KeymapStacks) {
        (self.keybindings, self.stacks.into_iter().collect())
    }
}

/// Build one target keymap in the registry's precedence order
/// (niubash#630, DESIGN §5.3): reedline defaults < host baseline
/// (menus, clipboard, native-widget presets) < bundle < script
/// (engine `bind`) < user. User bindings used to be applied *before*
/// the engine pass, so a startup script silently overrode the user's
/// own `NIU_BINDKEYS` — that inversion was a defect, not a baseline,
/// and this order is the fix. The adjudication happens here, at build
/// (registration) time: every rebuild reapplies the user layer over
/// whatever the script layer registered, so a mid-session script
/// rebind of a user-widget key can never take effect — it is recorded
/// as shadowed instead. Returns the map plus the registry rows and
/// dropped-bind records the build produced.
#[allow(clippy::too_many_arguments)]
fn build_keymap_for_target(
    defaults: Keybindings,
    target: NativeKeymapTarget,
    mode: crate::keymap_registry::KeymapMode,
    native_widgets: &NativeWidgetConfig,
    native_widget_bindings: &[NativeWidgetBinding],
    user_widget_bindings: &[NativeWidgetBinding],
    engine_bindings: &[crate::bind_surface::EngineBindEntry],
    completion_interaction: CompletionInteraction,
) -> (
    Keybindings,
    Vec<crate::keymap_registry::KeymapEntry>,
    Vec<crate::keymap_registry::DroppedBind>,
) {
    use crate::keymap_registry::{entries_from_stacks, KeymapSource, UserBindingVia};
    let mut builder = LayeredKeymap::new(defaults);
    builder.layer(KeymapSource::HostBaseline, |keybindings| {
        add_menu_keybindings(keybindings, completion_interaction)
    });
    builder.layer(KeymapSource::HostBaseline, |keybindings| {
        add_system_clipboard_keybindings(keybindings)
    });
    builder.layer(KeymapSource::HostBaseline, |keybindings| {
        add_gnu_command_default_keybindings(keybindings, target)
    });
    builder.layer(KeymapSource::HostBaseline, |keybindings| {
        add_native_widget_keybindings(keybindings, target, native_widgets, native_widget_bindings)
    });
    if native_widgets.import_bindkeys {
        for binding in native_widget_bindings {
            let source = KeymapSource::Bundle {
                file: binding
                    .source_file
                    .as_ref()
                    .map(|path| path.display().to_string()),
                line: binding.line,
            };
            builder.layer(source, |keybindings| {
                apply_bundle_widget_binding(keybindings, target, binding)
            });
        }
    }
    let dropped = builder
        .layer(KeymapSource::Script, |keybindings| {
            add_engine_bind_keybindings_recorded(keybindings, target, engine_bindings, 0)
        })
        .1;
    // niubash#775: plugin registrations made through the live readline ABI
    // (`rl_add_defun`/`rl_bind_key*`) join the script layer.  The user's own
    // NIU_BINDKEYS layer still applies last, matching GNU's last-write rule.
    builder.layer(KeymapSource::Script, |keybindings| {
        crate::readline_live::add_plugin_keybindings(keybindings, target)
    });
    builder.layer(
        KeymapSource::User {
            via: UserBindingVia::NiuBindkeys,
        },
        |keybindings| add_user_widget_keybindings(keybindings, user_widget_bindings),
    );
    let (keybindings, stacks) = builder.finish();
    (
        keybindings,
        entries_from_stacks(mode, stacks, engine_bindings),
        dropped,
    )
}

/// Everything the chord layer needs on top of the inner machine,
/// built from the engine snapshot for one edit mode (niubash#731:
/// the quote sequences joined the chords here; the parts travel as
/// one struct so `build_edit_mode`'s return stays a plain triple).
struct ChordLayerParts {
    chords: Vec<ChordBinding>,
    alt_keys: std::collections::HashSet<KeyCode>,
    quote_sequences: Vec<Vec<(KeyModifiers, KeyCode)>>,
}

#[allow(clippy::too_many_arguments)]
fn build_edit_mode(
    mode: EditorMode,
    native_widgets: &NativeWidgetConfig,
    native_widget_bindings: &[NativeWidgetBinding],
    user_widget_bindings: &[NativeWidgetBinding],
    engine_bindings: &[crate::bind_surface::EngineBindEntry],
    engine_bind_generation: u64,
    completion_interaction: CompletionInteraction,
) -> (
    Box<dyn EditMode>,
    crate::keymap_registry::KeymapRegistry,
    ChordLayerParts,
) {
    use crate::keymap_registry::{KeymapMode, KeymapRegistry};
    match mode {
        EditorMode::Emacs => {
            let (keybindings, entries, dropped) = build_keymap_for_target(
                default_emacs_keybindings(),
                NativeKeymapTarget::Emacs,
                KeymapMode::Emacs,
                native_widgets,
                native_widget_bindings,
                user_widget_bindings,
                engine_bindings,
                completion_interaction,
            );
            let alt_keys = alt_bound_keys(&keybindings);
            (
                Box::new(Emacs::new(keybindings)),
                KeymapRegistry {
                    generation: engine_bind_generation,
                    editor_mode: "emacs".to_string(),
                    entries,
                    dropped,
                },
                ChordLayerParts {
                    chords: {
                        let mut chords =
                            collect_chord_bindings(engine_bindings, NativeKeymapTarget::Emacs, 0);
                        chords.extend(gnu_default_chord_bindings());
                        let abi_unbinds =
                            crate::readline_live::plugin_chord_unbinds(NativeKeymapTarget::Emacs);
                        chords.retain(|chord| !abi_unbinds.contains(&chord.keys));
                        chords.extend(
                            crate::readline_live::plugin_chords(NativeKeymapTarget::Emacs)
                                .into_iter()
                                .map(|(keys, event)| ChordBinding { keys, event }),
                        );
                        chords
                    },
                    alt_keys,
                    quote_sequences: collect_quote_sequences(
                        engine_bindings,
                        NativeKeymapTarget::Emacs,
                    ),
                },
            )
        }
        EditorMode::Vi => {
            let (insert_keybindings, mut entries, mut dropped) = build_keymap_for_target(
                default_vi_insert_keybindings(),
                NativeKeymapTarget::ViInsert,
                KeymapMode::ViInsert,
                native_widgets,
                native_widget_bindings,
                user_widget_bindings,
                engine_bindings,
                completion_interaction,
            );
            let (normal_keybindings, normal_entries, normal_dropped) = build_keymap_for_target(
                default_vi_normal_keybindings(),
                NativeKeymapTarget::ViNormal,
                KeymapMode::ViNormal,
                native_widgets,
                native_widget_bindings,
                user_widget_bindings,
                engine_bindings,
                completion_interaction,
            );
            entries.extend(normal_entries);
            for bind in normal_dropped {
                if !dropped.contains(&bind) {
                    dropped.push(bind);
                }
            }
            let alt_keys = alt_bound_keys(&insert_keybindings);
            (
                Box::new(Vi::new(
                    insert_keybindings,
                    normal_keybindings,
                    // reedline 0.52 added visual mode with its own keybinding
                    // set; niubash has never bound visual keys of its own, so
                    // the upstream defaults are the faithful choice.
                    default_vi_visual_keybindings(),
                )),
                KeymapRegistry {
                    generation: engine_bind_generation,
                    editor_mode: "vi".to_string(),
                    entries,
                    dropped,
                },
                ChordLayerParts {
                    chords: {
                        let mut chords = Vec::new();
                        for target in [NativeKeymapTarget::ViInsert, NativeKeymapTarget::ViNormal] {
                            chords.extend(collect_chord_bindings(engine_bindings, target, 0));
                            chords.extend(
                                crate::readline_live::plugin_chords(target)
                                    .into_iter()
                                    .map(|(keys, event)| ChordBinding { keys, event }),
                            );
                        }
                        let abi_unbinds: Vec<_> =
                            [NativeKeymapTarget::ViInsert, NativeKeymapTarget::ViNormal]
                                .into_iter()
                                .flat_map(crate::readline_live::plugin_chord_unbinds)
                                .collect();
                        chords.retain(|chord| !abi_unbinds.contains(&chord.keys));
                        chords
                    },
                    alt_keys,
                    quote_sequences: {
                        let mut quotes = Vec::new();
                        for target in [NativeKeymapTarget::ViInsert, NativeKeymapTarget::ViNormal] {
                            quotes.extend(collect_quote_sequences(engine_bindings, target));
                        }
                        quotes
                    },
                },
            )
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
    /// niubash#631: the cycle interaction's shared session cell, when
    /// this editor was built for `completion.interaction = cycle` on a
    /// non-GNU style. The CycleMenu publishes its session (snapshot +
    /// spliced flag) there; this wrapper consumes it in
    /// [`Self::delegate`] for the keys a menu cannot finish itself:
    /// Esc rolls the line back to the snapshot, an Enter inside a live
    /// session accepts the candidate in the line and submits in the
    /// same press, and any other edit key ends the session as
    /// accepted-in-place. `None` under `insert` and for the GNU style
    /// — every #631 path is inert then.
    cycle_session: Option<SharedCycleSession>,
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

    /// Attach the cycle interaction's shared session cell
    /// (niubash#631) — see the `cycle_session` field.
    fn with_cycle_session(mut self, cycle_session: Option<SharedCycleSession>) -> Self {
        self.cycle_session = cycle_session;
        self
    }

    /// Whether the attached cycle session is live right now.
    fn cycle_session_open(&self) -> bool {
        self.cycle_session
            .as_ref()
            .and_then(|session| session.lock().ok())
            .is_some_and(|state| state.is_open())
    }

    /// niubash#631: the navigation splice inside a live cycle
    /// session, as the edit batch that replaces the current candidate
    /// with the one `target_of` picks from the shared cell. Issued
    /// HERE, at key-translation time, because the menu cannot: a
    /// menu-side splice runs at paint time in
    /// `update_working_details`, and the painter composes the frame's
    /// input line before that hook — the line would lag the grid's
    /// highlight by a full paint on every navigation key. The batch
    /// is the rollback's proven whole-line shape (Clear +
    /// InsertString + MoveToPosition): mid-session the buffer is
    /// always the snapshot with its span replaced by the current
    /// candidate, so the next buffer is a pure string computation
    /// over the shared cell, which this also advances (index, region,
    /// nav_seq — the menu consumes one nav_seq per Edit echo the
    /// engine reports back). `None` when no live session can navigate
    /// (the key then takes its normal translation: TAB #1 opens the
    /// menu through the bindings, and the menu begins the session
    /// itself).
    fn cycle_splice_edits(
        &self,
        target_of: impl FnOnce(&crate::completion::cycle_menu::CycleSession) -> Option<usize>,
    ) -> Option<Vec<EditCommand>> {
        let session = self.cycle_session.as_ref()?;
        let mut state = session.lock().ok()?;
        if !state.is_open() || state.candidates.len() < 2 {
            return None;
        }
        let target = target_of(&state)?;
        let snapshot = state.snapshot.clone()?;
        state.index = target;
        let text = state.candidates[state.index].splice_text();
        let (start, span_end) = state.span;
        let mut new_buffer = String::with_capacity(snapshot.buffer.len() + text.len());
        new_buffer.push_str(&snapshot.buffer[..start]);
        new_buffer.push_str(&text);
        new_buffer.push_str(&snapshot.buffer[span_end..]);
        state.region = (start, start + text.len());
        state.nav_seq += 1;
        state.spliced = true;
        Some(crate::completion::cycle_menu::whole_line_edits(
            &new_buffer,
            start + text.len(),
        ))
    }

    /// The TAB (`delta` +1) / Shift-TAB (-1) walk: the next candidate
    /// in cycle order, with wrap-around.
    fn cycle_navigation_edits(&self, delta: isize) -> Option<Vec<EditCommand>> {
        self.cycle_splice_edits(|state| {
            let len = state.candidates.len() as isize;
            Some((state.index as isize + delta).rem_euclid(len) as usize)
        })
    }

    /// niubash#778: an arrow key inside a live cycle session moves
    /// the highlight through the painted grid — up/down within a
    /// column, left/right across columns, per
    /// [`grid_neighbor`](crate::completion::cycle_menu::grid_neighbor)
    /// over the geometry the menu published into the cell. Before
    /// #778 the wrapper classified every arrow as session-ending, so
    /// Down silently killed the session (and the NEXT TAB then took
    /// the menu-local path, which is why a highlight mysteriously
    /// "appeared" only after a Down+TAB), while Left/Right closed
    /// the menu outright through the engine's Edit notification.
    /// A move that cannot change the candidate (left/right in a
    /// single-column grid) still splices the current candidate: the
    /// key is consumed as navigation and the session lives on.
    fn cycle_arrow_edits(&self, dcol: isize, drow: isize) -> Option<Vec<EditCommand>> {
        self.cycle_splice_edits(|state| {
            // No paint yet (or a stale cell): a single column is the
            // conservative shape — vertical steps still walk the set.
            let rows = if state.grid_rows == 0 {
                state.candidates.len()
            } else {
                state.grid_rows
            };
            Some(crate::completion::cycle_menu::grid_neighbor(
                state.index,
                state.candidates.len(),
                rows,
                dcol,
                drow,
            ))
        })
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
            cycle_session: None,
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
    /// The paste layer carries no mode semantics of its own, so a
    /// mode-specific event — in particular `ReedlineEvent::SwitchMode`,
    /// which is how the engine asks a machine to become the vi/emacs
    /// machine a `bind` target names — belongs to the wrapped machine.
    fn handle_mode_specific_event(&mut self, event: ReedlineEvent) -> reedline::EventStatus {
        self.inner.handle_mode_specific_event(event)
    }

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
        // niubash#631: a TAB / Shift-TAB inside a live cycle session
        // IS the navigation splice, issued here at translation time
        // (see `cycle_navigation_edits`) — the key never reaches the
        // menu bindings, whose MenuNext the menu could only apply at
        // paint time, one frame behind the grid's highlight. With no
        // live session the key falls through to its normal
        // translation (TAB #1 opens the menu through the bindings).
        // niubash#778: the arrow keys are navigation inside a live
        // session too (see `cycle_arrow_edits`) — grid movement, not
        // history or cursor motion, and never a session end.
        if let crossterm29::event::Event::Key(key) = &raw {
            if key.kind == crossterm29::event::KeyEventKind::Press {
                let delta = match (key.code, key.modifiers) {
                    (crossterm29::event::KeyCode::Tab, modifiers)
                        if modifiers == crossterm29::event::KeyModifiers::NONE =>
                    {
                        Some(1)
                    }
                    (crossterm29::event::KeyCode::BackTab, _) => Some(-1),
                    _ => None,
                };
                if let Some(delta) = delta {
                    if let Some(edits) = self.cycle_navigation_edits(delta) {
                        self.menu_possible = true;
                        return ReedlineEvent::Edit(edits);
                    }
                }
                let arrow = match (key.code, key.modifiers) {
                    (crossterm29::event::KeyCode::Down, modifiers)
                        if modifiers == crossterm29::event::KeyModifiers::NONE =>
                    {
                        Some((0, 1))
                    }
                    (crossterm29::event::KeyCode::Up, modifiers)
                        if modifiers == crossterm29::event::KeyModifiers::NONE =>
                    {
                        Some((0, -1))
                    }
                    (crossterm29::event::KeyCode::Right, modifiers)
                        if modifiers == crossterm29::event::KeyModifiers::NONE =>
                    {
                        Some((1, 0))
                    }
                    (crossterm29::event::KeyCode::Left, modifiers)
                        if modifiers == crossterm29::event::KeyModifiers::NONE =>
                    {
                        Some((-1, 0))
                    }
                    _ => None,
                };
                if let Some((dcol, drow)) = arrow {
                    if let Some(edits) = self.cycle_arrow_edits(dcol, drow) {
                        self.menu_possible = true;
                        return ReedlineEvent::Edit(edits);
                    }
                }
            }
        }
        // niubash#631: classify the raw key against a live cycle
        // session BEFORE anything else translates it. This wrapper is
        // the only component that sees raw keys synchronously — the
        // CycleMenu learns of a key a paint later — so the session's
        // end states are decided here: deciding them in the menu would
        // let an Esc in the same key burst be translated before the
        // menu knew the session had been edited or rolled back.
        // Navigation keys (TAB / Shift-TAB and, since niubash#778,
        // the plain arrow keys) leave the session to the
        // menu; Enter is decided with the submit shape below; a bare
        // Esc consumes the session and (when a candidate was spliced)
        // arms the rollback edits; every other editing key ends the
        // session as accepted-in-place. Only key presses and pastes
        // classify — non-key events (ConPTY's focus flicker rides this
        // path between real keys) and key releases leave the session
        // untouched.
        let mut cycle_rollback: Option<Vec<EditCommand>> = None;
        if let Some(session) = &self.cycle_session {
            enum CycleKey {
                Navigate,
                Accept,
                Rollback,
                EndSession,
                Ignore,
            }
            let classification = match &raw {
                crossterm29::event::Event::Key(key)
                    if key.kind == crossterm29::event::KeyEventKind::Press =>
                {
                    match (key.code, key.modifiers) {
                        (crossterm29::event::KeyCode::Tab, modifiers)
                            if modifiers == crossterm29::event::KeyModifiers::NONE =>
                        {
                            CycleKey::Navigate
                        }
                        (crossterm29::event::KeyCode::BackTab, _) => CycleKey::Navigate,
                        // niubash#778: plain arrows navigate the grid
                        // (handled above); they must never read as a
                        // session-ending edit — that misclassification
                        // is what killed the session on the first Down.
                        (crossterm29::event::KeyCode::Up, modifiers)
                        | (crossterm29::event::KeyCode::Down, modifiers)
                        | (crossterm29::event::KeyCode::Left, modifiers)
                        | (crossterm29::event::KeyCode::Right, modifiers)
                            if modifiers == crossterm29::event::KeyModifiers::NONE =>
                        {
                            CycleKey::Navigate
                        }
                        (crossterm29::event::KeyCode::Esc, modifiers)
                            if modifiers == crossterm29::event::KeyModifiers::NONE =>
                        {
                            CycleKey::Rollback
                        }
                        (crossterm29::event::KeyCode::Enter, modifiers)
                            if !modifiers.contains(crossterm29::event::KeyModifiers::CONTROL) =>
                        {
                            CycleKey::Accept
                        }
                        _ => CycleKey::EndSession,
                    }
                }
                crossterm29::event::Event::Paste(_) => CycleKey::EndSession,
                _ => CycleKey::Ignore,
            };
            match classification {
                CycleKey::Rollback => {
                    if let Ok(mut state) = session.lock() {
                        if let Some(snapshot) = state.take_rollback() {
                            cycle_rollback =
                                Some(crate::completion::cycle_menu::rollback_edits(&snapshot));
                        }
                    }
                }
                CycleKey::EndSession => {
                    if let Ok(mut state) = session.lock() {
                        state.end_if_open();
                    }
                }
                CycleKey::Navigate | CycleKey::Accept | CycleKey::Ignore => {}
            }
        }
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
        // niubash#631: under the cycle interaction, an Enter inside a
        // live session accepts the candidate already in the line AND
        // runs the command in the same press (DESIGN §7.2) — a
        // `MenuAccept` rides ahead of the submit: the engine's accept
        // closes the menu over the spliced candidate (the CycleMenu's
        // `replace_in_buffer` is a no-op; the text is already there)
        // and the Enter then submits the accepted line. A bare Enter
        // would die in the engine's first-refusal instead (the #486
        // two-Enter shape, which is the *insert* interaction's
        // contract, not cycle's). The gates mirror the GNU wrap above:
        // `submit_carries_enter` + `enter_press` + the TAB window keep
        // the `Multiple` out of Ctrl+R search, and the live-session
        // check keeps every non-completion submit byte-identical
        // (`MenuAccept` is inapplicable without an open menu, but the
        // wrap itself must not exist outside a session).
        if submit_carries_enter && enter_press && self.menu_possible && self.cycle_session_open() {
            self.menu_possible = false;
            let mut events = vec![ReedlineEvent::MenuAccept];
            match translated {
                ReedlineEvent::Enter => events.push(ReedlineEvent::Enter),
                ReedlineEvent::Multiple(rest) => events.extend(rest),
                _ => unreachable!("checked above"),
            }
            return ReedlineEvent::Multiple(events);
        }
        // niubash#631: an Esc that consumed a spliced cycle session
        // carries the rollback edits ahead of its own translation, so
        // the pre-completion line is restored in the same engine batch
        // that closes the menu, restore first.
        if let Some(edits) = cycle_rollback {
            let mut events = vec![ReedlineEvent::Edit(edits)];
            match translated {
                ReedlineEvent::Multiple(rest) => events.extend(rest),
                other => events.push(other),
            }
            return ReedlineEvent::Multiple(events);
        }
        translated
    }
}

/// A bind entry the editor cannot mirror, classified by *why*
/// (niubash#612). Every unusable entry is recorded in this list — it
/// is the bridge's diagnostic record — and, since niubash#630, also
/// lands as a persistent record in the unified keymap registry
/// (see `crate::keymap_registry`), whose reminder pass decides what
/// is surfaced at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnusableBind {
    keyseq: String,
    reason: UnusableBindReason,
}

/// Why a bind entry cannot be mirrored. The first two classes are
/// structural: the editor cannot express them *by design*, so the
/// bridge drops them silently whoever emitted them — a per-startup
/// warning about a design limitation is unactionable noise, and
/// matching emitters or individual binds instead of the mechanism
/// would never converge (any integration script can register such
/// binds; fzf's `--bash` output, which registers one of each class,
/// is the motivating instance, not the criterion). Everything else
/// is new, actionable information and keeps the warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnusableBindReason {
    /// A readline editing-mode toggle function: switching keymaps is
    /// a session-level choice in niu (the configured `EditorMode`),
    /// not a per-key widget the bridge can deliver.
    ModeToggleFunction,
    /// Anything else (unparseable sequence, unknown function, ...).
    Other,
}

impl UnusableBindReason {
    /// Retained for the #612 test surface (the production surfacing
    /// decision lives in the registry's reminder pass, niubash#630).
    #[cfg(test)]
    fn is_structural(self) -> bool {
        matches!(self, UnusableBindReason::ModeToggleFunction)
    }
}

/// Readline's editing-mode toggle functions are a closed class (the
/// Bash manual's bindable-command list has exactly these two): each
/// switches the editing keymap, which niu chooses per session, so the
/// bridge can never mirror them — in any keymap, on any sequence,
/// from any source.
fn is_editing_mode_toggle_function(name: &str) -> bool {
    matches!(name, "vi-editing-mode" | "emacs-editing-mode")
}

/// Classify an undeliverable readline function name (niubash#612):
/// editing-mode toggles are structural; any other name (a real
/// function niu does not mirror, a typo, ...) stays actionable.
fn unusable_function_reason(name: &str) -> UnusableBindReason {
    if is_editing_mode_toggle_function(name) {
        UnusableBindReason::ModeToggleFunction
    } else {
        UnusableBindReason::Other
    }
}

/// Record an entry the editor cannot mirror, classified by reason
/// (niubash#612). The entry also lands in the unified keymap
/// registry's dropped records (niubash#630), with the coarse reason
/// refined by payload shape (DESIGN §6.1: a response sequence, an
/// unknown function, an undeliverable sequence shape).
fn note_unusable_bind(
    unusable: &mut Vec<UnusableBind>,
    dropped: &mut Vec<crate::keymap_registry::DroppedBind>,
    entry: &crate::bind_surface::EngineBindEntry,
    reason: UnusableBindReason,
) {
    unusable.push(UnusableBind {
        keyseq: entry.keyseq.clone(),
        reason,
    });
    let structural = match reason {
        UnusableBindReason::ModeToggleFunction => {
            Some(crate::keymap_registry::DroppedReason::ModeToggleFunction)
        }
        UnusableBindReason::Other => None,
    };
    dropped.push(crate::keymap_registry::dropped_bind_for(
        entry,
        crate::keymap_registry::dropped_reason(structural, &entry.keyseq, &entry.kind),
    ));
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
/// insert their translated text. Entries the editor cannot mirror
/// are reported back classified by reason (niubash#612): structural
/// classes (chained sequences, editing-mode toggle functions) are
/// recorded silently; the records feed the keymap registry's
/// reminder pass (niubash#630).
///
/// This coarse view is retained for the #612 test surface; production
/// builds go through [`add_engine_bind_keybindings_recorded`].
#[cfg(test)]
fn add_engine_bind_keybindings(
    keybindings: &mut Keybindings,
    target: NativeKeymapTarget,
    entries: &[crate::bind_surface::EngineBindEntry],
) -> Vec<UnusableBind> {
    add_engine_bind_keybindings_recorded(keybindings, target, entries, 0).0
}

/// [`add_engine_bind_keybindings`], additionally returning the
/// dropped-bind records for the unified keymap registry
/// (niubash#630): every unusable entry, refined by shape, plus
/// key-script macros — mirrored today (their text inserts literally)
/// but recorded as *mirrored with loss*, since the host has no
/// key-script executor (DESIGN §4 R1). Recording a macro does not
/// change its translation here; the translation widening is the K3
/// slice's business.
///
/// `index_base` is where this slice's `bind -x` indices live in the
/// shell's combined resolution list: the engine snapshot resolves at
/// its own positions (base 0); the `[keymap]` user rules resolve
/// right behind it (base = the snapshot's length), so a user rule's
/// trigger can never run an engine entry's command.
fn add_engine_bind_keybindings_recorded(
    keybindings: &mut Keybindings,
    target: NativeKeymapTarget,
    entries: &[crate::bind_surface::EngineBindEntry],
    index_base: usize,
) -> (Vec<UnusableBind>, Vec<crate::keymap_registry::DroppedBind>) {
    let mut unusable = Vec::new();
    let mut dropped = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if !engine_keymap_applies(entry.keymap.as_deref(), target) {
            continue;
        }
        let Some((modifiers, key_code)) = parse_key_sequence(&entry.keyseq) else {
            // A clean chain of key units is a chord: `ChordEditMode`
            // delivers those (niubash#630), so it is neither dropped nor
            // reported. Anything else that fails to parse is malformed
            // and stays actionable (niubash#612).
            if parse_chord_sequence(&entry.keyseq).is_some() {
                continue;
            }
            note_unusable_bind(
                &mut unusable,
                &mut dropped,
                entry,
                UnusableBindReason::Other,
            );
            continue;
        };
        let event = match &entry.kind {
            crate::bind_surface::EngineBindKind::Execute { .. } => {
                ReedlineEvent::ExecuteHostCommand(format!(
                    "{BINDX_HOST_COMMAND_PREFIX}{}",
                    index_base + index
                ))
            }
            crate::bind_surface::EngineBindKind::Macro { text } => {
                if crate::keymap_registry::is_script_macro_text(text) {
                    // K3: the key-script executor — the macro decodes to
                    // the keystrokes it carries and binds
                    // `ReedlineEvent::KeySequence`, reedline's replay
                    // queue; each key then dispatches through the live
                    // keymap exactly like readline replay. A macro that
                    // decodes to no keys (or pure self-insert text, where
                    // the literal path is identical) keeps the literal
                    // insert; one that decodes to nothing at all keeps
                    // its dropped record too.
                    match script_macro_key_sequence(text) {
                        Some(event) => event,
                        None => {
                            dropped.push(crate::keymap_registry::dropped_bind_for(
                                entry,
                                crate::keymap_registry::DroppedReason::ScriptMacro,
                            ));
                            edit_event(EditCommand::InsertString(text.clone()))
                        }
                    }
                } else {
                    edit_event(EditCommand::InsertString(text.clone()))
                }
            }
            // A readline function name the editor cannot deliver is
            // skipped; the sentinel fallthrough of native_widget_event is
            // for zsh-style shell-function widgets, which `bind` to a
            // readline function position has no meaning for.
            crate::bind_surface::EngineBindKind::Function { name } => {
                // GNU `do-lowercase-version` (text.c:1179) is not a
                // command: `_rl_dispatch_subseq` (readline.c:916-922)
                // special-cases it and re-dispatches the pressed key
                // lowercased in the same map. The editor's emacs
                // machine already lowercases every modified character
                // before lookup (emacs.rs `single_key_event`), and the
                // chord layer folds an ESC-prefixed uppercase partner
                // to its lowercase Meta form, so installing a flat
                // binding here would be wrong twice over: parse_key_sequence
                // has already lowercased `\eA` to Alt+a, and a binding
                // there would shadow Meta+a's own command. Skip it;
                // the fallback is the dispatch layer's job.
                if name == "do-lowercase-version" {
                    continue;
                }
                // `quoted-insert` is delivered by the chord layer's
                // quote state (see `collect_quote_sequences`), not by
                // a flat binding: the key itself must never reach the
                // inner machine (niubash#731). Multi-key sequences
                // bound to it never get this far — the chord check
                // above already passed them to the layer.
                if name == "quoted-insert" {
                    continue;
                }
                // `self-insert` means "insert the pressed key's own
                // character": the event depends on the key, which only
                // this layer knows, so it is translated here instead of
                // in the name table (niubash#731). A key with no
                // character of its own keeps the actionable record.
                if name == "self-insert" {
                    match key_literal_text(modifiers, key_code) {
                        Some(text) => edit_event(EditCommand::InsertString(text)),
                        None => {
                            note_unusable_bind(
                                &mut unusable,
                                &mut dropped,
                                entry,
                                UnusableBindReason::Other,
                            );
                            continue;
                        }
                    }
                } else {
                    match native_widget_event(name) {
                        // native_widget_event's sentinel fallthrough means
                        // "unknown readline function": skipped, like GNU's
                        // silent rl_parse_and_bind drop (wt100 probe).
                        Some(ReedlineEvent::ExecuteHostCommand(payload))
                            if payload.starts_with(WIDGET_HOST_COMMAND_PREFIX) =>
                        {
                            note_unusable_bind(
                                &mut unusable,
                                &mut dropped,
                                entry,
                                unusable_function_reason(name),
                            );
                            continue;
                        }
                        Some(event) => event,
                        None => {
                            note_unusable_bind(
                                &mut unusable,
                                &mut dropped,
                                entry,
                                unusable_function_reason(name),
                            );
                            continue;
                        }
                    }
                }
            }
        };
        keybindings.add_binding(modifiers, key_code, event);
    }
    (unusable, dropped)
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

/// Decode an engine macro's stored byte text into reedline key
/// combinations for [`ReedlineEvent::KeySequence`] replay (K3).
///
/// The engine hands the host the post-translation form: `\C-`/`\e`
/// escapes are already raw bytes, so a control byte is a keystroke
/// token and a printable char is a literal insertion key. ESC starts a
/// meta unit — `\e`+char is Alt, `\e`+control is Ctrl-Alt, and a CSI
/// (`\e[..`) or SS3 (`\eO.`) terminal sequence resolves through the
/// same spellings [`parse_key_sequence`] owns. An unresolvable
/// terminal sequence contributes nothing, like an unbound key inside
/// a readline macro.
///
/// Returns `None` only when the text carried no keys at all (the
/// caller then keeps the literal insert and the dropped record);
/// a pure self-insert decode collapses back to the literal insert
/// event, which is identical for every byte it could carry.
fn script_macro_key_sequence(text: &str) -> Option<ReedlineEvent> {
    let kc = |modifier: KeyModifiers, key_code: KeyCode| KeyCombination { modifier, key_code };
    let mut keys: Vec<KeyCombination> = Vec::new();
    let normalized = normalize_macro_escapes(text);
    let mut chars = normalized.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                None => keys.push(kc(KeyModifiers::NONE, KeyCode::Esc)),
                Some(open @ ('[' | 'O')) => {
                    // Reassemble the terminal sequence into the textual
                    // spelling parse_key_sequence owns ("\e[3~", "\eOA").
                    let mut seq = String::from("\\e");
                    seq.push(open);
                    if open == '[' {
                        // CSI: parameter/intermediate bytes until a final
                        // byte in 0x40..=0x7e.
                        while let Some(&next) = chars.peek() {
                            chars.next();
                            seq.push(next);
                            if ('\u{40}'..='\u{7e}').contains(&next) {
                                break;
                            }
                        }
                    } else if let Some(&next) = chars.peek() {
                        chars.next();
                        seq.push(next);
                    }
                    if let Some((modifier, key_code)) = parse_key_sequence(&seq) {
                        keys.push(kc(modifier, key_code));
                    }
                }
                Some(next) if next.is_control() => {
                    if let Some(key_code) = control_byte_key_code(next) {
                        keys.push(kc(KeyModifiers::CONTROL | KeyModifiers::ALT, key_code));
                    }
                }
                Some(next) => keys.push(kc(KeyModifiers::ALT, KeyCode::Char(next))),
            },
            '\u{7f}' => keys.push(kc(KeyModifiers::NONE, KeyCode::Backspace)),
            '\t' => keys.push(kc(KeyModifiers::NONE, KeyCode::Tab)),
            '\n' | '\r' => keys.push(kc(KeyModifiers::NONE, KeyCode::Enter)),
            c if c.is_control() => {
                if let Some(key_code) = control_byte_key_code(c) {
                    keys.push(kc(KeyModifiers::CONTROL, key_code));
                }
            }
            c => keys.push(kc(KeyModifiers::NONE, KeyCode::Char(c))),
        }
    }
    if keys.is_empty() {
        return None;
    }
    if keys
        .iter()
        .all(|key| key.modifier == KeyModifiers::NONE && matches!(key.key_code, KeyCode::Char(_)))
    {
        return Some(edit_event(EditCommand::InsertString(text.to_string())));
    }
    Some(ReedlineEvent::KeySequence(keys))
}

/// Fold the written `\C-x`/`\M-x`/`\e`/`\\` escape spellings in a macro
/// text down to the raw-byte form the engine normally stores. The
/// stored form passes through untouched (it carries no backslashes the
/// caller did not write); the written form arrives via `[keymap].bind`
/// rules and tests. Unknown escapes keep their backslash, like GNU's
/// permissive keyseq reader.
fn normalize_macro_escapes(text: &str) -> String {
    if !text.contains('\\') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            None => out.push('\\'),
            Some(tag @ ('C' | 'M')) if chars.peek() == Some(&'-') => {
                chars.next(); // consume '-'
                match chars.next() {
                    Some(key) => {
                        if tag == 'M' {
                            out.push('\u{1b}');
                            out.push(key);
                        } else {
                            out.push(control_byte_for_letter(key));
                        }
                    }
                    None => {
                        out.push('\\');
                        out.push(tag);
                        out.push('-');
                    }
                }
            }
            Some('e') => out.push('\u{1b}'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('a') => out.push('\u{7}'),
            Some('b') => out.push('\u{8}'),
            Some('d') => out.push('\u{7f}'),
            Some('f') => out.push('\u{c}'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('v') => out.push('\u{b}'),
            Some('x') => {
                // \xHH: two hex digits; fewer keeps the escape literal.
                let hi = chars.next();
                let lo = chars.next();
                match (hi, lo) {
                    (Some(h), Some(l)) if h.is_ascii_hexdigit() && l.is_ascii_hexdigit() => {
                        let byte = u8::from_str_radix(&format!("{h}{l}"), 16).unwrap_or(0);
                        out.push(byte as char);
                    }
                    _ => {
                        out.push('\\');
                        out.push('x');
                        if let Some(h) = hi {
                            out.push(h);
                        }
                        if let Some(l) = lo {
                            out.push(l);
                        }
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

/// The control byte a `\C-` letter names (readline translate_keyseq):
/// letters fold into the low five bits, `@ [ \ ] ^ _` land on their
/// partners, `?` is DEL. Characters outside the table pass through so
/// a stray `\C-(` stays visible instead of silently vanishing.
fn control_byte_for_letter(key: char) -> char {
    match key {
        '@' => '\u{0}',
        'a'..='z' | 'A'..='Z' => char::from_u32((key as u32) & 0x1f).unwrap_or(key),
        '[' => '\u{1b}',
        '\\' => '\u{1c}',
        ']' => '\u{1d}',
        '^' => '\u{1e}',
        '_' => '\u{1f}',
        '?' => '\u{7f}',
        other => other,
    }
}

/// The `KeyCode` a raw control byte names, minus the exceptions the
/// caller maps first (ESC, DEL, Tab, Enter): C-@ is space, C-\ through
/// C-_ are their punctuation partners, and the rest are the letter
/// block. Returns `None` for bytes with no crossterm spelling.
fn control_byte_key_code(byte: char) -> Option<KeyCode> {
    match byte as u32 {
        0 => Some(KeyCode::Char(' ')),
        0x01..=0x1a => char::from_u32(byte as u32 + 0x60).map(KeyCode::Char),
        0x1c => Some(KeyCode::Char('\\')),
        0x1d => Some(KeyCode::Char(']')),
        0x1e => Some(KeyCode::Char('^')),
        0x1f => Some(KeyCode::Char('_')),
        _ => None,
    }
}

fn add_menu_keybindings(
    keybindings: &mut Keybindings,
    completion_interaction: CompletionInteraction,
) {
    // niubash#631: TAB's shape follows the interaction axis. Under
    // `insert` it is the pre-#631 binding: open the menu, falling back
    // to the engine's own `Complete` edit when no menu takes it. Under
    // `cycle`, TAB is the cycle key: while the CycleMenu is open it is
    // `MenuNext` (advance + splice the next candidate); with no menu
    // open `MenuNext` is inapplicable and the `UntilFound` falls
    // through to opening the menu, whose activation splices the first
    // candidate. The `Edit(Complete)` fallback is insert-only — under
    // cycle it would splice the common prefix behind the menu's back
    // and desync the frozen candidate set.
    let tab_event = match completion_interaction {
        CompletionInteraction::Insert => ReedlineEvent::UntilFound(vec![
            ReedlineEvent::Menu(COMPLETION_MENU.to_string()),
            ReedlineEvent::Edit(vec![EditCommand::Complete]),
        ]),
        CompletionInteraction::Cycle | CompletionInteraction::Select => {
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::MenuNext,
                ReedlineEvent::Menu(COMPLETION_MENU.to_string()),
            ])
        }
    };
    keybindings.add_binding(KeyModifiers::NONE, KeyCode::Tab, tab_event);
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
pub(crate) enum NativeKeymapTarget {
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
///
/// The batch form is retained for the test surface; the registry
/// build applies bindings one by one through
/// [`apply_bundle_widget_binding`] for per-binding attribution.
#[cfg(test)]
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
        apply_bundle_widget_binding(keybindings, target, binding);
    }
}

/// Apply one bundle-declared widget binding (the per-binding form of
/// [`add_bundle_widget_keybindings`], so the keymap registry build
/// can attribute each slot to its declaring file and line).
fn apply_bundle_widget_binding(
    keybindings: &mut Keybindings,
    target: NativeKeymapTarget,
    binding: &NativeWidgetBinding,
) {
    if !native_widget_keymap_applies(binding.keymap.as_deref(), target) {
        return;
    }
    let Some(key) = binding.key.as_deref().and_then(parse_key_sequence) else {
        return;
    };
    let Some(event) = native_widget_event(&binding.widget) else {
        return;
    };
    keybindings.add_binding(key.0, key.1, event);
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

/// The literal text a key itself carries, for the readline functions
/// whose meaning is the pressed key's own character: `self-insert`
/// (the bound key) and `quoted-insert` (the key pressed after the
/// quoting key, niubash#731). A control combination carries its
/// control byte; a meta combination carries the character itself
/// (the ESC is the prefix mechanism, not data). Keys with no
/// character of their own (arrows, function keys, ...) carry nothing.
fn key_literal_text(modifiers: KeyModifiers, code: KeyCode) -> Option<String> {
    match code {
        KeyCode::Char(c) => {
            if modifiers.contains(KeyModifiers::CONTROL) {
                control_char(c).map(|ch| ch.to_string())
            } else {
                Some(c.to_string())
            }
        }
        KeyCode::Enter => Some("\n".to_string()),
        KeyCode::Tab => Some("\t".to_string()),
        KeyCode::Backspace => Some("\u{7f}".to_string()),
        KeyCode::Esc => Some("\u{1b}".to_string()),
        _ => None,
    }
}

/// The control byte a Ctrl+key combination names: letters fold to
/// 0x01–0x1a, and the symbol row keeps its ASCII control names
/// (C-@ / C-Space is NUL, C-? is DEL, ...).
fn control_char(c: char) -> Option<char> {
    match c {
        'a'..='z' => char::from_u32(c as u32 - 'a' as u32 + 1),
        'A'..='Z' => char::from_u32(c as u32 - 'A' as u32 + 1),
        '@' | ' ' => Some('\0'),
        '[' => Some('\u{1b}'),
        '\\' => Some('\u{1c}'),
        ']' => Some('\u{1d}'),
        '^' => Some('\u{1e}'),
        '_' => Some('\u{1f}'),
        '?' => Some('\u{7f}'),
        _ => None,
    }
}

fn native_widget_event(widget: &str) -> Option<ReedlineEvent> {
    match widget {
        "autosuggest-accept" => Some(ReedlineEvent::HistoryHintComplete),
        "autosuggest-execute" => Some(ReedlineEvent::Multiple(vec![
            ReedlineEvent::HistoryHintComplete,
            ReedlineEvent::Enter,
        ])),
        "autosuggest-partial-accept" => Some(ReedlineEvent::HistoryHintWordComplete),
        // GNU's own spellings of the substring-search pair
        // (`history-substring-search-backward`/`-forward`, funmap.c)
        // ride the same arms as the zsh spellings (niubash#731).
        "history-substring-search-up"
        | "history-substring-search-backward"
        | "history-search-backward"
        | "up-line-or-history" => Some(ReedlineEvent::Up),
        "history-substring-search-down"
        | "history-substring-search-forward"
        | "history-search-forward"
        | "down-line-or-history" => Some(ReedlineEvent::Down),
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
        // GNU readline names for the two word-case transforms and the
        // transpose. The editor delivers all three (emacs Ctrl+T is
        // `SwapGraphemes` in reedline's own table), so a user re-binding
        // them in an rc file must mirror instead of being reported as
        // "not in the host's deliverable set" (niubash#630 gap list).
        "upcase-word" => Some(edit_event(EditCommand::UppercaseWord)),
        "downcase-word" => Some(edit_event(EditCommand::LowercaseWord)),
        // The third word-case transform and the word transpose: the
        // editor carries both edits (`CapitalizeWord`, `SwapWords`),
        // so a bind of the GNU names mirrors like its siblings
        // (niubash#731).
        "capitalize-word" => Some(edit_event(EditCommand::CapitalizeWord)),
        "transpose-chars" => Some(edit_event(EditCommand::SwapGraphemes)),
        "transpose-words" => Some(edit_event(EditCommand::SwapWords)),
        // GNU `unix-word-rubout` (Ctrl+W) is the same "kill the word
        // before the cursor into the cut buffer" gesture as
        // `backward-kill-word`; readline keeps both names.
        "unix-word-rubout" => Some(edit_event(EditCommand::CutWordLeft)),
        // `previous-history` / `next-history` are GNU's one-step
        // history moves — the same shape as the `*-line-or-history`
        // aliases above.
        "previous-history" => Some(ReedlineEvent::Up),
        "next-history" => Some(ReedlineEvent::Down),
        // GNU's Ctrl+R (`reverse-search-history`) and the interactive
        // search spelling niu already carried are one action.
        "reverse-search-history" => Some(ReedlineEvent::SearchHistory),
        // PgUp/PgDn in bash's default map: history jumps (the reedline
        // fork pinned in Cargo.toml carries these two events).
        "beginning-of-history" => Some(ReedlineEvent::FirstHistoryEntry),
        "end-of-history" => Some(ReedlineEvent::LastHistoryEntry),
        // GNU's editing-mode toggles: session-level mode switches, which
        // reedline delivers as `SwitchMode` (the standby machine built in
        // `build_line_editor` is what accepts the target).
        "vi-editing-mode" => Some(ReedlineEvent::SwitchMode(PromptEditMode::Vi(
            PromptViMode::Insert,
        ))),
        "emacs-editing-mode" => Some(ReedlineEvent::SwitchMode(PromptEditMode::Emacs)),
        // `complete` is readline's "complete as TAB would"; the host's
        // completion event is exactly that (menu first, then the
        // editor's own completion).
        "complete" => Some(completion_event()),
        // `menu-complete` is GNU's cycling completion: the same event
        // (the menu cycles under the host's interaction rules,
        // niubash#731). The backward/old variants stay undelivered —
        // the editor has no open-the-menu-backward event.
        "menu-complete" => Some(completion_event()),
        // GNU's listing / bulk-insert completion functions (complete.c
        // `rl_possible_completions`, `rl_insert_completions`).
        "possible-completions" => Some(ReedlineEvent::PossibleCompletions),
        "insert-completions" => Some(ReedlineEvent::InsertCompletions),
        "kill-word" => Some(edit_event(EditCommand::CutWordRight)),
        "kill-line" => Some(edit_event(EditCommand::CutToLineEnd)),
        "backward-kill-line" | "unix-line-discard" => {
            Some(edit_event(EditCommand::CutFromLineStart))
        }
        "kill-whole-line" => Some(edit_event(EditCommand::CutCurrentLine)),
        "yank" => Some(edit_event(EditCommand::PasteCutBufferBefore)),
        "undo" => Some(edit_event(EditCommand::Undo)),
        "suspend" => Some(ReedlineEvent::Suspend),
        "redo" => Some(edit_event(EditCommand::Redo)),
        "clear-screen" => Some(ReedlineEvent::ClearScreen),
        // GNU `clear-display` clears the screen AND the scrollback,
        // then redraws (text.c `rl_clear_display` calls
        // `_rl_clear_screen (1)`): the editor's `ClearScrollback` is
        // that shape, where `clear-screen` above is the milder one
        // (niubash#731).
        "clear-display" => Some(ReedlineEvent::ClearScrollback),
        "redisplay" | "redraw-current-line" => Some(ReedlineEvent::Repaint),
        "expand-or-complete" | "complete-word" => Some(completion_event()),
        "menu-previous" => Some(ReedlineEvent::MenuPrevious),
        "history-incremental-search-backward" => Some(ReedlineEvent::SearchHistory),
        // GNU kill-ring and word-transform functions the fork's
        // `EditCommand` set delivers directly.
        "yank-pop" => Some(edit_event(EditCommand::YankPop)),
        "revert-line" => Some(edit_event(EditCommand::RevertLine)),
        "delete-horizontal-space" => Some(edit_event(EditCommand::DeleteHorizontalSpace)),
        // GNU keeps the emacs and vi spellings for the same
        // "delete back to the last path separator" gesture.
        "unix-filename-rubout" | "vi-filename-rubout" => {
            Some(edit_event(EditCommand::CutFilenameLeft))
        }
        "exchange-point-and-mark" => Some(edit_event(EditCommand::SwapCursorAndAnchor)),
        // GNU `set-mark` (C-@ / M-SPC): drop the mark at point.
        "set-mark" => Some(edit_event(EditCommand::SetMark)),
        // Verbatim next-key insertion (`rl_quoted_insert`).
        "quoted-insert" => Some(ReedlineEvent::QuotedInsert),
        // Accept the line and preload the following history entry
        // (`rl_operate_and_get_next`, C-o).
        "operate-and-get-next" => Some(ReedlineEvent::OperateAndGetNext),
        // Insert-key toggle; `None` flips, matching GNU's no-argument
        // `rl_overwrite_mode`.
        "overwrite-mode" => Some(ReedlineEvent::OverwriteMode(None)),
        // GNU keyboard-macro trio (C-x ( / C-x ) / C-x e).
        "start-kbd-macro" => Some(ReedlineEvent::StartKbdMacro),
        "end-kbd-macro" => Some(ReedlineEvent::EndKbdMacro),
        "call-last-kbd-macro" => Some(ReedlineEvent::CallLastKbdMacro),
        // GNU `character-search` (C-]) / `character-search-backward`
        // (M-C-]): the next key is the search target.
        "character-search" => Some(ReedlineEvent::CharSearch {
            direction: Direction::Forward,
        }),
        "character-search-backward" => Some(ReedlineEvent::CharSearch {
            direction: Direction::Backward,
        }),
        // GNU non-incremental history search (M-n/M-p): collect a
        // string in the minibuffer, then search newer/older entries.
        "non-incremental-forward-search-history" => Some(ReedlineEvent::NonIncrementalSearch {
            direction: Direction::Forward,
            prompt_char: ':',
            pattern: false,
        }),
        "non-incremental-reverse-search-history" => Some(ReedlineEvent::NonIncrementalSearch {
            direction: Direction::Backward,
            prompt_char: ':',
            pattern: false,
        }),
        // GNU `yank-last-arg` (M-.): `None` is GNU's `'$'` — the last
        // argument. `yank-nth-arg` defaults to argument 1; explicit
        // counts reach the editor through `WithArg`.
        "yank-last-arg" | "insert-last-argument" => {
            Some(ReedlineEvent::YankLastArg { count: None })
        }
        "yank-nth-arg" => Some(ReedlineEvent::YankNthArg { count: 1 }),
        // Numeric-argument accumulation (M-0..M-9 / M-- / C-u). `None`
        // is GNU's `M--` negate.
        "digit-argument" => Some(ReedlineEvent::DigitArgument(None)),
        "universal-argument" => Some(ReedlineEvent::UniversalArgument),
        // GNU `edit-and-execute-command` (C-x C-e): open the line in
        // $VISUAL/$EDITOR.
        "edit-and-execute-command" => Some(ReedlineEvent::OpenEditor),
        // GNU `insert-comment` (text.c `rl_insert_comment`, no
        // explicit argument — the only shape reachable without
        // readline's prefix-argument machinery): move to the line
        // start, insert the comment-begin string (GNU's default is
        // `#`, rlconf.h `RL_COMMENT_BEGIN_DEFAULT`), and accept the
        // line. A customized comment-begin is not consulted, the same
        // static-arm limit as the rest of this table (niubash#731).
        "insert-comment" => Some(ReedlineEvent::Multiple(vec![
            ReedlineEvent::Edit(vec![
                EditCommand::MoveToLineStart { select: false },
                EditCommand::InsertString("#".to_string()),
            ]),
            ReedlineEvent::Enter,
        ])),
        // GNU `tab-insert` inserts a literal tab where TAB itself is
        // the completion key (niubash#731).
        "tab-insert" => Some(edit_event(EditCommand::InsertChar('\t'))),
        // The host-implemented readline commands (niubash#765):
        // dump-functions / dump-macros / dump-variables print editor
        // and registry state (bind.c rl_dump_functions /
        // rl_dump_macros / rl_dump_variables) and re-read-init-file
        // re-reads the rc's bind state (bind.c
        // rl_re_read_init_file). The pinned editor has no events
        // for them, so they ride the readline host-command channel
        // and `run_readline_command` performs them against the
        // suspended editor — never the shell-function sentinel.
        "dump-functions" | "dump-macros" | "dump-variables" | "re-read-init-file" => {
            Some(ReedlineEvent::ExecuteHostCommand(format!(
                "{}{}",
                READLINE_HOST_COMMAND_PREFIX, widget
            )))
        }
        // niubash#766: tilde-expand (util.c rl_tilde_expand) edits
        // the word at point and execute-named-command (text.c
        // rl_execute_named_command) reads a name in the minibuffer —
        // both are host work against the suspended editor, so they
        // ride the same readline host-command channel.
        // insert-comment needs no arm here: it shipped as a host
        // composite in the #731 line.
        "tilde-expand" | "execute-named-command" => Some(ReedlineEvent::ExecuteHostCommand(
            format!("{}{}", READLINE_HOST_COMMAND_PREFIX, widget),
        )),
        // niubash#767: shell-expand-line (bashline.c
        // shell_expand_line) expands the line through the engine
        // and dynamic-complete-history (bashline.c
        // dynamic_complete_history) completes from history tokens —
        // both need the session and the editor's history, so they
        // ride the same readline host-command channel.
        "shell-expand-line" | "dynamic-complete-history" => {
            Some(ReedlineEvent::ExecuteHostCommand(format!(
                "{}{}",
                READLINE_HOST_COMMAND_PREFIX, widget
            )))
        }
        // GNU `abort` (util.c _rl_abort_internal): clear the pending
        // command and ding without abandoning the line — host work
        // against the suspended editor. `history-expand-line`
        // (bashline.c history_expand_line) expands only the history
        // stage of the line. The context-specific completion entry
        // points (bashline.c bash_complete_command /
        // bash_complete_variable / bash_complete_filename /
        // bash_complete_hostname / bash_complete_username /
        // bash_brace_completion) pin the candidate source while reusing
        // the host completion engine, and glob-complete-word
        // (bashline.c:4068) uses the engine's pathname expansion through
        // Session::glob_filename. All ride the readline host-command
        // channel.
        "abort"
        | "history-expand-line"
        | "complete-command"
        | "complete-variable"
        | "complete-filename"
        | "complete-hostname"
        | "complete-username"
        | "complete-into-braces"
        | "glob-complete-word" => Some(ReedlineEvent::ExecuteHostCommand(format!(
            "{}{}",
            READLINE_HOST_COMMAND_PREFIX, widget
        ))),
        // Unknown widget names become shell-function widgets: the host
        // intercepts the sentinel payload, runs the named function with the
        // editor state, and applies its outcome before resuming the editor.
        _ => Some(ReedlineEvent::ExecuteHostCommand(format!(
            "{}{}",
            WIDGET_HOST_COMMAND_PREFIX, widget
        ))),
    }
}

/// Whether a GNU readline name is actually delivered by the host
/// (a native editor event, a host composite, or a readline
/// host-command) rather than falling through to the shell-function
/// widget sentinel. Only delivered names earn their GNU default
/// bindings from [`crate::readline_commands::GNU_DEFAULT_COMMAND_BINDINGS`].
fn gnu_name_is_delivered(name: &str) -> bool {
    match native_widget_event(name) {
        Some(ReedlineEvent::ExecuteHostCommand(payload)) => {
            !payload.starts_with(WIDGET_HOST_COMMAND_PREFIX)
        }
        Some(_) => true,
        None => false,
    }
}

/// The one-key entries of the GNU default command bindings
/// (niubash#765/#766/#767), applied in the host baseline of the
/// emacs keymap — GNU's defaults live in the emacs keymap; the vi
/// keymaps carry their own spellings. Multi-key entries ride the
/// chord layer instead (see [`gnu_default_chord_bindings`]).
fn add_gnu_command_default_keybindings(keybindings: &mut Keybindings, target: NativeKeymapTarget) {
    if target != NativeKeymapTarget::Emacs {
        return;
    }
    for (keyseq, name) in crate::readline_commands::GNU_DEFAULT_COMMAND_BINDINGS {
        if !gnu_name_is_delivered(name) {
            continue;
        }
        // Single keys only; `parse_key_sequence` returns None for
        // multi-key spellings (those ride the chord layer).
        let Some((modifiers, code)) = parse_key_sequence(keyseq) else {
            continue;
        };
        if let Some(event) = native_widget_event(name) {
            keybindings.add_binding(modifiers, code, event.clone());
            // Terminals encode Meta-TAB as ESC TAB, not ESC Ctrl-I:
            // the `\M-\C-i` spelling (CONTROL|ALT + 'i') also answers
            // to ALT+Tab, the byte shape that actually arrives.
            if (modifiers, code)
                == (
                    KeyModifiers::CONTROL | KeyModifiers::ALT,
                    KeyCode::Char('i'),
                )
            {
                keybindings.add_binding(KeyModifiers::ALT, KeyCode::Tab, event);
            }
        }
    }
}

/// The multi-key entries of the GNU default command bindings
/// (C-x C-r for re-read-init-file), joining the engine chord
/// bindings in the chord layer.
fn gnu_default_chord_bindings() -> Vec<ChordBinding> {
    let mut chords: Vec<ChordBinding> = crate::readline_commands::GNU_DEFAULT_COMMAND_BINDINGS
        .iter()
        .filter_map(|(keyseq, name)| {
            let keys = parse_chord_sequence(keyseq)?;
            if keys.len() < 2 || !gnu_name_is_delivered(name) {
                return None;
            }
            native_widget_event(name).map(|event| ChordBinding { keys, event })
        })
        .collect();
    // GNU emacs_ctlx_keymap: the keyboard-macro trio, C-x C-u undo and
    // C-x C-x exchange-point-and-mark. The native Emacs machine resolves
    // these through `pending_prefix`, but niu's ChordEditMode sits in
    // front of it and claims C-x as a chord prefix; without explicit
    // entries the C-x half is consumed and the macro commands never run.
    for (key, event) in [
        ('(', ReedlineEvent::StartKbdMacro),
        (')', ReedlineEvent::EndKbdMacro),
        ('e', ReedlineEvent::CallLastKbdMacro),
        ('u', edit_event(EditCommand::Undo)),
        ('x', edit_event(EditCommand::SwapCursorAndAnchor)),
    ] {
        chords.push(ChordBinding {
            keys: vec![
                (KeyModifiers::CONTROL, KeyCode::Char('x')),
                (KeyModifiers::NONE, KeyCode::Char(key)),
            ],
            event,
        });
    }
    chords
}

/// One multi-key binding: the key units and the event the last unit fires.
struct ChordBinding {
    keys: Vec<(KeyModifiers, KeyCode)>,
    event: ReedlineEvent,
}

/// Lookup index over [`ChordBinding`]s.
///
/// Chords are few (a handful of `bind` sequences in a typical rc), so this
/// is deliberately flat rather than a trie: a hash lookup per keypress
/// replaces the old linear `exact()`/`is_prefix()` scans while keeping the
/// exact-match and prefix sets explicit and easy to audit. The maps are
/// built once at editor build, where the old code scanned on every key.
#[derive(Default)]
struct ChordIndex {
    /// Full sequences. Duplicates keep the FIRST binding, matching the old
    /// `iter().find()` priority.
    exact: HashMap<Vec<(KeyModifiers, KeyCode)>, ReedlineEvent>,
    /// Every proper prefix of every chord. A full sequence that is also a
    /// prefix of a longer chord belongs here too (readline waits for the
    /// longer binding).
    prefixes: HashSet<Vec<(KeyModifiers, KeyCode)>>,
}

impl ChordIndex {
    fn new(chords: &[ChordBinding]) -> Self {
        let mut index = Self::default();
        if !chords.is_empty() {
            // The old `is_prefix(&[])` scan returned true for any non-empty
            // chord set; keep that edge case exact.
            index.prefixes.insert(Vec::new());
        }
        for chord in chords {
            for len in 1..chord.keys.len() {
                index.prefixes.insert(chord.keys[..len].to_vec());
            }
            index
                .exact
                .entry(chord.keys.clone())
                .or_insert_with(|| chord.event.clone());
        }
        index
    }

    fn exact(&self, keys: &[(KeyModifiers, KeyCode)]) -> Option<ReedlineEvent> {
        self.exact.get(keys).cloned()
    }

    fn is_prefix(&self, keys: &[(KeyModifiers, KeyCode)]) -> bool {
        self.prefixes.contains(keys)
    }
}

/// Collect the engine `bind` entries that are chords (two or more key
/// units) into the layer `ChordEditMode` walks. `Execute` entries keep the
/// same `bind -x` index the flat keymap would have used; macros insert
/// their text; function names go through [`native_widget_event`] and fall
/// back to the shell-function sentinel like any other widget.
/// The key codes that carry a real ALT binding in this keymap — the set the
/// ESC-prefix logic may synthesize a meta key for.
fn alt_bound_keys(keybindings: &Keybindings) -> std::collections::HashSet<KeyCode> {
    keybindings
        .get_keybindings()
        .keys()
        .filter(|combination| combination.modifier.contains(KeyModifiers::ALT))
        .map(|combination| combination.key_code)
        .collect()
}

fn collect_chord_bindings(
    entries: &[crate::bind_surface::EngineBindEntry],
    target: NativeKeymapTarget,
    index_base: usize,
) -> Vec<ChordBinding> {
    let mut chords = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if !engine_keymap_applies(entry.keymap.as_deref(), target) {
            continue;
        }
        let Some(keys) = parse_chord_sequence(&entry.keyseq) else {
            continue;
        };
        let event = match &entry.kind {
            crate::bind_surface::EngineBindKind::Execute { .. } => {
                ReedlineEvent::ExecuteHostCommand(format!(
                    "{BINDX_HOST_COMMAND_PREFIX}{}",
                    index_base + index
                ))
            }
            crate::bind_surface::EngineBindKind::Macro { text } => {
                edit_event(EditCommand::InsertString(text.clone()))
            }
            crate::bind_surface::EngineBindKind::Function { name } => {
                // `quoted-insert` sequences arm the chord layer's
                // quote state; they are not chord events (see
                // `collect_quote_sequences`, niubash#731).
                if name == "quoted-insert" {
                    continue;
                }
                // A chord ending in `self-insert` inserts the chord's
                // last key — the same key-dependent translation as the
                // flat path (niubash#731).
                if name == "self-insert" {
                    match keys
                        .last()
                        .and_then(|(mods, code)| key_literal_text(*mods, *code))
                    {
                        Some(text) => edit_event(EditCommand::InsertString(text)),
                        None => continue,
                    }
                } else {
                    match native_widget_event(name) {
                        Some(ReedlineEvent::ExecuteHostCommand(payload))
                            if payload.starts_with(WIDGET_HOST_COMMAND_PREFIX) =>
                        {
                            continue
                        }
                        Some(event) => event,
                        None => continue,
                    }
                }
            }
        };
        chords.push(ChordBinding { keys, event });
    }
    chords
}

/// Collect the key sequences bound to GNU `quoted-insert` for the
/// chord layer's quote state (niubash#731): any parseable sequence
/// counts — a single key (`"\C-v"`) or a chain — filtered by the
/// same keymap targeting as every other engine-bind consumer. The
/// pinned reedline fork has no quoted-insert event of its own (no
/// such variant in its `ReedlineEvent`/`EditCommand` at rev
/// 401393f), so the delivery is the host layer's: completing one of
/// these sequences arms the quote, and the next key inserts its own
/// character literally.
fn collect_quote_sequences(
    entries: &[crate::bind_surface::EngineBindEntry],
    target: NativeKeymapTarget,
) -> Vec<Vec<(KeyModifiers, KeyCode)>> {
    let mut sequences = Vec::new();
    for entry in entries {
        if !engine_keymap_applies(entry.keymap.as_deref(), target) {
            continue;
        }
        let crate::bind_surface::EngineBindKind::Function { name } = &entry.kind else {
            continue;
        };
        if name != "quoted-insert" {
            continue;
        }
        let keys = parse_chord_sequence(&entry.keyseq)
            .or_else(|| parse_key_sequence(&entry.keyseq).map(|single| vec![single]));
        if let Some(keys) = keys {
            sequences.push(keys);
        }
    }
    sequences
}

/// Multi-key (chord) dispatch, layered above the paste/typeahead machine.
///
/// reedline's flat keymap binds one key at a time, so a readline chord
/// (`\C-x\C-e`, fzf's `\C-\e(`) used to be dropped. This layer buffers the
/// first unit only when some chord starts with it, completes on an exact
/// match, and — on a mismatch — replays the buffered keys through the inner
/// machine before the current one, so nothing is swallowed. A lone prefix
/// waits for the next key up to `keymap.keyseq-timeout` (readline's
/// `keyseq-timeout`: 500 ms by default, `0` waits forever; the upstream
/// chord design is nushell/reedline#1204).
struct ChordEditMode {
    inner: Box<dyn EditMode>,
    chords: ChordIndex,
    /// Sequences bound to GNU `quoted-insert`, indexed like the
    /// chords; completing one ARMS the quote state below instead of
    /// firing an event (the stored event is a dummy, never
    /// returned). Empty when nothing binds `quoted-insert` — the
    /// layer is then exactly the pre-#731 machine (niubash#731).
    quotes: ChordIndex,
    /// Armed by a completed quote sequence: the next key inserts
    /// its own character literally, bypassing every binding (GNU
    /// `rl_quoted_insert`). No expiry: GNU waits as long as the
    /// user takes to press the quoted key.
    quote_armed: bool,
    /// The configured chord wait. `Duration::ZERO` is readline's `0`:
    /// wait forever (no expiry).
    keyseq_timeout: Duration,
    pending: Vec<crossterm29::event::Event>,
    /// When the latest pending unit was parked: the keyseq-timeout
    /// deadline restarts at each key (GNU's per-key wait,
    /// niubash#731), so this is the last key's arrival, not the
    /// first's. A prefix that does not complete inside the window
    /// stops being a chord prefix.
    pending_since: Option<std::time::Instant>,
    /// Keys that carry a real ALT binding: terminals spell Alt-C as
    /// `ESC c`, and emacs-mode readline treats ESC as the meta prefix.
    alt_keys: std::collections::HashSet<KeyCode>,
    /// Peek for the ESC partner without consuming the input stream.
    lookahead: Option<Lookahead>,
}

/// How long to wait for the ESC partner. A terminal sends `ESC c` back to
/// back; a human pressing Escape alone does not.
const ESC_META_BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

impl ChordEditMode {
    fn new(
        inner: Box<dyn EditMode>,
        chords: Vec<ChordBinding>,
        alt_keys: std::collections::HashSet<KeyCode>,
        lookahead: Option<Lookahead>,
        keyseq_timeout: Duration,
    ) -> Self {
        Self {
            inner,
            chords: ChordIndex::new(&chords),
            quotes: ChordIndex::default(),
            quote_armed: false,
            keyseq_timeout,
            pending: Vec::new(),
            pending_since: None,
            alt_keys,
            lookahead,
        }
    }

    /// Register the sequences that arm the quote state (see
    /// `collect_quote_sequences`). Builder form so the many test
    /// constructions of this layer keep their shape; with no call,
    /// the quote layer is empty and inert.
    fn with_quote_sequences(mut self, sequences: Vec<Vec<(KeyModifiers, KeyCode)>>) -> Self {
        self.quotes = ChordIndex::new(
            &sequences
                .into_iter()
                .map(|keys| ChordBinding {
                    keys,
                    event: ReedlineEvent::None,
                })
                .collect::<Vec<_>>(),
        );
        self
    }

    fn exact(&self, keys: &[(KeyModifiers, KeyCode)]) -> Option<ReedlineEvent> {
        self.chords.exact(keys)
    }

    fn is_prefix(&self, keys: &[(KeyModifiers, KeyCode)]) -> bool {
        self.chords.is_prefix(keys)
    }

    fn quote_exact(&self, keys: &[(KeyModifiers, KeyCode)]) -> bool {
        self.quotes.exact(keys).is_some()
    }

    fn quote_is_prefix(&self, keys: &[(KeyModifiers, KeyCode)]) -> bool {
        self.quotes.is_prefix(keys)
    }

    fn forward(&mut self, event: ReedlineRawEvent) -> ReedlineEvent {
        self.inner.parse_event(event)
    }
}

fn chord_key(event: &crossterm29::event::Event) -> Option<(KeyModifiers, KeyCode)> {
    match event {
        crossterm29::event::Event::Key(key)
            if key.kind == crossterm29::event::KeyEventKind::Press =>
        {
            Some((key.modifiers, key.code))
        }
        _ => None,
    }
}

/// GNU's `rl_do_lowercase_version` dispatch rule (readline.c:916-922):
/// when the pressed Meta key is an uppercase letter whose entry is
/// `do-lowercase-version`, readline re-dispatches the *lowercase* key
/// in the same map. The pinned editor lowercases modified characters
/// before looking them up, but a terminal spells Meta as `ESC` + the
/// character, so an uppercase partner (`ESC B`, i.e. Meta-B) reaches
/// this layer as `(NONE, 'B')` and would otherwise be delivered as a
/// literal `B`. Folding it to the ALT-bound lowercase key here keeps
/// Meta-A…Meta-Z meaningful. A letter with no ALT-bound lowercase form
/// stays an ordinary ESC + letter (GNU dings for the unbound map).
fn meta_fallback_key(
    code: KeyCode,
    alt_keys: &std::collections::HashSet<KeyCode>,
) -> Option<KeyCode> {
    let candidate = match code {
        KeyCode::Char(c) if c.is_ascii_uppercase() => KeyCode::Char(c.to_ascii_lowercase()),
        other => other,
    };
    alt_keys.contains(&candidate).then_some(candidate)
}

fn push_event(events: &mut Vec<ReedlineEvent>, event: ReedlineEvent) {
    match event {
        ReedlineEvent::None => {}
        ReedlineEvent::Multiple(inner) => {
            for event in inner {
                push_event(events, event);
            }
        }
        other => events.push(other),
    }
}

impl EditMode for ChordEditMode {
    fn parse_event(&mut self, event: ReedlineRawEvent) -> ReedlineEvent {
        self.dispatch(crossterm29::event::Event::from(event))
    }

    fn edit_mode(&self) -> PromptEditMode {
        self.inner.edit_mode()
    }

    fn handle_mode_specific_event(&mut self, event: ReedlineEvent) -> reedline::EventStatus {
        self.inner.handle_mode_specific_event(event)
    }
}

impl ChordEditMode {
    fn dispatch(&mut self, raw: crossterm29::event::Event) -> ReedlineEvent {
        let Some(key) = chord_key(&raw) else {
            // Non-key traffic (key releases, focus flicker, resize): the
            // lookahead can pull one in as an Esc partner and hand it
            // straight here — an unconvertible event must be inert, not
            // a panic (niubash#742's select-Esc drive exposed this).
            return match ReedlineRawEvent::try_from(raw) {
                Ok(raw) => self.forward(raw),
                Err(_) => ReedlineEvent::None,
            };
        };
        // An armed quote consumes the key as data before anything
        // else can claim it — including the meta fold below, so a
        // quoted ESC inserts the ESC byte itself (niubash#731). A
        // key with no character of its own is simply spent (GNU
        // rings the bell and inserts nothing).
        if self.quote_armed {
            self.quote_armed = false;
            return match key_literal_text(key.0, key.1) {
                Some(text) => edit_event(EditCommand::InsertString(text)),
                None => ReedlineEvent::None,
            };
        }
        // ESC as readline's meta prefix (emacs mode only: vi uses ESC to
        // leave insert mode). Only when the peeked partner actually has an
        // ALT binding, so a bare Escape keeps its immediate meaning.
        if key == (KeyModifiers::NONE, KeyCode::Esc)
            && self.inner.edit_mode() == PromptEditMode::Emacs
            && !self.alt_keys.is_empty()
        {
            if let Some(partner) = self
                .lookahead
                .as_mut()
                .and_then(|peek| peek(ESC_META_BUDGET))
            {
                if let Some(code) = chord_key(&partner)
                    .and_then(|(_, code)| meta_fallback_key(code, &self.alt_keys))
                {
                    let synthetic = crossterm29::event::Event::Key(
                        crossterm29::event::KeyEvent::new(code, KeyModifiers::ALT),
                    );
                    return self.forward(
                        ReedlineRawEvent::try_from(synthetic).expect("synthetic meta key"),
                    );
                }
                // The peeked event is not a meta partner: deliver ESC and it
                // in order, now. Parking it would delay the key until the
                // *next* keystroke (reedline only calls us when input
                // arrives).
                let esc = self.forward(ReedlineRawEvent::try_from(raw).expect("escape"));
                let partner = self.dispatch(partner);
                let mut events = Vec::new();
                push_event(&mut events, esc);
                push_event(&mut events, partner);
                return match events.len() {
                    0 => ReedlineEvent::None,
                    1 => events.pop().expect("one event"),
                    _ => ReedlineEvent::Multiple(events),
                };
            }
            return self.forward(ReedlineRawEvent::try_from(raw).expect("escape"));
        }
        if self.pending.is_empty() {
            // A lone quote key arms the quote state at once — unless
            // the key also starts a longer sequence, where the
            // both-binding-and-prefix rule below (wait for the chord)
            // wins, as it does for any other single binding
            // (niubash#731).
            if !self.is_prefix(std::slice::from_ref(&key))
                && !self.quote_is_prefix(std::slice::from_ref(&key))
                && self.quote_exact(std::slice::from_ref(&key))
            {
                self.quote_armed = true;
                return ReedlineEvent::None;
            }
            // A key that starts a chord waits, even when it also has its own
            // binding: readline fires the single binding only after
            // `keyseq-timeout` expires, which the flush below reproduces.
            if self.is_prefix(std::slice::from_ref(&key))
                || self.quote_is_prefix(std::slice::from_ref(&key))
            {
                self.pending.push(raw);
                self.pending_since = Some(std::time::Instant::now());
                // `Pending` is the editor's "half a multi-key sequence"
                // signal: the parse loop erases that key from a macro
                // recording (GNU subtracts rl_key_sequence_length at
                // macro.c:276), and the engine drops it without a repaint.
                // Returning `None` here made the `C-x` of `C-x )` part of
                // the recorded macro, so a later replay left a lone C-x
                // parked and swallowed the next chord.
                return ReedlineEvent::Pending;
            }
            return self.forward(ReedlineRawEvent::try_from(raw).expect("key event"));
        }
        // A prefix older than readline's keyseq-timeout stops being a
        // prefix: replay it on its own, then treat the current key fresh.
        // `0` is readline's "wait forever" spelling.
        if self.keyseq_timeout != Duration::ZERO
            && self
                .pending_since
                .is_some_and(|since| since.elapsed() > self.keyseq_timeout)
        {
            let mut events = Vec::new();
            for parked in std::mem::take(&mut self.pending) {
                let event = self.forward(ReedlineRawEvent::try_from(parked).expect("parked key"));
                push_event(&mut events, event);
            }
            self.pending_since = None;
            let current = self.dispatch(raw);
            push_event(&mut events, current);
            return match events.len() {
                0 => ReedlineEvent::None,
                1 => events.pop().expect("one event"),
                _ => ReedlineEvent::Multiple(events),
            };
        }
        let mut candidate: Vec<(KeyModifiers, KeyCode)> =
            self.pending.iter().filter_map(chord_key).collect();
        candidate.push(key);
        if let Some(event) = self.exact(&candidate) {
            self.pending.clear();
            self.pending_since = None;
            return event;
        }
        // A completed quote sequence arms instead of firing
        // (niubash#731).
        if self.quote_exact(&candidate) {
            self.pending.clear();
            self.pending_since = None;
            self.quote_armed = true;
            return ReedlineEvent::None;
        }
        if self.is_prefix(&candidate) {
            self.pending.push(raw);
            // The wait restarts at each key: GNU gives every key of
            // the sequence its own keyseq-timeout window (the #704
            // review's chord observation ② — the deadline used to
            // run from the FIRST parked key, so a slow-but-steady
            // 3+ key chord expired mid-sequence, niubash#731).
            self.pending_since = Some(std::time::Instant::now());
            // Same `Pending` contract as the first parked key above: a
            // chord's prefix half is not a macro-recording key.
            return ReedlineEvent::Pending;
        }
        if self.quote_is_prefix(&candidate) {
            self.pending.push(raw);
            self.pending_since = Some(std::time::Instant::now());
            return ReedlineEvent::None;
        }
        // Mismatch: replay the parked keys, then the current one, in order.
        let mut events = Vec::new();
        for parked in std::mem::take(&mut self.pending) {
            let event = self.forward(ReedlineRawEvent::try_from(parked).expect("parked key"));
            push_event(&mut events, event);
        }
        let event = self.forward(ReedlineRawEvent::try_from(raw).expect("key event"));
        push_event(&mut events, event);
        match events.len() {
            0 => ReedlineEvent::None,
            1 => events.pop().expect("one event"),
            _ => ReedlineEvent::Multiple(events),
        }
    }
}

/// readline's `keyseq-timeout` default, in milliseconds.
const DEFAULT_KEYSEQ_TIMEOUT_MS: u64 = 500;

/// `set keyseq-timeout` as the engine recorded it (`bind 'set
/// keyseq-timeout 250'` in the rc, or `~/.inputrc`). `None` when the engine
/// has no such variable (older revision).
fn engine_keyseq_timeout(shell: &Shell) -> Option<std::time::Duration> {
    let raw = shell.executor().bind_readline_variable("keyseq-timeout")?;
    raw.trim()
        .parse::<u64>()
        .ok()
        .map(std::time::Duration::from_millis)
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

/// Parse one GNU `bind` argument string, restricted to the options that
/// register a binding (`-m`, `-x`). The binding spec itself goes through
/// the engine's own parsers — the same functions the `bind` builtin
/// dispatches to — so a rule means exactly what the same text means at
/// the shell prompt; there is no second grammar. Failures carry the
/// reason verbatim: `niu config migrate` refuses a legacy rule with it
/// instead of writing an unusable rc line.
pub(crate) fn parse_keymap_bind_rule(
    spec: &str,
) -> Result<crate::bind_surface::EngineBindEntry, String> {
    use crate::bind_surface::{EngineBindEntry, EngineBindKind};
    let mut rest = spec.trim();
    if rest.is_empty() {
        return Err("the rule is empty".to_string());
    }
    let mut keymap: Option<String> = None;
    let mut execute = false;
    // Leading registration options, in bind's own getopt shape: `-m`
    // takes the keymap name (attached or as the next word), `-x` takes
    // the rest of the string as its spec. Anything else that looks
    // like an option cannot register a binding and is refused.
    loop {
        if rest == "-x" || rest.starts_with("-x ") || rest.starts_with("-x\t") {
            execute = true;
            rest = rest[2..].trim_start();
            break;
        }
        if rest == "-m" || rest.starts_with("-m ") || rest.starts_with("-m\t") {
            let after = rest[2..].trim_start();
            let name = after.split_whitespace().next().unwrap_or_default();
            if name.is_empty() {
                return Err("-m requires a keymap name".to_string());
            }
            keymap = Some(name.to_string());
            rest = after[name.len()..].trim_start();
            continue;
        }
        if rest.starts_with('-') && rest.len() > 2 {
            // Attached-value forms: `-x"…"` takes the rest as its
            // spec; `-m<name>` takes the rest of the word as the name.
            if let Some(attached) = rest.strip_prefix("-x") {
                execute = true;
                rest = attached.trim_start();
                break;
            }
            if let Some(attached) = rest.strip_prefix("-m") {
                let name = attached.split_whitespace().next().unwrap_or_default();
                keymap = Some(name.to_string());
                rest = attached[name.len()..].trim_start();
                continue;
            }
            let option = rest.chars().nth(1).unwrap_or_default();
            return Err(unsupported_keymap_rule_option(option));
        }
        if rest.starts_with('-') {
            let option = rest.chars().nth(1).unwrap_or_default();
            return Err(unsupported_keymap_rule_option(option));
        }
        break;
    }
    if let Some(name) = &keymap {
        if !crate::bind_surface::is_known_keymap(name) {
            return Err(rubash::builtins::bind::format_invalid_keymap(name));
        }
    }
    // The engine's errors carry, for the positional channel, the
    // `readline: …` text GNU itself prints; prefer it when present —
    // it names the defect in the rule author's own spelling.
    let reason =
        |err: rubash::builtins::bind::BindSpecError| err.readline_message.unwrap_or(err.message);
    if execute {
        let (keyseq, command) =
            rubash::builtins::bind::parse_unix_command_spec(rest).map_err(reason)?;
        return Ok(EngineBindEntry {
            keyseq,
            kind: EngineBindKind::Execute { command },
            keymap,
        });
    }
    if rubash::builtins::bind::strip_set_keyword(rest).is_some() {
        return Err(
            "`set …` rules set readline variables; [keymap] carries key bindings only".to_string(),
        );
    }
    let parsed = rubash::builtins::bind::parse_positional_binding(rest).map_err(reason)?;
    let kind = match parsed.spec {
        rubash::builtins::bind::BindingSpec::Function { name } => EngineBindKind::Function { name },
        rubash::builtins::bind::BindingSpec::Macro { text } => EngineBindKind::Macro { text },
    };
    Ok(EngineBindEntry {
        keyseq: parsed.keyseq,
        kind,
        keymap,
    })
}

/// Why a would-be option in a `[keymap].bind` rule is refused: only
/// `-m` and `-x` register bindings; `-r` in particular is a removal,
/// and removals live in their own array by design (DESIGN §5.2).
fn unsupported_keymap_rule_option(option: char) -> String {
    if option == 'r' {
        "removals are not bind rules — list the key under [keymap].unbind instead".to_string()
    } else {
        format!(
            "unsupported bind option '-{option}' in a keymap rule \
             (only -m and -x register bindings)"
        )
    }
}

/// Parse one `[keymap].unbind` entry (niubash#630, K4): a keyseq in
/// bind's written spelling, optionally wrapped in the double quotes
/// the listing form shows (`"\C-t"`). An unbind names exactly one
/// key — the same single-key parser the bridge mirrors binds with
/// decides, and it rejects multi-key sequences whole.
pub(crate) fn parse_keymap_unbind_key(spec: &str) -> Result<(KeyModifiers, KeyCode), String> {
    let trimmed = spec.trim();
    let inner = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(trimmed);
    if inner.is_empty() {
        return Err("the entry is empty".to_string());
    }
    parse_key_sequence(inner).ok_or_else(|| {
        format!(
            "'{spec}' does not name a single key the editor can bind \
             (unbind takes one keyseq in bind spelling)"
        )
    })
}

pub(crate) fn parse_key_sequence(value: &str) -> Option<(KeyModifiers, KeyCode)> {
    if let Some(key) = parse_named_key_sequence(value) {
        return Some(key);
    }
    match value {
        "^[[A" | "\\e[A" | "\\eOA" => Some((KeyModifiers::NONE, KeyCode::Up)),
        "^[[B" | "\\e[B" | "\\eOB" => Some((KeyModifiers::NONE, KeyCode::Down)),
        "^[[C" | "\\e[C" | "\\eOC" => Some((KeyModifiers::NONE, KeyCode::Right)),
        "^[[D" | "\\e[D" | "\\eOD" => Some((KeyModifiers::NONE, KeyCode::Left)),
        // The VT100/xterm keypad and editing-key spellings real terminals
        // send, which GNU's own default map binds (`bind -p` on bash 5.3:
        // `"\e[1~": beginning-of-line`, `"\e[4~": end-of-line`,
        // `"\e[3~": delete-char`, ...). Both the `~`-numbered and the
        // `O`/`[` letter forms are accepted; `\e[7~`/`\e[8~` are the rxvt
        // spellings of Home/End.
        "\\e[1~" | "^[[1~" | "\\e[7~" | "^[[7~" | "\\e[H" | "^[[H" | "\\eOH" => {
            Some((KeyModifiers::NONE, KeyCode::Home))
        }
        "\\e[4~" | "^[[4~" | "\\e[8~" | "^[[8~" | "\\e[F" | "^[[F" | "\\eOF" => {
            Some((KeyModifiers::NONE, KeyCode::End))
        }
        "\\e[2~" | "^[[2~" => Some((KeyModifiers::NONE, KeyCode::Insert)),
        "\\e[3~" | "^[[3~" => Some((KeyModifiers::NONE, KeyCode::Delete)),
        "\\e[5~" | "^[[5~" | "\\e[I" | "^[[I" => Some((KeyModifiers::NONE, KeyCode::PageUp)),
        "\\e[6~" | "^[[6~" | "\\e[G" | "^[[G" => Some((KeyModifiers::NONE, KeyCode::PageDown)),
        // CSI modifier spellings in GNU's terminal table (xterm/rxvt
        // send these for modified arrows and Delete). The terminal
        // layer delivers the same chord as KeyCode+modifier, so map
        // both spellings to one key.
        "\\e[1;3C" => Some((KeyModifiers::ALT, KeyCode::Right)),
        "\\e[1;3D" => Some((KeyModifiers::ALT, KeyCode::Left)),
        "\\e[1;5C" | "\\e[5C" => Some((KeyModifiers::CONTROL, KeyCode::Right)),
        "\\e[1;5D" | "\\e[5D" => Some((KeyModifiers::CONTROL, KeyCode::Left)),
        "\\e[3;5~" => Some((KeyModifiers::CONTROL, KeyCode::Delete)),
        // A bare ESC is one of GNU's default binds (`"\e": emacs-editing-mode`).
        // It arrives here only when it is the whole keyseq; `\e[...` forms
        // were matched above.
        "\\e" | "^[" => Some((KeyModifiers::NONE, KeyCode::Esc)),
        "^?" => Some((KeyModifiers::NONE, KeyCode::Backspace)),
        _ => parse_meta_control_combined(value)
            .or_else(|| parse_alt_key_sequence(value))
            .or_else(|| parse_meta_key_sequence(value))
            .or_else(|| parse_control_key_sequence(value))
            .or_else(|| parse_plain_key_sequence(value)),
    }
}

/// Split a readline keyseq into per-key units and parse each unit.
///
/// The flat keymap owns single-key sequences; this returns `Some` only for
/// a clean chain of two or more units (niubash#630: bash's
/// `\C-x\C-e`-style chords, fzf's `\C-\e(`). A token that does not parse
/// makes the whole sequence unparseable — the caller keeps reporting those
/// (they are typos, not chords).
fn parse_chord_sequence(value: &str) -> Option<Vec<(KeyModifiers, KeyCode)>> {
    let units = split_key_units(value)?;
    if units.len() < 2 {
        return None;
    }
    units
        .into_iter()
        .map(parse_key_sequence)
        .collect::<Option<Vec<_>>>()
}

/// Split a readline keyseq into its written units without interpreting
/// them: `\C-x`, `\M-x`, `\e` (ESC), `\e[...]` terminal sequences, `^X`,
/// and single literal characters.
fn split_key_units(value: &str) -> Option<Vec<&str>> {
    let mut rest = value;
    let mut units = Vec::new();
    while !rest.is_empty() {
        let (unit, tail) = if let Some(tail) = rest
            .strip_prefix("\\C-")
            .or_else(|| rest.strip_prefix("\\M-"))
        {
            // The unit is the prefix plus one (possibly escaped)
            // character — or, for the combined `\M-\C-x` spelling,
            // the second prefix plus its character.
            let tail = tail
                .strip_prefix("\\C-")
                .or_else(|| tail.strip_prefix("\\M-"))
                .unwrap_or(tail);
            let ch = tail.chars().next()?;
            let end = rest.len() - tail.len() + ch.len_utf8();
            (&rest[..end], &rest[end..])
        } else if rest.starts_with("\\e[") {
            let end = rest
                .char_indices()
                .skip(2)
                .find(|(_, c)| c.is_ascii_alphabetic() || *c == '~')
                .map(|(i, c)| i + c.len_utf8())?;
            (&rest[..end], &rest[end..])
        } else if let Some(tail) = rest.strip_prefix("\\e") {
            // `\e` alone is ESC; `\eX` is the meta form one key (the same
            // shape `parse_key_sequence` accepts as ALT+X), so it is not a
            // chord unit boundary.
            let mut chars = tail.chars();
            let end = match (chars.next(), chars.next()) {
                (Some('['), _) => unreachable!("handled above"),
                (Some(ch), None) => rest.len() - tail.len() + ch.len_utf8(),
                _ => rest.len() - tail.len(),
            };
            (&rest[..end], &rest[end..])
        } else if let Some(tail) = rest.strip_prefix('^') {
            let ch = tail.chars().next()?;
            let end = 1 + ch.len_utf8();
            (&rest[..end], &rest[end..])
        } else {
            let ch = rest.chars().next()?;
            (&rest[..ch.len_utf8()], &rest[ch.len_utf8()..])
        };
        units.push(unit);
        rest = tail;
    }
    Some(units)
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

/// The combined meta-control spelling `\M-\C-x` (either prefix
/// order), one key carrying both modifiers — GNU's
/// rl_translate_keyseq accepts it (e.g. `\M-\C-e` for
/// shell-expand-line, `\M-\C-i` for dynamic-complete-history).
fn parse_meta_control_combined(value: &str) -> Option<(KeyModifiers, KeyCode)> {
    let rest = value
        .strip_prefix("\\M-\\C-")
        .or_else(|| value.strip_prefix("\\C-\\M-"))?;
    let mut chars = rest.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some((
        KeyModifiers::CONTROL | KeyModifiers::ALT,
        KeyCode::Char(ch.to_ascii_lowercase()),
    ))
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
        // niubash#642: through the shared funnel, so the session-end
        // history write runs on this path too (it used to skip it).
        end_repl_session(&mut shell.borrow_mut(), status);
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
    // A leftover pre-1.5.0 ~/.niubash/config.toml is never read. Say so
    // once per interactive session, with the one-time migration command;
    // the file itself never affects startup.
    if let Some(notice) = crate::config::legacy_config_notice() {
        eprintln!("{notice}");
    }
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
    // (The loop used to snapshot the engine-bind generation here
    // and compare it against the executor between prompts; it
    // now asks Shell::engine_bindings_stale, which reads the
    // generation through the bind surface — row 11.)
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
        if shell.borrow().engine_bindings_stale() {
            shell.borrow_mut().load_engine_bindings();
            line_editor = build_line_editor(&shell)?;
        }
        // niubash#775: a loadable builtin may have called rl_add_defun /
        // rl_bind_key / rl_unbind_* since the last build (at startup or from
        // a bind -x command).  Rebuild from the live ABI keymaps so a binding
        // registered mid-session fires on the very next key.  A host-command
        // keypress returns to the loop with the edit buffer intact, so carry
        // the buffer/point into the fresh editor rather than dropping them.
        if crate::readline_live::is_stale() {
            let buffer = line_editor.current_buffer_contents().to_string();
            let point = line_editor.current_insertion_point();
            let mut rebuilt = build_line_editor(&shell)?;
            if !buffer.is_empty() {
                rebuilt.run_edit_commands(&[
                    EditCommand::Clear,
                    EditCommand::InsertString(buffer),
                    EditCommand::MoveToPosition {
                        position: point,
                        select: false,
                    },
                ]);
            }
            line_editor = rebuilt;
        }
        // niubash#646: refresh the HIST* settings snapshot before the
        // line is read, so the record-time filters, the list stifle,
        // and the session-end write all see the values the previous
        // command (or PROMPT_COMMAND) left in the environment — GNU
        // reads each variable at the moment of its operation.
        shell.borrow_mut().refresh_history_settings();
        // niubash#661: the refresh followed a live `HISTFILE` redirect
        // on the provider side; the editor's history instance still
        // backs the old file, so rebuild the editor over the new one —
        // the same lossless construction path as the rebuilds below
        // (the buffer is empty at this boundary and history state
        // lives in the file and the shared ledger/settings cells).
        if shell.borrow_mut().take_history_redirect() {
            line_editor = build_line_editor(&shell)?;
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
                let status = shell.borrow().session().last_status();
                // The jump's status IS the session status (GNU's
                // jump_to_top_level unwinds reader_loop and exit_shell
                // carries it); the REPL's Ok(()) return otherwise always
                // maps to rc 0 in main, so carry the status out directly —
                // the same shape as the engine's piped `-i` route
                // (src/main.rs run_interactive_stdin).
                // niubash#642: through the shared funnel, so the
                // session-end history write runs on this path too (it
                // used to skip it, resurrecting every cleared line).
                end_repl_session(&mut shell.borrow_mut(), status);
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
            let prompt_text = prompt.render_prompt_left().into_owned();
            // niubash#775: before the read loop dispatches any key, the ABI
            // sees the editor's fresh (empty) buffer and the prompt.
            crate::readline_live::begin_read(
                line_editor.current_buffer_contents(),
                line_editor.current_insertion_point(),
                &prompt_text,
            );
            typeahead.disarm_and_reinject();
            line_editor.read_line(&prompt)
        } else {
            let prompt = shell.borrow().prompt.clone();
            let prompt = ContinuationPrompt::new(&prompt, prompt.vi_indicators());
            let prompt_text = prompt.render_prompt_left().into_owned();
            crate::readline_live::begin_read(
                line_editor.current_buffer_contents(),
                line_editor.current_insertion_point(),
                &prompt_text,
            );
            typeahead.disarm_and_reinject();
            line_editor.read_line(&prompt)
        };
        // The foreground command (or widget/exit path) below owns the
        // terminal: the guard stays disarmed until the next prompt
        // rebuild at the top of the loop.
        typeahead.disarm_and_reinject();

        // niubash#669: while the foreground work below runs, watch the
        // console input queue for a Ctrl+C that arrived as a raw input
        // record — those never become a CTRL_C_EVENT, so the console
        // handler cannot fire for them and a foreground command would
        // run to completion uninterruptible. The watch synthesizes the
        // missing delivery (engine note + group broadcast) and drops
        // when this iteration ends. Windows-only effect.
        let _foreground_ctrl_c_watch = crate::ctrl_c::ForegroundWatch::arm();

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
                // niubash#775: GNU keeps the accepted line in rl_line_buffer
                // while the command runs, so a plugin builtin invoked as an
                // ordinary command observes the line the user submitted.
                crate::readline_live::finish_accepted(&line);
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
                            // niubash#642: self-update ends the session
                            // like any other exit — EXIT trap and the
                            // session-end history write included (this
                            // path used to skip both).
                            end_repl_session(&mut shell.borrow_mut(), code);
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
                crate::readline_live::finish_abandoned();
                println!();
                if !pending.is_empty() {
                    pending.clear();
                    continue;
                }
                let _ = shell.borrow_mut().finish_with_exit_trap(0);
                break;
            }
            Ok(Signal::CtrlC) => {
                crate::readline_live::finish_abandoned();
                // niubash#600: GNU shows a literal `^C` where the line
                // was abandoned (the kernel's ECHOCTL echoes it in
                // cooked mode; readline echoes it in raw mode). Reedline
                // prints nothing for the byte it swallows, so the host
                // echoes it here — ahead of the newline below, the
                // on-screen shape is `<partial line>^C` then a fresh
                // prompt, like GNU.
                print!("^C");
                println!();
                let handler_queued = crate::ctrl_c::consume_ctrl_c();
                if handler_queued {
                    flush_repl_output();
                }
                // niubash#616: GNU runs a trapped INT at the prompt
                // immediately, before the next prompt. Reedline runs the
                // tty in raw mode, so no kernel SIGINT reaches the engine
                // at the prompt — deliver exactly one through the queue
                // helper and let the engine's own boundary dispatch
                // adjudicate it: a trapped INT runs its action now, an
                // untrapped one lands as the DISCARD classification
                // (#610). The dispatch also settles the status half of
                // this arm: $? = 130 (128+SIGINT) on trap entry and again
                // after the trap, which is #611's bookkeeping for the
                // untrapped case and GNU's rule for both.
                crate::ctrl_c::queue_prompt_sigint(handler_queued);
                shell.borrow_mut().dispatch_prompt_interrupt();
                flush_repl_output();
                pending.clear();
                // A trap action that itself exits (`trap 'exit 3' INT`)
                // ends the session with its status, like a submitted
                // line's top-level exit (niubash R1).
                let jump = shell.borrow_mut().take_session_exit_jump();
                if let Some(status) = jump {
                    end_repl_session(&mut shell.borrow_mut(), status);
                }
                continue;
            }
            Ok(_) => continue,
            Err(e) => {
                crate::readline_live::finish_abandoned();
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
        // niubash#646: same settings refresh as the line-editor loop.
        shell.refresh_history_settings();
        // niubash#191: same exit-jump consumption as the line-editor loop —
        // a PC `exit` ends the session before the next prompt is printed.
        // niubash#642: through the shared funnel, so the session-end
        // history write runs on this path too (it used to skip it).
        if shell.run_precmd_hooks() {
            let status = shell.session().last_status();
            end_repl_session(shell, status);
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
    use std::collections::HashSet;

    #[test]
    fn emacs_keybindings_keep_ctrl_r_history_search_and_tab_completion() {
        let mut keybindings = default_emacs_keybindings();
        add_menu_keybindings(&mut keybindings, CompletionInteraction::Insert);
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
                completion_interaction: CompletionInteraction::Insert,
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
        add_menu_keybindings(&mut insert, CompletionInteraction::Insert);
        add_menu_keybindings(&mut normal, CompletionInteraction::Insert);

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

    /// niubash#132: the system-clipboard paste binding must be registered
    /// in every keymap set `build_edit_mode` ships — Emacs, Vi insert and
    /// Vi normal — not just the two sets the original fix's tests sampled.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn issue132_system_clipboard_paste_bound_in_all_keymap_sets() {
        let mut emacs = default_emacs_keybindings();
        let mut vi_insert = default_vi_insert_keybindings();
        let mut vi_normal = default_vi_normal_keybindings();
        add_system_clipboard_keybindings(&mut emacs);
        add_system_clipboard_keybindings(&mut vi_insert);
        add_system_clipboard_keybindings(&mut vi_normal);
        for keybindings in [emacs, vi_insert, vi_normal] {
            assert_eq!(
                keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('v')),
                Some(ReedlineEvent::Edit(vec![EditCommand::PasteSystem]))
            );
        }
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
            "previous-history",
        ] {
            assert_eq!(native_widget_event(widget), Some(ReedlineEvent::Up));
        }
        for widget in [
            "history-substring-search-down",
            "history-search-forward",
            "down-line-or-history",
            "next-history",
        ] {
            assert_eq!(native_widget_event(widget), Some(ReedlineEvent::Down));
        }
    }

    /// The GNU meta-keymap completion commands that the pinned editor's
    /// own defaults do not carry are installed by the host baseline, not
    /// only by a `bind` at startup: M-* (insert-completions) has no
    /// engine-registry entry on a fresh shell, so without this table the
    /// key fell through and M-* was a no-op. M-g rides the readline host
    /// channel (glob-complete-word → Session::glob_filename). M-? is not
    /// installed: possible-completions is list-only, but the GNU menu's
    /// queued Activate would splice the common prefix; the unbound key
    /// leaves the line alone, which is the observable readline result.
    #[test]
    fn gnu_default_command_bindings_install_meta_completion_keys() {
        let mut keybindings = default_emacs_keybindings();
        add_gnu_command_default_keybindings(&mut keybindings, NativeKeymapTarget::Emacs);
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('*')),
            Some(ReedlineEvent::InsertCompletions)
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('g')),
            Some(ReedlineEvent::ExecuteHostCommand(format!(
                "{}glob-complete-word",
                READLINE_HOST_COMMAND_PREFIX
            )))
        );
    }

    /// The residue of GNU's default bind table that the host still
    /// cannot deliver, per group. This is a **shrink-only** pin:
    /// mapping one of these names (or teaching `parse_key_sequence`
    /// one of the terminal spellings) must delete its entry here, so
    /// the list doubles as the parity burn-down. Adding an entry is a
    /// regression unless a new GNU table row arrived with it.
    ///
    /// Groups:
    /// - `forward-search-history` (`\C-s`): the editor has
    ///   `ReedlineEvent::SearchHistory` for reverse incremental search
    ///   only; direction-aware incremental search still needs a ruline
    ///   event (see the issue).
    ///
    /// Delivered in this batch and removed from the list:
    /// `glob-complete-word` (`\eg`) through the engine's
    /// `Session::glob_filename` and the host readline channel;
    /// `complete-hostname` (`\e@`) from `/etc/hosts` plus the local
    /// hostname; `complete-username` (`\e~`) from `/etc/passwd`; and
    /// `complete-into-braces` (`\e{`) via the bracecomp.c port.
    ///
    /// Delivered in earlier batches: `abort` (`\C-g`) as a host command
    /// that dings without touching the line; `history-expand-line`
    /// (`\e^`) on the same channel; and `do-lowercase-version`
    /// (`\eA`–`\eN`, `\eP`–`\eZ`) at dispatch time (the editor
    /// lowercases modified keys, and the chord layer folds an
    /// ESC-prefixed uppercase partner).
    ///
    /// The CSI modified-arrow/Delete spellings (`\e[1;3C`, `\e[1;5C`,
    /// `\e[5C`, `\e[3;5~`, ...) are delivered: `parse_key_sequence`
    /// maps them to the same KeyCode+modifier chords the terminal
    /// layer sends.
    /// GNU default spellings the editor consumes before key dispatch:
    /// reedline's bracketed-paste envelope turns `\e[200~`…`\e[201~`
    /// into paste data (`Reedline::use_bracketed_paste`; the raw CSI
    /// sequence never reaches a keymap), so it is not a binding and
    /// must not be faked as one. The gate below still requires the
    /// bridge to *report* it, then excludes exactly this list.
    const EDITOR_CONSUMED_GNU_DEFAULTS: &[&str] = &["\\e[200~"];

    const KNOWN_UNMAPPED_GNU_DEFAULTS: &[&str] = &["\\C-s"];

    /// niubash#630 gap list: the **whole GNU default bind table** must
    /// mirror. rust line: `rubash::builtins::bind::DEFAULT_FUNCTION_BINDINGS`
    /// is the table `bind -p` prints for a fresh bash 5.3, so feeding it
    /// through the bridge is the compatibility gate for "GNU habits as the
    /// default".
    ///
    /// Everything deliverable must land — Home/End/Delete spellings,
    /// `\C-r` reverse-search, `\C-t` transpose, the PgUp/PgDn history
    /// jumps, and this batch's `\e&`/`\e~`/`\ex` host commands. The
    /// entries in [`KNOWN_UNMAPPED_GNU_DEFAULTS`] are the documented,
    /// grouped residue; any *new* unmapped key is a regression.
    #[test]
    fn gnu_default_bind_table_mirrors_except_known_gaps() {
        let entries: Vec<crate::bind_surface::EngineBindEntry> =
            rubash::builtins::bind::DEFAULT_FUNCTION_BINDINGS
                .iter()
                .map(|(keyseq, name)| crate::bind_surface::EngineBindEntry {
                    keyseq: (*keyseq).to_string(),
                    kind: crate::bind_surface::EngineBindKind::Function {
                        name: (*name).to_string(),
                    },
                    keymap: Some("emacs-standard".to_string()),
                })
                .collect();

        let mut keybindings = default_emacs_keybindings();
        let unusable =
            add_engine_bind_keybindings(&mut keybindings, NativeKeymapTarget::Emacs, &entries);
        let all_unmapped: Vec<String> = unusable.into_iter().map(|bind| bind.keyseq).collect();
        // The bracketed-paste marker is genuinely unconsumed by the
        // keymap — the editor's paste envelope owns it. Pin that
        // exclusion too, so it cannot silently grow or vanish.
        let mut editor_consumed: Vec<String> = all_unmapped
            .iter()
            .filter(|keyseq| EDITOR_CONSUMED_GNU_DEFAULTS.contains(&keyseq.as_str()))
            .cloned()
            .collect();
        editor_consumed.sort();
        assert_eq!(
            editor_consumed,
            vec!["\\e[200~".to_string()],
            "only the editor-consumed bracketed-paste marker may leave the keymap"
        );
        let mut unmapped: Vec<String> = all_unmapped
            .into_iter()
            .filter(|keyseq| !EDITOR_CONSUMED_GNU_DEFAULTS.contains(&keyseq.as_str()))
            .collect();
        unmapped.sort();
        let expected: Vec<String> = KNOWN_UNMAPPED_GNU_DEFAULTS
            .iter()
            .map(|keyseq| (*keyseq).to_string())
            .collect();
        assert_eq!(
            unmapped, expected,
            "GNU default table must mirror except the documented gaps"
        );

        // Spot-check the terminal spellings that used to be rejected: the
        // action, not just "no warning".
        assert_eq!(
            keybindings.find_binding(KeyModifiers::NONE, KeyCode::Home),
            Some(edit_event(EditCommand::MoveToLineStart { select: false }))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::NONE, KeyCode::End),
            Some(edit_event(EditCommand::MoveToLineEnd { select: false }))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::NONE, KeyCode::Delete),
            Some(edit_event(EditCommand::Delete))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('r')),
            Some(ReedlineEvent::SearchHistory)
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('t')),
            Some(edit_event(EditCommand::SwapGraphemes))
        );
        let host = |name: &str| {
            Some(ReedlineEvent::ExecuteHostCommand(format!(
                "{}{}",
                READLINE_HOST_COMMAND_PREFIX, name
            )))
        };
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('g')),
            host("abort")
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('^')),
            host("history-expand-line")
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('!')),
            host("complete-command")
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('$')),
            host("complete-variable")
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('/')),
            host("complete-filename")
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('@')),
            host("complete-hostname")
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('~')),
            host("complete-username")
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('{')),
            host("complete-into-braces")
        );
        // do-lowercase-version installs no flat binding: Meta-A stays
        // unbound while Meta-B keeps its own command (the default
        // table lists \eB before \eF, so a flat insert would have
        // overwritten this).
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('a')),
            None
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('b')),
            Some(edit_event(EditCommand::MoveWordLeft { select: false }))
        );
    }

    /// GNU `do-lowercase-version` (text.c:1179, dispatched at
    /// readline.c:916-922) is a dispatch rule, not a bindable command:
    /// the bridge must leave the lowercase Meta slots alone, and the
    /// ESC-meta layer must fold an uppercase partner to the lowercase
    /// Meta key. This is what the 25 `\eA`–`\eN`/`\eP`–`\eZ` default
    /// rows deliver.
    #[test]
    fn gnu_uppercase_meta_keys_fall_back_to_lowercase_commands() {
        use crossterm29::event::{Event, KeyEvent};
        let inner = Emacs::new(default_emacs_keybindings());
        // The real ALT binding set for the letters this test presses.
        let alt: HashSet<KeyCode> = [KeyCode::Char('b'), KeyCode::Char('f')]
            .into_iter()
            .collect();
        let partner = Event::Key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::NONE));
        let lookahead: Lookahead = Box::new(move |_| Some(partner.clone()));
        let mut mode = ChordEditMode::new(
            Box::new(inner),
            Vec::new(),
            alt,
            Some(lookahead),
            TEST_KEYSEQ_TIMEOUT,
        );
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Esc, KeyModifiers::NONE)),
            edit_event(EditCommand::MoveWordLeft { select: false }),
            "ESC B must dispatch Meta-b (backward-word), not insert B"
        );

        // A lowercase form with no ALT binding is not synthesized: GNU
        // keeps the unbound Meta key (and dings).
        assert_eq!(meta_fallback_key(KeyCode::Char('A'), &HashSet::new()), None);
        assert_eq!(
            meta_fallback_key(KeyCode::Char('B'), &HashSet::from([KeyCode::Char('b')])),
            Some(KeyCode::Char('b'))
        );
        // Non-letters pass through unchanged when ALT-bound.
        assert_eq!(
            meta_fallback_key(KeyCode::Right, &HashSet::from([KeyCode::Right])),
            Some(KeyCode::Right)
        );
    }

    /// niubash#630 gap list: GNU readline functions the editor can already
    /// deliver must mirror when a user re-binds them in an rc file. Before
    /// this, `bind '"\C-t": transpose-chars'` (and the word-case pair, and
    /// `unix-word-rubout`) landed in the unknown-widget sentinel and were
    /// reported as "function is not in the host's deliverable set".
    #[test]
    fn native_widget_maps_deliverable_gnu_readline_names() {
        assert_eq!(
            native_widget_event("transpose-chars"),
            Some(edit_event(EditCommand::SwapGraphemes))
        );
        assert_eq!(
            native_widget_event("upcase-word"),
            Some(edit_event(EditCommand::UppercaseWord))
        );
        assert_eq!(
            native_widget_event("downcase-word"),
            Some(edit_event(EditCommand::LowercaseWord))
        );
        assert_eq!(
            native_widget_event("unix-word-rubout"),
            Some(edit_event(EditCommand::CutWordLeft))
        );
        // `complete` shares the TAB completion event (menu first, then the
        // editor's own completion), never the host-command sentinel.
        match native_widget_event("complete") {
            Some(ReedlineEvent::UntilFound(events)) => {
                assert!(matches!(events.first(), Some(ReedlineEvent::Menu(_))));
            }
            other => panic!("complete must map to the completion event, got {other:?}"),
        }
        // GNU's editing-mode toggles are session-level switches, delivered
        // as `SwitchMode` (the standby machine built by `build_line_editor`
        // accepts the target).
        assert_eq!(
            native_widget_event("vi-editing-mode"),
            Some(ReedlineEvent::SwitchMode(PromptEditMode::Vi(
                PromptViMode::Insert
            )))
        );
        assert_eq!(
            native_widget_event("emacs-editing-mode"),
            Some(ReedlineEvent::SwitchMode(PromptEditMode::Emacs))
        );

        // A genuinely undeliverable name still falls through to the
        // shell-function-widget sentinel (the bridge reports those).
        assert!(matches!(
            native_widget_event("no_such_readline_fn"),
            Some(ReedlineEvent::ExecuteHostCommand(_))
        ));
    }

    /// niubash#765: the dump commands and re-read-init-file are
    /// readline functions performed by the host, so their names map
    /// to the readline host-command sentinel — never to the
    /// shell-function widget sentinel.
    #[test]
    fn issue765_readline_host_commands_use_readline_sentinel() {
        for name in [
            "dump-functions",
            "dump-macros",
            "dump-variables",
            "re-read-init-file",
        ] {
            assert_eq!(
                native_widget_event(name),
                Some(ReedlineEvent::ExecuteHostCommand(format!(
                    "{}{}",
                    READLINE_HOST_COMMAND_PREFIX, name
                ))),
                "{name} must ride the readline host channel"
            );
        }
    }

    /// This batch's GNU default functions ride the readline host
    /// channel — never the shell-function widget sentinel. `abort`
    /// (util.c rl_abort) and `history-expand-line` (bashline.c)
    /// and the context-specific completion entry points all need the
    /// live session/editor, so the host performs them.
    #[test]
    fn readline_burndown_host_commands_use_readline_sentinel() {
        for name in [
            "abort",
            "history-expand-line",
            "complete-command",
            "complete-variable",
            "complete-filename",
            "complete-hostname",
            "complete-username",
            "complete-into-braces",
            "glob-complete-word",
        ] {
            assert_eq!(
                native_widget_event(name),
                Some(ReedlineEvent::ExecuteHostCommand(format!(
                    "{}{}",
                    READLINE_HOST_COMMAND_PREFIX, name
                ))),
                "{name} must ride the readline host channel"
            );
        }
        assert!(
            crate::readline_commands::HOST_COMMAND_NAMES.contains(&"abort"),
            "abort is a readline host command"
        );
        assert!(
            crate::readline_commands::funmap_names().contains(&"history-expand-line"),
            "the dump must list history-expand-line"
        );
    }

    /// niubash#765: the GNU default C-x C-r chord for
    /// re-read-init-file joins the chord layer, and the dump lists
    /// every funmap name in GNU's plain form.
    #[test]
    fn issue765_gnu_defaults_and_dump_lines() {
        let chords = gnu_default_chord_bindings();
        assert!(
            chords.iter().any(|chord| {
                chord.keys
                    == vec![
                        (KeyModifiers::CONTROL, KeyCode::Char('x')),
                        (KeyModifiers::CONTROL, KeyCode::Char('r')),
                    ]
            }),
            "C-x C-r must be a default chord"
        );

        // No user bindings: defaults only. The engine default table
        // carries several terminal spellings of beginning-of-line
        // (\C-a plus the Home terminfo sequences), and the dumper
        // caps a line at five seqs in table order — assert the claim,
        // not one full spelling, so a table growth upstream does not
        // break this pin.
        let lines = dump_function_lines(&[]);
        let beginning_of_line = lines
            .iter()
            .find(|line| line.starts_with("beginning-of-line can be found on "))
            .expect("beginning-of-line must be bound in the default table");
        assert!(
            beginning_of_line.contains("\"\\C-a\""),
            "the C-a default must be reported: {beginning_of_line}"
        );
        assert!(lines.contains(&"re-read-init-file can be found on \"\\C-x\\C-r\".".to_string()));
        assert!(lines.contains(&"dump-functions is not bound to any keys".to_string()));
        // Sorted like rl_funmap_names.
        let mut sorted = lines.clone();
        sorted.sort();
        assert_eq!(lines, sorted);

        // A user function binding joins its function's list, and a
        // macro rebind of a default seq retires the function claim.
        let entries = vec![
            crate::bind_surface::EngineBindEntry {
                keyseq: "\\C-o".to_string(),
                kind: crate::bind_surface::EngineBindKind::Function {
                    name: "operate-and-get-next".to_string(),
                },
                keymap: None,
            },
            crate::bind_surface::EngineBindEntry {
                keyseq: "\\C-a".to_string(),
                kind: crate::bind_surface::EngineBindKind::Macro {
                    text: "home".to_string(),
                },
                keymap: None,
            },
        ];
        let lines = dump_function_lines(&entries);
        assert!(lines.contains(&"operate-and-get-next can be found on \"\\C-o\".".to_string()));
        // The \C-a claim is retired; the other default spellings
        // remain (the table also carries \eOH/\e[H terminfo forms).
        let beginning_of_line = lines
            .iter()
            .find(|line| line.starts_with("beginning-of-line can be found on "))
            .expect("the other beginning-of-line defaults remain");
        assert!(
            !beginning_of_line.contains("\"\\C-a\""),
            "the macro rebind must retire the C-a claim: {beginning_of_line}"
        );

        // The GNU default spellings parse as single keys: plain
        // meta forms and the combined meta-control form.
        assert_eq!(
            parse_key_sequence("\\M-~"),
            Some((KeyModifiers::ALT, KeyCode::Char('~')))
        );
        assert_eq!(
            parse_key_sequence("\\M-\\C-e"),
            Some((
                KeyModifiers::CONTROL | KeyModifiers::ALT,
                KeyCode::Char('e')
            ))
        );
        assert_eq!(
            parse_key_sequence("\\M-\\C-i"),
            Some((
                KeyModifiers::CONTROL | KeyModifiers::ALT,
                KeyCode::Char('i')
            ))
        );
    }

    /// niubash#766: tilde-expand and execute-named-command map to
    /// the readline host channel, and their GNU default keys land
    /// in the emacs baseline now that the names are delivered.
    /// insert-comment stays the #731 host composite.
    #[test]
    fn issue766_tilde_expand_and_execute_named_command_are_host_delivered() {
        for name in ["tilde-expand", "execute-named-command"] {
            assert_eq!(
                native_widget_event(name),
                Some(ReedlineEvent::ExecuteHostCommand(format!(
                    "{}{}",
                    READLINE_HOST_COMMAND_PREFIX, name
                ))),
                "{name} must ride the readline host channel"
            );
        }
        assert!(matches!(
            native_widget_event("insert-comment"),
            Some(ReedlineEvent::Multiple(_))
        ));

        let mut keybindings = Keybindings::default();
        add_gnu_command_default_keybindings(&mut keybindings, NativeKeymapTarget::Emacs);
        let tilde_event = ReedlineEvent::ExecuteHostCommand(format!(
            "{}tilde-expand",
            READLINE_HOST_COMMAND_PREFIX
        ));
        // emacs_keymap.c binds both M-& and M-~ to rl_tilde_expand, but
        // bashline.c:565-569 replaces M-~ with bash_complete_username, so
        // only M-& keeps tilde-expand.
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('&')),
            Some(tilde_event)
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('x')),
            Some(ReedlineEvent::ExecuteHostCommand(format!(
                "{}execute-named-command",
                READLINE_HOST_COMMAND_PREFIX
            )))
        );
        // This batch's host commands are real default bindings, not
        // gate-only translations.
        let host = |name: &str| {
            ReedlineEvent::ExecuteHostCommand(format!("{}{}", READLINE_HOST_COMMAND_PREFIX, name))
        };
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('g')),
            Some(host("abort"))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('^')),
            Some(host("history-expand-line"))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('!')),
            Some(host("complete-command"))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('$')),
            Some(host("complete-variable"))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('/')),
            Some(host("complete-filename"))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('@')),
            Some(host("complete-hostname"))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('~')),
            Some(host("complete-username"))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('{')),
            Some(host("complete-into-braces"))
        );

        // The vi target carries none of the emacs defaults.
        let mut vi = Keybindings::default();
        add_gnu_command_default_keybindings(&mut vi, NativeKeymapTarget::ViInsert);
        assert_eq!(vi.find_binding(KeyModifiers::ALT, KeyCode::Char('~')), None);
    }

    /// niubash#767: shell-expand-line and dynamic-complete-history
    /// map to the readline host channel, and their GNU default keys
    /// (M-C-e, M-TAB in both its spellings) bind in the emacs
    /// baseline now that the names are delivered.
    #[test]
    fn issue767_shell_expand_and_history_complete_are_host_delivered() {
        for name in ["shell-expand-line", "dynamic-complete-history"] {
            assert_eq!(
                native_widget_event(name),
                Some(ReedlineEvent::ExecuteHostCommand(format!(
                    "{}{}",
                    READLINE_HOST_COMMAND_PREFIX, name
                ))),
                "{name} must ride the readline host channel"
            );
        }

        let mut keybindings = Keybindings::default();
        add_gnu_command_default_keybindings(&mut keybindings, NativeKeymapTarget::Emacs);
        assert_eq!(
            keybindings.find_binding(
                KeyModifiers::CONTROL | KeyModifiers::ALT,
                KeyCode::Char('e')
            ),
            Some(ReedlineEvent::ExecuteHostCommand(format!(
                "{}shell-expand-line",
                READLINE_HOST_COMMAND_PREFIX
            )))
        );
        let history_event = ReedlineEvent::ExecuteHostCommand(format!(
            "{}dynamic-complete-history",
            READLINE_HOST_COMMAND_PREFIX
        ));
        assert_eq!(
            keybindings.find_binding(
                KeyModifiers::CONTROL | KeyModifiers::ALT,
                KeyCode::Char('i')
            ),
            Some(history_event.clone())
        );
        // The byte shape terminals actually send for Meta-TAB.
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Tab),
            Some(history_event)
        );
    }

    /// Second niubash#630 batch: readline functions deliverable only since
    /// the reedline fork gained `Suspend`/`QuotedInsert`/`OverwriteMode`/
    /// `CharSearch`/the yank-arg pair/`OperateAndGetNext`/the kbd-macro
    /// trio/the numeric-argument events/`ClearScrollback`/`OpenEditor` and
    /// the `EditCommand` additions (`YankPop`, `SwapWords`,
    /// `CapitalizeWord`, `SwapCursorAndAnchor`, `RevertLine`,
    /// `CutFilenameLeft`, `DeleteHorizontalSpace`). Every arm below must
    /// produce the real event shape, not the unknown-widget sentinel.
    #[test]
    fn native_widget_maps_fork_delivered_gnu_readline_names() {
        // EditCommand-backed arms.
        assert_eq!(
            native_widget_event("yank-pop"),
            Some(edit_event(EditCommand::YankPop))
        );
        assert_eq!(
            native_widget_event("transpose-words"),
            Some(edit_event(EditCommand::SwapWords))
        );
        assert_eq!(
            native_widget_event("capitalize-word"),
            Some(edit_event(EditCommand::CapitalizeWord))
        );
        assert_eq!(
            native_widget_event("exchange-point-and-mark"),
            Some(edit_event(EditCommand::SwapCursorAndAnchor))
        );
        assert_eq!(
            native_widget_event("revert-line"),
            Some(edit_event(EditCommand::RevertLine))
        );
        assert_eq!(
            native_widget_event("delete-horizontal-space"),
            Some(edit_event(EditCommand::DeleteHorizontalSpace))
        );
        for widget in ["unix-filename-rubout", "vi-filename-rubout"] {
            assert_eq!(
                native_widget_event(widget),
                Some(edit_event(EditCommand::CutFilenameLeft))
            );
        }
        assert_eq!(
            native_widget_event("tab-insert"),
            Some(edit_event(EditCommand::InsertChar('\t')))
        );

        // ReedlineEvent-backed arms.
        assert_eq!(
            native_widget_event("quoted-insert"),
            Some(ReedlineEvent::QuotedInsert)
        );
        assert_eq!(
            native_widget_event("operate-and-get-next"),
            Some(ReedlineEvent::OperateAndGetNext)
        );
        assert_eq!(
            native_widget_event("overwrite-mode"),
            Some(ReedlineEvent::OverwriteMode(None))
        );
        assert_eq!(
            native_widget_event("start-kbd-macro"),
            Some(ReedlineEvent::StartKbdMacro)
        );
        assert_eq!(
            native_widget_event("end-kbd-macro"),
            Some(ReedlineEvent::EndKbdMacro)
        );
        assert_eq!(
            native_widget_event("call-last-kbd-macro"),
            Some(ReedlineEvent::CallLastKbdMacro)
        );
        assert_eq!(
            native_widget_event("character-search"),
            Some(ReedlineEvent::CharSearch {
                direction: Direction::Forward
            })
        );
        assert_eq!(
            native_widget_event("character-search-backward"),
            Some(ReedlineEvent::CharSearch {
                direction: Direction::Backward
            })
        );
        // bashline.c:491 registers rl_yank_last_arg under the
        // insert-last-argument name; funmap.c carries yank-last-arg.
        for widget in ["yank-last-arg", "insert-last-argument"] {
            assert_eq!(
                native_widget_event(widget),
                Some(ReedlineEvent::YankLastArg { count: None })
            );
        }
        assert_eq!(
            native_widget_event("yank-nth-arg"),
            Some(ReedlineEvent::YankNthArg { count: 1 })
        );
        assert_eq!(
            native_widget_event("digit-argument"),
            Some(ReedlineEvent::DigitArgument(None))
        );
        assert_eq!(
            native_widget_event("universal-argument"),
            Some(ReedlineEvent::UniversalArgument)
        );
        assert_eq!(
            native_widget_event("clear-display"),
            Some(ReedlineEvent::ClearScrollback)
        );
        assert_eq!(
            native_widget_event("edit-and-execute-command"),
            Some(ReedlineEvent::OpenEditor)
        );

        // `insert-comment` is the composite: go to line start, insert
        // `#`, accept the line.
        assert_eq!(
            native_widget_event("insert-comment"),
            Some(ReedlineEvent::Multiple(vec![
                ReedlineEvent::Edit(vec![
                    EditCommand::MoveToLineStart { select: false },
                    EditCommand::InsertString("#".to_string()),
                ]),
                ReedlineEvent::Enter,
            ]))
        );
    }

    /// niubash#731 (K3 widening): GNU readline names the editor could
    /// already express but the bridge still dropped — the remaining
    /// word transforms, the cycling completion, the scrollback clear,
    /// the comment-and-accept composition, the literal tab, and GNU's
    /// own substring-search spellings. `redraw-current-line` (the
    /// #630 K3 note) is pinned here too: it already maps to `Repaint`.
    /// Names with no editor expression (kill-ring rotation, the mark
    /// register, prefix arguments...) keep the sentinel, so the
    /// bridge keeps reporting them instead of mis-delivering.
    #[test]
    fn issue731_native_widget_maps_widened_gnu_names() {
        assert_eq!(
            native_widget_event("capitalize-word"),
            Some(edit_event(EditCommand::CapitalizeWord))
        );
        assert_eq!(
            native_widget_event("transpose-words"),
            Some(edit_event(EditCommand::SwapWords))
        );
        assert_eq!(
            native_widget_event("clear-display"),
            Some(ReedlineEvent::ClearScrollback)
        );
        assert_eq!(
            native_widget_event("redraw-current-line"),
            Some(ReedlineEvent::Repaint)
        );
        assert_eq!(
            native_widget_event("tab-insert"),
            Some(edit_event(EditCommand::InsertChar('\t')))
        );
        assert_eq!(
            native_widget_event("insert-comment"),
            Some(ReedlineEvent::Multiple(vec![
                ReedlineEvent::Edit(vec![
                    EditCommand::MoveToLineStart { select: false },
                    EditCommand::InsertString("#".to_string()),
                ]),
                ReedlineEvent::Enter,
            ]))
        );
        // `menu-complete` shares the completion event, like `complete`.
        match native_widget_event("menu-complete") {
            Some(ReedlineEvent::UntilFound(events)) => {
                assert!(matches!(events.first(), Some(ReedlineEvent::Menu(_))));
            }
            other => panic!("menu-complete must map to the completion event, got {other:?}"),
        }
        assert_eq!(
            native_widget_event("history-substring-search-backward"),
            Some(ReedlineEvent::Up)
        );
        assert_eq!(
            native_widget_event("history-substring-search-forward"),
            Some(ReedlineEvent::Down)
        );
        // The fork's descending-gap batch delivered `set-mark`,
        // `possible-completions`, `insert-completions` and the
        // non-incremental search pair as real events.
        assert_eq!(
            native_widget_event("set-mark"),
            Some(edit_event(EditCommand::SetMark))
        );
        assert_eq!(
            native_widget_event("possible-completions"),
            Some(ReedlineEvent::PossibleCompletions)
        );
        assert_eq!(
            native_widget_event("insert-completions"),
            Some(ReedlineEvent::InsertCompletions)
        );
        assert_eq!(
            native_widget_event("non-incremental-forward-search-history"),
            Some(ReedlineEvent::NonIncrementalSearch {
                direction: Direction::Forward,
                prompt_char: ':',
                pattern: false,
            })
        );
        assert_eq!(
            native_widget_event("non-incremental-reverse-search-history"),
            Some(ReedlineEvent::NonIncrementalSearch {
                direction: Direction::Backward,
                prompt_char: ':',
                pattern: false,
            })
        );
        // `abort` is a host readline command (GNU's rl_abort clears
        // the pending command and dings, never the line), so it rides
        // the readline sentinel rather than the shell-function widget
        // sentinel.
        assert_eq!(
            native_widget_event("abort"),
            Some(ReedlineEvent::ExecuteHostCommand(format!(
                "{}abort",
                READLINE_HOST_COMMAND_PREFIX
            )))
        );
    }

    /// niubash#731: `self-insert` is key-dependent — the bridge
    /// delivers it as inserting the bound key's own character (a
    /// control key inserts its control byte), on the flat path and
    /// at the end of a chord alike; a key with no character of its
    /// own stays an actionable drop.
    #[test]
    fn issue731_self_insert_inserts_the_bound_key() {
        use crate::bind_surface::{EngineBindEntry as BindEntry, EngineBindKind as BindKind};
        let function = |keyseq: &str, name: &str| BindEntry {
            keyseq: keyseq.to_string(),
            kind: BindKind::Function {
                name: name.to_string(),
            },
            keymap: None,
        };
        let mut keybindings = default_emacs_keybindings();
        let entries = vec![
            function("x", "self-insert"),
            function(r"\C-a", "self-insert"),
            function(r"\e[5~", "self-insert"),
        ];
        let unusable =
            add_engine_bind_keybindings(&mut keybindings, NativeKeymapTarget::Emacs, &entries);
        assert_eq!(
            keybindings.find_binding(KeyModifiers::NONE, KeyCode::Char('x')),
            Some(edit_event(EditCommand::InsertString("x".to_string())))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('a')),
            Some(edit_event(EditCommand::InsertString("\u{1}".to_string())))
        );
        // PgUp carries no character: recorded, not delivered.
        assert_eq!(unusable.len(), 1);
        assert_eq!(unusable[0].keyseq, r"\e[5~");

        // Chord-final self-insert inserts the chord's last key.
        let chords = collect_chord_bindings(
            &[function(r"\C-xa", "self-insert")],
            NativeKeymapTarget::Emacs,
            0,
        );
        assert_eq!(chords.len(), 1);
        assert_eq!(
            chords[0].event,
            edit_event(EditCommand::InsertString("a".to_string()))
        );
    }

    /// niubash#731: a `quoted-insert` bind is delivered by the chord
    /// layer's quote state — it is neither flat-bound nor recorded
    /// as dropped — and its sequences (single key or chain) are
    /// collected per keymap target.
    #[test]
    fn issue731_quoted_insert_bind_is_collected_not_dropped() {
        use crate::bind_surface::{EngineBindEntry as BindEntry, EngineBindKind as BindKind};
        let function = |keyseq: &str, keymap: Option<&str>, name: &str| BindEntry {
            keyseq: keyseq.to_string(),
            kind: BindKind::Function {
                name: name.to_string(),
            },
            keymap: keymap.map(str::to_string),
        };
        let entries = vec![
            function(r"\C-v", Some("emacs-standard"), "quoted-insert"),
            function(r"\C-x\C-q", None, "quoted-insert"),
            function(r"\C-g", None, "no-such-fn"),
        ];
        let mut keybindings = default_emacs_keybindings();
        let unusable =
            add_engine_bind_keybindings(&mut keybindings, NativeKeymapTarget::Emacs, &entries);
        assert_eq!(
            unusable,
            vec![UnusableBind {
                keyseq: r"\C-g".to_string(),
                reason: UnusableBindReason::Other,
            }],
            "only the genuinely unknown function is recorded"
        );
        assert_eq!(
            collect_quote_sequences(&entries, NativeKeymapTarget::Emacs),
            vec![
                vec![(KeyModifiers::CONTROL, KeyCode::Char('v'))],
                vec![
                    (KeyModifiers::CONTROL, KeyCode::Char('x')),
                    (KeyModifiers::CONTROL, KeyCode::Char('q')),
                ],
            ]
        );
        // Keymap-targeted collection does not leak across keymaps.
        assert!(collect_quote_sequences(&entries, NativeKeymapTarget::ViInsert).is_empty());
        // And with no quoted-insert binds the layer stays empty.
        let plain = vec![function(r"\C-g", None, "redisplay")];
        assert!(collect_quote_sequences(&plain, NativeKeymapTarget::Emacs).is_empty());
    }

    /// niubash#731: with a quote sequence registered, the quoting
    /// key arms the state and the next key inserts literally — a
    /// bound key inserts its byte instead of firing, and a quoted
    /// ESC is data, not a meta prefix. The state spends itself on
    /// exactly one key.
    #[test]
    fn issue731_quoted_insert_inserts_the_next_key_literally() {
        let inner = Emacs::new(default_emacs_keybindings());
        let mut mode = ChordEditMode::new(
            Box::new(inner),
            Vec::new(),
            HashSet::new(),
            None,
            TEST_KEYSEQ_TIMEOUT,
        )
        .with_quote_sequences(vec![vec![(KeyModifiers::CONTROL, KeyCode::Char('v'))]]);

        // C-v arms; C-a (bound to line-start) inserts its byte.
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Char('v'), KeyModifiers::CONTROL)),
            ReedlineEvent::None
        );
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            edit_event(EditCommand::InsertString("\u{1}".to_string()))
        );
        // Disarmed again: C-a is the inner machine's own binding.
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            edit_event(EditCommand::MoveToLineStart { select: false })
        );
        // A quoted ESC inserts the ESC byte (it is not meta-folded).
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Char('v'), KeyModifiers::CONTROL)),
            ReedlineEvent::None
        );
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Esc, KeyModifiers::NONE)),
            edit_event(EditCommand::InsertString("\u{1b}".to_string()))
        );
        // A quoted key with no character of its own is simply spent.
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Char('v'), KeyModifiers::CONTROL)),
            ReedlineEvent::None
        );
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Up, KeyModifiers::NONE)),
            ReedlineEvent::None
        );
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            edit_event(EditCommand::MoveToLineStart { select: false })
        );
    }

    /// niubash#731 (the #704 review's chord observation ②): the
    /// keyseq-timeout window restarts at each parked key — GNU
    /// waits per key, so a three-key chord typed steadily, each
    /// gap inside the window but the total past it, still fires.
    /// (Under the old first-key accounting the prefix expired
    /// before the third key and the chord never completed.)
    #[test]
    fn issue731_chord_timeout_restarts_at_each_key() {
        use crossterm29::event::{KeyCode as Code, KeyModifiers as Mods};
        let inner = Emacs::new(default_emacs_keybindings());
        let chords = vec![ChordBinding {
            keys: vec![
                (Mods::CONTROL, Code::Char('x')),
                (Mods::NONE, Code::Char('a')),
                (Mods::NONE, Code::Char('b')),
            ],
            event: edit_event(EditCommand::InsertString("TRIPLE".to_string())),
        }];
        let mut mode = ChordEditMode::new(
            Box::new(inner),
            chords,
            HashSet::new(),
            None,
            Duration::from_millis(600),
        );
        assert_eq!(
            mode.parse_event(chord_test_key(Code::Char('x'), Mods::CONTROL)),
            ReedlineEvent::Pending
        );
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            mode.parse_event(chord_test_key(Code::Char('a'), Mods::NONE)),
            ReedlineEvent::Pending
        );
        std::thread::sleep(Duration::from_millis(300));
        // ~700 ms since the first key (past the 600 ms window),
        // ~300 ms since the second: only the per-key deadline lets
        // the chord complete.
        assert_eq!(
            mode.parse_event(chord_test_key(Code::Char('b'), Mods::NONE)),
            edit_event(EditCommand::InsertString("TRIPLE".to_string()))
        );
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
    fn parse_chord_sequence_splits_readline_chains() {
        // bash's `\C-x\C-e` and fzf's `\C-\e(` are chords; single units are
        // the flat keymap's business.
        assert_eq!(
            parse_chord_sequence(r"\C-x\C-e"),
            Some(vec![
                (KeyModifiers::CONTROL, KeyCode::Char('x')),
                (KeyModifiers::CONTROL, KeyCode::Char('e')),
            ])
        );
        assert_eq!(
            parse_chord_sequence(r"\C-\e("),
            // Readline semantics: `\\C-\\` consumes the escape, so this is
            // Ctrl-\\ then the *literal* `e` and `(` — the "impossible to
            // type" marker fzf picked.
            Some(vec![
                (KeyModifiers::CONTROL, KeyCode::Char('\\')),
                (KeyModifiers::NONE, KeyCode::Char('e')),
                (KeyModifiers::NONE, KeyCode::Char('(')),
            ])
        );
        // Terminal-sequence units split correctly.
        assert_eq!(
            parse_chord_sequence(r"\e[1~\C-a"),
            Some(vec![
                (KeyModifiers::NONE, KeyCode::Home),
                (KeyModifiers::CONTROL, KeyCode::Char('a')),
            ])
        );
        // Single key, malformed tail, empty: not chords.
        assert_eq!(parse_chord_sequence(r"\C-x"), None);
        assert_eq!(parse_chord_sequence(r"\C-x\C-"), None);
        assert_eq!(parse_chord_sequence(""), None);
    }

    fn chord_test_key(code: KeyCode, modifiers: KeyModifiers) -> ReedlineRawEvent {
        use crossterm29::event::{Event, KeyEvent};
        ReedlineRawEvent::try_from(Event::Key(KeyEvent::new(code, modifiers))).unwrap()
    }

    const TEST_KEYSEQ_TIMEOUT: Duration = Duration::from_millis(DEFAULT_KEYSEQ_TIMEOUT_MS);

    #[test]
    fn escape_partner_without_a_meta_binding_is_delivered_now() {
        use crossterm29::event::{Event, KeyEvent};
        let inner = Emacs::new(default_emacs_keybindings());
        // Only `c` has an ALT binding, so ESC + Down is not a meta fold.
        let alt: HashSet<KeyCode> = [KeyCode::Char('c')].into_iter().collect();
        let partner = Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let lookahead: Lookahead = Box::new(move |_| Some(partner.clone()));
        let mut mode = ChordEditMode::new(
            Box::new(inner),
            Vec::new(),
            alt,
            Some(lookahead),
            TEST_KEYSEQ_TIMEOUT,
        );

        let event = mode.parse_event(chord_test_key(KeyCode::Esc, KeyModifiers::NONE));
        match event {
            ReedlineEvent::Multiple(events) => assert_eq!(
                events.len(),
                2,
                "ESC and the peeked key must both be delivered at once"
            ),
            other => panic!("expected ESC + Down as one batch, got {other:?}"),
        }
    }

    #[test]
    fn a_key_that_is_both_binding_and_prefix_waits_for_the_chord() {
        let mut keybindings = default_emacs_keybindings();
        keybindings.add_binding(
            KeyModifiers::CONTROL,
            KeyCode::Char('x'),
            edit_event(EditCommand::InsertString("SINGLE".to_string())),
        );
        let inner = Emacs::new(keybindings);
        let chords = vec![ChordBinding {
            keys: vec![
                (KeyModifiers::CONTROL, KeyCode::Char('x')),
                (KeyModifiers::NONE, KeyCode::Char('z')),
            ],
            event: edit_event(EditCommand::InsertString("QUICK".to_string())),
        }];
        let mut mode = ChordEditMode::new(
            Box::new(inner),
            chords,
            HashSet::new(),
            None,
            TEST_KEYSEQ_TIMEOUT,
        );

        // The key is parked even though it has its own binding.
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            ReedlineEvent::Pending
        );
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Char('z'), KeyModifiers::NONE)),
            edit_event(EditCommand::InsertString("QUICK".to_string()))
        );

        // Past the timeout the single binding fires, then the key stands alone.
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            ReedlineEvent::Pending
        );
        std::thread::sleep(TEST_KEYSEQ_TIMEOUT + std::time::Duration::from_millis(50));
        match mode.parse_event(chord_test_key(KeyCode::Char('z'), KeyModifiers::NONE)) {
            ReedlineEvent::Multiple(events) => assert!(
                events.contains(&edit_event(EditCommand::InsertString("SINGLE".to_string()))),
                "the expired prefix replays its own binding: {events:?}"
            ),
            other => panic!("expected the single binding then the key, got {other:?}"),
        }
    }

    #[test]
    fn chord_prefix_expires_after_the_keyseq_timeout() {
        use crossterm29::event::{Event, KeyCode as Code, KeyEvent, KeyModifiers as Mods};
        let inner = Emacs::new(default_emacs_keybindings());
        let chords = vec![ChordBinding {
            keys: vec![
                (Mods::CONTROL, Code::Char('x')),
                (Mods::NONE, Code::Char('z')),
            ],
            event: edit_event(EditCommand::InsertString("QUICK".to_string())),
        }];
        let mut mode = ChordEditMode::new(
            Box::new(inner),
            chords,
            HashSet::new(),
            None,
            TEST_KEYSEQ_TIMEOUT,
        );
        let key = |code, modifiers| {
            ReedlineRawEvent::try_from(Event::Key(KeyEvent::new(code, modifiers))).unwrap()
        };
        // Inside the window: parked, then completed.
        assert_eq!(
            mode.parse_event(key(Code::Char('x'), Mods::CONTROL)),
            ReedlineEvent::Pending
        );
        assert_eq!(
            mode.parse_event(key(Code::Char('z'), Mods::NONE)),
            edit_event(EditCommand::InsertString("QUICK".to_string()))
        );
        // Past the window: the prefix is replayed on its own, the key is fresh.
        assert_eq!(
            mode.parse_event(key(Code::Char('x'), Mods::CONTROL)),
            ReedlineEvent::Pending
        );
        std::thread::sleep(TEST_KEYSEQ_TIMEOUT + std::time::Duration::from_millis(50));
        let event = mode.parse_event(key(Code::Char('z'), Mods::NONE));
        assert_ne!(
            event,
            edit_event(EditCommand::InsertString("QUICK".to_string())),
            "an expired prefix must not complete a chord"
        );
    }

    #[test]
    fn chord_index_matches_exact_prefix_and_mismatch() {
        let chord = |keys: &[(KeyModifiers, KeyCode)], text: &str| ChordBinding {
            keys: keys.to_vec(),
            event: edit_event(EditCommand::InsertString(text.to_string())),
        };
        let ctrl_x = (KeyModifiers::CONTROL, KeyCode::Char('x'));
        let z = (KeyModifiers::NONE, KeyCode::Char('z'));
        let e = (KeyModifiers::NONE, KeyCode::Char('e'));
        let ctrl_a = (KeyModifiers::CONTROL, KeyCode::Char('a'));
        let b = (KeyModifiers::NONE, KeyCode::Char('b'));
        let index = ChordIndex::new(&[
            chord(&[ctrl_x, z], "first"),
            // Duplicate sequences keep the first binding, like the old
            // `iter().find()` scan.
            chord(&[ctrl_x, z], "second"),
            chord(&[ctrl_x, z, e], "longer"),
            chord(&[ctrl_a, b], "other"),
        ]);

        assert_eq!(
            index.exact(&[ctrl_x, z]),
            Some(edit_event(EditCommand::InsertString("first".to_string())))
        );
        assert_eq!(
            index.exact(&[ctrl_x, z, e]),
            Some(edit_event(EditCommand::InsertString("longer".to_string())))
        );
        assert_eq!(index.exact(&[ctrl_x]), None, "only full sequences match");
        assert_eq!(index.exact(&[ctrl_x, z, e, z]), None);

        assert!(index.is_prefix(&[ctrl_x]));
        assert!(
            index.is_prefix(&[]),
            "the empty prefix keeps the old scan's edge case"
        );
        assert!(
            index.is_prefix(&[ctrl_x, z]),
            "an exact sequence that prefixes a longer chord still waits"
        );
        assert!(index.is_prefix(&[ctrl_a]));
        assert!(
            !index.is_prefix(&[ctrl_x, z, e]),
            "a leaf chord is not a prefix"
        );
        assert!(!index.is_prefix(&[ctrl_x, b]), "mismatched tail");
        assert!(!index.is_prefix(&[e]), "unrelated key");
    }

    #[test]
    fn zero_keyseq_timeout_waits_forever() {
        let inner = Emacs::new(default_emacs_keybindings());
        let chords = vec![ChordBinding {
            keys: vec![
                (KeyModifiers::CONTROL, KeyCode::Char('x')),
                (KeyModifiers::NONE, KeyCode::Char('z')),
            ],
            event: edit_event(EditCommand::InsertString("QUICK".to_string())),
        }];
        let mut mode = ChordEditMode::new(
            Box::new(inner),
            chords,
            HashSet::new(),
            None,
            Duration::ZERO,
        );
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            ReedlineEvent::Pending
        );
        std::thread::sleep(TEST_KEYSEQ_TIMEOUT + std::time::Duration::from_millis(50));
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Char('z'), KeyModifiers::NONE)),
            edit_event(EditCommand::InsertString("QUICK".to_string())),
            "0 is readline's wait-forever spelling"
        );
    }

    #[test]
    fn custom_keyseq_timeout_replaces_the_default_window() {
        let inner = Emacs::new(default_emacs_keybindings());
        let chords = vec![ChordBinding {
            keys: vec![
                (KeyModifiers::CONTROL, KeyCode::Char('x')),
                (KeyModifiers::NONE, KeyCode::Char('z')),
            ],
            event: edit_event(EditCommand::InsertString("QUICK".to_string())),
        }];
        let mut mode = ChordEditMode::new(
            Box::new(inner),
            chords,
            HashSet::new(),
            None,
            Duration::from_millis(40),
        );
        assert_eq!(
            mode.parse_event(chord_test_key(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            ReedlineEvent::Pending
        );
        std::thread::sleep(Duration::from_millis(80));
        let event = mode.parse_event(chord_test_key(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_ne!(
            event,
            edit_event(EditCommand::InsertString("QUICK".to_string())),
            "the configured 40 ms window must expire before 80 ms"
        );
    }

    #[test]
    fn collect_chord_bindings_carries_events_and_bindx_indices() {
        use crate::bind_surface::{EngineBindEntry as BindEntry, EngineBindKind as BindKind};
        let entries = vec![
            BindEntry {
                keyseq: r"\C-t".to_string(),
                kind: BindKind::Execute {
                    command: "flat".to_string(),
                },
                keymap: None,
            },
            BindEntry {
                keyseq: r"\C-x\C-e".to_string(),
                kind: BindKind::Execute {
                    command: "chord".to_string(),
                },
                keymap: None,
            },
            BindEntry {
                keyseq: r"\C-x\C-r".to_string(),
                kind: BindKind::Macro {
                    text: "reload".to_string(),
                },
                keymap: None,
            },
        ];
        let chords = collect_chord_bindings(&entries, NativeKeymapTarget::Emacs, 0);
        assert_eq!(chords.len(), 2);
        assert_eq!(
            chords[0].event,
            ReedlineEvent::ExecuteHostCommand(format!("{BINDX_HOST_COMMAND_PREFIX}1"))
        );
        assert_eq!(
            chords[1].event,
            edit_event(EditCommand::InsertString("reload".to_string()))
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
            crate::bind_surface::EngineBindEntry {
                keyseq: r#"\C-r"#.to_string(),
                kind: crate::bind_surface::EngineBindKind::Execute {
                    command: "__fzf_history__".to_string(),
                },
                keymap: Some("emacs-standard".to_string()),
            },
            crate::bind_surface::EngineBindEntry {
                keyseq: r#"\C-t"#.to_string(),
                kind: crate::bind_surface::EngineBindKind::Macro {
                    text: "ins-macro".to_string(),
                },
                keymap: None,
            },
            crate::bind_surface::EngineBindEntry {
                keyseq: r#"\C-o"#.to_string(),
                kind: crate::bind_surface::EngineBindKind::Function {
                    name: "accept-line".to_string(),
                },
                keymap: None,
            },
            // Unknown readline function name: GNU silently drops it
            // (wt100 probe), so the bridge reports it unusable.
            crate::bind_surface::EngineBindEntry {
                keyseq: r#"\C-g"#.to_string(),
                kind: crate::bind_surface::EngineBindKind::Function {
                    name: "no-such-fn".to_string(),
                },
                keymap: None,
            },
            // Multi-key sequence: reedline cannot represent it.
            crate::bind_surface::EngineBindEntry {
                keyseq: r#"\C-x\C-f"#.to_string(),
                kind: crate::bind_surface::EngineBindKind::Execute {
                    command: "multi".to_string(),
                },
                keymap: None,
            },
            // vi-insert keymap entry must not land on the emacs keymap
            // (\C-6 is unbound in the emacs defaults, so a hit would be
            // ours and ours alone).
            crate::bind_surface::EngineBindEntry {
                keyseq: r#"\C-6"#.to_string(),
                kind: crate::bind_surface::EngineBindKind::Execute {
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
            vec![UnusableBind {
                keyseq: r"\C-g".to_string(),
                reason: UnusableBindReason::Other,
            }],
            "the chord entry is delivered by the chord layer, not dropped"
        );
    }

    /// The keyseqs out of an unusable list that the bridge would
    /// actually print as startup warnings (niubash#612).
    fn warned_keyseqs(unusable: &[UnusableBind]) -> Vec<&str> {
        unusable
            .iter()
            .filter(|bind| !bind.reason.is_structural())
            .map(|bind| bind.keyseq.as_str())
            .collect()
    }

    // niubash#612, motivating instance: the two workaround shapes in
    // fzf's `--bash` script (a chained-sequence bind and the `\C-z`
    // editing-mode toggles) are recorded with structural reasons and
    // produce no warning, while the same script's working `bind -x`
    // entries still mirror.
    #[test]
    fn issue612_fzf_workaround_binds_drop_silently() {
        use crate::bind_surface::{EngineBindEntry as BindEntry, EngineBindKind as BindKind};
        let function = |keyseq: &str, keymap: &str, name: &str| BindEntry {
            keyseq: keyseq.to_string(),
            kind: BindKind::Function {
                name: name.to_string(),
            },
            keymap: Some(keymap.to_string()),
        };
        // Verbatim from fzf 0.74.5 `shell/key-bindings.bash`.
        let workarounds = vec![
            function(r"\C-\e(", "emacs-standard", "redraw-current-line"),
            function(r"\C-z", "vi-command", "emacs-editing-mode"),
            function(r"\C-z", "vi-insert", "emacs-editing-mode"),
            function(r"\C-z", "emacs-standard", "vi-editing-mode"),
        ];
        // Nothing in fzf's workaround set is dropped any more: the `\C-z`
        // toggles map onto `SwitchMode`, and the `\C-\e(` chord is
        // delivered by `ChordEditMode`.

        for (target, mut keybindings, expected) in [
            (
                NativeKeymapTarget::Emacs,
                default_emacs_keybindings(),
                Vec::new(),
            ),
            (
                NativeKeymapTarget::ViInsert,
                default_vi_insert_keybindings(),
                Vec::new(),
            ),
            (
                NativeKeymapTarget::ViNormal,
                default_vi_normal_keybindings(),
                Vec::new(),
            ),
        ] {
            let unusable = add_engine_bind_keybindings(&mut keybindings, target, &workarounds);
            assert_eq!(unusable, expected, "recorded list on {target:?}");
            assert_eq!(
                warned_keyseqs(&unusable),
                Vec::<&str>::new(),
                "no warnings on {target:?}"
            );
        }

        // The same script's working binds are untouched: they land as
        // bindx host-command triggers at their registry indices.
        let mut keybindings = default_emacs_keybindings();
        let mut entries = vec![
            BindEntry {
                keyseq: r"\C-t".to_string(),
                kind: BindKind::Execute {
                    command: "fzf-file-widget".to_string(),
                },
                keymap: Some("emacs-standard".to_string()),
            },
            BindEntry {
                keyseq: r"\C-r".to_string(),
                kind: BindKind::Execute {
                    command: "__fzf_history__".to_string(),
                },
                keymap: Some("emacs-standard".to_string()),
            },
            BindEntry {
                keyseq: r"\ec".to_string(),
                kind: BindKind::Execute {
                    command: "__fzf_cd__".to_string(),
                },
                keymap: Some("emacs-standard".to_string()),
            },
        ];
        entries.extend(workarounds);
        let unusable =
            add_engine_bind_keybindings(&mut keybindings, NativeKeymapTarget::Emacs, &entries);
        assert_eq!(warned_keyseqs(&unusable), Vec::<&str>::new());
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('t')),
            Some(ReedlineEvent::ExecuteHostCommand(
                "__niu_bindx 0".to_string()
            ))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::CONTROL, KeyCode::Char('r')),
            Some(ReedlineEvent::ExecuteHostCommand(
                "__niu_bindx 1".to_string()
            ))
        );
        assert_eq!(
            keybindings.find_binding(KeyModifiers::ALT, KeyCode::Char('c')),
            Some(ReedlineEvent::ExecuteHostCommand(
                "__niu_bindx 2".to_string()
            ))
        );
    }

    // niubash#630 closed both structural classes: chained sequences are
    // delivered by `ChordEditMode` (the chord layer), editing-mode toggles
    // by `SwitchMode`, and `redraw-current-line` maps to `Repaint`. What
    // still warns is exactly what a user can act on: an unknown function
    // and a malformed sequence.
    #[test]
    fn issue612_structural_classes_are_gone() {
        use crate::bind_surface::{EngineBindEntry as BindEntry, EngineBindKind as BindKind};
        let mut keybindings = default_emacs_keybindings();
        let entries = vec![
            // A chord: delivered by the chord layer, never dropped.
            BindEntry {
                keyseq: r"\C-x\C-a".to_string(),
                kind: BindKind::Execute {
                    command: "hypothetical-tool-cmd".to_string(),
                },
                keymap: None,
            },
            // A chord macro, same.
            BindEntry {
                keyseq: r"\C-b\C-b".to_string(),
                kind: BindKind::Macro {
                    text: "xx".to_string(),
                },
                keymap: Some("emacs-standard".to_string()),
            },
            // A mode toggle: maps to SwitchMode.
            BindEntry {
                keyseq: r"\C-q".to_string(),
                kind: BindKind::Function {
                    name: "vi-editing-mode".to_string(),
                },
                keymap: None,
            },
            // A deliverable readline function: maps to Repaint.
            BindEntry {
                keyseq: r"\C-y".to_string(),
                kind: BindKind::Function {
                    name: "redraw-current-line".to_string(),
                },
                keymap: Some("emacs-standard".to_string()),
            },
            // Unknown function name: actionable, still warns.
            BindEntry {
                keyseq: r"\C-g".to_string(),
                kind: BindKind::Function {
                    name: "no-such-fn".to_string(),
                },
                keymap: None,
            },
            // Malformed sequence (dangling modifier): still warns.
            BindEntry {
                keyseq: r"\C-".to_string(),
                kind: BindKind::Execute {
                    command: "broken".to_string(),
                },
                keymap: None,
            },
        ];
        let unusable =
            add_engine_bind_keybindings(&mut keybindings, NativeKeymapTarget::Emacs, &entries);
        let reasons: Vec<UnusableBindReason> = unusable.iter().map(|bind| bind.reason).collect();
        assert_eq!(
            reasons,
            vec![UnusableBindReason::Other, UnusableBindReason::Other]
        );
        assert_eq!(warned_keyseqs(&unusable), vec![r"\C-g", r"\C-"]);
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

    // niubash#630, Appendix A snapshot-equivalence pin: with no
    // engine binds and no precedence overlap, the layered registry
    // build must produce exactly the keymap the pre-registry build
    // order produced — the refactor is a move, not a behavior change.
    #[test]
    fn issue630_layered_build_matches_legacy_order() {
        let config = NativeWidgetConfig {
            enabled: true,
            presets: vec!["autosuggestions".to_string()],
            import_bindkeys: true,
        };
        let bundle = vec![native_widget_binding("^ ", None, "autosuggest-accept")];
        let user = vec![native_widget_binding("^F", None, "autosuggest-accept")];
        for (defaults, target, mode) in [
            (
                default_emacs_keybindings(),
                NativeKeymapTarget::Emacs,
                crate::keymap_registry::KeymapMode::Emacs,
            ),
            (
                default_vi_insert_keybindings(),
                NativeKeymapTarget::ViInsert,
                crate::keymap_registry::KeymapMode::ViInsert,
            ),
            (
                default_vi_normal_keybindings(),
                NativeKeymapTarget::ViNormal,
                crate::keymap_registry::KeymapMode::ViNormal,
            ),
        ] {
            // The pre-registry build order, verbatim, plus the
            // niubash#765/#766/#767 host-command defaults (a host
            // baseline layer the registry build gained with this
            // batch; the order relative to the other baseline
            // layers is the snapshot pin).
            let mut legacy = defaults.clone();
            add_menu_keybindings(&mut legacy, CompletionInteraction::Insert);
            add_system_clipboard_keybindings(&mut legacy);
            add_gnu_command_default_keybindings(&mut legacy, target);
            add_native_widget_keybindings(&mut legacy, target, &config, &bundle);
            add_bundle_widget_keybindings(&mut legacy, target, &config, &bundle);
            add_user_widget_keybindings(&mut legacy, &user);
            add_engine_bind_keybindings(&mut legacy, target, &[]);

            let (built, entries, dropped) = build_keymap_for_target(
                defaults,
                target,
                mode,
                &config,
                &bundle,
                &user,
                &[],
                CompletionInteraction::Insert,
            );
            assert_eq!(
                built.get_keybindings(),
                legacy.get_keybindings(),
                "keymap drift on {target:?}"
            );
            assert!(dropped.is_empty(), "no drops without engine binds");
            assert_eq!(
                entries.len(),
                built.get_keybindings().len(),
                "the registry lists exactly the effective table on {target:?}"
            );
        }
    }

    // niubash#630, the one deliberate precedence change (DESIGN §5.3):
    // a user binding (NIU_BINDKEYS) now beats a script `bind` on the
    // same key — before the registry build, the engine pass ran last
    // and silently overrode the user. The registry records the win
    // and what it shadowed.
    #[test]
    fn issue630_user_binding_beats_script_binding() {
        use crate::bind_surface::{EngineBindEntry as BindEntry, EngineBindKind as BindKind};
        let config = NativeWidgetConfig::default();
        let engine = vec![BindEntry {
            keyseq: r"\C-g".to_string(),
            kind: BindKind::Execute {
                command: "script-widget".to_string(),
            },
            keymap: None,
        }];
        let user = vec![native_widget_binding("^G", None, "accept-line")];
        let (built, entries, dropped) = build_keymap_for_target(
            default_emacs_keybindings(),
            NativeKeymapTarget::Emacs,
            crate::keymap_registry::KeymapMode::Emacs,
            &config,
            &[],
            &user,
            &engine,
            CompletionInteraction::Insert,
        );
        assert!(dropped.is_empty());
        assert_eq!(
            built.find_binding(KeyModifiers::CONTROL, KeyCode::Char('g')),
            Some(ReedlineEvent::Enter),
            "the user's binding wins the slot"
        );
        let row = entries
            .iter()
            .find(|entry| entry.key_display == "\"\\C-g\"")
            .expect("registry row for the contested key");
        assert_eq!(
            row.source,
            crate::keymap_registry::KeymapSource::User {
                via: crate::keymap_registry::UserBindingVia::NiuBindkeys
            }
        );
        assert_eq!(
            row.shadowed,
            Some(crate::keymap_registry::KeymapSource::Script)
        );
        assert_eq!(row.action, "accept line");
    }

    // niubash#630: the recorded half of the bridge — dropped records
    // classify by shape (DESIGN §6.1), and a key-script macro is
    // recorded as mirrored-with-loss while its translation stays the
    // pre-K3 literal insert.
    #[test]
    fn issue630_dropped_records_classify_by_shape() {
        use crate::bind_surface::{EngineBindEntry as BindEntry, EngineBindKind as BindKind};
        use crate::keymap_registry::{DroppedKind, DroppedReason};
        let entry = |keyseq: &str, kind: BindKind| BindEntry {
            keyseq: keyseq.to_string(),
            kind,
            keymap: None,
        };
        let engine = vec![
            entry(
                r"\C-\e(",
                BindKind::Function {
                    name: "redraw-current-line".to_string(),
                },
            ),
            entry(
                r"\C-z",
                BindKind::Function {
                    name: "vi-editing-mode".to_string(),
                },
            ),
            // zoxide's response-sequence trick (DESIGN §3 #5).
            entry(
                r"\e[0n",
                BindKind::Execute {
                    command: "__zoxide_hook".to_string(),
                },
            ),
            entry(
                r"\C-g",
                BindKind::Function {
                    name: "no-such-readline-function".to_string(),
                },
            ),
            // fzf's Alt-C macro (DESIGN §3 #7): control escapes and a
            // command substitution — a key script, not text.
            entry(
                r"\ec",
                BindKind::Macro {
                    text: r" \C-b\C-k \C-u`__fzf_cd__`\e\C-e".to_string(),
                },
            ),
            // The same macro in its stored form: the engine has
            // already translated the escapes to raw control bytes.
            entry(
                r"\C-x",
                BindKind::Macro {
                    text: " \u{2}\u{b} __fzf_cd__".to_string(),
                },
            ),
        ];
        let (built, _entries, dropped) = build_keymap_for_target(
            default_emacs_keybindings(),
            NativeKeymapTarget::Emacs,
            crate::keymap_registry::KeymapMode::Emacs,
            &NativeWidgetConfig::default(),
            &[],
            &[],
            &engine,
            CompletionInteraction::Insert,
        );
        let shapes: Vec<(&str, DroppedReason)> = dropped
            .iter()
            .map(|bind| (bind.keyseq.as_str(), bind.reason))
            .collect();
        assert_eq!(
            shapes,
            vec![
                (r"\e[0n", DroppedReason::ResponseSequence),
                (r"\C-g", DroppedReason::UnknownFunction),
            ],
            "the chord, the mode toggle and both macros are delivered now"
        );
        assert_eq!(dropped[0].kind, DroppedKind::Execute);
        // K3: a script macro is replay, not literal text. The written
        // `\ec` form decodes to the same bytes the stored form carries:
        // space, C-b, C-k, C-u, `__fzf_cd__`, Alt-Ctrl-e — a
        // `KeySequence`, not an InsertString.
        let alt_c = built
            .find_binding(KeyModifiers::ALT, KeyCode::Char('c'))
            .expect("the fzf macro stays bound");
        let ReedlineEvent::KeySequence(keys) = alt_c else {
            panic!("the script macro replays, it does not insert text: {alt_c:?}")
        };
        assert!(keys.iter().any(|key| {
            key.modifier == KeyModifiers::CONTROL && key.key_code == KeyCode::Char('b')
        }));
        // The stored form (raw control bytes) decodes identically.
        let ctrl_x = built
            .find_binding(KeyModifiers::CONTROL, KeyCode::Char('x'))
            .expect("the stored-form macro stays bound");
        assert!(
            matches!(ctrl_x, ReedlineEvent::KeySequence(_)),
            "raw-byte macros replay too: {ctrl_x:?}"
        );
    }

    #[test]
    fn k3_script_macro_decodes_keystrokes() {
        use crate::bind_surface::{EngineBindEntry as BindEntry, EngineBindKind as BindKind};
        // fzf Alt-C, stored form: " \x02\x0b \x15`__fzf_cd__`\x1b\x05\x1br\r"
        // = space C-b C-k space C-u `__fzf_cd__` M-C-e M-r Enter.
        let entry = BindEntry {
            keyseq: r"\ec".to_string(),
            kind: BindKind::Macro {
                text: " \u{2}\u{b} \u{15}`__fzf_cd__`\u{1b}\u{5}\u{1b}r\r".to_string(),
            },
            keymap: None,
        };
        let (built, _, dropped) = build_keymap_for_target(
            default_emacs_keybindings(),
            NativeKeymapTarget::Emacs,
            crate::keymap_registry::KeymapMode::Emacs,
            &NativeWidgetConfig::default(),
            &[],
            &[],
            std::slice::from_ref(&entry),
            CompletionInteraction::Insert,
        );
        assert!(dropped.is_empty(), "a replayable macro is not dropped");
        let ReedlineEvent::KeySequence(keys) = built
            .find_binding(KeyModifiers::ALT, KeyCode::Char('c'))
            .expect("bound")
        else {
            panic!("expected KeySequence")
        };
        let at = |i: usize| (keys[i].modifier, keys[i].key_code);
        assert_eq!(at(0), (KeyModifiers::NONE, KeyCode::Char(' ')));
        assert_eq!(at(1), (KeyModifiers::CONTROL, KeyCode::Char('b')));
        assert_eq!(at(2), (KeyModifiers::CONTROL, KeyCode::Char('k')));
        assert_eq!(at(4), (KeyModifiers::CONTROL, KeyCode::Char('u')));
        // `__fzf_cd__` is literal text keys, then Alt+Ctrl-e, Alt-r, Enter.
        assert_eq!(
            *keys.last().unwrap(),
            KeyCombination {
                modifier: KeyModifiers::NONE,
                key_code: KeyCode::Enter
            }
        );
        assert!(keys.iter().any(|key| key
            == &KeyCombination {
                modifier: KeyModifiers::CONTROL | KeyModifiers::ALT,
                key_code: KeyCode::Char('e')
            }));
    }

    // niubash#630, K4: the `[keymap].bind` rule parser takes the bind
    // builtin's own argument strings — plain, macro, -x and -m forms —
    // through the engine's parsers (no second grammar), and refuses
    // everything that cannot register a binding, with the reason.
    #[test]
    fn issue630_keymap_bind_rule_parser_forms() {
        use crate::bind_surface::EngineBindKind as BindKind;
        let entry = parse_keymap_bind_rule(r#""\C-g": redisplay"#).expect("function form");
        assert_eq!(entry.keyseq, r"\C-g");
        assert_eq!(
            entry.kind,
            BindKind::Function {
                name: "redisplay".to_string()
            }
        );
        assert_eq!(entry.keymap, None);

        let entry = parse_keymap_bind_rule(r#""\C-g": "ins-text""#).expect("macro form");
        assert_eq!(
            entry.kind,
            BindKind::Macro {
                text: "ins-text".to_string()
            }
        );

        let entry = parse_keymap_bind_rule(r#"-x "\C-t": my-cmd"#).expect("-x form");
        assert_eq!(entry.keyseq, r"\C-t");
        assert_eq!(
            entry.kind,
            BindKind::Execute {
                command: "my-cmd".to_string()
            }
        );

        let entry = parse_keymap_bind_rule(r#"-m vi-insert "\C-g": redisplay"#).expect("-m form");
        assert_eq!(entry.keymap.as_deref(), Some("vi-insert"));

        let entry = parse_keymap_bind_rule(r#"-m emacs-standard -x "\C-r": hist-widget"#)
            .expect("-m + -x form");
        assert_eq!(entry.keymap.as_deref(), Some("emacs-standard"));
        assert_eq!(
            entry.kind,
            BindKind::Execute {
                command: "hist-widget".to_string()
            }
        );

        // Refusals: each names why; none of them parses partially.
        for (spec, needle) in [
            ("", "empty"),
            ("hello", "missing separator"),
            (r#"-r "\C-t""#, "unbind"),
            ("-p", "unsupported bind option"),
            (
                r#"-m no-such-keymap "\C-g": redisplay"#,
                "invalid keymap name",
            ),
            ("set completion-ignore-case on", "readline variables"),
            (r#""\C-g""#, "no key sequence terminator"),
        ] {
            let err = parse_keymap_bind_rule(spec).expect_err("must refuse");
            assert!(
                err.contains(needle),
                "spec {spec:?} refused with {err:?}, expected a mention of {needle:?}"
            );
        }

        // The unbind parser: one key, in bind spelling, quoted or bare;
        // a chain names no single key and is refused whole.
        assert_eq!(
            parse_keymap_unbind_key(r#""\C-t""#),
            Ok((KeyModifiers::CONTROL, KeyCode::Char('t')))
        );
        assert_eq!(
            parse_keymap_unbind_key(r"\C-t"),
            Ok((KeyModifiers::CONTROL, KeyCode::Char('t')))
        );
        assert!(parse_keymap_unbind_key(r#""\C-x\C-f""#).is_err());
        assert!(parse_keymap_unbind_key("").is_err());
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

    /// niubash#631: a detector carrying a cycle session cell, plus the
    /// cell itself so a test can publish the session state the
    /// CycleMenu would have published.
    fn cycle_detector() -> (PasteChunkEditMode, SharedCycleSession) {
        let (detector, _queue) =
            paste_detector_and_queue_for_style(Vec::new(), CompletionStyle::Column);
        let session: SharedCycleSession = std::sync::Arc::new(std::sync::Mutex::new(
            crate::completion::cycle_menu::CycleSession::default(),
        ));
        (detector.with_cycle_session(Some(session.clone())), session)
    }

    /// Publish a live, spliced cycle session over the line `git ch`
    /// (the state the CycleMenu leaves after TAB #1 on `git ch`).
    fn arm_cycle_session(session: &SharedCycleSession, spliced: bool) {
        let mut state = session.lock().unwrap();
        state.open = true;
        state.snapshot = Some(crate::completion::cycle_menu::CycleSnapshot {
            buffer: "git ch".to_string(),
            cursor: 6,
        });
        state.spliced = spliced;
    }

    fn esc() -> ReedlineRawEvent {
        paste_key(
            crossterm29::event::KeyCode::Esc,
            crossterm29::event::KeyModifiers::NONE,
        )
    }

    #[test]
    fn issue631_tab_binding_shape_follows_the_interaction_axis() {
        // insert: the pre-#631 binding, byte for byte — open the menu,
        // falling back to the engine's Complete edit.
        let mut insert_map = default_emacs_keybindings();
        add_menu_keybindings(&mut insert_map, CompletionInteraction::Insert);
        assert_eq!(
            insert_map.find_binding(KeyModifiers::NONE, KeyCode::Tab),
            Some(ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu(COMPLETION_MENU.to_string()),
                ReedlineEvent::Edit(vec![EditCommand::Complete]),
            ]))
        );
        // cycle: TAB is MenuNext first (advance an open menu), falling
        // through to opening the menu when none is open.
        let mut cycle_map = default_emacs_keybindings();
        add_menu_keybindings(&mut cycle_map, CompletionInteraction::Cycle);
        assert_eq!(
            cycle_map.find_binding(KeyModifiers::NONE, KeyCode::Tab),
            Some(ReedlineEvent::UntilFound(vec![
                ReedlineEvent::MenuNext,
                ReedlineEvent::Menu(COMPLETION_MENU.to_string()),
            ]))
        );
        // Shift-TAB navigates backwards under both interactions.
        for map in [insert_map, cycle_map] {
            assert_eq!(
                map.find_binding(KeyModifiers::SHIFT, KeyCode::BackTab),
                Some(ReedlineEvent::MenuPrevious)
            );
        }
    }

    #[test]
    fn issue631_wrapper_esc_rolls_back_a_spliced_cycle_session() {
        let (mut detector, session) = cycle_detector();
        arm_cycle_session(&session, true);
        detector.parse_event(tab());
        let event = detector.parse_event(esc());
        match &event {
            ReedlineEvent::Multiple(events) => {
                assert_eq!(events.len(), 2, "{event:?}");
                // Restore first, then the Esc that closes the menu.
                assert_eq!(
                    events[0],
                    ReedlineEvent::Edit(crate::completion::cycle_menu::rollback_edits(
                        &crate::completion::cycle_menu::CycleSnapshot {
                            buffer: "git ch".to_string(),
                            cursor: 6,
                        },
                    ))
                );
                assert!(matches!(&events[1], ReedlineEvent::Esc));
            }
            other => panic!("expected rollback + Esc, got {other:?}"),
        }
        // The session was consumed: no second rollback can fire.
        assert!(!session.lock().unwrap().is_open());
    }

    #[test]
    fn issue631_wrapper_esc_without_a_splice_stays_a_plain_esc() {
        let (mut detector, session) = cycle_detector();
        arm_cycle_session(&session, false);
        detector.parse_event(tab());
        let event = detector.parse_event(esc());
        assert!(
            matches!(event, ReedlineEvent::Esc),
            "nothing was spliced, so nothing rolls back: {event:?}"
        );
        assert!(!session.lock().unwrap().is_open());
    }

    #[test]
    fn issue631_wrapper_enter_accepts_and_submits_inside_a_live_cycle_session() {
        let (mut detector, session) = cycle_detector();
        arm_cycle_session(&session, true);
        detector.parse_event(tab());
        let event = detector.parse_event(cr());
        match &event {
            ReedlineEvent::Multiple(events) => {
                assert_eq!(events.len(), 2, "{event:?}");
                assert!(matches!(&events[0], ReedlineEvent::MenuAccept));
                assert!(matches!(&events[1], ReedlineEvent::Enter));
            }
            other => panic!("expected accept + submit, got {other:?}"),
        }

        // No live session (cell attached but closed): the Enter stays
        // bare — the #486 first-refusal shape decides, not a wrap.
        let (mut detector, session) = cycle_detector();
        assert!(!session.lock().unwrap().is_open());
        detector.parse_event(tab());
        let event = detector.parse_event(cr());
        assert!(
            matches!(event, ReedlineEvent::Enter),
            "no live cycle session, no accept wrap: {event:?}"
        );
    }

    /// Publish a live cycle session the way the CycleMenu leaves it
    /// after TAB #1 on `git ch`: candidates frozen, candidate 1 in
    /// the line (region 4..12), snapshot span 4..6.
    fn arm_cycle_session_with_candidates(session: &SharedCycleSession) {
        use crate::completion::cycle_menu::CycleCandidate;
        let mut state = session.lock().unwrap();
        state.open = true;
        state.snapshot = Some(crate::completion::cycle_menu::CycleSnapshot {
            buffer: "git ch".to_string(),
            cursor: 6,
        });
        state.spliced = true;
        state.candidates = vec![
            CycleCandidate {
                value: "checkout".to_string(),
                append_whitespace: false,
            },
            CycleCandidate {
                value: "cherry".to_string(),
                append_whitespace: false,
            },
        ];
        state.span = (4, 6);
        state.index = 0;
        state.region = (4, 12);
        state.nav_seq = 0;
    }

    #[test]
    fn issue631_wrapper_tab_issues_the_navigation_splice_itself() {
        let (mut detector, session) = cycle_detector();
        arm_cycle_session_with_candidates(&session);

        // TAB: the wrapper returns the whole-line splice to candidate
        // 2 directly — no MenuNext reaches the bindings, so the line
        // and the grid highlight can never sit a paint apart.
        let event = detector.parse_event(tab());
        assert_eq!(
            event,
            ReedlineEvent::Edit(crate::completion::cycle_menu::whole_line_edits(
                "git cherry",
                10,
            ))
        );
        {
            let state = session.lock().unwrap();
            assert_eq!(state.index, 1);
            assert_eq!(state.region, (4, 10));
            assert_eq!(state.nav_seq, 1);
        }
        // TAB again wraps to candidate 1; Shift-TAB wraps back.
        let event = detector.parse_event(tab());
        assert_eq!(
            event,
            ReedlineEvent::Edit(crate::completion::cycle_menu::whole_line_edits(
                "git checkout",
                12,
            ))
        );
        let event = detector.parse_event(paste_key(
            crossterm29::event::KeyCode::BackTab,
            crossterm29::event::KeyModifiers::SHIFT,
        ));
        assert_eq!(
            event,
            ReedlineEvent::Edit(crate::completion::cycle_menu::whole_line_edits(
                "git cherry",
                10,
            ))
        );
        let state = session.lock().unwrap();
        assert_eq!(state.index, 1);
        assert_eq!(state.nav_seq, 3);
    }

    #[test]
    fn issue631_wrapper_edit_key_ends_the_cycle_session_as_accepted() {
        let (mut detector, session) = cycle_detector();
        arm_cycle_session(&session, true);
        // A plain character is an edit on the spliced candidate: the
        // session ends (accepted in place) at translation time, so a
        // following Esc is a plain menu-close Esc with no rollback.
        detector.parse_event(paste_key(
            crossterm29::event::KeyCode::Char('x'),
            crossterm29::event::KeyModifiers::NONE,
        ));
        assert!(!session.lock().unwrap().is_open());
        let event = detector.parse_event(esc());
        assert!(
            matches!(event, ReedlineEvent::Esc),
            "the accepted session leaves nothing to roll back: {event:?}"
        );
    }

    /// niubash#778: publish a live cycle session over `git ch` with
    /// four candidates painted in a 2-row packed grid (column-major):
    ///   checkout    cherry-pick
    ///   cherry      chmod
    fn arm_cycle_session_with_grid(session: &SharedCycleSession) {
        use crate::completion::cycle_menu::CycleCandidate;
        let mut state = session.lock().unwrap();
        state.open = true;
        state.snapshot = Some(crate::completion::cycle_menu::CycleSnapshot {
            buffer: "git ch".to_string(),
            cursor: 6,
        });
        state.spliced = true;
        state.candidates = ["checkout", "cherry", "cherry-pick", "chmod"]
            .into_iter()
            .map(|value| CycleCandidate {
                value: value.to_string(),
                append_whitespace: false,
            })
            .collect();
        state.span = (4, 6);
        state.index = 0;
        state.region = (4, 12);
        state.nav_seq = 0;
        state.grid_rows = 2;
    }

    fn arrow(code: crossterm29::event::KeyCode) -> ReedlineRawEvent {
        paste_key(code, crossterm29::event::KeyModifiers::NONE)
    }

    #[test]
    fn issue778_wrapper_arrows_navigate_the_grid_and_keep_the_session() {
        use crossterm29::event::KeyCode;
        let (mut detector, session) = cycle_detector();
        arm_cycle_session_with_grid(&session);
        let edits = |buffer: &str, cursor: usize| {
            ReedlineEvent::Edit(crate::completion::cycle_menu::whole_line_edits(
                buffer, cursor,
            ))
        };

        // Down steps within the column; the wrapper issues the splice
        // itself (line and highlight move in the same frame) and the
        // session stays live — before #778 this key ended it.
        let event = detector.parse_event(arrow(KeyCode::Down));
        assert_eq!(event, edits("git cherry", 10));
        assert!(session.lock().unwrap().is_open());
        // Right steps to the same row of the next column; Left back.
        let event = detector.parse_event(arrow(KeyCode::Right));
        assert_eq!(event, edits("git chmod", 9));
        let event = detector.parse_event(arrow(KeyCode::Left));
        assert_eq!(event, edits("git cherry", 10));
        // Down from the column's bottom wraps to the next column's top.
        let event = detector.parse_event(arrow(KeyCode::Down));
        assert_eq!(event, edits("git cherry-pick", 15));
        // Up from a column's top lands on the previous column's
        // bottom; then within the column; then wraps to the last.
        let event = detector.parse_event(arrow(KeyCode::Up));
        assert_eq!(event, edits("git cherry", 10));
        let event = detector.parse_event(arrow(KeyCode::Up));
        assert_eq!(event, edits("git checkout", 12));
        let event = detector.parse_event(arrow(KeyCode::Up));
        assert_eq!(event, edits("git chmod", 9));

        {
            let state = session.lock().unwrap();
            assert!(state.is_open());
            assert_eq!(state.index, 3);
            assert_eq!(state.nav_seq, 7, "one echo per arrow splice");
        }

        // TAB continues the linear cycle from the arrow's landing.
        let event = detector.parse_event(tab());
        assert_eq!(event, edits("git checkout", 12));
        assert!(session.lock().unwrap().is_open());
    }

    #[test]
    fn issue778_wrapper_arrow_without_a_live_session_falls_through() {
        use crossterm29::event::KeyCode;
        let (mut detector, session) = cycle_detector();
        assert!(!session.lock().unwrap().is_open());
        // No session: the arrow keeps its normal translation (history
        // / cursor motion from the wrapped edit mode) — it is NOT
        // swallowed as a navigation splice.
        let event = detector.parse_event(arrow(KeyCode::Down));
        assert!(
            !matches!(event, ReedlineEvent::Edit(_)),
            "no live session, no navigation splice: {event:?}"
        );
        assert!(!session.lock().unwrap().is_open());
    }

    #[test]
    fn issue778_wrapper_esc_still_rolls_back_after_arrow_navigation() {
        use crossterm29::event::KeyCode;
        let (mut detector, session) = cycle_detector();
        arm_cycle_session_with_grid(&session);
        detector.parse_event(arrow(KeyCode::Down));
        // The session survived the arrow, so Esc still owns it: the
        // rollback edits restore the pre-completion snapshot.
        let event = detector.parse_event(esc());
        match &event {
            ReedlineEvent::Multiple(events) => {
                assert_eq!(
                    events[0],
                    ReedlineEvent::Edit(crate::completion::cycle_menu::rollback_edits(
                        &crate::completion::cycle_menu::CycleSnapshot {
                            buffer: "git ch".to_string(),
                            cursor: 6,
                        },
                    ))
                );
            }
            other => panic!("expected rollback + Esc, got {other:?}"),
        }
        assert!(!session.lock().unwrap().is_open());
    }
}
