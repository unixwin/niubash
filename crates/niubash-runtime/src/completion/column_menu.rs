//! Column-style completion menu: reedline's [`ColumnarMenu`] with the niubash
//! stop rule.
//!
//! Behavior contract (owner ruling 2026-10-26; zsh/bash parity):
//!
//! * **TAB #1** splices the longest common prefix and then *stops* — the
//!   menu deactivates itself instead of lingering on the re-queried set.
//!   Typing `cd proj` and pressing TAB lands on `cd projects/` with no
//!   grid; the window where the user may just want to stop (or press Enter
//!   to run exactly what is on the line) is never skipped.
//! * **TAB #2** re-activates on the settled line and shows the grid (the
//!   children of `projects/`, the ambiguous set, …).
//! * A TAB that splices **nothing** (no common prefix beyond the typed
//!   text) leaves the menu up — the grid is the disambiguation aid there,
//!   same as before.
//!
//! This mirrors [`crate::completion::gnu_menu::GnuMenu`], whose TAB #1 also
//! deactivates itself so the next TAB routes a fresh `Activate`. The only
//! difference is the render surface: column grid here, printed transcript
//! list there.
//!
//! Implementation note: the deactivation applies **only** to the fresh
//! `Activate` path (`can_partially_complete` reached straight after a TAB
//! with no active menu). Arrow-key navigation (`MenuNext`/`MenuPrevious`),
//! `Edit` batches, and repaints clear the one-shot flag first and delegate
//! untouched, so in-menu navigation keeps working exactly as reedline
//! defines it.

use reedline::{
    ColumnarMenu, Completer, Editor, Menu, MenuEvent, MenuSettings, Painter, Suggestion,
};

/// [`ColumnarMenu`] wrapper enforcing the stop-after-splice rule.
pub struct ColumnMenu {
    inner: ColumnarMenu,
    /// Set by `menu_event(Activate)`, cleared by any other menu event and
    /// after the first `can_partially_complete` call. While set, a
    /// successful splice deactivates the menu.
    fresh_activation: bool,
}

impl ColumnMenu {
    pub fn new(inner: ColumnarMenu) -> Self {
        Self {
            inner,
            fresh_activation: false,
        }
    }
}

impl Menu for ColumnMenu {
    fn settings(&self) -> &MenuSettings {
        self.inner.settings()
    }

    fn name(&self) -> &str {
        self.inner.name()
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

    fn handle_menu_event(&mut self, event: &MenuEvent) {
        self.inner.handle_menu_event(event);
    }

    fn menu_event(&mut self, event: MenuEvent) {
        // Only a fresh Activate arms the one-shot stop rule; every other
        // event (navigation, edits, deactivation) disarms it so the
        // in-menu flows keep reedline's exact behavior.
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
        let armed = self.fresh_activation;
        self.fresh_activation = false;
        let spliced = self
            .inner
            .can_partially_complete(values_updated, editor, completer);
        if armed && spliced && !self.inner.results_are_provisional() {
            // TAB #1 landed a splice: stop here (zsh/bash parity) instead
            // of lingering on the re-queried set. The next TAB re-activates
            // fresh and shows the grid. Full deactivation path, same as a
            // received `Deactivate`.
            self.inner.menu_event(MenuEvent::Deactivate);
        }
        spliced
    }

    fn update_values(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        self.inner.update_values(editor, completer);
    }

    fn reset_position(&mut self) {
        self.inner.reset_position();
    }

    fn update_working_details(
        &mut self,
        editor: &mut Editor,
        completer: &mut dyn Completer,
        painter: &Painter,
    ) {
        self.inner.update_working_details(editor, completer, painter);
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
    use reedline::{Completer, CompletionResult, LineBuffer, Span, Suggestion, UndoBehavior};

    /// Prefix completer over a fixed candidate list (mirrors the shape
    /// niubash's real completer produces for path words).
    struct PrefixCompleter {
        values: Vec<String>,
    }

    impl PrefixCompleter {
        fn new(values: &[&str]) -> Self {
            Self {
                values: values.iter().map(|value| value.to_string()).collect(),
            }
        }
    }

    impl Completer for PrefixCompleter {
        fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
            let word_start = line[..pos].rfind(' ').map(|index| index + 1).unwrap_or(0);
            let word = &line[word_start..pos];
            let suggestions: Vec<Suggestion> = self
                .values
                .iter()
                .filter(|value| value.starts_with(word))
                .map(|value| Suggestion {
                    value: value.clone(),
                    display_override: None,
                    description: None,
                    style: None,
                    extra: None,
                    span: Span {
                        start: word_start,
                        end: pos,
                    },
                    append_whitespace: false,
                    match_indices: None,
                })
                .collect();
            CompletionResult::fresh(suggestions)
        }
    }

    fn editor_with(line: &str) -> Editor {
        let mut editor = Editor::default();
        editor.edit_buffer(
            |buf: &mut LineBuffer| buf.set_buffer(line.to_string()),
            UndoBehavior::CreateUndoPoint,
        );
        editor
    }

    fn activate(menu: &mut ColumnMenu, editor: &mut Editor, completer: &mut dyn Completer) {
        menu.menu_event(MenuEvent::Activate(false));
        menu.update_values(editor, completer);
    }

    #[test]
    fn unique_dir_splice_deactivates_menu() {
        // `cd proj<TAB>` with a single `projects/` candidate: the splice
        // lands and the menu closes — no children grid on the same TAB.
        // (Owner ruling 2026-10-26: zsh/bash parity, never skip the stop
        // window where the user may just want to `cd projects/`.)
        let mut menu = ColumnMenu::new(ColumnarMenu::default());
        let mut completer = PrefixCompleter::new(&["projects/"]);
        let mut editor = editor_with("cd proj");
        activate(&mut menu, &mut editor, &mut completer);
        assert!(menu.is_active());
        assert!(menu.can_partially_complete(false, &mut editor, &mut completer));
        assert_eq!(editor.get_buffer(), "cd projects/");
        assert!(
            !menu.is_active(),
            "unique splice must close the menu instead of listing children"
        );
    }

    #[test]
    fn ambiguous_prefix_splice_deactivates_menu() {
        // `cd ruba<TAB>` over 20 `rubash*` dirs: the common prefix splices
        // and the menu closes — the grid is a second TAB away.
        let mut menu = ColumnMenu::new(ColumnarMenu::default());
        let mut completer = PrefixCompleter::new(&["rubash/", "rubash-1013/", "rubash-1020/"]);
        let mut editor = editor_with("cd ruba");
        activate(&mut menu, &mut editor, &mut completer);
        assert!(menu.can_partially_complete(false, &mut editor, &mut completer));
        assert_eq!(editor.get_buffer(), "cd rubash");
        assert!(
            !menu.is_active(),
            "spliced prefix must not auto-show the grid on the same TAB"
        );
    }

    #[test]
    fn no_splice_keeps_menu_open_for_disambiguation() {
        // `cd r<TAB>` with genuinely ambiguous candidates and no common
        // prefix beyond the typed text: nothing splices, the grid stays up
        // as the disambiguation aid.
        let mut menu = ColumnMenu::new(ColumnarMenu::default());
        let mut completer = PrefixCompleter::new(&["rubash/", "repro/"]);
        let mut editor = editor_with("cd r");
        activate(&mut menu, &mut editor, &mut completer);
        assert!(!menu.can_partially_complete(false, &mut editor, &mut completer));
        assert!(menu.is_active());
        assert_eq!(menu.get_values().len(), 2);
    }

    #[test]
    fn navigation_path_splice_leaves_menu_alone() {
        // Arrow-key navigation (`MenuNext`) also routes through
        // `can_partially_complete`; the stop rule must not fire there or
        // in-menu navigation would die mid-browse.
        let mut menu = ColumnMenu::new(ColumnarMenu::default());
        let mut completer = PrefixCompleter::new(&["rubash/", "rubash-1013/"]);
        let mut editor = editor_with("cd ruba");
        activate(&mut menu, &mut editor, &mut completer);
        // Simulate the navigation flow: a non-Activate event disarms the
        // one-shot rule before the partial call.
        menu.menu_event(MenuEvent::NextElement);
        assert!(menu.can_partially_complete(false, &mut editor, &mut completer));
        assert_eq!(editor.get_buffer(), "cd rubash");
        assert!(
            menu.is_active(),
            "navigation-flow splice must keep the menu alive"
        );
    }
}
