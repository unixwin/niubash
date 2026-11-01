# Baseline failures and exact gaps

Binary: `target/debug/niu`, niu 1.4.4, master `fade8ab`, built with
`CARGO_PROFILE_DEV_DEBUG=0`.
Full matrix: `baseline/summary.md` / `baseline/results.json`
(**PASS 38 / FAIL 18 / MISSING 2**, 58 cases).

The 18 FAILs split into one #778 cluster (9), one style-presentation gap
(6), and one insert-contract gap (3). The 2 MISSING entries are not
implemented knobs, not broken ones.

## A. #778 cluster — 9 FAILs

All are the default `column` + `select` path. Repro fixture: a cwd with
directories `cmp-aa … cmp-ff`, a 40-column window, no completion env (so the
built-in `select` default applies).

```sh
cd <fixture>          # cmp-aa cmp-bb cmp-cc cmp-dd cmp-ee cmp-ff
# type:
cd cmp-<TAB>
```

| Case | Expected | Actual |
| --- | --- | --- |
| `defaults/select_tab1_first_candidate` | `cd cmp-aa/`, first cell reverse-video | `cd cmp-`, no selection |
| `defaults/select_tab2_cycles` | `cd cmp-bb/`, second cell selected | `cd cmp-aa/` (first splice lands on TAB #2), no cell selected |
| `select/shift_tab_previous` | `cd cmp-ff/`, selected | `cd cmp-ff/`, no cell selected |
| `select/down_one_row` | `cd cmp-bb/` | `cd cmp-` (unchanged) |
| `select/up_one_row_wraps` | `cd cmp-ff/` | `cd cmp-` |
| `select/right_one_column` | `cd cmp-cc/` | `cd cmp-` |
| `select/left_one_column_wraps` | `cd cmp-ee/` | `cd cmp-` |
| `select/enter_accept_runs` | Enter runs `cd cmp-aa/` | Enter runs `cd cmp-` → error, cwd stays `work` |
| `styles/select_inline_cycles` | TAB1 `cmp-aa/`, TAB2 `cmp-bb/` | TAB1 `cd cmp-` + grid, TAB2 `cmp-aa/` |

Source of the defect:

* `crates/niubash-runtime/src/completion/cycle_menu.rs:422-426` — select mode
  intentionally opens unselected (`has_selection = false`) instead of
  splicing candidate 0 (zsh `Src/Zle/complist.c:2813` sets `mselect` to the
  first match at menu entry).
* `crates/niubash-runtime/src/completion/cycle_menu.rs:548-556` — the first
  navigate special case is what makes TAB #2 splice candidate 0 and Shift-TAB
  splice the last, instead of stepping from the TAB-#1 selection.
* `crates/niubash-runtime/src/completion/cycle_menu.rs:592-597` —
  `drain_events` handles `NextElement` / `PreviousElement` only; the engine's
  `MoveUp` / `MoveDown` / `MoveLeft` / `MoveRight` events fall into `_ => {}`,
  so the zsh arrow bindings are swallowed.
* `crates/niubash-runtime/src/completion/cycle_menu.rs:748-752` — the
  selection style is gated on `has_selection`, which select mode only sets
  after the first navigate; this is the missing first highlight on TAB #1/#2.
* `tests/issue742_completion_zsh.rs:37,59` still pins the old unselected
  TAB #1 behavior; `fix/778-select-first-tab` updates both tests and the
  `config.rs`/`cycle_menu.rs` comments to the owner contract.

Not the cause: `cycle` interaction (same fixture) passes all five of its
cases, and `select/esc_rollback`, `select/lone_candidate`,
`select/bare_tab_indent`, `select/grid_column_major`, `select/lscolors` pass.
`styles/select_inline_cycles` fails on the same first-TAB splice; its line
behavior should pass once #778 lands.

## B. Style presentation ignored under `select`/`cycle` — 6 FAILs

Repro: an rc with a describing provider, then `mytool <TAB>` in the default
`column` + `select` mode:

```sh
_mytool_comp() {
  NIU_COMP_RESULT="$(printf 'start\tdesc-start\nstop\tdesc-stop\nstatus\tdesc-status')"
}
NIU_COMPDEFS="mytool:_mytool_comp"

# NIU_COMPLETION_STYLE=column|ide|list; then:
mytool <TAB>
```

Every style renders the *same* packed grid `start  stop  status`, with no
descriptions:

| Case | Documented shape | Actual |
| --- | --- | --- |
| `styles/select_column_descriptions` | descriptions force one vertical column, description beside value | horizontal `start stop status`, no description |
| `styles/select_ide_descriptions` | multi-column popup, description of the selected candidate | horizontal grid, no description |
| `styles/select_list_descriptions` | fish-style vertical list, one candidate per row, descriptions | horizontal grid, no description |
| `styles/cycle_column_descriptions` | same column shape under `cycle` | horizontal `start stop status`, no description |
| `styles/cycle_ide_descriptions` | same ide popup under `cycle` | horizontal grid, no description |
| `styles/cycle_list_descriptions` | same fish list under `cycle` | horizontal grid, no description |

Source:

* `crates/niubash-runtime/src/repl.rs:573-582` — when interaction is
  `select` or `cycle`, every non-GNU style returns the same `CycleMenu`
  before the `match config.completion_style` at line 590, so the per-style
  menus at `repl.rs:598-646` are unreachable. The `cycle` style cases prove
  the same gap is not select-specific.
* `crates/niubash-runtime/src/completion/cycle_menu.rs:744-753` — the grid
  paints `Suggestion::display_value()` and the style; it never reads
  `Suggestion::description`, so descriptions cannot render.

The docs claim the axes are orthogonal (“Five shapes, one TAB contract”,
`docs/src/completion.md:41-50`; per-style shapes
`docs/src/advanced-usage.md:211-231`). Under `select` the presentation axis
is ignored; `gnu` is the documented exception (its interaction stays literal
and the matrix confirms that).

## C. `insert` first-TAB contract for ide/list/inline — 3 FAILs

Repro: `NIU_COMPLETION_INTERACTION=insert`, fixture `alpha.txt`,
`alpine.txt`, type `ls al<TAB>`.

| Case | Expected (docs two-step rule) | Actual |
| --- | --- | --- |
| `insert/ide_prefix_two_step` | TAB #1 `ls alp`, no menu; TAB #2 lists | TAB #1 already opens a two-row Ide menu |
| `insert/list_prefix_two_step` | TAB #1 `ls alp`, no menu; TAB #2 lists | TAB #1 leaves `ls al` and opens a pager `0: alpha.txt`, `1: alpine.txt`, `Page 1…` |
| `insert/inline_first_match` | TAB #1 inserts first match `ls alpha.txt` | TAB #1 leaves `ls al` and opens a one-row pager |

Source:

* `crates/niubash-runtime/src/repl.rs:3398-3402` — under `insert`, TAB is
  `Menu(COMPLETION_MENU)` first and only falls back to `Edit(Complete)`;
  whether the first TAB prefixes or lists is therefore delegated to the
  stock menu.
* `crates/niubash-runtime/src/repl.rs:633-645` — `List` uses the stock
  `ListMenu` page (index prefixes + `Page 1: records …`), and `Inline` uses
  `ListMenu` with page size 1; neither splices the common prefix or the
  first match on TAB #1.
* `crates/niubash-runtime/src/repl.rs:598-610` — `Ide` is the stock
  `IdeMenu` with no niu stop rule, so it opens on TAB #1; only `Column`
  has the `ColumnMenu` stop rule (`repl.rs:611-631`).

`column` and `gnu` insert cases pass; `insert/esc_keeps_prefix` and
`insert/enter_confirms_then_runs` pass. The 5 styles x 3 interactions grid
is complete: `insert/*` (5), `styles/select_*` (5), `styles/cycle_*` (5).

## D. MISSING knobs — 2

| Knob | Evidence | Where |
| --- | --- | --- |
| `completion.page_size` | `NIU_COMPLETION_PAGE_SIZE=3` ignored; 12/12 candidates shown; `niu config` has no row | fixed at `crates/niubash-runtime/src/config.rs:218,228` (`completion_page_size: 10`); no resolver/env |
| `completion.behavior` (prefix/substring) | `NIU_COMPLETION_BEHAVIOR=substring`, `ls pha<TAB>` produces no candidates; no code path/key | no occurrences in `crates/`; only prefix matching is implemented |

These are recorded as MISSING rather than FAIL because the option is not
exposed at all; the matrix asserts the absence explicitly.

## PASS coverage worth naming

* `NIU_COMPLETION_STYLE` all five values + all documented aliases
  (`readline`, `col`, `grid`, `vertical`, `cycle`, `menu`) and the code-only
  `smart`/`popup` aliases.
* `NIU_COMPLETION_INTERACTION` `insert`/`cycle`; `select` is the default.
* env > default, invalid-value diagnostic + fallback for both knobs.
* config file `[completion] style/interaction` is **not** read; one
  migration notice is printed (`precedence/config_file_ignored`,
  `precedence/config_migration_notice`).
* bare-TAB indent, lone candidate, Esc rollback (select/cycle), insert
  Enter-confirm-then-run.
* column-major packed grid and LS_COLORS `di=01;35`.
* `NIU_COMPDEFS` function beats static tables; a GNU compspec owns the
  command over static tables; compdef + compspec merge; `NIU_COMPDEFS`
  directory `.toml` and `.bash` files serve candidates.
