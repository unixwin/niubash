//! Cycle interaction for TAB completion (niubash#631): the
//! `completion.interaction = cycle`/`select` behaviors — `select`
//! (zsh menu-select) is the default since niubash#742's ruling,
//! superseding the 2026-10-28 `cycle` pick.
//!
//! Behavior contract (DESIGN §7, as ruled):
//!
//! * **TAB #1** queries the candidates once, puts the *first candidate*
//!   into the line (not the common prefix — cycling replaces the
//!   prefix-commit of the `insert` interaction) and shows the candidate
//!   grid with that candidate highlighted — the first TAB is already a
//!   selection (niubash#778): no unselected frame, no "first navigate
//!   picks an edge" step. The candidate set is frozen
//!   for the whole session: it was computed against the line as typed,
//!   and re-querying after a splice would narrow it to the spliced
//!   candidate's own matches.
//! * **Further TABs / Shift-TABs** move the highlight linearly
//!   through the set with wrap-around (a full loop lands back on the
//!   first candidate), splicing each highlighted candidate into the
//!   line in place of the previous one. **Under menu-select the arrow
//!   keys navigate the painted grid** (niubash#778): Down / Up move one
//!   painted line, Right / Left one painted cell column. This follows
//!   zsh `domenuselect` (`Src/Zle/complist.c`): the walk carries
//!   `mcol`/`wishcol`, skips padding/empty cells via `adjust_mcol`,
//!   wraps every movement at the edges (zshmodules: "All movement
//!   functions wrap around at the edges"), and therefore handles a
//!   short last column by falling back to the nearest occupied cell.
//!   From the TAB-#1 highlight (index 0) Down steps down a row. The
//!   splice itself is issued by the edit-mode
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

use super::gnu_menu::{packed_column_layout, render_list_packed, GNU_COLUMN_GUTTER};

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

/// The painted grid as zsh's `menuselect` sees it: terminal cells, not
/// candidate indices. Each candidate column starts at `starts[col]` and
/// occupies `fields[col]` cells (content + the GNU gutter); `mcols` is
/// the terminal width. `Src/Zle/complist.c` (`domenuselect`,
/// `adjust_mcol`, and the movement labels around L2940-3210) navigates
/// this cell table, which is what makes a short last column behave the
/// way zsh does. Mirrored here only as geometry — no zsh code.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CycleGrid {
    /// Data rows of the packed layout.
    pub rows: usize,
    /// Terminal cell columns per row (`zterm_columns` in zsh).
    pub mcols: usize,
    /// Start cell column of every candidate column.
    pub starts: Vec<usize>,
    /// Cell width of every candidate column.
    pub fields: Vec<usize>,
}

impl CycleGrid {
    /// The candidate column count.
    pub fn columns(&self) -> usize {
        self.starts.len()
    }

    /// The candidate at `(row, x)`, or `None` for a padding/empty cell.
    fn cell(&self, len: usize, row: usize, x: usize) -> Option<usize> {
        if row >= self.rows || x >= self.mcols {
            return None;
        }
        for (col, (&start, &field)) in self.starts.iter().zip(&self.fields).enumerate() {
            let end = start.saturating_add(field).min(self.mcols);
            if x >= start && x < end {
                let index = col * self.rows + row;
                return (index < len).then_some(index);
            }
        }
        None
    }

    /// The top-left cell of an index.
    pub fn position(&self, index: usize) -> (usize, usize) {
        let rows = self.rows.max(1);
        let col = (index / rows).min(self.starts.len().saturating_sub(1));
        (index % rows, self.starts.get(col).copied().unwrap_or(0))
    }
}

/// A direction in the painted grid (niubash#778). TAB / Shift-TAB are
/// deliberately not here: they stay linear next/previous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridMove {
    Up,
    Down,
    Left,
    Right,
}

/// A navigation key for the cycle menu: TAB / Shift-TAB stay linear,
/// the arrow keys walk the painted grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleMove {
    Linear(isize),
    Grid(GridMove),
}

/// Move the menu selection one painted grid step, mirroring zsh
/// `domenuselect` (`Src/Zle/complist.c`): Up/Down move one line and
/// Left/Right one terminal column; all four wrap at the edges, and the
/// `wishcol` cell is carried through vertical movement. The cell walk
/// skips padding and empty cells, so a short last column falls back to
/// the nearest candidate instead of stepping into the void. Returns
/// `(candidate index, mcol, wish_col)`.
pub fn grid_move(
    grid: &CycleGrid,
    len: usize,
    index: usize,
    mcol: usize,
    wish_col: usize,
    movement: GridMove,
) -> (usize, usize, usize) {
    if len == 0 {
        return (0, 0, 0);
    }
    if grid.rows == 0 || grid.starts.is_empty() || grid.mcols == 0 {
        // No painted layout yet: preserve the historical linear fallback.
        let next = match movement {
            GridMove::Up | GridMove::Left => (index + len - 1) % len,
            GridMove::Down | GridMove::Right => (index + 1) % len,
        };
        return (next, mcol, wish_col);
    }
    let (row, start) = grid.position(index);
    // A full wrap may cross an empty row/column twice; the budget keeps a
    // degenerate one-column grid terminating instead of spinning.
    let mut budget = 8 * (grid.mcols + grid.rows + len + 1);
    let (next_row, next_col, next_wish) = match movement {
        GridMove::Down => step_down(grid, len, row, mcol, wish_col, &mut budget),
        GridMove::Up => step_up(grid, len, row, mcol, wish_col, &mut budget),
        GridMove::Right => step_right(grid, len, row, mcol, wish_col, &mut budget),
        GridMove::Left => step_left(grid, len, row, mcol, wish_col, &mut budget),
    };
    let next_index = grid
        .cell(len, next_row, next_col)
        .unwrap_or_else(|| grid.cell(len, row, start).unwrap_or(index));
    (next_index, next_col, next_wish)
}

/// zsh `adjust_mcol`: the nearest occupied cell to `wish` on `row`; the
/// left/right tie is broken by the distance from the current `mcol`.
fn adjust_mcol(
    grid: &CycleGrid,
    len: usize,
    row: usize,
    wish: usize,
    mcol: usize,
) -> Option<usize> {
    let wish = wish.min(grid.mcols.saturating_sub(1));
    let left = (0..=wish).rev().find(|x| grid.cell(len, row, *x).is_some());
    let right = (wish..grid.mcols).find(|x| grid.cell(len, row, *x).is_some());
    match (left, right) {
        (None, None) => None,
        (Some(p), None) => Some(p),
        (None, Some(n)) => Some(n),
        (Some(p), Some(n)) => {
            if (mcol as isize - p as isize) < (n as isize - mcol as isize) {
                Some(p)
            } else {
                Some(n)
            }
        }
    }
}

fn step_down(
    grid: &CycleGrid,
    len: usize,
    row: usize,
    mcol: usize,
    wish: usize,
    budget: &mut usize,
    // wrap flags are threaded through the diagonal `goto` edges
) -> (usize, usize, usize) {
    let mut wrap = 0u8;
    step_down_with(grid, len, row, mcol, wish, &mut wrap, budget)
}

fn step_down_with(
    grid: &CycleGrid,
    len: usize,
    start_row: usize,
    start_col: usize,
    wish: usize,
    wrap: &mut u8,
    budget: &mut usize,
) -> (usize, usize, usize) {
    let mut row = start_row;
    let mut col = start_col;
    loop {
        if *budget == 0 {
            return (start_row, start_col, wish);
        }
        *budget -= 1;
        if row == grid.rows.saturating_sub(1) {
            if *wrap & 2 != 0 {
                row = start_row;
                col = start_col;
                break;
            }
            row = 0;
            *wrap |= 1;
        } else {
            row += 1;
        }
        if let Some(next) = adjust_mcol(grid, len, row, wish, col) {
            col = next;
            break;
        }
    }
    if *wrap == 1 {
        return step_right_with(grid, len, row, col, wish, wrap, budget);
    }
    (row, col, wish)
}

fn step_up(
    grid: &CycleGrid,
    len: usize,
    row: usize,
    mcol: usize,
    wish: usize,
    budget: &mut usize,
) -> (usize, usize, usize) {
    let mut wrap = 0u8;
    step_up_with(grid, len, row, mcol, wish, &mut wrap, budget)
}

fn step_right(
    grid: &CycleGrid,
    len: usize,
    row: usize,
    mcol: usize,
    wish: usize,
    budget: &mut usize,
) -> (usize, usize, usize) {
    let mut wrap = 0u8;
    step_right_with(grid, len, row, mcol, wish, &mut wrap, budget)
}

fn step_right_with(
    grid: &CycleGrid,
    len: usize,
    start_row: usize,
    start_col: usize,
    _wish: usize,
    wrap: &mut u8,
    budget: &mut usize,
) -> (usize, usize, usize) {
    let row = start_row;
    let mut col = start_col;
    let current = grid.cell(len, row, col);
    loop {
        if *budget == 0 {
            return (start_row, start_col, start_col);
        }
        *budget -= 1;
        if col == grid.mcols.saturating_sub(1) {
            if *wrap & 1 != 0 {
                col = start_col;
                break;
            }
            col = 0;
            *wrap |= 2;
        } else {
            col += 1;
        }
        match grid.cell(len, row, col) {
            Some(index) if Some(index) != current => break,
            _ => {}
        }
    }
    let wish = col;
    if *wrap == 2 {
        return step_down_with(grid, len, row, col, wish, wrap, budget);
    }
    (row, col, wish)
}

fn step_left(
    grid: &CycleGrid,
    len: usize,
    row: usize,
    mcol: usize,
    wish: usize,
    budget: &mut usize,
) -> (usize, usize, usize) {
    let mut wrap = 0u8;
    step_left_with(grid, len, row, mcol, wish, &mut wrap, budget)
}

fn step_left_with(
    grid: &CycleGrid,
    len: usize,
    start_row: usize,
    start_col: usize,
    wish: usize,
    wrap: &mut u8,
    budget: &mut usize,
) -> (usize, usize, usize) {
    let row = start_row;
    let mut col = start_col;
    let mut wish = wish;
    let current = grid.cell(len, row, col);
    loop {
        if *budget == 0 {
            return (start_row, start_col, wish);
        }
        *budget -= 1;
        if col == 0 {
            if *wrap & 1 != 0 {
                col = start_col;
                break;
            }
            col = grid.mcols.saturating_sub(1);
            *wrap |= 2;
        } else {
            col -= 1;
        }
        match grid.cell(len, row, col) {
            Some(index) if Some(index) != current => break,
            _ => {}
        }
    }
    wish = col;
    if *wrap == 2 {
        let last = grid.mcols.saturating_sub(1);
        col = adjust_mcol(grid, len, row, last, last).unwrap_or(col);
        wish = last;
        return step_up_with(grid, len, row, col, wish, wrap, budget);
    }
    (row, col, wish)
}

fn step_up_with(
    grid: &CycleGrid,
    len: usize,
    start_row: usize,
    start_col: usize,
    wish: usize,
    wrap: &mut u8,
    budget: &mut usize,
) -> (usize, usize, usize) {
    let mut row = start_row;
    let mut col = start_col;
    let mut wish = wish;
    loop {
        if *budget == 0 {
            return (start_row, start_col, wish);
        }
        *budget -= 1;
        if row == 0 {
            if *wrap & 2 != 0 {
                row = start_row;
                col = start_col;
                break;
            }
            row = grid.rows.saturating_sub(1);
            *wrap |= 1;
        } else {
            row -= 1;
        }
        if let Some(next) = adjust_mcol(grid, len, row, wish, col) {
            col = next;
            break;
        }
    }
    if *wrap == 1 {
        if col == wish {
            return step_left_with(grid, len, row, col, wish, wrap, budget);
        }
        wish = col;
    }
    (row, col, wish)
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
    /// an Esc would need to roll back. TAB #1 splices candidate 0, so
    /// this is true for every live session with a menu (the lone
    /// candidate session ends at once instead).
    pub spliced: bool,
    /// The interaction that opened the session: under `select`
    /// (niubash#778) the arrow keys navigate the menu as well as
    /// TAB / Shift-TAB; under `cycle` they keep their line/history
    /// meaning.
    pub select_mode: bool,
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
    /// The painted grid geometry (niubash#778), column-major
    /// (`index = column * rows + row`). Published by the menu at
    /// session start and on every repaint; empty before the first
    /// layout.
    pub grid: CycleGrid,
    /// The selection's terminal cell column (zsh `mcol`) and the cell
    /// carried through vertical moves (`wishcol`).
    pub mcol: usize,
    pub wish_col: usize,
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
        select_mode: bool,
    ) {
        self.open = true;
        self.snapshot = Some(snapshot);
        self.spliced = false;
        self.select_mode = select_mode;
        self.candidates = candidates;
        self.span = span;
        self.index = 0;
        self.region = span;
        self.grid = CycleGrid::default();
        self.mcol = 0;
        self.wish_col = 0;
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
        self.select_mode = false;
        self.candidates = Vec::new();
        self.index = 0;
        self.grid = CycleGrid::default();
        self.mcol = 0;
        self.wish_col = 0;
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

/// niubash#742: what a bare TAB (nothing completable — an empty or
/// whitespace-only line before the cursor) inserts. zsh's shape is a
/// literal TAB/indent; four spaces is the indentation the editor can
/// render deterministically.
const BARE_TAB_INDENT: &str = "    ";

/// Whether the buffer before the cursor is empty or whitespace only —
/// the bare-TAB shape where completion has nothing to offer and the
/// key means "indent" (niubash#742).
fn buffer_before_cursor_is_blank(editor: &Editor) -> bool {
    let buffer = editor.get_buffer();
    let cursor = editor.line_buffer().insertion_point().min(buffer.len());
    buffer[..cursor].trim().is_empty()
}

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
    /// zsh `mcol` / `wishcol`: the selection's painted cell column,
    /// carried through vertical moves (niubash#778).
    mcol: usize,
    wish_col: usize,
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
    /// niubash#742: the zsh menu-select interaction. Since niubash#778
    /// TAB #1 splices candidate 0 under both interactions; the flag is
    /// published to [`CycleSession`] so the wrapper lets the arrow keys
    /// navigate a `select` menu too.
    select_mode: bool,
    /// Whether a candidate is currently spliced into the line. TAB #1
    /// sets it (niubash#778); it only goes false when a session ends.
    has_selection: bool,
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
            mcol: 0,
            wish_col: 0,
            session_begun: false,
            activation_pending: false,
            queried: false,
            region: (0, 0),
            echoes_seen: 0,
            provisional: false,
            session,
            menu_open: None,
            terminal_columns: 80,
            select_mode: false,
            has_selection: false,
        }
    }

    /// Enable the zsh menu-select form (niubash#742).
    #[must_use]
    pub fn with_select_mode(mut self, on: bool) -> Self {
        self.select_mode = on;
        self
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

    /// Publish the grid geometry the arrow keys navigate (niubash#778):
    /// the same [`packed_column_layout`] the painter renders — columns
    /// fill column-major, so Left/Right step one column (`rows`
    /// candidates) and Up/Down one row (one candidate). The wrapper
    /// reads this at key-translation time to splice the target in the
    /// same batch; zsh wraps every movement at the grid edges
    /// (zshmodules: "All movement functions wrap around at the edges"),
    /// which the modulo arithmetic preserves.
    fn publish_grid(&self) {
        let mcols = self.terminal_columns.max(1);
        let (_, rows, col_widths) = packed_column_layout(&self.display_widths, mcols);
        let fields: Vec<usize> = col_widths
            .iter()
            .map(|width| (*width).max(1) + GNU_COLUMN_GUTTER)
            .collect();
        let mut starts = Vec::with_capacity(fields.len());
        let mut x: usize = 0;
        for field in &fields {
            starts.push(x);
            x = x.saturating_add(*field);
        }
        let grid = CycleGrid {
            rows,
            mcols,
            starts,
            fields,
        };
        if let Ok(mut session) = self.session.lock() {
            if session.open && session.grid != grid {
                // A resize (or the first paint) changed the columns:
                // re-anchor the highlight to the same candidate's cell.
                // Unchanged geometry preserves zsh's `wishcol` across
                // paints.
                let (_, start) = grid.position(session.index);
                session.grid = grid;
                session.mcol = start;
                session.wish_col = start;
            }
        }
    }

    /// The painted layout's cell position for an index (top-left cell).
    fn grid_position(&self, index: usize) -> (usize, usize) {
        let mcols = self.terminal_columns.max(1);
        let (_, rows, col_widths) = packed_column_layout(&self.display_widths, mcols);
        let rows = rows.max(1);
        let fields: Vec<usize> = col_widths
            .iter()
            .map(|width| (*width).max(1) + GNU_COLUMN_GUTTER)
            .collect();
        let col = (index / rows).min(fields.len().saturating_sub(1));
        let start = fields.iter().take(col).sum::<usize>();
        (index % rows, start)
    }

    /// Begin the session over the current values: snapshot the line,
    /// publish the session to the shared cell, splice candidate 0 and
    /// highlight it in the grid (niubash#778: the first TAB is already
    /// a selection — there is no unselected first frame). A lone
    /// candidate completes outright and ends the session at once
    /// (GNU/zsh shape — nothing left to cycle).
    fn begin_session(&mut self, editor: &mut Editor) {
        let snapshot = CycleSnapshot {
            buffer: editor.get_buffer().to_string(),
            cursor: editor.line_buffer().insertion_point(),
        };
        let span = self.values[0].span;
        self.region = (span.start, span.end);
        self.index = 0;
        self.mcol = 0;
        self.wish_col = 0;
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
            session.begin(snapshot, candidates, (span.start, span.end), self.select_mode);
        }
        self.publish_grid();
        if self.values.len() == 1 {
            // A lone candidate completes outright and ends the session
            // at once (GNU/zsh shape — nothing left to cycle, under
            // select as well: menu-select has no menu for one match).
            self.splice(editor);
            self.has_selection = true;
            self.end_session();
        } else {
            // niubash#778 / the #742 owner contract: TAB #1 puts the
            // FIRST candidate into the line and paints the grid with it
            // highlighted. `select` and `cycle` share this shape — the
            // old "menu-select opens unselected, the splice waits for
            // the first navigate" frame left the first TAB with an
            // empty grid selection and the typed prefix still in the
            // line (P0 #778: the owner's menu-select expects the same
            // splice-on-first-TAB as zsh).
            self.splice(editor);
            self.has_selection = true;
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
        self.mcol = 0;
        self.wish_col = 0;
        self.provisional = false;
        self.has_selection = false;
        // The grid is gone — stand the autosuggest hinter back up.
        // Sessions that end without a Deactivate event (empty answers,
        // the bare-TAB indent) would otherwise leave the ghost
        // suppressed for the rest of the line.
        if let Some(menu_open) = &self.menu_open {
            menu_open.store(false, Ordering::Relaxed);
        }
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
    ///
    /// niubash#742: a TAB whose buffer is empty or whitespace-only
    /// before the cursor is an *indent* key, not a completion request
    /// — zsh's shape splices a four-space indent where niubash used to
    /// open the command-position grid on an empty line.
    fn apply_activate(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        if self.session_begun {
            return;
        }
        if buffer_before_cursor_is_blank(editor) {
            editor.edit_buffer(
                |buffer| buffer.insert_str(BARE_TAB_INDENT),
                UndoBehavior::CreateUndoPoint,
            );
            self.end_session();
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

    /// Move the highlight by one step and splice the newly highlighted
    /// candidate. TAB / Shift-TAB step linearly (niubash#778 keeps that
    /// product rule); the arrow keys navigate the painted cell grid with
    /// zsh's wrap semantics (see [`grid_move`]).
    fn apply_navigate(&mut self, editor: &mut Editor, movement: CycleMove) {
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
        let (index, mcol, wish_col) = match movement {
            CycleMove::Linear(delta) => {
                let len = self.values.len() as isize;
                let index = (self.index as isize + delta).rem_euclid(len) as usize;
                let (_, start) = self.grid_position(index);
                (index, start, start)
            }
            CycleMove::Grid(direction) => {
                let len = self.values.len();
                let (index, mcol, wish_col) = match self.session.lock() {
                    Ok(session) if session.open => grid_move(
                        &session.grid,
                        len,
                        self.index,
                        session.mcol,
                        session.wish_col,
                        direction,
                    ),
                    _ => (self.index, 0, 0),
                };
                (index, mcol, wish_col)
            }
        };
        // TAB #1 already highlighted candidate 0 (niubash#778), so
        // every navigate steps from the current highlight; there is no
        // unselected-open edge case left to special-case.
        self.index = index;
        self.mcol = mcol;
        self.wish_col = wish_col;
        self.has_selection = true;
        self.splice(editor);
        // The wrapper normally drives navigation itself (see
        // `CycleSession::nav_seq`); this menu-local path is the
        // fallback for a MenuNext/MenuPrevious/MenuUp/MenuDown/
        // MenuLeft/MenuRight delivered straight to the menu (a user
        // binding, say). Mirror the outcome into
        // the cell so the wrapper's view stays truthful — without a
        // nav_seq bump: no Edit echo follows a menu-side splice.
        if let Ok(mut session) = self.session.lock() {
            session.index = self.index;
            session.region = self.region;
            session.mcol = self.mcol;
            session.wish_col = self.wish_col;
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
                self.mcol = session.mcol;
                self.wish_col = session.wish_col;
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
                // TAB / Shift-TAB stay linear, as ruled (niubash#778).
                MenuEvent::NextElement => self.apply_navigate(editor, CycleMove::Linear(1)),
                MenuEvent::PreviousElement => self.apply_navigate(editor, CycleMove::Linear(-1)),
                // Arrow keys: one painted row / cell column, with zsh's
                // mcol/wishcol edge wrap (niubash#778).
                MenuEvent::MoveUp => self.apply_navigate(editor, CycleMove::Grid(GridMove::Up)),
                MenuEvent::MoveDown => self.apply_navigate(editor, CycleMove::Grid(GridMove::Down)),
                MenuEvent::MoveLeft => {
                    self.apply_navigate(editor, CycleMove::Grid(GridMove::Left))
                }
                MenuEvent::MoveRight => {
                    self.apply_navigate(editor, CycleMove::Grid(GridMove::Right))
                }
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
        if self.session_begun {
            self.publish_grid();
        }
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
        let (_, rows, _) = packed_column_layout(&self.display_widths, terminal_columns as usize);
        rows.min(u16::MAX as usize) as u16
    }

    fn menu_string(&self, available_lines: u16, use_ansi_coloring: bool) -> String {
        if !self.session_begun || self.values.is_empty() {
            return String::new();
        }
        // The selected cell wears the stock selected style
        // (MenuTextStyle's default: green, bold, reversed) — the
        // settings' own color is not readable outside reedline, and
        // the default is what every stock menu paints. niubash#743: an
        // unselected cell keeps the candidate's own LS_COLORS style;
        // the selected cell composes it with the selection marker
        // (reverse over the file color, like zsh's menu-select).
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
                if !use_ansi_coloring {
                    return text.to_string();
                }
                if self.has_selection && position == self.index {
                    let style = suggestion
                        .style
                        .map(|style| style.bold().reverse())
                        .unwrap_or(selected_style);
                    format!("{}{}{}", style.prefix(), text, style.suffix())
                } else if let Some(style) = suggestion.style {
                    format!("{}{}{}", style.prefix(), text, style.suffix())
                } else {
                    text.to_string()
                }
            })
            .collect();
        let values: Vec<&str> = cells.iter().map(String::as_str).collect();
        render_list_packed(
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

    // ─── niubash#742: select (zsh menu-select) form ───────────────────

    #[test]
    fn issue778_select_first_tab_splices_and_paints_first_candidate_selected() {
        let mut completer = PrefixCompleter::new(&["checkout", "cherry", "cherry-pick"]);
        let (mut menu, session) = menu_with_session();
        menu = menu.with_select_mode(true);
        let mut editor = editor_with("git ch");

        activate(&mut menu, &mut editor, &mut completer);

        // The owner contract (#742, restated by #778): TAB #1 puts the
        // FIRST candidate into the line and paints the grid with it
        // highlighted. No unselected frame, no "wait for a navigate".
        assert!(menu.is_active());
        assert_eq!(editor.get_buffer(), "git checkout");
        {
            let session = session.lock().unwrap();
            assert!(session.open);
            assert!(session.spliced);
            assert!(session.select_mode);
            assert_eq!(session.index, 0);
            assert_eq!(
                session.snapshot,
                Some(CycleSnapshot {
                    buffer: "git ch".to_string(),
                    cursor: 6,
                })
            );
        }

        // The grid's first cell wears the selection style in the very
        // first paint after TAB.
        let selected = nu_ansi_term::Style::new()
            .fg(nu_ansi_term::Color::Green)
            .bold()
            .reverse();
        let menu_string = menu.menu_string(20, true);
        let styled = format!("{}checkout{}", selected.prefix(), selected.suffix());
        assert!(
            menu_string.contains(&styled),
            "first grid cell is selected on TAB #1:\n{menu_string:?}"
        );
    }

    #[test]
    fn issue778_select_navigation_steps_from_the_first_tab_highlight() {
        let mut completer = PrefixCompleter::new(&["checkout", "cherry", "cherry-pick"]);
        let (mut menu, _session) = menu_with_session();
        menu = menu.with_select_mode(true);
        let mut editor = editor_with("git ch");
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "git checkout");

        // From index 0 a forward navigate steps to candidate 1 (it does
        // not "pick the first candidate" a second time).
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
        // Next wraps to the first candidate.
        navigate(
            &mut menu,
            &mut editor,
            &mut completer,
            MenuEvent::NextElement,
        );
        assert_eq!(editor.get_buffer(), "git checkout");
    }

    #[test]
    fn issue778_select_backward_navigate_wraps_to_the_last_candidate() {
        let mut completer = PrefixCompleter::new(&["checkout", "cherry", "cherry-pick"]);
        let (mut menu, _session) = menu_with_session();
        menu = menu.with_select_mode(true);
        let mut editor = editor_with("git ch");
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "git checkout");

        // Shift-TAB / Up from candidate 0 wraps backwards to the last.
        navigate(
            &mut menu,
            &mut editor,
            &mut completer,
            MenuEvent::PreviousElement,
        );
        assert_eq!(editor.get_buffer(), "git cherry-pick");
        // And forward from the last wraps back to the first.
        navigate(
            &mut menu,
            &mut editor,
            &mut completer,
            MenuEvent::NextElement,
        );
        assert_eq!(editor.get_buffer(), "git checkout");
    }

    /// The #778 arrow walk against a 3x2 grid whose last column is one
    /// short (`c0..c4` at 10 terminal columns: `c0 c2 c4 / c1 c3`).
    ///
    /// The expected index sequence is what zsh 5.9.2 produces under a
    /// PTY for the same list and keys; the implementation follows
    /// `Src/Zle/complist.c` `domenuselect` (mline/mcol/wishcol,
    /// `adjust_mcol`, `src/Zle/complist.c:2940-3210`) and
    /// `Doc/Zsh/mod_complist.yo:337-343,397` ("All movement functions
    /// wrap around at the edges"). No zsh code is copied.
    #[test]
    fn issue778_arrow_grid_walk_matches_zsh_menuselect() {
        let mut completer = PrefixCompleter::new(&["c0", "c1", "c2", "c3", "c4"]);
        let (mut menu, session) = menu_with_session();
        menu = menu.with_select_mode(true);
        menu.terminal_columns = 10;
        let mut editor = editor_with("c");
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "c0");
        {
            let state = session.lock().unwrap();
            assert_eq!((state.grid.columns(), state.grid.rows), (3, 2));
            assert_eq!(state.grid.starts, vec![0, 4, 8]);
            assert_eq!(state.grid.mcols, 10);
            assert_eq!(state.grid.cell(5, 1, 8), None, "short last column cell");
        }

        let walk = [
            (MenuEvent::MoveDown, "c1"),
            (MenuEvent::MoveDown, "c2"),
            (MenuEvent::MoveDown, "c3"),
            (MenuEvent::MoveRight, "c1"),
            (MenuEvent::MoveRight, "c3"),
            (MenuEvent::MoveRight, "c1"),
            (MenuEvent::MoveLeft, "c4"),
            (MenuEvent::MoveUp, "c3"),
            (MenuEvent::MoveUp, "c2"),
            (MenuEvent::MoveUp, "c1"),
            (MenuEvent::MoveLeft, "c4"),
            (MenuEvent::MoveRight, "c1"),
            (MenuEvent::MoveUp, "c0"),
            (MenuEvent::MoveDown, "c1"),
        ];
        for (event, expected) in walk {
            navigate(&mut menu, &mut editor, &mut completer, event);
            assert_eq!(
                editor.get_buffer(),
                expected,
                "zsh menuselect grid walk"
            );
        }
    }

    #[test]
    fn issue778_select_left_right_are_a_no_op_in_a_single_column() {
        // A terminal too narrow for two cells after the #741 cap fits
        // one column (two rows): there is no neighbor column to move
        // to, so Left/Right are a no-op.
        let mut completer = PrefixCompleter::new(&["checkout", "cherry"]);
        let (mut menu, session) = menu_with_session();
        menu = menu.with_select_mode(true);
        menu.terminal_columns = 1;
        let mut editor = editor_with("git ch");
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "git checkout");
        {
            let state = session.lock().unwrap();
            assert_eq!((state.grid.columns(), state.grid.rows), (1, 2));
        }
        navigate(
            &mut menu,
            &mut editor,
            &mut completer,
            MenuEvent::MoveRight,
        );
        assert_eq!(editor.get_buffer(), "git checkout");
        navigate(
            &mut menu,
            &mut editor,
            &mut completer,
            MenuEvent::MoveLeft,
        );
        assert_eq!(editor.get_buffer(), "git checkout");
    }

    #[test]
    fn issue742_select_lone_candidate_completes_outright() {
        let mut completer = PrefixCompleter::new(&["checkout", "status"]);
        let (mut menu, session) = menu_with_session();
        menu = menu.with_select_mode(true);
        let mut editor = editor_with("git che");
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "git checkout");
        assert!(!menu.is_active());
        assert!(!session.lock().unwrap().open);
    }

    #[test]
    fn issue742_bare_tab_inserts_indent_and_never_opens_the_grid() {
        // An empty line: TAB is an indent key (zsh's shape), the
        // command-position grid never opens and no session begins.
        let mut completer = PrefixCompleter::new(&["checkout", "cherry"]);
        let (mut menu, session) = menu_with_session();
        let mut editor = editor_with("");
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "    ");
        assert!(!menu.is_active());
        assert!(!session.lock().unwrap().open);

        // A whitespace-only prefix indents further (re-indent case).
        let mut completer = PrefixCompleter::new(&["checkout", "cherry"]);
        let (mut menu, session) = menu_with_session();
        let mut editor = editor_with("  ");
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "      ");
        assert!(!menu.is_active());
        assert!(!session.lock().unwrap().open);
    }

    #[test]
    fn issue742_tab_on_a_line_with_content_still_completes() {
        // The indent rule is scoped to the blank-prefix case: `git ch`
        // still opens the candidate session.
        let mut completer = PrefixCompleter::new(&["checkout", "cherry"]);
        let (mut menu, _session) = menu_with_session();
        let mut editor = editor_with("git ch");
        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "git checkout");
        assert!(menu.is_active());
    }

    // ─── niubash#743: LS_COLORS-style candidate coloring ──────────────

    #[test]
    fn issue743_unselected_cells_paint_the_candidate_style() {
        let mut completer = PrefixCompleter::new(&["checkout", "cherry"]);
        let (mut menu, _session) = menu_with_session();
        let mut editor = editor_with("git ch");
        activate(&mut menu, &mut editor, &mut completer);

        // Paint candidate 1 as a directory (bold blue).
        let dir_style = nu_ansi_term::Style::new()
            .fg(nu_ansi_term::Color::Blue)
            .bold();
        menu.values[1].style = Some(dir_style);

        let menu_string = menu.menu_string(20, true);
        let styled = format!("{}cherry{}", dir_style.prefix(), dir_style.suffix());
        assert!(
            menu_string.contains(&styled),
            "unselected cell keeps its file color:\n{menu_string:?}"
        );
        // The selected cell (index 0) wears the composed selection
        // style, not the plain text.
        assert!(menu_string.contains("checkout"));
    }

    #[test]
    fn issue743_selected_cell_composes_file_color_with_selection() {
        let mut completer = PrefixCompleter::new(&["checkout", "cherry"]);
        let (mut menu, _session) = menu_with_session();
        let mut editor = editor_with("git ch");
        activate(&mut menu, &mut editor, &mut completer);

        let dir_style = nu_ansi_term::Style::new().fg(nu_ansi_term::Color::Blue);
        menu.values[0].style = Some(dir_style);
        let menu_string = menu.menu_string(20, true);

        // The selected styled cell: file color kept, bold + reverse added.
        let selected = dir_style.bold().reverse();
        let styled = format!("{}checkout{}", selected.prefix(), selected.suffix());
        assert!(
            menu_string.contains(&styled),
            "selected cell = file color + selection marker:\n{menu_string:?}"
        );
    }

    // ─── niubash#741: the rendered grid must not collapse ─────────

    #[test]
    fn issue741_menu_string_keeps_grid_with_wide_outlier() {
        // 30 candidates of width 24 plus one of width 100, at the
        // menu's default 80-column terminal: uncapped, any second
        // column next to the outlier's busts the budget and the grid
        // renders one-per-line (the #741 collapse). The layout must
        // truncate the outlier and keep at least two columns.
        let mut names: Vec<String> = (0..30)
            .map(|i| {
                let mut s = format!("c{i:02}");
                while s.len() < 24 {
                    s.push('x');
                }
                s
            })
            .collect();
        names.push(format!("c{}", "z".repeat(99)));
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let mut completer = PrefixCompleter::new(&refs);
        let (mut menu, _session) = menu_with_session();
        menu = menu.with_select_mode(true);
        let mut editor = editor_with("c");
        activate(&mut menu, &mut editor, &mut completer);

        let rendered = menu.menu_string(100, false);
        assert!(
            rendered.contains('…'),
            "the wide outlier is visibly truncated:\n{rendered}"
        );
        let heads: Vec<String> = (0..30).map(|i| format!("c{i:02}")).collect();
        let multi = rendered
            .split("\r\n")
            .filter(|line| heads.iter().filter(|h| line.contains(h.as_str())).count() >= 2)
            .count();
        assert!(
            multi >= 1,
            "at least one grid row carries 2+ candidates (multi-column):\n{rendered}"
        );
    }

    #[test]
    fn issue743_no_ansi_mode_strips_styles() {
        let mut completer = PrefixCompleter::new(&["checkout", "cherry"]);
        let (mut menu, _session) = menu_with_session();
        let mut editor = editor_with("git ch");
        activate(&mut menu, &mut editor, &mut completer);
        menu.values[1].style = Some(nu_ansi_term::Style::new().fg(nu_ansi_term::Color::Blue));
        let menu_string = menu.menu_string(20, false);
        assert!(!menu_string.contains('\u{1b}'));
    }
}
