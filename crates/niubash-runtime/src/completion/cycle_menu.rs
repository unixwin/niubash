//! Cycle interaction for TAB completion (niubash#631): the
//! `completion.interaction = cycle` behavior — the default since
//! 1.5.0 (owner ruling 2026-10-28).
//!
//! Behavior contract (DESIGN §7, as ruled):
//!
//! * **TAB #1** queries the candidates once, puts the *first candidate*
//!   into the line (not the common prefix — cycling replaces the
//!   prefix-commit of the `insert` interaction) and shows the candidate
//!   grid with that candidate highlighted. The candidate set is frozen
//!   for the whole session: it was computed against the line as typed,
//!   and re-querying after a splice would narrow it to the spliced
//!   candidate's own matches.
//! * **Further TABs / Shift-TABs** move the highlight through the set
//!   with wrap-around (a full loop lands back on the first candidate),
//!   splicing each highlighted candidate into the line in place of the
//!   previous one. The splice itself is issued by the edit-mode
//!   wrapper at key-translation time (see [`CycleSession::nav_seq`]):
//!   a menu-side splice could only run at paint time, in
//!   `update_working_details`, and the painter composes the frame's
//!   input line *before* that hook — the line would lag the grid's
//!   highlight by a full paint on every TAB. This menu keeps the
//!   fallback splice for a navigation event delivered straight to it.
//! * **Any other edit key** ends the session as *accepted in place*:
//!   the candidate in the line stays and the key applies on top of it.
//! * **Enter** accepts the candidate in the line and runs the command;
//!   **Esc** rolls the line back to its pre-completion snapshot. Those
//!   two are completed by the edit-mode wrapper
//!   (`PasteChunkEditMode`), not here: a reedline `Menu` never sees raw
//!   keys, and at `Deactivate` time it is handed no `Editor`, so the
//!   rollback edit cannot be issued from the menu. The menu publishes
//!   the session (snapshot + "a candidate is in the line") into the
//!   shared [`CycleSession`] cell; the wrapper consumes it at key
//!   translation time — synchronously, before the engine sees the key —
//!   which is what makes the rollback exact even inside a key burst.
//!
//! A decorator over the presentation menus was weighed and rejected:
//! reedline's stock menus expose no selected index, re-query on every
//! event (narrowing the frozen set), and recompute spans against a
//! buffer the cycling itself mutates. So under `interaction = cycle`
//! this menu renders its own grid (the GNU menu's column geometry) for
//! every non-GNU style; `completion.style` keeps owning the `insert`
//! interaction's presentation, and the GNU style is exempt from the
//! axis entirely (its printed list is the literal readline shape).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use reedline::menu_functions::completer_input;
use reedline::{
    Completer, CompletionResult, EditCommand, Editor, InputMode, Menu, MenuEvent, MenuSettings,
    Painter, Suggestion, UndoBehavior,
};
use unicode_width::UnicodeWidthStr;

use super::gnu_menu::{column_layout, render_list};

/// The line as it stood when a cycle session began: what Esc restores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleSnapshot {
    /// The whole buffer (cycle completion is a whole-line snapshot —
    /// the pre-completion state is the only well-defined rollback
    /// target once candidates have been spliced in and out).
    pub buffer: String,
    /// The cursor's byte offset in `buffer`.
    pub cursor: usize,
}

/// One frozen candidate, as the edit-mode wrapper needs it for the
/// navigation splice it issues (the display side stays in the menu).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleCandidate {
    /// The replacement text (without the whitespace suffix — the
    /// splice adds it per `append_whitespace`).
    pub value: String,
    /// Whether accepting/cycling onto this candidate appends a space.
    pub append_whitespace: bool,
}

impl CycleCandidate {
    /// The text a splice of this candidate puts into the line.
    pub fn splice_text(&self) -> String {
        let mut text = self.value.clone();
        if self.append_whitespace {
            text.push(' ');
        }
        text
    }
}

/// The session state shared between [`CycleMenu`] (publisher) and the
/// edit-mode wrapper (consumer). The menu owns the session's truth;
/// the wrapper reads/consumes it for the keys the menu cannot finish
/// itself: TAB / Shift-TAB navigation (spliced by the wrapper at key
/// translation time — a menu-side splice lands in
/// `update_working_details`, whose frame the painter has already
/// composed the input line for, so the line would lag the highlight
/// by a full paint), Esc rollback, Enter accept-and-run, and
/// edit-key accept-in-place.
#[derive(Debug, Default)]
pub struct CycleSession {
    /// A cycle session is live: the menu is open over a frozen set.
    pub open: bool,
    /// The pre-completion snapshot, published when the session begins.
    pub snapshot: Option<CycleSnapshot>,
    /// A candidate has been spliced into the line — there is something
    /// an Esc would need to roll back. (A session whose first TAB is
    /// still being painted has a snapshot but nothing spliced yet;
    /// Esc there is the plain menu close.)
    pub spliced: bool,
    /// The frozen candidate set, in cycle order.
    pub candidates: Vec<CycleCandidate>,
    /// The replaced span in the *snapshot* buffer: mid-session the
    /// current buffer is always the snapshot with this span replaced
    /// by the current candidate's splice text (any other edit ends
    /// the session), which is what makes the wrapper's whole-line
    /// navigation splice a pure string computation.
    pub span: (usize, usize),
    /// The highlighted candidate's index into `candidates`.
    pub index: usize,
    /// The byte range in the *current* buffer the current candidate's
    /// splice text occupies.
    pub region: (usize, usize),
    /// How many navigation splices the wrapper has issued this
    /// session. Each one reaches the menu as an `Edit` event (the
    /// engine reports the wrapper's edit to the active menu); the
    /// menu consumes one sequence number per such echo to tell it
    /// apart from a genuine user edit, which ends the session.
    pub nav_seq: u64,
}

impl CycleSession {
    fn begin(
        &mut self,
        snapshot: CycleSnapshot,
        candidates: Vec<CycleCandidate>,
        span: (usize, usize),
    ) {
        self.open = true;
        self.snapshot = Some(snapshot);
        self.spliced = false;
        self.candidates = candidates;
        self.span = span;
        self.index = 0;
        self.region = span;
        self.nav_seq = 0;
    }

    fn mark_spliced(&mut self, region: (usize, usize)) {
        self.spliced = true;
        self.region = region;
    }

    fn end(&mut self) {
        self.open = false;
        self.snapshot = None;
        self.spliced = false;
        self.candidates = Vec::new();
        self.index = 0;
        self.nav_seq = 0;
    }

    /// Whether a session is live (the edit-mode wrapper's Enter gate).
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Consume the session for an Esc: returns the snapshot to roll
    /// back to when a candidate was actually spliced into the line,
    /// and ends the session either way — an Esc always closes the
    /// cycle menu, spliced or not, and no later key may act on the
    /// consumed state.
    pub fn take_rollback(&mut self) -> Option<CycleSnapshot> {
        let rollback = if self.open && self.spliced {
            self.snapshot.clone()
        } else {
            None
        };
        self.end();
        rollback
    }

    /// End the session without a rollback (an edit key accepted the
    /// candidate in place). No-op when no session is live.
    pub fn end_if_open(&mut self) {
        if self.open {
            self.end();
        }
    }
}

/// The shared cell, created per editor build and handed to both the
/// menu and the edit-mode wrapper.
pub type SharedCycleSession = Arc<Mutex<CycleSession>>;

/// The edit batch that puts `buffer` on the line with the cursor at
/// `cursor`: clear, insert, reposition. This is the whole-buffer
/// replacement shape the widget outcome path already uses — a
/// select-then-insert construction was tried first for the rollback
/// and does not replace the selection in this editor (the snapshot
/// landed appended to the spliced line; PTY transcript in the cluster
/// report). Both the Esc rollback and the wrapper's navigation
/// splice are whole-line rewrites, so they share the constructor.
pub fn whole_line_edits(buffer: &str, cursor: usize) -> Vec<EditCommand> {
    vec![
        EditCommand::Clear,
        EditCommand::InsertString(buffer.to_string()),
        EditCommand::MoveToPosition {
            position: cursor,
            select: false,
        },
    ]
}

/// The edit commands that restore `snapshot` in the editor. Pure
/// constructor — the wrapper prepends these to the Esc's own
/// translation, so the restore and the menu close land in one engine
/// batch, restore first.
pub fn rollback_edits(snapshot: &CycleSnapshot) -> Vec<EditCommand> {
    whole_line_edits(&snapshot.buffer, snapshot.cursor)
}

/// The cycle-interaction completion menu. See the [module docs](self).
///
/// Like [`super::gnu_menu::GnuMenu`], it carries a [`MenuSettings`]
/// (the engine routes menu events by `settings().name`) and queues
/// engine events for processing where an `Editor` is reachable —
/// with one difference: `Activate` is *also* drained from
/// [`Menu::can_partially_complete`], the engine's event-path hook, so
/// the session (and its shared-cell publication) exists before the
/// next raw key is translated. Navigation events are applied at paint
/// time, like the GNU menu's.
pub struct CycleMenu {
    settings: MenuSettings,
    active: bool,
    /// Engine events queued by [`CycleMenu::menu_event`], processed
    /// FIFO (the GnuMenu deque discipline: a single slot let a trailing
    /// Edit overwrite a pending Activate and desync `active`).
    events: VecDeque<MenuEvent>,
    /// The frozen candidate set of the live session.
    values: Vec<Suggestion>,
    /// Display widths parallel to `values` (ANSI-free display text).
    display_widths: Vec<usize>,
    /// The highlighted candidate's index into `values`.
    index: usize,
    /// Whether the live session has begun (snapshot taken, first
    /// candidate spliced). Before that, an activation may still be
    /// waiting for its first answer (`Pending`).
    session_begun: bool,
    /// An `Activate` was processed but produced no values yet.
    activation_pending: bool,
    /// The completer has been asked for the current activation —
    /// a `Pending` answer must not make the drain ask twice for the
    /// same TAB (the settle's `update_values` is the re-ask path).
    queried: bool,
    /// The byte range in the *current* buffer the next splice
    /// replaces: the snapshot span at session start, thereafter the
    /// previously spliced candidate's extent.
    region: (usize, usize),
    /// Navigation echoes already consumed (see
    /// [`CycleSession::nav_seq`]): an `Edit` event whose sequence
    /// number is still outstanding is the wrapper's navigation splice
    /// arriving, not a user edit.
    echoes_seen: u64,
    /// Whether the last query's answer may still be superseded
    /// (`Pending` / `Stale`): such a set navigates normally but the
    /// engine refuses to accept from it.
    provisional: bool,
    /// The shared session cell (see the module docs).
    session: SharedCycleSession,
    /// The grid-visibility flag the autosuggest hinter reads (the
    /// ColumnMenu discipline: raised on Activate, lowered on any
    /// other menu event, so the ghost text stands down while the
    /// grid is on screen).
    menu_open: Option<Arc<AtomicBool>>,
    /// Terminal width seen at the last paint; 80 before the first.
    terminal_columns: usize,
}

impl CycleMenu {
    pub fn new(session: SharedCycleSession) -> Self {
        Self {
            settings: MenuSettings::default(),
            active: false,
            events: VecDeque::new(),
            values: Vec::new(),
            display_widths: Vec::new(),
            index: 0,
            session_begun: false,
            activation_pending: false,
            queried: false,
            region: (0, 0),
            echoes_seen: 0,
            provisional: false,
            session,
            menu_open: None,
            terminal_columns: 80,
        }
    }

    /// Name the menu (the engine routes `ReedlineEvent::Menu(name)` by it).
    #[must_use]
    pub fn with_name(mut self, name: &str) -> Self {
        self.settings = self.settings.with_name(name);
        self
    }

    /// Attach the grid-visibility flag shared with the hinter.
    #[must_use]
    pub fn with_menu_open_flag(mut self, menu_open: Arc<AtomicBool>) -> Self {
        self.menu_open = Some(menu_open);
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

    /// Fetch the candidate set for the current line. Only ever called
    /// outside a begun session — the set is frozen once cycling starts.
    fn query(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        self.queried = true;
        let (input, pos) = Self::completer_query(editor);
        let (result, _base_ranges) = completer.complete_with_base_ranges(&input, pos);
        match result {
            CompletionResult::Fresh { suggestions, .. } => {
                self.provisional = false;
                self.adopt(suggestions);
            }
            CompletionResult::Stale { suggestions, .. } => {
                self.provisional = true;
                self.adopt(suggestions);
            }
            CompletionResult::Pending => {
                self.provisional = true;
                self.display_widths = Vec::new();
                self.values = Vec::new();
            }
        }
    }

    fn adopt(&mut self, suggestions: Arc<[Suggestion]>) {
        self.display_widths = suggestions
            .iter()
            .map(|suggestion| suggestion.display_value().width())
            .collect();
        self.values = suggestions.to_vec();
    }

    /// Begin the session over the current values: snapshot the line,
    /// publish the session to the shared cell, splice the first
    /// candidate. A lone candidate completes outright and ends the
    /// session at once (GNU/zsh shape — nothing left to cycle).
    fn begin_session(&mut self, editor: &mut Editor) {
        let snapshot = CycleSnapshot {
            buffer: editor.get_buffer().to_string(),
            cursor: editor.line_buffer().insertion_point(),
        };
        let span = self.values[0].span;
        self.region = (span.start, span.end);
        self.index = 0;
        self.echoes_seen = 0;
        self.session_begun = true;
        self.activation_pending = false;
        let candidates: Vec<CycleCandidate> = self
            .values
            .iter()
            .map(|suggestion| CycleCandidate {
                value: suggestion.value.clone(),
                append_whitespace: suggestion.append_whitespace,
            })
            .collect();
        if let Ok(mut session) = self.session.lock() {
            session.begin(snapshot, candidates, (span.start, span.end));
        }
        self.splice(editor);
        if self.values.len() == 1 {
            self.end_session();
        }
    }

    /// Splice the highlighted candidate into the line, replacing the
    /// tracked region, and land the cursor at the end of the splice.
    /// `append_whitespace` is honored exactly like a stock accept: a
    /// trailing space joins the replacement (and the region), so the
    /// next cycle step replaces it too instead of accumulating spaces.
    fn splice(&mut self, editor: &mut Editor) {
        let Some(suggestion) = self.values.get(self.index) else {
            return;
        };
        let mut text = suggestion.value.clone();
        if suggestion.append_whitespace {
            text.push(' ');
        }
        let (start, end) = self.region;
        let new_end = start + text.len();
        editor.edit_buffer(
            |buffer| {
                buffer.replace_range(start..end, &text);
                buffer.set_insertion_point(new_end);
            },
            UndoBehavior::CreateUndoPoint,
        );
        self.region = (start, new_end);
        if let Ok(mut session) = self.session.lock() {
            session.mark_spliced(self.region);
        }
    }

    /// End the session (any reason): the menu closes and the shared
    /// cell is cleared, so no later key can act on a stale snapshot.
    /// The buffer is left exactly as it stands — ending a session
    /// never edits the line; the rollback is the wrapper's Esc path,
    /// driven by the snapshot the cell held *before* this clear.
    fn end_session(&mut self) {
        self.active = false;
        self.session_begun = false;
        self.activation_pending = false;
        self.queried = false;
        self.echoes_seen = 0;
        self.values = Vec::new();
        self.display_widths = Vec::new();
        self.index = 0;
        self.provisional = false;
        if let Ok(mut session) = self.session.lock() {
            session.end();
        }
    }

    /// Back to factory state (the `Deactivate` path via `clear_input`).
    fn reset(&mut self) {
        self.events.clear();
        self.end_session();
    }

    /// TAB #1: query (unless the engine's `update_values` just did),
    /// then begin the session over whatever the query produced. No
    /// candidates: the TAB was inert, the menu closes at once.
    fn apply_activate(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        if self.session_begun {
            return;
        }
        self.activation_pending = true;
        if self.values.is_empty() && !self.queried {
            self.query(editor, completer);
        }
        if self.values.is_empty() {
            // Nothing to cycle (yet): a `Pending` answer keeps the
            // activation pending for the deferred settle; a final
            // empty answer closes the menu — an empty cycle session
            // must never wedge the editor (niubash#373's shape).
            if !self.provisional {
                self.end_session();
            }
            return;
        }
        self.begin_session(editor);
    }

    /// Move the highlight by `delta` (wrapping) and splice the newly
    /// highlighted candidate. A full loop lands back on the first
    /// candidate — the wrap is the semantics, pinned by test.
    fn apply_navigate(&mut self, editor: &mut Editor, delta: isize) {
        if !self.session_begun {
            // Belt and braces for a late first answer: if values are
            // somehow on hand for a pending activation, this TAB
            // begins the session instead of navigating a void.
            if self.activation_pending && !self.values.is_empty() {
                self.begin_session(editor);
            }
            return;
        }
        if self.values.is_empty() {
            return;
        }
        let len = self.values.len() as isize;
        self.index = (self.index as isize + delta).rem_euclid(len) as usize;
        self.splice(editor);
        // The wrapper normally drives navigation itself (see
        // `CycleSession::nav_seq`); this menu-local path is the
        // fallback for a MenuNext/MenuPrevious delivered straight to
        // the menu (a user binding, say). Mirror the outcome into
        // the cell so the wrapper's view stays truthful — without a
        // nav_seq bump: no Edit echo follows a menu-side splice.
        if let Ok(mut session) = self.session.lock() {
            session.index = self.index;
            session.region = self.region;
        }
    }

    /// An edit landed while the menu was up. Two kinds arrive here:
    /// the wrapper's navigation splice (an echo — the session
    /// continues; sync the highlight from the shared cell), and a
    /// genuine user edit, whose arrival breaks the session's frozen
    /// premise (spans against the snapshot line), so the session ends
    /// as accepted-in-place: the candidate in the line stays, the
    /// edit already applied on top of it.
    fn apply_edit(&mut self) {
        if let Ok(session) = self.session.lock() {
            if session.open && self.echoes_seen < session.nav_seq {
                self.echoes_seen += 1;
                self.index = session.index;
                self.region = session.region;
                return;
            }
        }
        self.end_session();
    }

    /// Run every queued engine event in arrival order.
    fn drain_events(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        while let Some(event) = self.events.pop_front() {
            match event {
                MenuEvent::Activate(_) => self.apply_activate(editor, completer),
                MenuEvent::NextElement => self.apply_navigate(editor, 1),
                MenuEvent::PreviousElement => self.apply_navigate(editor, -1),
                MenuEvent::Edit(_) => self.apply_edit(),
                // Deactivate was applied by `handle_menu_event`
                // (set_active + clear_input); nothing is left to do.
                _ => {}
            }
        }
    }
}

impl Menu for CycleMenu {
    fn settings(&self) -> &MenuSettings {
        &self.settings
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
        if let Some(menu_open) = &self.menu_open {
            // The hinter flag tracks grid visibility: up on Activate,
            // down on Deactivate. Unlike the ColumnMenu discipline
            // (any other event lowers it), a navigation *echo* Edit
            // must not lower it — the grid is still on screen — while
            // a genuine Edit ends the session and lowers it.
            let raise = match &event {
                MenuEvent::Activate(_) => Some(true),
                MenuEvent::Deactivate => Some(false),
                MenuEvent::Edit(_) => {
                    let echo_pending = self
                        .session
                        .lock()
                        .map(|session| session.open && self.echoes_seen < session.nav_seq)
                        .unwrap_or(false);
                    if echo_pending {
                        None
                    } else {
                        Some(false)
                    }
                }
                _ => None,
            };
            if let Some(raise) = raise {
                menu_open.store(raise, Ordering::Relaxed);
            }
        }
        self.handle_menu_event(&event);
        self.events.push_back(event);
    }

    fn can_quick_complete(&self) -> bool {
        // A lone candidate must NOT turn the next TAB into Enter (the
        // engine's MenuNext lone-value conversion reads this flag):
        // TAB cycles, Enter is the user's own key.
        false
    }

    fn can_partially_complete(
        &mut self,
        _values_updated: bool,
        editor: &mut Editor,
        completer: &mut dyn Completer,
    ) -> bool {
        // The event-path drain: an Activate queued by `menu_event`
        // begins the session HERE, synchronously, so the shared cell
        // is already published when the next raw key is translated
        // (the wrapper's Esc/Enter decisions read it). Navigation
        // events are not in the queue yet at this hook (the engine
        // calls it before `menu_event(NextElement)`), so this is a
        // no-op for them. The stock common-prefix splice stays off —
        // cycling owns the buffer; returning false is the contract.
        self.drain_events(editor, completer);
        false
    }

    fn update_values(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        // The set is frozen once a session is live (or an activation
        // already holds values): re-querying would narrow it to the
        // spliced candidate's matches. Before that, this is where the
        // engine's Activate path fills the set.
        if self.session_begun || !self.values.is_empty() {
            return;
        }
        self.query(editor, completer);
        // A `Pending` first answer left the activation pending with
        // the Activate event already consumed; the deferred settle
        // replays through here, so this is where that session begins
        // — no queued event remains to begin it.
        if self.activation_pending && !self.values.is_empty() {
            self.begin_session(editor);
        }
    }

    fn reset_position(&mut self) {
        self.index = 0;
    }

    fn update_working_details(
        &mut self,
        editor: &mut Editor,
        completer: &mut dyn Completer,
        painter: &Painter,
    ) {
        self.terminal_columns = painter.screen_width() as usize;
        self.drain_events(editor, completer);
    }

    fn replace_in_buffer(&self, _editor: &mut Editor) {
        // Accept is a deliberate no-op: the highlighted candidate is
        // already in the line (that is the cycle form). The engine's
        // Deactivate right after ends the session.
    }

    fn menu_required_lines(&self, terminal_columns: u16) -> u16 {
        if !self.session_begun {
            return 0;
        }
        let (_, rows) = column_layout(
            self.display_widths.len(),
            self.display_widths.iter().copied().max().unwrap_or(0),
            terminal_columns as usize,
        );
        rows.min(u16::MAX as usize) as u16
    }

    fn menu_string(&self, available_lines: u16, use_ansi_coloring: bool) -> String {
        if !self.session_begun || self.values.is_empty() {
            return String::new();
        }
        // The selected cell wears the stock selected style
        // (MenuTextStyle's default: green, bold, reversed) — the
        // settings' own color is not readable outside reedline, and
        // the default is what every stock menu paints.
        let selected_style = nu_ansi_term::Style::new()
            .fg(nu_ansi_term::Color::Green)
            .bold()
            .reverse();
        let cells: Vec<String> = self
            .values
            .iter()
            .enumerate()
            .map(|(position, suggestion)| {
                let text = suggestion.display_value();
                if use_ansi_coloring && position == self.index {
                    format!(
                        "{}{}{}",
                        selected_style.prefix(),
                        text,
                        selected_style.suffix()
                    )
                } else {
                    text.to_string()
                }
            })
            .collect();
        let values: Vec<&str> = cells.iter().map(String::as_str).collect();
        render_list(
            &values,
            &self.display_widths,
            self.terminal_columns,
            available_lines,
        )
    }

    fn min_rows(&self) -> u16 {
        self.menu_required_lines(self.terminal_columns as u16)
    }

    fn get_values(&self) -> &[Suggestion] {
        &self.values
    }

    fn results_are_provisional(&self) -> bool {
        self.provisional
    }

    fn is_awaiting_first_answer(&self) -> bool {
        self.active && self.activation_pending && self.values.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reedline::{CompletionResult, Span};

    /// The GnuMenu harness completer: candidates INCLUDE the exact
    /// typed word, the shape niubash's real completer produces.
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

    fn menu_with_session() -> (CycleMenu, SharedCycleSession) {
        let session: SharedCycleSession = Arc::new(Mutex::new(CycleSession::default()));
        (CycleMenu::new(Arc::clone(&session)), session)
    }

    /// Drive an Activate the way the engine does: `menu_event`, then
    /// the event-path partial-completion hook.
    fn activate(menu: &mut CycleMenu, editor: &mut Editor, completer: &mut dyn Completer) {
        menu.menu_event(MenuEvent::Activate(false));
        let _ = menu.can_partially_complete(false, editor, completer);
    }

    /// Drive a navigation event the way the engine does: the partial
    /// hook first (a no-op for navigation), then `menu_event`, then
    /// the paint-time drain.
    fn navigate(
        menu: &mut CycleMenu,
        editor: &mut Editor,
        completer: &mut dyn Completer,
        event: MenuEvent,
    ) {
        let _ = menu.can_partially_complete(false, editor, completer);
        menu.menu_event(event);
        menu.drain_events(editor, completer);
    }

    #[test]
    fn issue631_first_tab_splices_first_candidate_and_publishes_snapshot() {
        let mut completer = PrefixCompleter::new(&["checkout", "cherry", "cherry-pick"]);
        let (mut menu, session) = menu_with_session();
        let mut editor = editor_with("git ch");

        activate(&mut menu, &mut editor, &mut completer);

        assert_eq!(editor.get_buffer(), "git checkout");
        assert!(menu.is_active());
        let session = session.lock().unwrap();
        assert!(session.open);
        assert!(session.spliced);
        assert_eq!(
            session.snapshot,
            Some(CycleSnapshot {
                buffer: "git ch".to_string(),
                cursor: 6,
            })
        );
    }

    #[test]
    fn issue631_cycle_wraps_a_full_loop_back_to_the_start() {
        let mut completer = PrefixCompleter::new(&["checkout", "cherry", "cherry-pick"]);
        let (mut menu, _session) = menu_with_session();
        let mut editor = editor_with("git ch");

        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "git checkout");
        navigate(
            &mut menu,
            &mut editor,
            &mut completer,
            MenuEvent::NextElement,
        );
        assert_eq!(editor.get_buffer(), "git cherry");
        navigate(
            &mut menu,
            &mut editor,
            &mut completer,
            MenuEvent::NextElement,
        );
        assert_eq!(editor.get_buffer(), "git cherry-pick");
        // The full loop: the third TAB lands back on the first candidate.
        navigate(
            &mut menu,
            &mut editor,
            &mut completer,
            MenuEvent::NextElement,
        );
        assert_eq!(editor.get_buffer(), "git checkout");
        // And Shift-TAB wraps backwards to the last candidate.
        navigate(
            &mut menu,
            &mut editor,
            &mut completer,
            MenuEvent::PreviousElement,
        );
        assert_eq!(editor.get_buffer(), "git cherry-pick");
    }

    #[test]
    fn issue631_rollback_edits_restore_the_snapshot_line_and_cursor() {
        // The constructor is pinned structurally (the engine applies
        // the batch through `ReedlineEvent::Edit`; the end-to-end
        // restore is PTY-proven — see the cluster REPORT). The shape
        // is the contract: clear the line, insert the snapshot text,
        // put the cursor back at the snapshot offset.
        let snapshot = CycleSnapshot {
            buffer: "git ch".to_string(),
            cursor: 6,
        };
        assert_eq!(
            rollback_edits(&snapshot),
            vec![
                EditCommand::Clear,
                EditCommand::InsertString("git ch".to_string()),
                EditCommand::MoveToPosition {
                    position: 6,
                    select: false,
                },
            ]
        );
    }

    #[test]
    fn issue631_edit_key_ends_the_session_as_accepted_in_place() {
        let mut completer = PrefixCompleter::new(&["checkout", "cherry"]);
        let (mut menu, session) = menu_with_session();
        let mut editor = editor_with("git ch");
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "git checkout");

        // A typed character lands on the spliced candidate; the menu's
        // Edit event ends the session without touching the buffer, and
        // the shared cell is cleared — a later Esc has nothing to roll
        // back.
        editor.edit_buffer(
            |buffer| buffer.insert_str("!"),
            UndoBehavior::CreateUndoPoint,
        );
        menu.menu_event(MenuEvent::Edit(false));
        menu.drain_events(&mut editor, &mut completer);

        assert_eq!(editor.get_buffer(), "git checkout!");
        assert!(!menu.is_active());
        let session = session.lock().unwrap();
        assert!(!session.open);
        assert!(session.snapshot.is_none());
    }

    #[test]
    fn issue631_empty_candidate_set_closes_the_menu_at_once() {
        let mut completer = PrefixCompleter::new(&["checkout"]);
        let (mut menu, session) = menu_with_session();
        let mut editor = editor_with("git zz");
        activate(&mut menu, &mut editor, &mut completer);
        assert!(!menu.is_active());
        assert_eq!(editor.get_buffer(), "git zz");
        assert!(!session.lock().unwrap().open);
    }

    #[test]
    fn issue631_lone_candidate_completes_outright_and_ends_the_session() {
        let mut completer = PrefixCompleter::new(&["checkout", "status"]);
        let (mut menu, session) = menu_with_session();
        let mut editor = editor_with("git che");
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "git checkout");
        assert!(!menu.is_active());
        assert!(!session.lock().unwrap().open);
    }

    /// A completer whose first answer is `Pending` and whose second
    /// is the real set — the shape of a host completer whose answer
    /// lands a frame after the TAB.
    struct PendingOnceCompleter {
        inner: PrefixCompleter,
        asked: usize,
    }

    impl Completer for PendingOnceCompleter {
        fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
            self.asked += 1;
            if self.asked == 1 {
                return CompletionResult::Pending;
            }
            self.inner.complete(line, pos)
        }
    }

    #[test]
    fn issue631_wrapper_navigation_echo_keeps_the_session_and_syncs_the_highlight() {
        let mut completer = PrefixCompleter::new(&["checkout", "cherry", "cherry-pick"]);
        let (mut menu, session) = menu_with_session();
        let mut editor = editor_with("git ch");
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "git checkout");

        // The wrapper's navigation lands: it splices candidate 2 into
        // the editor itself and records the move in the cell; the
        // engine then reports that edit to the menu as an Edit event.
        // The menu must read it as the session continuing (an echo),
        // sync its highlight, and stay open.
        {
            let mut state = session.lock().unwrap();
            state.index = 1;
            state.region = (4, 10);
            state.nav_seq = 1;
        }
        editor.edit_buffer(
            |buffer| {
                buffer.replace_range(4..12, "cherry");
                buffer.set_insertion_point(10);
            },
            UndoBehavior::CreateUndoPoint,
        );
        menu.menu_event(MenuEvent::Edit(false));
        menu.drain_events(&mut editor, &mut completer);
        assert!(menu.is_active());
        assert!(session.lock().unwrap().open);

        // The highlight really synced: a menu-local PreviousElement
        // steps from candidate 2 back to candidate 1.
        navigate(
            &mut menu,
            &mut editor,
            &mut completer,
            MenuEvent::PreviousElement,
        );
        assert_eq!(editor.get_buffer(), "git checkout");

        // A further Edit with no outstanding echo is a genuine user
        // edit: the session ends as accepted-in-place.
        menu.menu_event(MenuEvent::Edit(false));
        menu.drain_events(&mut editor, &mut completer);
        assert!(!menu.is_active());
        assert!(!session.lock().unwrap().open);
    }

    #[test]
    fn issue631_pending_first_answer_begins_the_session_when_it_lands() {
        let mut completer = PendingOnceCompleter {
            inner: PrefixCompleter::new(&["checkout", "cherry"]),
            asked: 0,
        };
        let (mut menu, session) = menu_with_session();
        let mut editor = editor_with("git ch");

        // TAB: the answer is Pending — the menu is active but no
        // session has begun and nothing was spliced.
        activate(&mut menu, &mut editor, &mut completer);
        assert!(menu.is_active());
        assert!(menu.results_are_provisional());
        assert_eq!(editor.get_buffer(), "git ch");
        assert!(!session.lock().unwrap().open);

        // The deferred settle replays through update_values: the
        // session begins there, over the landed set.
        menu.update_values(&mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "git checkout");
        assert!(session.lock().unwrap().open);
    }

    #[test]
    fn issue631_deactivate_clears_the_shared_session() {
        let mut completer = PrefixCompleter::new(&["checkout", "cherry"]);
        let (mut menu, session) = menu_with_session();
        let mut editor = editor_with("git ch");
        activate(&mut menu, &mut editor, &mut completer);
        assert!(session.lock().unwrap().open);
        menu.menu_event(MenuEvent::Deactivate);
        assert!(!menu.is_active());
        let session = session.lock().unwrap();
        assert!(!session.open);
        assert!(session.snapshot.is_none());
    }
}
