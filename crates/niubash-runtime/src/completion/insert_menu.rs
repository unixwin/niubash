//! First-TAB common-prefix stop for the `insert` interaction.
//!
//! niubash#486/#778 established the two-step stop rule for the default
//! `column` menu ([`super::column_menu::ColumnMenu`]) and the literal GNU
//! menu ([`super::gnu_menu::GnuMenu`]). The stock `IdeMenu` and `ListMenu`
//! used by the other styles have no such rule: activating them on the first
//! TAB opens a popup/pager before any common prefix has been spliced.
//!
//! This decorator gives them the same contract without touching the stock
//! menus' navigation or Enter handling:
//!
//! * a *fresh* activation (the TAB routing, not a navigation event) asks the
//!   completer for the current set and splices what the interaction axis
//!   promises — the longest common prefix, or the whole value for a lone
//!   candidate — then deactivates itself;
//! * when the typed word already **is** the common prefix (nothing to
//!   splice), the menu stays up: TAB #2's list is the disambiguation aid;
//! * any later activation is the same test against the now-longer line, so
//!   the second TAB naturally opens the list and further TABs keep it.
//!
//! The style still owns the painted shape (the wrapped menu renders it);
//! only the first-TAB stop is added here. `cycle`/`select` never construct
//! this wrapper.

use reedline::menu_functions::completer_input;
use reedline::{
    Completer, CompletionResult, Editor, InputMode, Menu, MenuEvent, MenuSettings, Painter,
    Suggestion, UndoBehavior,
};

/// A presentation menu (Ide/List/Inline) with the insert interaction's
/// first-TAB prefix stop.
pub struct InsertStopMenu {
    inner: Box<dyn Menu>,
    /// Set by `menu_event(Activate)`, consumed by the next
    /// `can_partially_complete`. Only a fresh activation may splice.
    fresh_activation: bool,
}

impl InsertStopMenu {
    pub fn new(inner: Box<dyn Menu>) -> Self {
        Self {
            inner,
            fresh_activation: false,
        }
    }

    /// Longest common prefix of the candidate values, as bytes.
    fn common_prefix(values: &[Suggestion]) -> String {
        let Some(first) = values.first() else {
            return String::new();
        };
        let mut prefix = first.value.clone();
        for value in &values[1..] {
            let mut end = 0;
            for (a, b) in prefix.chars().zip(value.value.chars()) {
                if a != b {
                    break;
                }
                end += a.len_utf8();
            }
            prefix.truncate(end);
            if prefix.is_empty() {
                break;
            }
        }
        prefix
    }

    /// Splice `replacement` over the suggestion's span, leaving the cursor
    /// after it (the same whole-span edit the cycle menu uses).
    fn splice(editor: &mut Editor, suggestion: &Suggestion, replacement: &str) {
        let start = suggestion.span.start;
        let end = suggestion.span.end;
        if start > end || end > editor.get_buffer().len() {
            return;
        }
        let new_end = start + replacement.len();
        editor.edit_buffer(
            |buffer| {
                buffer.replace_range(start..end, replacement);
                buffer.set_insertion_point(new_end);
            },
            UndoBehavior::CreateUndoPoint,
        );
    }

    fn deactivate(&mut self) {
        self.inner.menu_event(MenuEvent::Deactivate);
        self.fresh_activation = false;
    }
}

impl Menu for InsertStopMenu {
    fn settings(&self) -> &MenuSettings {
        self.inner.settings()
    }

    fn is_active(&self) -> bool {
        self.inner.is_active()
    }

    fn set_active(&mut self, active: bool) {
        self.inner.set_active(active);
    }

    fn clear_input(&mut self) {
        self.inner.clear_input();
    }

    fn on_activate(&mut self) {
        self.inner.on_activate();
    }

    fn on_deactivate(&mut self) {
        self.inner.on_deactivate();
    }

    fn menu_event(&mut self, event: MenuEvent) {
        self.fresh_activation = matches!(event, MenuEvent::Activate(_));
        self.inner.menu_event(event);
    }

    fn can_quick_complete(&self) -> bool {
        self.inner.can_quick_complete()
    }

    fn can_partially_complete(
        &mut self,
        values_updated: bool,
        editor: &mut Editor,
        completer: &mut dyn Completer,
    ) -> bool {
        if !std::mem::take(&mut self.fresh_activation) {
            return self
                .inner
                .can_partially_complete(values_updated, editor, completer);
        }

        // Query the completer directly: ListMenu only caches one page
        // (`page_size = 1` for inline), so its `get_values()` would hide the
        // rest of the set from the common-prefix computation.
        let (input, pos) = completer_input(
            editor.get_buffer(),
            editor.line_buffer().insertion_point(),
            None,
            InputMode::CursorPrefix,
        );
        let values: Vec<Suggestion> = match completer.complete(&input, pos) {
            CompletionResult::Fresh { suggestions, .. } => suggestions.to_vec(),
            // A stale/pending answer has no span we may trust; keep the
            // menu up and let the deferred settle repaint it (the same
            // guard the stock menus use).
            CompletionResult::Stale { .. } | CompletionResult::Pending => {
                if !values_updated {
                    self.inner.update_values(editor, completer);
                }
                return false;
            }
        };
        if values.is_empty() {
            // No candidates: a silent first TAB, not an empty popup/pager.
            self.inner.update_values(editor, completer);
            self.deactivate();
            return false;
        }

        let first = values[0].clone();
        let prefix = Self::common_prefix(&values);
        let buffer = editor.get_buffer();
        let start = first.span.start.min(buffer.len());
        let end = first.span.end.min(buffer.len());
        let typed = buffer.get(start..end).unwrap_or("");

        if values.len() == 1 {
            let mut replacement = first.value.clone();
            if first.append_whitespace {
                replacement.push(' ');
            }
            Self::splice(editor, &first, &replacement);
            self.deactivate();
            return true;
        }

        if prefix.len() > typed.len() && prefix.starts_with(typed) {
            Self::splice(editor, &first, &prefix);
            self.deactivate();
            return true;
        }

        // Nothing to splice: the grid/list is the disambiguation aid. Keep
        // it open (TAB #2 lands here). Fill the wrapped menu's own cache for
        // this paint.
        self.inner.update_values(editor, completer);
        false
    }

    fn update_values(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        self.inner.update_values(editor, completer);
    }

    fn reset_position(&mut self) {
        self.inner.reset_position();
    }

    fn reload(&mut self, updated: bool, editor: &mut Editor, completer: &mut dyn Completer) {
        self.inner.reload(updated, editor, completer);
    }

    fn update_working_details(
        &mut self,
        editor: &mut Editor,
        completer: &mut dyn Completer,
        painter: &Painter,
    ) {
        self.inner
            .update_working_details(editor, completer, painter);
    }

    fn replace_in_buffer(&self, editor: &mut Editor) {
        self.inner.replace_in_buffer(editor);
    }

    fn menu_required_lines(&self, terminal_columns: u16) -> u16 {
        self.inner.menu_required_lines(terminal_columns)
    }

    fn menu_string(&self, available_lines: u16, use_ansi_coloring: bool) -> String {
        self.inner.menu_string(available_lines, use_ansi_coloring)
    }

    fn min_rows(&self) -> u16 {
        self.inner.min_rows()
    }

    fn get_values(&self) -> &[Suggestion] {
        self.inner.get_values()
    }

    fn results_are_provisional(&self) -> bool {
        self.inner.results_are_provisional()
    }

    fn is_awaiting_first_answer(&self) -> bool {
        self.inner.is_awaiting_first_answer()
    }

    fn is_visible(&self) -> bool {
        self.inner.is_visible()
    }

    fn set_cursor_pos(&mut self, pos: (u16, u16)) {
        self.inner.set_cursor_pos(pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reedline::Span;

    fn suggestion(value: &str, start: usize, end: usize) -> Suggestion {
        Suggestion {
            value: value.to_string(),
            display_override: None,
            description: None,
            style: None,
            extra: None,
            span: Span { start, end },
            append_whitespace: false,
            match_indices: None,
        }
    }

    #[test]
    fn common_prefix_is_the_shared_typed_extension() {
        let values = [
            suggestion("alpha.txt", 3, 5),
            suggestion("alpine.txt", 3, 5),
        ];
        assert_eq!(InsertStopMenu::common_prefix(&values), "alp");
    }

    #[test]
    fn common_prefix_of_a_lone_value_is_the_value() {
        let values = [suggestion("only-dir/", 3, 5)];
        assert_eq!(InsertStopMenu::common_prefix(&values), "only-dir/");
    }

    #[test]
    fn common_prefix_handles_multibyte_values() {
        let values = [suggestion("café-one", 3, 4), suggestion("café-two", 3, 4)];
        assert_eq!(InsertStopMenu::common_prefix(&values), "café-");
    }

    #[test]
    fn common_prefix_can_be_empty() {
        let values = [suggestion("alpha", 0, 1), suggestion("beta", 0, 1)];
        assert_eq!(InsertStopMenu::common_prefix(&values), "");
    }
}
