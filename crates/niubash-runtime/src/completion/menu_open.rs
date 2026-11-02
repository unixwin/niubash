//! The shared menu-open flag for the menus that cannot carry it
//! themselves (niubash#832).
//!
//! The history autosuggest hinter stands down while a completion menu
//! is on screen: it reads one `Arc<AtomicBool>` the menus raise while
//! active (see [`crate::autosuggest`]). [`super::cycle_menu::CycleMenu`]
//! and [`super::column_menu::ColumnMenu`] carry the flag natively, but
//! the GNU menu, the stock reedline menus (Ide/List — the List menu
//! serves the completion list style, the inline style, AND the history
//! menu) have no slot for it, so those styles painted the ghost text
//! over the open menu — and Right accepted the ghost
//! (`HistoryHintComplete` precedes `MenuRight` in the default
//! `UntilFound`) instead of navigating the grid.
//!
//! This decorator gives any menu the flag: after every event or
//! activation change is delegated, the flag is re-stored from the
//! wrapped menu's own `is_active()`, so it cannot disagree with the
//! menu's real state — including a deactivation the wrapped menu
//! performs on itself inside `can_partially_complete` (the
//! [`super::insert_menu::InsertStopMenu`] splice-and-stop path sits
//! inside this wrapper for exactly that reason).

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use reedline::{Completer, Editor, Menu, MenuEvent, MenuSettings, Painter, Suggestion};

/// A menu that mirrors its wrapped menu's active state into the
/// shared menu-open flag the autosuggest hinter consults.
pub struct MenuOpenFlagMenu {
    inner: Box<dyn Menu>,
    menu_open: Arc<AtomicBool>,
}

impl MenuOpenFlagMenu {
    pub fn new(inner: Box<dyn Menu>, menu_open: Arc<AtomicBool>) -> Self {
        Self { inner, menu_open }
    }

    fn sync_flag(&self) {
        self.menu_open
            .store(self.inner.is_active(), Ordering::Relaxed);
    }
}

impl Menu for MenuOpenFlagMenu {
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
        self.sync_flag();
    }

    fn clear_input(&mut self) {
        self.inner.clear_input();
        self.sync_flag();
    }

    fn on_activate(&mut self) {
        self.inner.on_activate();
    }

    fn on_deactivate(&mut self) {
        self.inner.on_deactivate();
        self.menu_open.store(false, Ordering::Relaxed);
    }

    fn handle_menu_event(&mut self, event: &MenuEvent) {
        self.inner.handle_menu_event(event);
        self.sync_flag();
    }

    fn menu_event(&mut self, event: MenuEvent) {
        self.inner.menu_event(event);
        self.sync_flag();
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
        let spliced = self
            .inner
            .can_partially_complete(values_updated, editor, completer);
        // The wrapped menu may have deactivated itself (the
        // insert-stop splice path); the flag must follow it down.
        self.sync_flag();
        spliced
    }

    fn update_values(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        self.inner.update_values(editor, completer);
    }

    fn reset_position(&mut self) {
        self.inner.reset_position();
    }

    fn reload(&mut self, updated: bool, editor: &mut Editor, completer: &mut dyn Completer) {
        self.inner.reload(updated, editor, completer);
        self.sync_flag();
    }

    fn update_working_details(
        &mut self,
        editor: &mut Editor,
        completer: &mut dyn Completer,
        painter: &Painter,
    ) {
        self.inner
            .update_working_details(editor, completer, painter);
        self.sync_flag();
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
    use reedline::{IdeMenu, ListMenu, MenuBuilder};

    fn flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    fn raised(flag: &Arc<AtomicBool>) -> bool {
        flag.load(Ordering::Relaxed)
    }

    /// niubash#832: a stock ListMenu (the completion List style, the
    /// inline style, and the history menu all ride it) raises the
    /// shared flag while active and lowers it on Deactivate — the
    /// hinter stands down for exactly the menu's visible life.
    #[test]
    fn issue832_list_menu_flag_tracks_activation() {
        let flag = flag();
        let mut menu = MenuOpenFlagMenu::new(
            Box::new(ListMenu::default().with_name("completion_menu")),
            flag.clone(),
        );
        assert_eq!(menu.name(), "completion_menu");
        assert!(!raised(&flag));
        menu.menu_event(MenuEvent::Activate(false));
        assert!(menu.is_active());
        assert!(raised(&flag), "an open list menu must raise the flag");
        menu.menu_event(MenuEvent::Deactivate);
        assert!(!menu.is_active());
        assert!(!raised(&flag), "the flag must fall with the menu");
    }

    /// niubash#832: the Ide popup gets the same tracking, and a direct
    /// `set_active` (the engine's other activation face) syncs too.
    #[test]
    fn issue832_ide_menu_flag_tracks_activation_and_set_active() {
        let flag = flag();
        let mut menu = MenuOpenFlagMenu::new(Box::new(IdeMenu::default()), flag.clone());
        menu.set_active(true);
        assert!(raised(&flag));
        menu.set_active(false);
        assert!(!raised(&flag));
        menu.menu_event(MenuEvent::Activate(false));
        assert!(raised(&flag));
        menu.menu_event(MenuEvent::Deactivate);
        assert!(!raised(&flag));
    }
}
