//! GNU readline-style TAB completion (the opt-in `gnu` style).
//!
//! Behavior contract (owner ruling 2026-10-12; its default pick was
//! superseded by the owner ruling of 2026-10-14, which made `column`
//! the default from 1.4.0 on) — GNU readline's factory semantics, not
//! the interactive menus the other four styles draw:
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
//! Implementation rides reedline 0.52's `Menu` abstraction — the
//! "print + redraw" exit shape the owner asked the menu/painter layers to be
//! weighed against. The double-TAB state machine runs at event-dispatch
//! time, in [`Menu::can_partially_complete`] — the hook the engine calls
//! from its `Menu` event arm before the batch's repaint, where the editor
//! and completer are reachable. It must not run in
//! [`Menu::update_working_details`]: the engine composes the frame's line
//! strings from the editor *before* that paint-time hook, so a buffer
//! mutation there paints one frame late (niubash#713). Paint time keeps
//! only the queued `Edit` traffic (list dismissal) via
//! `update_working_details`. After TAB #1 the menu *deactivates itself*, so the next TAB
//! routes a fresh `MenuEvent::Activate` through the engine and the
//! "second TAB lists" decision is carried entirely by this struct. The one
//! GNU deviation is placement: reedline's painter owns menu geometry and
//! draws the list in the slot under the input line instead of scrolling it
//! into the transcript above the prompt; printing it host-side would fight
//! the painter's cached prompt anchor (the exact hazard
//! `invalidate_anchor_if_host_completer_runs` exists for) for no behavioral
//! gain, so the interactive semantics win and the anchor stays intact.

use reedline::menu_functions::{completer_input, CompletionDisplay};
use reedline::{Completer, Editor, InputMode, Menu, MenuEvent, MenuSettings, Painter, Suggestion};
use unicode_width::UnicodeWidthChar;

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
/// reedline 0.52 routes `Menu::name` / `Menu::indicator` through the
/// mandatory [`Menu::settings`], so this menu carries a [`MenuSettings`] and
/// its builders write the `name` / `marker` straight into it — the engine
/// routes `ReedlineEvent::Menu(name)` by `settings().name`, and the painter
/// swaps the prompt indicator for `settings().marker` while the list is up.
pub struct GnuMenu {
    settings: MenuSettings,
    active: bool,
    /// Engine events queued by [`GnuMenu::menu_event`]. An `Activate` is
    /// normally consumed at dispatch time by
    /// [`GnuMenu::can_partially_complete`] (niubash#713); what reaches the
    /// paint-time drain in [`GnuMenu::update_working_details`] is the
    /// `Edit` traffic (and, for direct drives of this type, `Activate`s no
    /// dispatch hook consumed).
    ///
    /// The queue is a deque, not a single slot: `handle_menu_event` applies
    /// `Activate`'s `active` flip IMMEDIATELY, so a keystroke landing in the
    /// same engine batch after a TAB (a TAB pressed mid-burst, or typed
    /// faster than a repaint) is delivered here as an `Edit` while an
    /// `Activate` is still pending. A single slot let that `Edit` overwrite
    /// the `Activate` — the prefix splice never ran (the completion was
    /// silently lost) and `active` stayed true with the phase machine never
    /// engaged, leaving a phantom menu that ate navigation keys and Enter.
    events: std::collections::VecDeque<MenuEvent>,
    /// Candidates from the latest completion query, with display metrics.
    completions: CompletionDisplay,
    phase: GnuPhase,
    /// Buffer contents captured when the last attempt or list landed — the
    /// "line unmodified" anchor of the double-TAB semantics.
    anchor_buffer: Option<String>,
    /// Terminal width seen at the last paint; layout falls back to 80 before
    /// the first [`GnuMenu::update_working_details`].
    terminal_columns: usize,
    /// The queued `Activate` came from the engine's list-only
    /// `ReedlineEvent::PossibleCompletions` (M-? / M-=), not from a TAB.
    /// [`GnuMenu::update_values`] sets it: the engine's
    /// `list_possible_completions` activates the menu and refreshes the
    /// values directly, without the dispatch-time
    /// [`GnuMenu::can_partially_complete`] hook a TAB goes through, so the
    /// paint-time drain must print the candidates without splicing the
    /// common prefix (GNU `complete.c` `rl_possible_completions` never
    /// changes `rl_line_buffer` or point).
    list_only_activation: bool,
}

impl Default for GnuMenu {
    fn default() -> Self {
        Self {
            settings: MenuSettings::default(),
            active: false,
            events: std::collections::VecDeque::new(),
            completions: CompletionDisplay::default(),
            phase: GnuPhase::Idle,
            anchor_buffer: None,
            terminal_columns: 80,
            list_only_activation: false,
        }
    }
}

impl GnuMenu {
    /// Name the menu (the engine routes `ReedlineEvent::Menu(name)` by it).
    #[must_use]
    pub fn with_name(mut self, name: &str) -> Self {
        self.settings = self.settings.with_name(name);
        self
    }

    /// Set the marker painted as the prompt indicator while the list is up.
    #[must_use]
    pub fn with_marker(mut self, marker: &str) -> Self {
        self.settings = self.settings.with_marker(marker);
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
pub(crate) fn column_layout(
    count: usize,
    longest: usize,
    terminal_columns: usize,
) -> (usize, usize) {
    let count = count.max(1);
    let field = longest.max(1).saturating_add(GNU_COLUMN_GUTTER);
    let columns = (terminal_columns / field).max(1).min(count);
    let rows = count.div_ceil(columns);
    (columns, rows)
}

/// niubash#741 — the zsh-family packed layout the interactive grid uses.
///
/// Entries still fill column-major like GNU, but each column's field is
/// the widest entry *in that column* (zsh's `list-packed` geometry), so
/// one long name cannot flatten the whole grid to a single column the
/// way the uniform field does — GNU math at `field = longest + gutter`
/// yields one column as soon as the longest candidate passes half the
/// terminal width, which is exactly the collapse this issue reports.
/// The largest column count whose rows still fit the terminal wins.
///
/// Returns `(columns, rows, col_widths)` — `col_widths[c]` is the max
/// display width among column `c`'s entries (gutter NOT included).
///
/// The uncapped search alone still collapses when *every* column is
/// wide — a set whose display values all carry a long shared prefix
/// (the typed directory part of the completed word rides in every
/// candidate's display value), or one outlier so wide that even the
/// narrowest second column no longer fits beside it. zsh's complist
/// answers that shape by truncating overlong entries, not by giving up
/// the grid, so when the uncapped search bottoms out at one column
/// this layout retries with each width capped at the two-column bound
/// `(terminal - gutter) / 2` (a lone candidate caps at the full
/// terminal width instead): two capped columns plus the gutter always
/// fit, so a collapsed set with at least two candidates provably
/// comes back with at least two columns, and
/// [`render_list_packed`] cuts the overlong cell text to match. Sets
/// that already lay out multi-column never see the cap — their layout
/// is byte-identical to the uncapped one.
pub(crate) fn packed_column_layout(
    widths: &[usize],
    terminal_columns: usize,
) -> (usize, usize, Vec<usize>) {
    let (columns, rows, col_widths) = packed_layout_uncapped(widths, terminal_columns);
    let lone_overwide =
        widths.len() == 1 && col_widths.first().copied().unwrap_or(0) > terminal_columns;
    if columns >= 2 || (widths.len() <= 1 && !lone_overwide) {
        return (columns, rows, col_widths);
    }
    let cap = if widths.len() <= 1 {
        terminal_columns.max(1)
    } else {
        ((terminal_columns.saturating_sub(GNU_COLUMN_GUTTER)) / 2).max(1)
    };
    let capped: Vec<usize> = widths.iter().map(|width| (*width).min(cap)).collect();
    packed_layout_uncapped(&capped, terminal_columns)
}

/// The packed search over the widths exactly as given: the largest
/// column count whose per-column maxima plus gutters fit the
/// terminal, at least one column.
fn packed_layout_uncapped(widths: &[usize], terminal_columns: usize) -> (usize, usize, Vec<usize>) {
    let count = widths.len().max(1);
    // More columns than the terminal has cells could never fit; the
    // gutter alone costs one cell per extra column.
    let n_max = count.min(terminal_columns.max(1));
    for n in (1..=n_max).rev() {
        let rows = count.div_ceil(n);
        let mut col_widths = vec![0usize; n];
        for (index, width) in widths.iter().enumerate() {
            let col = index / rows;
            col_widths[col] = col_widths[col].max(*width);
        }
        let total: usize =
            col_widths.iter().sum::<usize>() + GNU_COLUMN_GUTTER * n.saturating_sub(1);
        if total <= terminal_columns {
            return (n, rows, col_widths);
        }
    }
    // Even one column does not fit (a name wider than the terminal):
    // the single column it must take anyway.
    (1, count, vec![widths.iter().copied().max().unwrap_or(0)])
}

/// Render the candidate grid. `widths` are the per-candidate display widths
/// (ANSI-stripped) parallel to `values`; entries fill column-major (down
/// first, like bash); trailing padding is trimmed at line ends and the
/// return carries no trailing newline. When the rows cannot fit
/// `available_lines` the list is clipped to what fits — reedline has
/// already scrolled to make room up to the screen height, and GNU's
/// `--More--` paging needs an interaction channel a printed list must not
/// have.
pub(crate) fn render_list(
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
    let field = widths.iter().copied().max().unwrap_or(0).max(1) + GNU_COLUMN_GUTTER;
    render_grid(values, widths, &vec![field; columns], rows, available_lines)
}

/// One lexical piece of a pre-styled cell: a whole escape sequence
/// (zero visible width, never split) or one visible character.
#[derive(Debug, Clone, Copy)]
enum CellToken<'a> {
    Escape(&'a str),
    Visible(char),
}

impl CellToken<'_> {
    fn visible_width(&self) -> usize {
        match self {
            CellToken::Escape(_) => 0,
            CellToken::Visible(ch) => UnicodeWidthChar::width(*ch).unwrap_or(0),
        }
    }
}

/// Split a pre-styled cell into escape sequences and visible
/// characters. Only CSI sequences (the SGR shape every style in play
/// emits) and two-byte escapes are recognized; anything else is
/// visible text.
fn cell_tokens(text: &str) -> Vec<CellToken<'_>> {
    let mut tokens = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after_esc) = rest.strip_prefix('\u{1b}') {
            if let Some(csi) = after_esc.strip_prefix('[') {
                // CSI: parameter/intermediate bytes until a final byte
                // in 0x40..=0x7E.
                let end = csi
                    .char_indices()
                    .find(|(_, ch)| ('\u{40}'..='\u{7e}').contains(ch))
                    .map(|(index, ch)| index + ch.len_utf8())
                    .unwrap_or(csi.len());
                let seq_len = 2 + end;
                tokens.push(CellToken::Escape(&rest[..seq_len]));
                rest = &rest[seq_len..];
            } else if !after_esc.is_empty() {
                let ch_len = after_esc.chars().next().unwrap().len_utf8();
                tokens.push(CellToken::Escape(&rest[..1 + ch_len]));
                rest = &rest[1 + ch_len..];
            } else {
                tokens.push(CellToken::Escape(&rest[..1]));
                rest = &rest[1..];
            }
        } else {
            let ch = rest.chars().next().unwrap();
            tokens.push(CellToken::Visible(ch));
            rest = &rest[ch.len_utf8()..];
        }
    }
    tokens
}

/// niubash#741: cut a cell's visible text to `cap` columns, zsh-style,
/// when its plain width exceeds the cap. The cut is in the MIDDLE —
/// head and tail both survive — because the grid's overlong displays
/// are shared-prefix values (the typed directory part in front, the
/// distinguishing file name at the end): an end-only cut would show
/// ninety identical prefixes. A `…` marks the cut. Escape sequences
/// are copied whole and never counted, so a pre-styled cell keeps its
/// style prefix/suffix intact; only visible characters are dropped.
pub(crate) fn truncate_display(text: &str, plain_width: usize, cap: usize) -> String {
    if plain_width <= cap || cap == 0 {
        return text.to_string();
    }
    let tokens = cell_tokens(text);
    // One column pays for the marker; the rest splits head/tail.
    let budget = cap - 1;
    let head_budget = budget / 2;
    let tail_budget = budget - head_budget;

    // Head: escapes ride free; visible characters accumulate up to
    // the head budget and the walk stops at the first that overflows.
    let mut head_end = 0;
    let mut head_width = 0;
    for (index, token) in tokens.iter().enumerate() {
        match token {
            CellToken::Escape(_) => head_end = index + 1,
            CellToken::Visible(_) => {
                let width = token.visible_width();
                if head_width + width > head_budget {
                    break;
                }
                head_width += width;
                head_end = index + 1;
            }
        }
    }

    // Tail: the trailing escape run (a style suffix) always survives;
    // visible characters accumulate backwards up to the tail budget.
    let mut tail_start = tokens.len();
    while tail_start > 0 && matches!(tokens[tail_start - 1], CellToken::Escape(_)) {
        tail_start -= 1;
    }
    let mut tail_width = 0;
    let mut index = tail_start;
    while index > 0 {
        let token = &tokens[index - 1];
        if matches!(token, CellToken::Escape(_)) {
            index -= 1;
            continue;
        }
        let width = token.visible_width();
        if tail_width + width > tail_budget {
            break;
        }
        tail_width += width;
        index -= 1;
        tail_start = index;
    }
    // The two regions can only meet on escape tokens; never overlap.
    let tail_start = tail_start.max(head_end);

    let mut out = String::new();
    for token in &tokens[..head_end] {
        match token {
            CellToken::Escape(seq) => out.push_str(seq),
            CellToken::Visible(ch) => out.push(*ch),
        }
    }
    out.push('…');
    for token in &tokens[tail_start..] {
        match token {
            CellToken::Escape(seq) => out.push_str(seq),
            CellToken::Visible(ch) => out.push(*ch),
        }
    }
    out
}

/// niubash#741: same column-major render as [`render_list`], on the
/// packed layout — each column pads to ITS OWN widest entry. Cells
/// wider than their painted column (the capped layout's overlong
/// entries) are truncated to the column width; the truncation runs
/// against the column the cell is actually painted in, which under
/// [`render_grid`]'s clipped-row refill is not always its layout
/// column.
pub(crate) fn render_list_packed(
    values: &[&str],
    widths: &[usize],
    terminal_columns: usize,
    available_lines: u16,
) -> String {
    if values.is_empty() {
        return String::new();
    }
    let (columns, rows, col_widths) = packed_column_layout(widths, terminal_columns);
    let fields: Vec<usize> = col_widths
        .iter()
        .map(|w| (*w).max(1) + GNU_COLUMN_GUTTER)
        .collect();
    debug_assert_eq!(fields.len(), columns);
    let painted_rows = rows.min(available_lines.max(1) as usize).max(1);
    let last_column = columns - 1;
    let column_of = |index: usize| (index / painted_rows).min(last_column);
    let truncated: Vec<String> = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            truncate_display(
                value,
                widths.get(index).copied().unwrap_or(0),
                col_widths[column_of(index)],
            )
        })
        .collect();
    let effective_widths: Vec<usize> = widths
        .iter()
        .enumerate()
        .map(|(index, width)| (*width).min(col_widths[column_of(index)]))
        .collect();
    let truncated_refs: Vec<&str> = truncated.iter().map(String::as_str).collect();
    render_grid(
        &truncated_refs,
        &effective_widths,
        &fields,
        rows,
        available_lines,
    )
}

/// Shared grid renderer: `fields[col]` is column `col`'s field width
/// (entry width + gutter). `rows` is the unclipped layout depth; the
/// emitted grid is clipped to `available_lines` rows.
fn render_grid(
    values: &[&str],
    widths: &[usize],
    fields: &[usize],
    rows: usize,
    available_lines: u16,
) -> String {
    let rows = rows.min(available_lines.max(1) as usize).max(1);
    let mut out = String::new();
    for row in 0..rows {
        let mut line = String::new();
        for (col, field) in fields.iter().enumerate() {
            let index = col * rows + row;
            if index >= values.len() {
                continue;
            }
            let cell_width = widths.get(index).copied().unwrap_or(0);
            let pad = field.saturating_sub(cell_width).max(GNU_COLUMN_GUTTER);
            line.push_str(values[index]);
            let last_in_row = col + 1 == fields.len() || index + 1 >= values.len();
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
            // CRLF, not bare LF: reedline's painter prints `menu_string`
            // verbatim (upstream menus all emit "\r\n"); a bare '\n' drops
            // one row down without returning to column 0 on POSIX terminals
            // (the staircase), while Windows consoles coerce it — which is
            // why this only reproduced off-Windows.
            out.push_str("\r\n");
        }
    }
    out
}

impl GnuMenu {
    /// Back to factory state: inactive, no attempt anchored, no list.
    /// Pending engine events are dropped too — a deactivate must not let a
    /// pre-reset `Activate` replay on a later line.
    fn reset(&mut self) {
        self.active = false;
        self.phase = GnuPhase::Idle;
        self.anchor_buffer = None;
        self.completions = CompletionDisplay::default();
        self.events.clear();
        self.list_only_activation = false;
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

    /// M-? / M-= (GNU `rl_possible_completions`): print the candidates and
    /// leave the line exactly as it was. No prefix splice, no lone-candidate
    /// accept, no point move — readline's possible-completions is a pure
    /// listing command. The phase becomes `Listed` so the painter draws the
    /// grid; the anchor is the untouched buffer, so any edit dismisses it.
    fn apply_list_only(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        self.refresh_values(editor, completer);
        if self.completions.suggestions().is_empty() {
            // GNU rings the bell for an empty set; nothing to list.
            self.reset();
            return;
        }
        self.anchor_buffer = Some(editor.get_buffer().to_string());
        self.phase = GnuPhase::Listed;
        self.active = true;
    }

    /// Buffer edited while the list is on screen: a changed line dismisses
    /// the list (GNU's printed list belongs to the line it listed); an
    /// unchanged buffer (TAB #N's `Complete` lands here) keeps it up.
    fn apply_edit(&mut self, editor: &Editor) {
        if self.phase != GnuPhase::Listed {
            // `active` is only ever set by the Listed phase, so active-
            // without-list is desync (e.g. an Activate handle_menu_event
            // applied before its queued processing was superseded): the
            // phantom menu must release its claim on Enter/arrows.
            self.active = false;
            return;
        }
        if self.anchor_buffer.as_deref() != Some(editor.get_buffer()) {
            self.reset();
        }
    }

    /// Run every queued engine event in arrival order. Activated-at-
    /// dispatch events and the edits that trailed them in the same batch
    /// must both be seen: dropping the Activate left `active` desynced
    /// (niubash#537's phantom menu) and lost the prefix splice.
    fn drain_events(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        while let Some(event) = self.events.pop_front() {
            match event {
                MenuEvent::Activate(_) if self.list_only_activation => {
                    self.list_only_activation = false;
                    self.apply_list_only(editor, completer);
                }
                MenuEvent::Activate(_) => self.apply_activate(editor, completer),
                MenuEvent::Edit(_) => self.apply_edit(editor),
                // Deactivate was applied by `handle_menu_event`; navigation
                // events never reach this menu (no arrow-key bindings) and must
                // not disturb the printed list if they ever do.
                _ => {}
            }
        }
    }
}

impl Menu for GnuMenu {
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
        self.handle_menu_event(&event);
        self.events.push_back(event);
    }

    fn can_quick_complete(&self) -> bool {
        false
    }

    fn can_partially_complete(
        &mut self,
        _values_updated: bool,
        editor: &mut Editor,
        completer: &mut dyn Completer,
    ) -> bool {
        // niubash#713: this hook is the engine's dispatch-time completion
        // slot — `decide_menu_completion` calls it from the `Menu` event
        // arm, BEFORE the batch's repaint. The GNU state machine runs HERE,
        // not in `update_working_details`: the engine composes the frame's
        // line strings from the editor before that paint-time hook runs, so
        // a splice deferred to it lands in the buffer after the frame was
        // already composed and only becomes visible at the NEXT keypress's
        // repaint — the one-frame-late TAB this issue reports. (The other
        // styles commit through this same hook, which is why only the GNU
        // style lagged.) The engine's own splice machinery stays off — the
        // state machine does its own accept/prefix work and the return
        // stays `false`.
        self.apply_activate(editor, completer);
        // The `Activate` this dispatch belongs to was queued by
        // `menu_event` moments earlier; it has now been applied, so drop
        // every pending `Activate` — the paint-time drain must not run the
        // same TAB twice. Queued `Edit`s keep their paint-time semantics.
        self.events
            .retain(|event| !matches!(event, MenuEvent::Activate(_)));
        self.list_only_activation = false;
        false
    }

    fn update_values(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        // The engine's list_possible_completions calls this right after
        // queuing its `Activate`; a TAB's activation is consumed by
        // `can_partially_complete` before this hook and leaves no queued
        // `Activate` behind. Seeing one here therefore means the menu was
        // opened by M-? / M-= (or another listing call), which must not
        // splice the common prefix.
        if self
            .events
            .iter()
            .any(|event| matches!(event, MenuEvent::Activate(_)))
        {
            self.list_only_activation = true;
        }
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
        self.drain_events(editor, completer);
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
    use unicode_width::UnicodeWidthStr;

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
    /// crate-private) and then forwards to the same [`GnuMenu::drain_events`].
    fn activate(menu: &mut GnuMenu, editor: &mut Editor, completer: &mut dyn Completer) {
        menu.menu_event(MenuEvent::Activate(false));
        menu.drain_events(editor, completer);
    }

    /// Drive an engine Edit (a TAB that fell through the already-active Menu
    /// arm, or a keystroke) the same way.
    fn edit_event(menu: &mut GnuMenu, editor: &mut Editor, completer: &mut dyn Completer) {
        menu.menu_event(MenuEvent::Edit(false));
        menu.drain_events(editor, completer);
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

    /// Drive the engine's `ReedlineEvent::PossibleCompletions` shape: it
    /// activates the menu and refreshes the values directly, then the paint
    /// cycle drains the queued `Activate`.
    fn list_only(menu: &mut GnuMenu, editor: &mut Editor, completer: &mut dyn Completer) {
        menu.menu_event(MenuEvent::Activate(false));
        menu.update_values(editor, completer);
        menu.drain_events(editor, completer);
    }

    /// GNU `rl_possible_completions` (M-? / M-=) prints the candidates and
    /// leaves `rl_line_buffer`/`rl_point` untouched — unlike TAB, it never
    /// splices the common prefix.
    #[test]
    fn possible_completions_lists_without_splicing_the_prefix() {
        let mut completer = PrefixCompleter::new(&["alpha.txt", "alpine.txt"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("cat al");

        list_only(&mut menu, &mut editor, &mut completer);

        assert_eq!(editor.get_buffer(), "cat al", "M-? never edits the line");
        assert_eq!(
            editor.line_buffer().insertion_point(),
            6,
            "M-? never moves point"
        );
        assert!(menu.is_active(), "the candidate list is on screen");
        assert_eq!(menu.phase, GnuPhase::Listed);
        let list = menu.menu_string(10, false);
        assert!(list.contains("alpha.txt"), "{list}");
        assert!(list.contains("alpine.txt"), "{list}");
    }

    /// A single candidate is still printed: possible-completions lists, it
    /// does not accept the way TAB #1 does.
    #[test]
    fn possible_completions_lists_a_lone_candidate_without_accepting() {
        let mut completer = PrefixCompleter::new(&["alpha.txt"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("cat alph");

        list_only(&mut menu, &mut editor, &mut completer);

        assert_eq!(editor.get_buffer(), "cat alph");
        assert!(menu.is_active());
        assert!(menu.menu_string(10, false).contains("alpha.txt"));
    }

    /// After TAB #1 spliced `alp`, M-? lists the set anchored on that
    /// buffer without touching it again.
    #[test]
    fn possible_completions_after_a_prefix_attempt_keeps_the_buffer() {
        let mut completer = PrefixCompleter::new(&["alpha.txt", "alpine.txt"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("cat al");

        activate(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "cat alp");

        list_only(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "cat alp", "M-? re-splices nothing");
        assert_eq!(editor.line_buffer().insertion_point(), 7);
        assert!(menu.is_active());
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
        edit_event(&mut menu, &mut editor, &mut completer);
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
        edit_event(&mut menu, &mut editor, &mut completer);

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

    /// Drive ONE engine TAB the way the reedline engine actually dispatches
    /// it (fork `engine.rs`, the `ReedlineEvent::Menu` arm): `menu_event`
    /// queues the `Activate`, then `decide_menu_completion` calls
    /// `can_partially_complete` — all BEFORE the batch's repaint.
    /// `update_working_details` is deliberately NOT driven here: the engine
    /// composes the frame's line strings from the editor before that hook
    /// runs, so whatever the menu has not done by the end of this helper is
    /// absent from the frame the user sees for this TAB.
    fn dispatch_tab(menu: &mut GnuMenu, editor: &mut Editor, completer: &mut dyn Completer) {
        menu.menu_event(MenuEvent::Activate(false));
        let _ = menu.can_partially_complete(false, editor, completer);
    }

    /// niubash#713: the TAB #1 splice must already be in the buffer when the
    /// engine composes the frame — no subsequent keypress, no paint-time
    /// drain. Deferring the splice to `update_working_details` painted it
    /// one frame late: the buffer was right, the screen was not, until the
    /// next keypress repainted.
    #[test]
    fn tab1_splice_is_in_the_frame_without_a_second_keypress() {
        let mut completer = PrefixCompleter::new(&["checkout", "checkouts"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("che");

        dispatch_tab(&mut menu, &mut editor, &mut completer);

        assert_eq!(
            editor.get_buffer(),
            "checkout",
            "the common prefix must be spliced at dispatch time, in this frame"
        );
        assert_eq!(menu.phase, GnuPhase::Attempted);
        assert!(!menu.is_active(), "no list after TAB #1");

        // The paint-time drain that still follows in the same frame must
        // not apply the same TAB a second time.
        menu.drain_events(&mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "checkout");
        assert_eq!(menu.phase, GnuPhase::Attempted);
    }

    /// niubash#713, the reporter's exact shape: a lone candidate completes
    /// outright — and must be visible in the TAB's own frame.
    #[test]
    fn tab1_lone_candidate_is_in_the_frame_without_a_second_keypress() {
        let mut completer = PrefixCompleter::new(&["Desktop/"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("Desk");

        dispatch_tab(&mut menu, &mut editor, &mut completer);

        assert_eq!(editor.get_buffer(), "Desktop/");
        assert!(!menu.is_active(), "a lone candidate never lists");

        menu.drain_events(&mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "Desktop/");
    }

    /// niubash#713, TAB #2 leg: the list decision is dispatch-time too —
    /// the menu is active and renderable in TAB #2's own frame, on top of
    /// the line TAB #1 spliced in TAB #1's frame.
    #[test]
    fn tab2_list_is_up_in_the_same_frame() {
        let mut completer = PrefixCompleter::new(&["checkout", "checkouts"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("check");

        dispatch_tab(&mut menu, &mut editor, &mut completer);
        assert_eq!(editor.get_buffer(), "checkout");
        dispatch_tab(&mut menu, &mut editor, &mut completer);

        assert!(menu.is_active(), "TAB #2 lists in its own frame");
        assert_eq!(menu.phase, GnuPhase::Listed);
        assert!(menu.menu_string(20, false).contains("checkouts"));

        menu.drain_events(&mut editor, &mut completer);
        assert!(menu.is_active(), "the drain must not un-list TAB #2");
        assert_eq!(editor.get_buffer(), "checkout");
    }

    /// niubash#537: `menu_event` must not lose a queued `Activate` when an
    /// `Edit` lands in the same engine batch — the engine repaints once per
    /// batch, so TAB + a typed char (a burst, or fast typing) arrive before
    /// `update_working_details` runs. `handle_menu_event` already flipped
    /// `active` on the Activate's dispatch; if the Edit then overwrote the
    /// pending Activate, the menu stayed `active` with the phase machine
    /// never engaged — a phantom menu eating navigation keys and Enter.
    #[test]
    fn same_batch_activate_then_edit_leaves_no_phantom_menu() {
        let mut completer = PrefixCompleter::new(&["checkout", "checkouts"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("che");

        // One batch: TAB's Activate, then the typed char's Edit — both
        // queued, neither processed until the (single) repaint drains.
        menu.menu_event(MenuEvent::Activate(false));
        menu.menu_event(MenuEvent::Edit(false));
        menu.drain_events(&mut editor, &mut completer);

        assert!(
            !menu.is_active(),
            "the menu must not stay active — no list was ever printed"
        );
        // The Activate ran too: the common prefix spliced into the line
        // before the trailing edit's word could be re-queried or the
        // attempt dropped entirely.
        assert_eq!(menu.phase, GnuPhase::Attempted);
    }

    /// The same race, opposite order kept honest: a menu that reached
    /// `Listed` and then saw a buffer-changing edit must reset, not linger.
    #[test]
    fn listed_then_edit_in_one_batch_dismisses() {
        let mut completer = PrefixCompleter::new(&["checkout", "checkouts"]);
        let mut menu = GnuMenu::default();
        let mut editor = editor_with("check");
        activate(&mut menu, &mut editor, &mut completer);
        activate(&mut menu, &mut editor, &mut completer);
        assert!(menu.is_active());
        editor.edit_buffer(
            |line| line.set_buffer("checkoutx".to_string()),
            UndoBehavior::CreateUndoPoint,
        );
        menu.menu_event(MenuEvent::Edit(false));
        menu.drain_events(&mut editor, &mut completer);
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
    fn packed_layout_keeps_a_long_outlier_from_flattening_the_grid() {
        // niubash#741's reproducer shape: one 40-char name among short
        // ones. GNU uniform math at field=42 fits 120/42 = 2 columns;
        // the packed layout isolates the outlier in its own column and
        // keeps far more.
        let mut widths: Vec<usize> = (0..30).map(|_| 4).collect();
        widths.push(40);
        let (columns, rows, col_widths) = packed_column_layout(&widths, 120);
        assert!(columns >= 3, "packed layout stays multi-column: {columns}");
        assert_eq!(col_widths.len(), columns);
        assert_eq!(
            *col_widths.iter().max().unwrap(),
            40,
            "the outlier's column carries its own width"
        );
        assert_eq!(rows, 31_usize.div_ceil(columns));

        // Rendered: short names share rows — never a one-per-line list.
        let values: Vec<String> = (0..30).map(|i| format!("f{i:02}")).collect();
        let mut values: Vec<&str> = values.iter().map(String::as_str).collect();
        values.push("veryverylongcandidatenamethatkeepsgoing1234");
        let mut widths: Vec<usize> = (0..30).map(|_| 3).collect();
        widths.push(42);
        let rendered = render_list_packed(&values, &widths, 120, 30);
        let first_line = rendered.lines().next().unwrap();
        assert!(
            first_line.matches("f0").count() + first_line.matches("f1").count() >= 2,
            "multiple short candidates share a row:\n{rendered}"
        );
    }

    #[test]
    fn packed_layout_all_wide_set_truncates_instead_of_collapsing() {
        // Every candidate wider than half the terminal: the uncapped
        // packed geometry is one column, but niubash#741's zsh-parity
        // ruling truncates instead of collapsing — the capped retry
        // keeps two columns at the two-column bound (80 - 2) / 2 = 39.
        // (This assertion replaces the #744-era "same floor as GNU"
        // pin, whose floor is the collapse the reopened issue
        // rejects.)
        let widths = [60usize, 62, 58];
        let (columns, _rows, col_widths) = packed_column_layout(&widths, 80);
        assert_eq!(columns, 2);
        assert!(col_widths.iter().all(|width| *width <= 39));
    }

    #[test]
    fn render_list_is_byte_exact_column_major() {
        // One row, three columns; every cell pads to field width (5 + 2),
        // the line-end trim drops only the trailing gutter.
        let values = ["alpha", "beta", "gamma"];
        let widths = [5, 4, 5];
        assert_eq!(render_list(&values, &widths, 21, 10), "alpha  beta   gamma");

        // Two rows, two columns: down first (alpha, beta | gamma).
        assert_eq!(
            render_list(&values, &widths, 14, 10),
            "alpha  gamma\r\nbeta"
        );

        // Clipped to one available line: the grid refills at rows=1, so the
        // first row carries the first two candidates in order.
        assert_eq!(render_list(&values, &widths, 14, 1), "alpha  beta");

        // Empty input renders nothing.
        assert_eq!(render_list(&[], &[], 80, 10), "");
    }

    /// Strip CSI escape sequences (the only kind the menu styles emit)
    /// so a rendered grid's visible widths can be measured.
    fn strip_csi(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{1b}' {
                if chars.next() == Some('[') {
                    for inner in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&inner) {
                            break;
                        }
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    fn visible_width(text: &str) -> usize {
        UnicodeWidthStr::width(strip_csi(text).as_str())
    }

    /// Build `count` display values whose widths cycle through
    /// `lo..=hi` (deterministic, unsorted), each value a distinct
    /// letter-run so grid cells stay recognizable in assertions.
    fn spread_values(count: usize, lo: usize, hi: usize, tag: char) -> Vec<String> {
        (0..count)
            .map(|i| {
                let w = lo + (i * 7) % (hi - lo + 1);
                let mut s = format!("{tag}{i:03}");
                while s.len() < w {
                    s.push('x');
                }
                s
            })
            .collect()
    }

    #[test]
    fn issue741_plain_geometry_stays_multi_column() {
        // The healthy baseline the collapse must not disturb: 90
        // candidates with display widths spread over 4..=28 (the
        // issue's home-dir shape) lay out multi-column and render
        // untruncated.
        let values = spread_values(90, 4, 28, 'f');
        let widths: Vec<usize> = values.iter().map(|v| v.width()).collect();
        let (columns, _rows, _col_widths) = packed_column_layout(&widths, 120);
        assert_eq!(columns, 4, "plain 90-item geometry at 120 columns");
        let (columns, _rows, _col_widths) = packed_column_layout(&widths, 200);
        assert_eq!(columns, 6, "plain 90-item geometry at 200 columns");

        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        let rendered = render_list_packed(&refs, &widths, 120, 30);
        assert!(
            !rendered.contains('…'),
            "no cell is truncated in the plain geometry:\n{rendered}"
        );
        for line in rendered.split("\r\n") {
            assert!(
                visible_width(line) <= 120,
                "line fits the terminal: {line:?}"
            );
        }
        let first_line = rendered.split("\r\n").next().unwrap();
        assert!(
            first_line.matches('f').count() >= 2,
            "multiple candidates share the first row:\n{rendered}"
        );
    }

    #[test]
    fn issue741_single_wide_outlier_keeps_at_least_two_columns() {
        // One display-width-100 candidate among 20..=28-wide
        // companions: any second column next to the outlier's busts
        // the uncapped budget at both 120 and 80, so the pre-fix
        // packed layout bottoms out at one column.
        let mut values = spread_values(89, 20, 28, 'o');
        values.push("z".repeat(100));
        let widths: Vec<usize> = values.iter().map(|v| v.width()).collect();
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        for terminal in [120usize, 80] {
            let (columns, _rows, _col_widths) = packed_column_layout(&widths, terminal);
            assert!(
                columns >= 2,
                "outlier set keeps >= 2 columns at {terminal}: got {columns}"
            );
            let rendered = render_list_packed(&refs, &widths, terminal, 100);
            assert!(
                rendered.contains('…'),
                "the outlier is visibly truncated at {terminal}:\n{rendered}"
            );
            for line in rendered.split("\r\n") {
                assert!(
                    visible_width(line) <= terminal,
                    "rendered line fits {terminal} columns: {line:?}"
                );
            }
        }
    }

    #[test]
    fn issue741_prefix_inflated_set_keeps_at_least_two_columns() {
        // Every display value carries the same 40-cell prefix (the
        // typed directory part of the completed word): widths
        // 50..=68 across the whole set. Each column's max is ~68, so
        // two columns already bust 120 uncapped — the pre-fix layout
        // returns a single column.
        let prefix = "d/".repeat(20);
        let values: Vec<String> = spread_values(90, 10, 28, 'n')
            .iter()
            .map(|name| format!("{prefix}{name}"))
            .collect();
        let widths: Vec<usize> = values.iter().map(|v| v.width()).collect();
        assert!(widths.iter().all(|w| (50..=68).contains(w)));
        let (columns, _rows, _col_widths) = packed_column_layout(&widths, 120);
        assert!(
            columns >= 2,
            "prefix-inflated set keeps >= 2 columns: got {columns}"
        );
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        let rendered = render_list_packed(&refs, &widths, 120, 100);
        assert!(
            rendered.contains('…'),
            "overlong cells are visibly truncated:\n{rendered}"
        );
        for line in rendered.split("\r\n") {
            assert!(
                visible_width(line) <= 120,
                "rendered line fits the terminal: {line:?}"
            );
        }
        // Middle truncation keeps the tail of each display value —
        // the file-name part after the shared prefix — on screen.
        assert!(
            rendered.contains("n089"),
            "a truncated cell still shows its distinguishing tail:\n{rendered}"
        );
    }

    #[test]
    fn issue741_styled_outlier_truncates_without_splitting_escapes() {
        // CycleMenu pre-styles cells (LS_COLORS / selection reverse):
        // the styled string's escapes must survive truncation whole,
        // with only the visible text cut.
        let styled = format!("\u{1b}[1;34m{}\u{1b}[0m", "y".repeat(100));
        let mut values: Vec<String> = spread_values(11, 20, 28, 's');
        values.push(styled);
        let widths: Vec<usize> = values.iter().map(|v| strip_csi(v).width()).collect();
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        let rendered = render_list_packed(&refs, &widths, 80, 100);
        assert!(
            rendered.contains("\u{1b}[1;34m"),
            "the style prefix survives intact:\n{rendered:?}"
        );
        assert!(
            rendered.contains("\u{1b}[0m"),
            "the style suffix survives intact:\n{rendered:?}"
        );
        assert!(
            rendered.contains('…'),
            "truncation is marked:\n{rendered:?}"
        );
        for line in rendered.split("\r\n") {
            assert!(
                visible_width(line) <= 80,
                "rendered line fits the terminal: {line:?}"
            );
        }
        let (columns, _rows, _col_widths) = packed_column_layout(&widths, 80);
        assert!(columns >= 2, "styled outlier set keeps >= 2 columns");
    }

    #[test]
    fn issue741_lone_overwide_candidate_truncates_to_the_terminal() {
        // A single candidate wider than the terminal: one column is
        // correct, but the cell must be cut to the terminal width
        // instead of overflowing the line.
        let values = ["x".repeat(200)];
        let widths = [200usize];
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        let (columns, _rows, _col_widths) = packed_column_layout(&widths, 80);
        assert_eq!(columns, 1);
        let rendered = render_list_packed(&refs, &widths, 80, 10);
        assert!(rendered.contains('…'), "the lone cell is marked truncated");
        assert_eq!(visible_width(&rendered), 80);
    }

    #[test]
    fn issue741_truncate_display_cuts_the_middle_and_keeps_escapes() {
        // Under the cap: untouched.
        assert_eq!(truncate_display("short", 5, 10), "short");
        // Middle cut: head and tail split the budget around the
        // one-column marker.
        assert_eq!(truncate_display("abcdefghij", 10, 5), "ab…ij");
        assert_eq!(truncate_display("abcdefghij", 10, 6), "ab…hij");
        // Cap of 1 is the marker alone.
        assert_eq!(truncate_display("abc", 3, 1), "…");
        // A styled cell keeps its prefix and suffix whole.
        assert_eq!(
            truncate_display("\u{1b}[31mabcdefghij\u{1b}[0m", 10, 5),
            "\u{1b}[31mab…ij\u{1b}[0m"
        );
        // Wide characters are measured in columns, never split: each
        // kana is 2 wide, so cap 5 fits one head + marker + one tail.
        assert_eq!(truncate_display("あいうえお", 10, 5), "あ…お");
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
