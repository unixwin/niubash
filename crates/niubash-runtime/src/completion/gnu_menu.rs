//! GNU readline-style TAB completion (the niubash default shape).
//!
//! Behavior contract (owner ruling 2026-10-12) — GNU readline's factory
//! semantics, not the interactive menus the other four styles draw:
//!
//! * **TAB #1** completes the longest common prefix of the candidates into
//!   the line (a lone candidate completes outright). Nothing is listed.
//! * **TAB #2 on an unchanged line** with an ambiguous set prints the
//!   candidates as width-adaptive columns — GNU `print_completions`
//!   geometry: the field width is the widest candidate plus a two-space
//!   gutter, the column count is what the terminal fits, entries fill
//!   column-major (down first) — and the prompt plus input line are
//!   repainted next to the list. The list is *printed output*, not a
//!   widget: nothing is selected, arrow keys stay on history, no popup.
//! * Further TABs on the same line keep the list up; any edit dismisses it
//!   and the next TAB starts a fresh attempt. (`show-all-if-ambiguous` /
//!   `show-all-if-unmodified` are the readline variables this state machine
//!   can grow later; deliberately not built yet.)
//!
//! Implementation rides reedline 0.50's `Menu` abstraction — the
//! "print + redraw" exit shape the owner asked the menu/painter layers to be
//! weighed against. The engine hands every menu [`Menu::update_working_details`]
//! (editor + completer + painter) right before each repaint, which is where
//! the double-TAB state machine runs (the `ColumnarMenu::apply_event`
//! pattern). After TAB #1 the menu *deactivates itself*, so the next TAB
//! routes a fresh `MenuEvent::Activate` through the engine and the
//! "second TAB lists" decision is carried entirely by this struct. The one
//! GNU deviation is placement: reedline's painter owns menu geometry and
//! draws the list in the slot under the input line instead of scrolling it
//! into the transcript above the prompt; printing it host-side would fight
//! the painter's cached prompt anchor (the exact hazard
//! `invalidate_anchor_if_host_completer_runs` exists for) for no behavioral
//! gain, so the interactive semantics win and the anchor stays intact.

use reedline::menu_functions::{completer_input, CompletionDisplay};
use reedline::{Completer, Editor, InputMode, Menu, MenuEvent, Painter, Suggestion};

/// Two-space gutter between completion columns — GNU readline separates
/// fields visually the same way (bash lists read `alpha  beta`).
const GNU_COLUMN_GUTTER: usize = 2;

/// Where the double-TAB state machine stands. `Attempted` is the GNU "prefix
/// completed, line unmodified" state that makes the next TAB print the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GnuPhase {
    /// No attempt in flight: the next TAB is a fresh prefix completion.
    Idle,
    /// An attempt landed (prefix spliced or lone candidate completed) and the
    /// line is unchanged since: the next TAB lists the candidates.
    Attempted,
    /// The candidate list is on screen (menu active). Further TABs keep it;
    /// any edit dismisses it.
    Listed,
}

/// The GNU-style completion menu. See the [module docs](self).
///
/// reedline 0.50 does not re-export `MenuSettings`, so this menu carries its
/// `name` and `marker` directly and overrides [`Menu::name`] /
/// [`Menu::indicator`] — the panicking `settings()` default is then never
/// reachable, and [`MenuBuilder`](reedline::MenuBuilder) is replaced by the
/// plain builder below.
pub struct GnuMenu {
    name: String,
    /// Painted in place of the prompt indicator while the list is up;
    /// callers hand in the shell's own indicator so the redrawn prompt is
    /// pixel-identical to a fresh one.
    marker: String,
    active: bool,
    /// Engine event queued by [`GnuMenu::menu_event`], processed at paint
    /// time in [`GnuMenu::update_working_details`] (where the editor and
    /// completer are reachable — the ColumnarMenu pattern).
    event: Option<MenuEvent>,
    /// Candidates from the latest completion query, with display metrics.
    completions: CompletionDisplay,
    phase: GnuPhase,
    /// Buffer contents captured when the last attempt or list landed — the
    /// "line unmodified" anchor of the double-TAB semantics.
    anchor_buffer: Option<String>,
    /// Terminal width seen at the last paint; layout falls back to 80 before
    /// the first [`GnuMenu::update_working_details`].
    terminal_columns: usize,
}

impl Default for GnuMenu {
    fn default() -> Self {
        Self {
            name: "menu".to_string(),
            marker: String::new(),
            active: false,
            event: None,
            completions: CompletionDisplay::default(),
            phase: GnuPhase::Idle,
            anchor_buffer: None,
            terminal_columns: 80,
        }
    }
}

impl GnuMenu {
    /// Name the menu (the engine routes `ReedlineEvent::Menu(name)` by it).
    #[must_use]
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    /// Set the marker painted as the prompt indicator while the list is up.
    #[must_use]
    pub fn with_marker(mut self, marker: &str) -> Self {
        self.marker = marker.to_string();
        self
    }

    /// The completer input: the buffer up to the cursor (reedline's
    /// `CursorPrefix` mode, what every niubash completion menu uses).
    fn completer_query(editor: &Editor) -> (String, usize) {
        completer_input(
            editor.get_buffer(),
            editor.line_buffer().insertion_point(),
            None,
            InputMode::CursorPrefix,
        )
    }
}

/// Column layout for a GNU `print_completions` grid: the field width is the
/// widest candidate plus the two-space gutter; as many columns as the
/// terminal fits, at least one; rows ceil(count / columns). Returns
/// `(columns, rows)`.
fn column_layout(count: usize, longest: usize, terminal_columns: usize) -> (usize, usize) {
    let count = count.max(1);
    let field = longest.max(1).saturating_add(GNU_COLUMN_GUTTER);
    let columns = (terminal_columns / field).max(1).min(count);
    let rows = count.div_ceil(columns);
    (columns, rows)
}

/// Render the candidate grid. `widths` are the per-candidate display widths
/// (ANSI-stripped) parallel to `values`; entries fill column-major (down
/// first, like bash); trailing padding is trimmed at line ends and the
/// return carries no trailing newline. When the rows cannot fit
/// `available_lines` the list is clipped to what fits — reedline has
/// already scrolled to make room up to the screen height, and GNU's
/// `--More--` paging needs an interaction channel a printed list must not
/// have.
fn render_list(
    values: &[&str],
    widths: &[usize],
    terminal_columns: usize,
    available_lines: u16,
) -> String {
    if values.is_empty() {
        return String::new();
    }
    let (columns, rows) = column_layout(
        values.len(),
        widths.iter().copied().max().unwrap_or(0),
        terminal_columns,
    );
    let rows = rows.min(available_lines.max(1) as usize).max(1);
    let field = widths.iter().copied().max().unwrap_or(0).max(1) + GNU_COLUMN_GUTTER;
    let mut out = String::new();
    for row in 0..rows {
        let mut line = String::new();
        for col in 0..columns {
            let index = col * rows + row;
            if index >= values.len() {
                continue;
            }
            let cell_width = widths.get(index).copied().unwrap_or(0);
            let pad = field.saturating_sub(cell_width).max(GNU_COLUMN_GUTTER);
            line.push_str(values[index]);
            let last_in_row = col + 1 == columns || index + 1 >= values.len();
            if !last_in_row {
                for _ in 0..pad {
                    line.push(' ');
                }
            }
        }
        while line.ends_with(' ') {
            line.pop();
        }
        out.push_str(&line);
        if row + 1 < rows {
            out.push('\n');
        }
    }
    out
}

impl GnuMenu {
    /// Back to factory state: inactive, no attempt anchored, no list.
    fn reset(&mut self) {
        self.active = false;
        self.phase = GnuPhase::Idle;
        self.anchor_buffer = None;
        self.completions = CompletionDisplay::default();
    }

    /// Fetch the candidates for the current line into `self.completions`.
    fn refresh_values(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        let (input, pos) = Self::completer_query(editor);
        let (result, base_ranges) = completer.complete_with_base_ranges(&input, pos);
        if let Some(completions) = CompletionDisplay::from_result(result, &base_ranges, editor) {
            self.completions = completions;
        }
    }

    /// TAB #1 (or any TAB whose line moved since the last attempt): complete
    /// the common prefix — a lone candidate completes outright — and
    /// deactivate so nothing is listed. When the line is unchanged since an
    /// ambiguous attempt, this is instead TAB #2: activate with the list.
    fn apply_activate(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        let buffer = editor.get_buffer().to_string();
        self.refresh_values(editor, completer);
        let count = self.completions.suggestions().len();
        let unchanged = self.anchor_buffer.as_deref() == Some(buffer.as_str());

        // TAB #2/#3 semantics: the line was not touched since the last
        // attempt and the set is ambiguous — show (or keep) the printed list.
        if unchanged && count > 1 && matches!(self.phase, GnuPhase::Attempted | GnuPhase::Listed) {
            self.phase = GnuPhase::Listed;
            self.active = true;
            return;
        }

        // Fresh attempt (TAB #1, or the line changed since the last one).
        match count {
            // Nothing to complete: GNU rings the bell; the list stays quiet.
            0 => self.reset(),
            // Lone candidate: complete it outright (GNU exact) — no list.
            1 => {
                self.completions.accept(0, editor, None);
                self.anchor_buffer = Some(editor.get_buffer().to_string());
                self.phase = GnuPhase::Attempted;
                self.active = false;
            }
            // Ambiguous: splice the shared prefix (no-op when the typed word
            // already is the prefix) and anchor the attempt for TAB #2.
            _ => {
                let _ = self.completions.common_prefix(editor);
                self.anchor_buffer = Some(editor.get_buffer().to_string());
                self.phase = GnuPhase::Attempted;
                self.active = false;
            }
        }
    }

    /// Buffer edited while the list is on screen: a changed line dismisses
    /// the list (GNU's printed list belongs to the line it listed); an
    /// unchanged buffer (TAB #N's `Complete` lands here) keeps it up.
    fn apply_edit(&mut self, editor: &Editor) {
        if self.phase != GnuPhase::Listed {
            return;
        }
        if self.anchor_buffer.as_deref() != Some(editor.get_buffer()) {
            self.reset();
        }
    }
}

impl Menu for GnuMenu {
    fn name(&self) -> &str {
        &self.name
    }

    fn indicator(&self) -> &str {
        &self.marker
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    fn clear_input(&mut self) {
        self.reset();
    }

    fn menu_event(&mut self, event: MenuEvent) {
        self.handle_menu_event(&event);
        self.event = Some(event);
    }

    fn can_quick_complete(&self) -> bool {
        false
    }

    fn can_partially_complete(
        &mut self,
        _values_updated: bool,
        _editor: &mut Editor,
        _completer: &mut dyn Completer,
    ) -> bool {
        // The GNU shape splices its own prefix in `apply_activate`; the
        // engine's quick/partial completion hooks stay off.
        false
    }

    fn update_values(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        self.refresh_values(editor, completer);
    }

    fn reset_position(&mut self) {
        // Nothing is selectable; nothing to reset.
    }

    fn update_working_details(
        &mut self,
        editor: &mut Editor,
        completer: &mut dyn Completer,
        painter: &Painter,
    ) {
        self.terminal_columns = painter.screen_width() as usize;
        match self.event.take() {
            Some(MenuEvent::Activate(_)) => self.apply_activate(editor, completer),
            Some(MenuEvent::Edit(_)) => self.apply_edit(editor),
            // Deactivate was applied by `handle_menu_event`; navigation
            // events never reach this menu (no arrow-key bindings) and must
            // not disturb the printed list if they ever do.
            _ => {}
        }
    }

    fn replace_in_buffer(&self, _editor: &mut Editor) {
        // No selection to accept — the GNU list is not navigable.
    }

    fn menu_required_lines(&self, terminal_columns: u16) -> u16 {
        if self.phase != GnuPhase::Listed {
            return 0;
        }
        let widths = &self.completions.display_widths;
        let (_, rows) = column_layout(
            widths.len(),
            widths.iter().copied().max().unwrap_or(0),
            terminal_columns as usize,
        );
        rows.min(u16::MAX as usize) as u16
    }

    fn menu_string(&self, available_lines: u16, _use_ansi_coloring: bool) -> String {
        if self.phase != GnuPhase::Listed {
            return String::new();
        }
        let suggestions = self.completions.suggestions();
        let values: Vec<&str> = suggestions.iter().map(Suggestion::display_value).collect();
        render_list(
            &values,
            &self.completions.display_widths,
            self.terminal_columns,
            available_lines,
        )
    }

    fn min_rows(&self) -> u16 {
        self.menu_required_lines(self.terminal_columns as u16)
    }

    fn get_values(&self) -> &[Suggestion] {
        self.completions.suggestions()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reedline::{CompletionResult, Span, UndoBehavior};

    /// A prefix completer that INCLUDES the exact typed word among its
    /// candidates — the shape niubash's real completer produces (reedline's
    /// `DefaultCompleter` drops the exact match, which no niubash completer
    /// does, and that would fake a lone candidate out of an ambiguous set).
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

    fn editor_with(buffer: &str) -> Editor {
        let mut editor = Editor::default();
        editor.edit_buffer(
            |line| line.set_buffer(buffer.to_string()),
            UndoBehavior::CreateUndoPoint,
        );
        editor
    }

    /// Drive an engine Activate through the paint-time dispatch. The real
    /// `update_working_details` records `painter.screen_width()` first
    /// (defaulting to 80 here — reedline's `Painter` constructor is
    /// crate-private) and then forwards to the same `apply_activate`.
    fn activate(menu: &mut GnuMenu, editor: &mut Editor, completer: &mut dyn Completer) {
        menu.menu_event(MenuEvent::Activate(false));
        assert!(menu.event.take().is_some(), "Activate must be queued");
        menu.apply_activate(editor, completer);
    }

    /// Drive an engine Edit (a TAB that fell through the already-active Menu
    /// arm, or a keystroke) the same way.
    fn edit_event(menu: &mut GnuMenu, editor: &Editor) {
        menu.menu_event(MenuEvent::Edit(false));
        assert!(menu.event.take().is_some(), "Edit must be queued");
        menu.apply_edit(editor);
    }

    #[test]
    fn tab1_splices_common_prefix_and_stays_quiet() {
        let mut completer = PrefixCompleter::new(&["checkout", "checkouts"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("che");

        activate(&mut menu, &mut editor, &mut completer);

        assert_eq!(
            editor.get_buffer(),
            "checkout",
            "TAB #1 completes the shared prefix"
        );
        assert!(!menu.is_active(), "no list after TAB #1");
        assert_eq!(menu.phase, GnuPhase::Attempted);
        assert_eq!(menu.menu_string(10, false), "", "nothing printed yet");
    }

    #[test]
    fn tab1_with_no_prefix_extension_still_anchors_the_attempt() {
        // `gi` against [git, gift]: the common prefix IS the typed word —
        // GNU rings the bell and waits for the second TAB. So does niu,
        // minus the bell.
        let mut completer = PrefixCompleter::new(&["git", "gift"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("gi");

        activate(&mut menu, &mut editor, &mut completer);

        assert_eq!(editor.get_buffer(), "gi");
        assert!(!menu.is_active());
        assert_eq!(menu.phase, GnuPhase::Attempted);
    }

    #[test]
    fn lone_candidate_completes_without_listing() {
        let mut completer = PrefixCompleter::new(&["checkout"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("che");

        activate(&mut menu, &mut editor, &mut completer);

        assert_eq!(
            editor.get_buffer(),
            "checkout",
            "lone candidate completes outright"
        );
        assert!(!menu.is_active(), "a lone candidate never lists");
    }

    #[test]
    fn ambiguous_second_tab_lists_and_stays_put() {
        let mut completer = PrefixCompleter::new(&["checkout", "checkouts"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("check");

        // TAB #1: the shared prefix "checkout" splices in.
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "checkout");
        assert!(!menu.is_active());

        // TAB #2 on the unchanged line: the list prints, menu turns active.
        activate(&mut menu, &mut editor, &mut completer);
        assert!(menu.is_active(), "TAB #2 on an unchanged line lists");
        assert_eq!(menu.phase, GnuPhase::Listed);
        let listed = menu.menu_string(20, false);
        assert!(
            listed.contains("checkout"),
            "list carries the candidates: {listed:?}"
        );
        assert!(
            listed.contains("checkouts"),
            "list carries the candidates: {listed:?}"
        );
        // The line itself is untouched by listing (non-interactive list).
        assert_eq!(editor.get_buffer(), "checkout");
    }

    #[test]
    fn third_tab_keeps_the_list() {
        let mut completer = PrefixCompleter::new(&["checkout", "checkouts"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("check");
        activate(&mut menu, &mut editor, &mut completer);
        activate(&mut menu, &mut editor, &mut completer);
        assert!(menu.is_active());

        // TAB #3 arrives as an Edit event (the engine falls through its
        // already-active Menu arm) with an unchanged buffer: list stays.
        edit_event(&mut menu, &editor);
        assert!(menu.is_active(), "unchanged line keeps the list up");
        assert_eq!(menu.phase, GnuPhase::Listed);
    }

    #[test]
    fn typing_dismisses_the_list_and_next_tab_starts_fresh() {
        let mut completer = PrefixCompleter::new(&["checkout", "checkouts"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("check");
        activate(&mut menu, &mut editor, &mut completer);
        activate(&mut menu, &mut editor, &mut completer);
        assert!(menu.is_active());

        // The user types more: the engine delivers an Edit event per key.
        editor.edit_buffer(
            |line| line.set_buffer("checkoutx".to_string()),
            UndoBehavior::CreateUndoPoint,
        );
        edit_event(&mut menu, &editor);

        assert!(!menu.is_active(), "an edit dismisses the printed list");
        assert_eq!(menu.phase, GnuPhase::Idle);

        // The next TAB is a fresh attempt, not a re-list: `checkoutx` has no
        // candidates, so the attempt lands quiet and stays harmless.
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "checkoutx", "no match, no edit");
        assert!(!menu.is_active());
        assert_eq!(menu.phase, GnuPhase::Idle);
    }

    #[test]
    fn changed_line_between_tabs_retries_instead_of_listing() {
        let mut completer = PrefixCompleter::new(&["checkout", "checkouts"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("che");
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(menu.phase, GnuPhase::Attempted);

        // The user rejects the prefix and retypes before the second TAB.
        editor.edit_buffer(
            |line| line.set_buffer("cheque".to_string()),
            UndoBehavior::CreateUndoPoint,
        );
        activate(&mut menu, &mut editor, &mut completer);

        assert!(
            !menu.is_active(),
            "a changed line must not turn the next TAB into a list"
        );
        assert_eq!(menu.phase, GnuPhase::Idle, "no candidates for `cheque`");
    }

    #[test]
    fn empty_completion_quiets_the_menu() {
        let mut completer = PrefixCompleter::new(&["checkout"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("zz");

        activate(&mut menu, &mut editor, &mut completer);

        assert_eq!(editor.get_buffer(), "zz", "no match, no edit");
        assert!(!menu.is_active());
        assert_eq!(menu.phase, GnuPhase::Idle);
    }

    #[test]
    fn deactivate_resets_the_state_machine() {
        let mut completer = PrefixCompleter::new(&["checkout", "checkouts"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("che");
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(menu.phase, GnuPhase::Attempted);

        // Esc/Enter route through MenuEvent::Deactivate (engine
        // deactivate_menus): the anchored attempt must not leak into the
        // next line.
        menu.menu_event(MenuEvent::Deactivate);
        assert_eq!(menu.phase, GnuPhase::Idle);
        assert!(!menu.is_active());
        assert_eq!(menu.anchor_buffer, None);
    }

    #[test]
    fn column_layout_matches_gnu_geometry() {
        // field = longest + 2; columns = width / field (min 1, at most one
        // per candidate); rows ceil(count / columns).
        assert_eq!(column_layout(3, 5, 21), (3, 1), "21 / 7 = 3 columns");
        assert_eq!(column_layout(3, 5, 20), (2, 2), "20 / 7 = 2 columns");
        assert_eq!(
            column_layout(3, 5, 6),
            (1, 3),
            "field wider than the terminal: one column"
        );
        assert_eq!(column_layout(1, 5, 7), (1, 1));
        assert_eq!(
            column_layout(0, 0, 80),
            (1, 1),
            "degenerate input stays safe"
        );
        assert_eq!(
            column_layout(10, 1, 80),
            (10, 1),
            "columns never exceed the candidate count"
        );
    }

    #[test]
    fn render_list_is_byte_exact_column_major() {
        // One row, three columns; every cell pads to field width (5 + 2),
        // the line-end trim drops only the trailing gutter.
        let values = ["alpha", "beta", "gamma"];
        let widths = [5, 4, 5];
        assert_eq!(render_list(&values, &widths, 21, 10), "alpha  beta   gamma");

        // Two rows, two columns: down first (alpha, beta | gamma).
        assert_eq!(render_list(&values, &widths, 14, 10), "alpha  gamma\nbeta");

        // Clipped to one available line: the grid refills at rows=1, so the
        // first row carries the first two candidates in order.
        assert_eq!(render_list(&values, &widths, 14, 1), "alpha  beta");

        // Empty input renders nothing.
        assert_eq!(render_list(&[], &[], 80, 10), "");
    }

    #[test]
    fn menu_required_lines_tracks_the_listed_rows() {
        let mut completer = PrefixCompleter::new(&["checkout", "checkouts"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("che");
        assert_eq!(
            menu.menu_required_lines(80),
            0,
            "quiet before the first TAB"
        );

        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(menu.menu_required_lines(80), 0, "still quiet after TAB #1");

        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(
            menu.menu_required_lines(80),
            1,
            "two candidates share one 80-col row"
        );
    }
}
