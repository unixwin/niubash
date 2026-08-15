//! Reedline REPL loop

use std::{borrow::Cow, io::Write};

use reedline::{
    default_emacs_keybindings, default_vi_insert_keybindings, default_vi_normal_keybindings,
    EditCommand, EditMode, Emacs, KeyCode, KeyModifiers, Keybindings, ListMenu, MenuBuilder,
    Prompt, PromptEditMode, PromptHistorySearch, Reedline, ReedlineEvent, ReedlineMenu, Signal, Vi,
};
use rubash::TokenKind;

use crate::autosuggest::HistoryAutosuggestHinter;
use crate::completion::WinuxshCompleter;
use crate::config::{EditorMode, MenuConfig, NativeWidgetBinding, NativeWidgetConfig};
use crate::history::LiveFileBackedHistory;
use crate::shell::Shell;
use crate::syntax_highlighting::WinuxshSyntaxHighlighter;

const COMPLETION_MENU: &str = "completion_menu";
const HISTORY_MENU: &str = "history_menu";

/// Extract the arguments of a `self-update` / `update-winuxsh` REPL command,
/// or `None` when the line is not one.
pub fn self_update_command_args(line: &str) -> Option<Vec<String>> {
    let mut parts = line.trim().split_whitespace();
    match parts.next()? {
        "self-update" | "update-winuxsh" => Some(parts.map(str::to_string).collect()),
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
pub fn build_line_editor(shell: &mut Shell) -> anyhow::Result<Reedline> {
    let history =
        LiveFileBackedHistory::with_file(shell.history_max_size, shell.history_path.clone())
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to open history file {}: {}",
                    shell.history_path.display(),
                    e
                )
            })?;

    let completer = WinuxshCompleter::new(shell.completion_state.clone());
    let menu_config = shell.menu_config;

    let completion_menu = ReedlineMenu::WithCompleter {
        menu: Box::new(configured_list_menu(
            COMPLETION_MENU,
            menu_config.completion_page_size,
            menu_config,
        )),
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
            shell.history_ignore_space_prefixed,
        ))
        .with_menu(completion_menu)
        .with_menu(history_menu)
        .with_edit_mode(build_edit_mode(
            shell.editor_mode,
            &shell.native_widgets,
            &shell.native_widget_bindings,
        ));

    if shell.autosuggest.history_strategy_enabled() {
        editor = editor.with_hinter(Box::new(HistoryAutosuggestHinter::new(&shell.autosuggest)));
    }
    if shell.syntax_highlighting.main_highlighter_enabled() {
        let functions = shell.executor.functions_snapshot();
        let commands = shell
            .aliases
            .keys()
            .map(String::as_str)
            .chain(functions.iter().map(String::as_str));
        editor = editor.with_highlighter(Box::new(
            WinuxshSyntaxHighlighter::new_with_commands_and_state(
                &shell.syntax_highlighting,
                commands,
                shell.completion_state.clone(),
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

fn history_exclusion_prefix(ignore_space_prefixed: bool) -> Option<String> {
    ignore_space_prefixed.then(|| " ".to_string())
}

fn build_edit_mode(
    mode: EditorMode,
    native_widgets: &NativeWidgetConfig,
    native_widget_bindings: &[NativeWidgetBinding],
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
            Box::new(Vi::new(insert_keybindings, normal_keybindings))
        }
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
    target: NativeKeymapTarget,
    config: &NativeWidgetConfig,
    bindings: &[NativeWidgetBinding],
) {
    if !config.enabled {
        return;
    }

    add_native_widget_preset_keybindings(keybindings, &config.presets);

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
        ("main", _) => true,
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
        _ => None,
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
        .all(|token| token.kind != TokenKind::HereDocBody || !token.value.starts_with('\x1f'))
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

    while index < chars.len() {
        let ch = chars[index];

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
pub fn run_repl(shell: &mut Shell) -> anyhow::Result<()> {
    // First-run setup wizard.
    if crate::setup_wizard::is_first_run() {
        let _ = crate::setup_wizard::run_wizard();
    }

    let welcome = format!(
        "Winuxsh {} \u{2014} bash-compatible shell for Windows. Type \u{2018}exit\u{2019} or press Ctrl+D to quit.",
        env!("CARGO_PKG_VERSION")
    );
    println!("{}", welcome);
    println!();

    shell.restore_last_working_dir_for_repl();
    shell.run_startup_rc();
    let mut line_editor = build_line_editor(shell)?;
    let mut pending = PendingReplInput::default();

    loop {
        let signal = if pending.is_empty() {
            shell.run_precmd_hooks();
            line_editor.read_line(&shell.prompt)
        } else {
            let prompt = ContinuationPrompt::new(&shell.prompt);
            line_editor.read_line(&prompt)
        };

        match signal {
            Ok(Signal::Success(buffer)) => {
                let line = buffer.trim_end_matches(['\r', '\n']);
                if pending.is_empty() && line.trim().is_empty() {
                    continue;
                }
                if pending.is_empty() && matches!(line.trim(), "exit" | "logout") {
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
                    let _ = shell.execute_interactive_script(&script);
                } else {
                    let _ = shell.execute_interactive_line(script.trim());
                }
                flush_repl_output();
            }
            Ok(Signal::CtrlD) => {
                println!();
                if !pending.is_empty() {
                    pending.clear();
                    continue;
                }
                break;
            }
            Ok(Signal::CtrlC) => {
                println!();
                pending.clear();
                continue;
            }
            Err(e) => {
                eprintln!("winuxsh: line editor error: {}", e);
                break;
            }
        }
    }

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

        add_native_widget_keybindings(
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

        add_native_widget_keybindings(
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

        add_native_widget_keybindings(
            &mut insert,
            NativeKeymapTarget::ViInsert,
            &config,
            &bindings,
        );
        add_native_widget_keybindings(
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

        add_native_widget_keybindings(
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

        add_native_widget_keybindings(
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
}
