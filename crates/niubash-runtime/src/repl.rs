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
    MenuBuilder, Prompt, PromptEditMode, PromptHistorySearch, Reedline, ReedlineEvent,
    ReedlineMenu, Signal, Vi,
};

const COMPLETION_MENU: &str = "completion_menu";
const HISTORY_MENU: &str = "history_menu";

/// `ExecuteHostCommand` payload prefix that identifies a shell-function
/// widget trigger. The host intercepts this before treating the signal as
/// submitted input.
pub const WIDGET_HOST_COMMAND_PREFIX: &str = "__niu_widget ";

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
    let history = LiveFileBackedHistory::with_mode(
        shell_ref.history_max_size,
        shell_ref.history_path.clone(),
        shell_ref.history_mode,
    )
    .map_err(|e| {
        anyhow::anyhow!(
            "failed to open history file {}: {}",
            shell_ref.history_path.display(),
            e
        )
    })?;

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
        .with_history(Box::new(history))
        .with_history_exclusion_prefix(history_exclusion_prefix(
            shell_ref.history_ignore_space_prefixed,
        ))
        .with_menu(completion_menu)
        .with_menu(history_menu)
        .with_edit_mode(build_edit_mode(
            shell_ref.editor_mode,
            &shell_ref.native_widgets,
            &shell_ref.native_widget_bindings,
            &shell_ref.user_widget_bindings,
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

fn build_edit_mode(
    mode: EditorMode,
    native_widgets: &NativeWidgetConfig,
    native_widget_bindings: &[NativeWidgetBinding],
    user_widget_bindings: &[NativeWidgetBinding],
) -> Box<dyn EditMode> {
    match mode {
        EditorMode::Emacs => {
            let mut keybindings = default_emacs_keybindings();
            add_menu_keybindings(&mut keybindings);
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
            Box::new(Emacs::new(keybindings))
        }
        EditorMode::Vi => {
            let mut insert_keybindings = default_vi_insert_keybindings();
            let mut normal_keybindings = default_vi_normal_keybindings();
            add_menu_keybindings(&mut insert_keybindings);
            add_menu_keybindings(&mut normal_keybindings);
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
            Box::new(Vi::new(insert_keybindings, normal_keybindings))
        }
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
            .or_else(|| parse_control_key_sequence(value))
            .or_else(|| parse_plain_key_sequence(value)),
    }
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
        self.lines.len() > 1
    }
}

struct ContinuationPrompt {
    indicator: String,
}

impl ContinuationPrompt {
    fn new(prompt: &dyn Prompt) -> Self {
        Self {
            indicator: prompt.render_prompt_multiline_indicator().into_owned(),
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

    fn render_prompt_indicator(&self, _prompt_mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
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
    // First-run setup wizard.
    if crate::setup_wizard::is_first_run() {
        let _ = crate::setup_wizard::run_wizard();
    }

    let welcome = format!(
        "Niubash {} \u{2014} bash-compatible shell for Windows. Type \u{2018}exit\u{2019} or press Ctrl+D to quit.",
        env!("CARGO_PKG_VERSION")
    );
    println!("{}", welcome);
    println!();

    shell.borrow_mut().restore_last_working_dir_for_repl();
    shell.borrow_mut().run_startup_rc();
    if let Some(notice) = crate::plugins::take_legacy_bundle_notice() {
        eprintln!("{}", notice);
    }
    let no_editing = shell.borrow().no_editing;
    if no_editing {
        return run_repl_without_line_editor(&mut shell.borrow_mut());
    }
    // User widget bindings and completion functions come from the rc
    // (NIU_BINDKEYS / NIU_COMPDEFS); read them after sourcing and before the
    // line editor is built.
    shell.borrow_mut().load_user_widget_bindings();
    shell.borrow_mut().load_user_compdefs();
    let mut line_editor = build_line_editor(&shell)?;
    let mut pending = PendingReplInput::default();

    loop {
        crate::console_guard::restore(&console_baseline);
        let signal = if pending.is_empty() {
            shell.borrow_mut().run_precmd_hooks();
            let prompt = shell.borrow().prompt.clone();
            line_editor.read_line(&prompt)
        } else {
            let prompt = shell.borrow().prompt.clone();
            let prompt = ContinuationPrompt::new(&prompt);
            line_editor.read_line(&prompt)
        };

        match signal {
            Ok(Signal::Success(buffer)) => {
                let mut line = buffer.trim_end_matches(['\r', '\n']).to_string();
                let mut widget_resumed_editing = false;
                if pending.is_empty() {
                    if let Some(widget) = WidgetInvocation::parse(&line) {
                        let available = shell.borrow().widget_function_available(&widget.function);
                        if available {
                            let editor_buffer = line_editor.current_buffer_contents().to_string();
                            let editor_cursor = line_editor.current_insertion_point();
                            let outcome = shell.borrow_mut().run_widget_function(
                                &widget.function,
                                &editor_buffer,
                                editor_cursor,
                            );
                            if outcome.accept {
                                // Submit the produced buffer (or the line as
                                // it stood) as ordinary user input.
                                line = outcome.buffer.unwrap_or(editor_buffer);
                            } else {
                                apply_widget_outcome(&mut line_editor, &outcome);
                                widget_resumed_editing = true;
                            }
                        }
                    }
                }
                if widget_resumed_editing {
                    flush_repl_output();
                    continue;
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
                    crate::ctrl_c::run_trap_hooks(&mut shell.borrow_mut(), "trapint");
                    flush_repl_output();
                }
                pending.clear();
                continue;
            }
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
        shell.run_precmd_hooks();
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
