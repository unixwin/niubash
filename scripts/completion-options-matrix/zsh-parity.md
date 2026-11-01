# zsh menu-select parity

Scope: only the completion behavior niubash advertises as zsh-style — the
`select` interaction (default since #742, restated by #778) plus the
`cycle`/`inline` menu-complete shapes. This is not a general zsh-surface audit.

## Oracle

Local checkouts used for every expectation below:

* zsh 5.9.2 `ddee3e7` at `/data/data/com.termux/files/home/refs/zsh`
* oh-my-zsh `17a2b4c` at `/data/data/com.termux/files/home/refs/oh-my-zsh`

| Claim | Source |
| --- | --- |
| Menu-select starts with the first match selected | `Src/Zle/complist.c:2813` `mselect = (*(minfo.cur))->gnum` (same assignment at 2472 and 3300 after completion/menu-complete) |
| Tab = complete-word, Enter/CR = accept-line, Up/Down = up/down-line-or-history, Left/Right = backward/forward-char | `Src/Zle/complist.c:3543-3560` `menuselect_bindings()` |
| Movement wraps at the grid edges | `Doc/Zsh/mod_complist.yo:397`; movement code `Src/Zle/complist.c:2940-3210` |
| OMZ opts into `menu select`, `auto_menu`, no `menu_complete`, and binds `^o` to `accept-and-infer-next-history` | `lib/completion.zsh:1-14` |

zsh's `menuselect` keymap binds no PageUp/PageDown and no Home/End; those
fall through to the current keymap. niu makes no menu page/home claim, so the
matrix probes PageDown as *unclaimed* and expects no selection change.

## Product overlay (owner rule, not a zsh deviation)

OMZ sets `unsetopt menu_complete` + `setopt auto_menu`, so in OMZ the first
TAB lists and the menu *starts* on a later TAB. The niu owner rule is
explicitly different: **default TAB is first-TAB select** — TAB #1 already
splices candidate 0 and paints it selected. That is intentional. niu's
menu-entry state matches zsh `mselect = first gnum`; only the trigger timing
is the product overlay. The matrix therefore scores `TAB #1 selects` against
the owner rule, not against OMZ's auto_menu timing.

The current master test `tests/issue742_completion_zsh.rs:37` pins the old
"TAB #1 opens unselected" shape; `fix/778-select-first-tab` updates that test
and the `cycle_menu.rs` docs to the owner rule. The baseline below is against
master `fade8ab`, before #778 merges.

## Matrix result

| Case | Expected (zsh + owner overlay) | Actual on master fade8ab | Status |
| --- | --- | --- | --- |
| `defaults/select_tab1_first_candidate` | TAB #1 line `cd cmp-aa/`, first cell reversed | line `cd cmp-`, nothing reversed | FAIL (#778) |
| `defaults/select_tab2_cycles` | TAB #2 line `cd cmp-bb/`, second cell reversed | line `cd cmp-aa/` (first splice lands on TAB #2), nothing reversed | FAIL (#778) |
| `select/shift_tab_previous` | Shift-TAB from candidate 0 wraps to `cmp-ff/`, reversed | line `cmp-ff/`, nothing reversed | FAIL (#778) |
| `select/down_one_row` | Down moves one row, `cmp-bb/` | line unchanged `cd cmp-` | FAIL (#778) |
| `select/up_one_row_wraps` | Up from first row wraps to `cmp-ff/` | line unchanged `cd cmp-` | FAIL (#778) |
| `select/right_one_column` | Right steps `rows` candidates (2), `cmp-cc/` | line unchanged `cd cmp-` | FAIL (#778) |
| `select/left_one_column_wraps` | Left wraps to `cmp-ee/` | line unchanged `cd cmp-` | FAIL (#778) |
| `select/enter_accept_runs` | Enter runs `cd cmp-aa/` | runs `cd cmp-` (the typed prefix), fails, cwd stays `work` | FAIL (#778) |
| `select/esc_rollback` | Esc restores `cd cmp-` | restored | PASS |
| `select/lone_candidate` | lone match completes outright | `cd only-dir/` | PASS |
| `select/bare_tab_indent` | blank-line TAB inserts four spaces, no grid | four spaces, no grid (owner overlay, not zsh) | PASS |
| `select/grid_column_major` | 3x2 column-major grid at 40 cols | `cmp-aa cmp-cc cmp-ee` / `cmp-bb cmp-dd cmp-ff` | PASS |
| `select/lscolors` | `di=01;35` paints directory cells | SGR `1;35` present | PASS |
| `select/page_keys_unclaimed` | PageDown changes nothing | line unchanged | PASS |
| `cycle/tab1_first_candidate` | TAB #1 first candidate + selected | `cd cmp-aa/`, reversed | PASS |
| `cycle/tab2_second` | second candidate | `cd cmp-bb/`, reversed | PASS |
| `cycle/shift_tab_wraps_last` | wrap backwards to last | `cd cmp-ff/`, reversed | PASS |
| `cycle/enter_accept_runs` | Enter runs the candidate | `cmp-aa` pwd | PASS |
| `cycle/esc_rollback` | Esc restores snapshot | `cd cmp-` | PASS |
| `inline/cycle_first_match_then_next` | inline style inserts first match, TAB cycles | TAB1 `cmp-aa/`, TAB2 `cmp-bb/` | PASS |
| `styles/select_inline_cycles` | inline presentation on the select axis: TAB1 `cmp-aa/`, TAB2 `cmp-bb/` | TAB1 `cd cmp-` + grid, TAB2 `cmp-aa/` | FAIL (#778) |

### Root cause of the FAILs (all #778)

* `crates/niubash-runtime/src/completion/cycle_menu.rs:422-426` — under
  `select`, `begin_session` deliberately leaves the first TAB unspliced and
  `has_selection = false`; zsh selects `gnum` at menu entry
  (`complist.c:2813`).
* `crates/niubash-runtime/src/completion/cycle_menu.rs:588-598` —
  `drain_events` maps only `NextElement`/`PreviousElement`; the engine's
  arrow events `MoveUp/MoveDown/MoveLeft/MoveRight` fall into `_ => {}`, so
  the zsh bindings are swallowed.
* Same range is why the first navigate splices candidate 0 but leaves the
  grid cell unstyled (the select-mode `has_selection` gate at line 748 only
  flips after a navigate), matching the "first highlight" failure.
* Enter inherits the same unselected start: with nothing spliced, accept-line
  submits the typed prefix instead of candidate 0.

The `cycle` and `inline` interactions do not share these failures; their
first-TAB splice/highlight path is the non-select branch at
`cycle_menu.rs:427-430`.

## Baseline separation

The 9 failures above (eight `defaults/select_*` + `select/*` cases plus
`styles/select_inline_cycles`) are one defect cluster (#778), not nine
independent completion-option gaps. On `fix/completion-options-honored` the
style-presentation and insert-first-TAB gaps were fixed separately; only this
#778 cluster remains, with the evidence and file:line anchors in
`failures.md`.
