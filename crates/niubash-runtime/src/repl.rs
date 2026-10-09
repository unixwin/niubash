//! Reedline REPL loop

use std::cell::RefCell;
use std::rc::Rc;
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
    ColumnarMenu, EditCommand, EditMode, Emacs, KeyCode, KeyModifiers, Keybindings, ListMenu,
    MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch, PromptViMode, Reedline,
    ReedlineEvent, ReedlineMenu, Signal, ValidationResult, Validator, Vi,
};

const COMPLETION_MENU: &str = "completion_menu";
const HISTORY_MENU: &str = "history_menu";

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

/// Extract the arguments of a `self-update` / `update-niubash` REPL command,
/// or `None` when the line is not one.
pub fn self_update_command_args(line: &str) -> Option<Vec<String>> {
    let mut parts = line.trim().split_whitespace();
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
    let history = LiveFileBackedHistory::with_mode(
        shell_ref.history_max_size,
        shell_ref.history_path.clone(),
        shell_ref.history_mode,
    );

    let completer = NiubashCompleter::new(shell_ref.completion_state.clone());
    // Shell-function completions reach the engine through the main-thread
    // bridge installed here; reedline completers must stay `Send`.
    crate::shell::install_completion_bridge(shell);
    let menu_config = shell_ref.menu_config;

    let completion_menu = ReedlineMenu::WithCompleter {
        menu: configured_completion_menu(COMPLETION_MENU, menu_config),
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
        // triaging this issue). On Windows a terminal paste still arrives
        // as per-line key events; Ctrl+V (PasteSystem, bound below) is the
        // one-shot clipboard path until crossterm grows a Windows input
        // parser.
        .use_bracketed_paste(cfg!(not(target_os = "windows")))
        .with_edit_mode(build_edit_mode(
            shell_ref.editor_mode,
            &shell_ref.native_widgets,
            &shell_ref.native_widget_bindings,
            &shell_ref.user_widget_bindings,
            &shell_ref.engine_bindings,
        ));

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
fn configured_completion_menu(name: &str, config: MenuConfig) -> Box<dyn reedline::Menu> {
    match config.completion_style {
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
            Box::new(Vi::new(insert_keybindings, normal_keybindings))
        }
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

fn add_system_clipboard_keybindings(keybindings: &mut Keybindings) {
    keybindings.add_binding(
        KeyModifiers::CONTROL,
        KeyCode::Char('v'),
        ReedlineEvent::Edit(vec![EditCommand::PasteSystem]),
    );
}

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
    match (keymap, target) {
        ("main" | "all", _) => true,
        ("emacs", NativeKeymapTarget::Emacs) => true,
        ("viins", NativeKeymapTarget::ViInsert) => true,
        ("vicmd", NativeKeymapTarget::ViNormal) => true,
        _ => false,
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
        "history-substring-search-up" => Some(ReedlineEvent::Up),
        "history-substring-search-down" => Some(ReedlineEvent::Down),
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
        "up-line-or-history" => Some(ReedlineEvent::Up),
        "down-line-or-history" => Some(ReedlineEvent::Down),
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
    Esac,
    Brace,
    Paren,
    FunctionBody,
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
                    "|" | "&&" | "||" => {
                        command_position = true;
                        trailing_list_operator = true;
                    }
                    "(" => {
                        stack.push(BlockClose::Paren);
                        command_position = true;
                        trailing_list_operator = false;
                    }
                    ")" => {
                        pop_if_matches(&mut stack, BlockClose::Paren);
                        command_position = false;
                        trailing_list_operator = false;
                    }
                    _ => {}
                }
                index += 1;
            }
            ReplToken::Word(word) => {
                trailing_list_operator = false;

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
                        stack.push(BlockClose::Esac);
                        command_position = false;
                    }
                    "fi" if command_position => {
                        pop_if_matches(&mut stack, BlockClose::Fi);
                        command_position = false;
                    }
                    "done" if command_position => {
                        pop_if_matches(&mut stack, BlockClose::Done);
                        command_position = false;
                    }
                    "esac" if command_position => {
                        pop_if_matches(&mut stack, BlockClose::Esac);
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
                        pop_if_matches(&mut stack, BlockClose::Brace);
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
            ';' | '(' | ')' => {
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

fn pop_if_matches(stack: &mut Vec<BlockClose>, expected: BlockClose) {
    if stack.last() == Some(&expected) {
        stack.pop();
    }
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
/// right after the setup wizard — it points at `about`, the in-shell tour.
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
    // First-run setup wizard, then the `about` tour: the screen clears, shows
    // what this shell is and where to go next, and waits for a keypress.
    let first_run = crate::setup_wizard::is_first_run();
    if first_run {
        let _ = crate::setup_wizard::run_wizard();
        if crate::terminal::stdout_is_terminal() {
            let _ = crate::easter_eggs::about_tour();
        }
    }

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
    shell.borrow_mut().run_startup_rc();
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
            let prompt = shell.borrow().prompt.clone();
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
                if pending.is_empty() && matches!(line.trim(), "exit" | "logout") {
                    let _ = shell.borrow_mut().finish_with_exit_trap(0);
                    break;
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
        if pending.is_empty() && matches!(line.trim(), "exit" | "logout") {
            let _ = shell.finish_with_exit_trap(0);
            break;
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
}
