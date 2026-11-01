# Completion matrix: fixed gaps and remaining #778 baseline

Branch: `fix/completion-options-honored`, based on the matrix commit
`524b2f8` (which sits on master `3018703`).
Binary: `target/debug/niu`, built in this worktree with cargo's default jobs.

| Run | PASS | FAIL | N/A | Total |
| --- | --- | --- | --- | --- |
| before (matrix branch) | 38 | 18 | 2 (MISSING) | 58 |
| after (this branch) | **47** | **9** | **2** | 58 |

The 9 remaining FAILs are exactly the #778 menu-select cluster owned by
`fix/778-select-first-tab`. This branch did **not** change that interaction.

## Fixed: style presentation under `select` / `cycle` (6 cases)

Before, `repl.rs:573-582` returned one `CycleMenu` for every non-GNU style,
so column/ide/list lost their shapes and descriptions. The interaction menu
now carries the selected style (`CycleMenu::with_style`) and renders:

* `column` — packed grid; when any candidate has a description, a single
  vertical column with the description beside each value;
* `ide` — packed popup; descriptions ride beside their value;
* `list` — one vertical row per candidate, description included;
* `inline` — unchanged packed grid (the line still cycles in place).

Selection/splicing/freeze behaviour is untouched. The shape is chosen only
in the paint helpers (`render_menu`, `render_vertical_cells`).

| Case | Before | After |
| --- | --- | --- |
| `styles/select_column_descriptions` | `start stop status` horizontal, no desc | 3 vertical rows with descriptions |
| `styles/select_ide_descriptions` | horizontal grid, no desc | popup with all descriptions |
| `styles/select_list_descriptions` | horizontal grid, no desc | vertical rows with descriptions |
| `styles/cycle_column_descriptions` | horizontal grid, no desc | vertical rows with descriptions |
| `styles/cycle_ide_descriptions` | horizontal grid, no desc | popup with all descriptions |
| `styles/cycle_list_descriptions` | horizontal grid, no desc | vertical rows with descriptions |

Source: `crates/niubash-runtime/src/completion/cycle_menu.rs`
(`with_style`, `render_menu`, `render_vertical_cells`) and
`crates/niubash-runtime/src/repl.rs` (`.with_style(config.completion_style)`).

## Fixed: `insert` first-TAB contract (3 cases)

New `crates/niubash-runtime/src/completion/insert_menu.rs` wraps the stock
Ide/List/Inline menus only for `completion.interaction = insert`. On a fresh
activation it queries the full candidate set (ListMenu's page cache hides all
but one candidate under inline), splices the longest common prefix — or the
whole value for a lone candidate — and deactivates. If the typed word already
is the common prefix, it keeps the menu up, so TAB #2 is the list. Navigation,
Enter and the painted style still belong to the wrapped menu.

| Case | Before | After |
| --- | --- | --- |
| `insert/ide_prefix_two_step` | TAB #1 opened a two-row popup | TAB #1 `ls alp`, no menu; TAB #2 popup |
| `insert/list_prefix_two_step` | TAB #1 left `ls al` + pager | TAB #1 `ls alp`, no menu; TAB #2 list |
| `insert/inline_prefix_two_step` | TAB #1 left `ls al` + pager | TAB #1 `ls alp`, no menu; TAB #2 list |

The insert axis owns TAB #1 (common prefix), so the inline case now asserts
that axis; the inline style's first-match/then-cycle shape remains pinned by
`inline/cycle_first_match_then_next` under `cycle`. `insert/enter_confirms_then_runs`
and `insert/esc_keeps_prefix` still pass, as do the existing `#486` Enter
tests for all four styles.

Source: `insert_menu.rs`; wiring at `repl.rs:598-660`
(The wrapper is constructed only when `completion_interaction == Insert`).

## N/A: internal-only knobs (2)

`completion.page_size` and `completion.behavior` are internal implementation
settings, not documented env/config options. No env names were invented; the
matrix now reports them as **N/A**:

* `knobs/page_size_not_an_option` — fixed `MenuConfig::completion_page_size`
  (10) in `crates/niubash-runtime/src/config.rs:218,228`; no resolver.
* `knobs/completion_behavior_not_an_option` — internal
  `CompletionBehavior` in `completion/mod.rs`; no resolver.

## Remaining: #778 menu-select cluster (9 FAILs, not touched)

Repro: cwd with `cmp-aa … cmp-ff`, 40 columns, default `column`+`select`,
type `cd cmp-<TAB>`.

| Case | Expected | Actual |
| --- | --- | --- |
| `defaults/select_tab1_first_candidate` | `cd cmp-aa/`, first cell reversed | `cd cmp-`, nothing selected |
| `defaults/select_tab2_cycles` | `cd cmp-bb/`, second cell selected | `cd cmp-aa/`, no highlighted cell |
| `select/shift_tab_previous` | `cd cmp-ff/`, selected | `cd cmp-ff/`, no highlighted cell |
| `select/down_one_row` | `cd cmp-bb/` | unchanged |
| `select/up_one_row_wraps` | `cd cmp-ff/` | unchanged |
| `select/right_one_column` | `cd cmp-cc/` | unchanged |
| `select/left_one_column_wraps` | `cd cmp-ee/` | unchanged |
| `select/enter_accept_runs` | runs `cd cmp-aa/` | runs `cd cmp-` → error |
| `styles/select_inline_cycles` | TAB1 `cmp-aa/`, TAB2 `cmp-bb/` | TAB1 `cd cmp-` + grid, TAB2 `cmp-aa/` |

Still in `cycle_menu.rs`: the unselected select start (`:422-426`), the
first-navigate edge pick (`:548-556`), ignored `Move*` events (`:592-597`),
and the `has_selection` highlight gate (`:748-752`; line numbers shifted by
this branch's render helpers, same code). `fix/778-select-first-tab` owns
those.

## Overlap with #778

* Both branches touch `cycle_menu.rs` and `repl.rs`.
* This branch adds presentation only: `style` field/`with_style`, render
  helpers, and the `InsertStopMenu` wrapper. It does not change
  `begin_session`, `apply_navigate`, `drain_events`, the selection style
  decision, or the TAB/Shift-TAB bindings.
* #778 changes the select start (`has_selection`/first splice), arrow event
  handling, and the highlight gate. Those edits compose with `render_menu`:
  the helper already paints `has_selection && index` when set, and the column
  no-description path is byte-identical to the old packed grid.
* The one shared assertion is `styles/select_inline_cycles`; it stays in the
  #778 baseline here and should flip on that branch.

## Focused tests

* `cargo test --locked -p niubash-runtime --lib 'completion::'` —
  127 passed, 0 failed (includes new
  `style_column_with_descriptions_renders_one_vertical_column`,
  `style_list_with_descriptions_renders_one_vertical_column`,
  `style_ide_with_descriptions_keeps_a_popup_and_shows_them`, and the
  `insert_menu` common-prefix tests).
* `NIU_INTERACTIVE_TESTS=1 cargo test --test interactive
  completion_menu_enter_commits_highlighted_candidate` — see run log.
* Full matrix: `scripts/completion-options-matrix/run.sh --record-baseline`.
