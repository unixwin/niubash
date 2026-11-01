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
| Menu-select starts with the first match selected | `Src/Zle/complist.c:2813` `mselect = (*(minfo.cur))->gnum` |
| Tab = complete-word, Enter/CR = accept-line, Up/Down = up/down-line-or-history, Left/Right = backward/forward-char | `Src/Zle/complist.c:3543-3560` `menuselect_bindings()` |
| Movement wraps at the grid edges | `Doc/Zsh/mod_complist.yo:397`; movement code `Src/Zle/complist.c:2940-3210` |
| OMZ opts into `menu select`, `auto_menu`, no `menu_complete`, and binds `^o` to `accept-and-infer-next-history` | `lib/completion.zsh:1-14` |

zsh's `menuselect` keymap binds no PageUp/PageDown and no Home/End; those
fall through to the current keymap. niu makes no menu page/home claim, so the
matrix probes PageDown as *unclaimed* and expects neither the line nor the
selected candidate to move.

## Product overlay (owner rule, not a zsh deviation)

OMZ sets `unsetopt menu_complete` + `setopt auto_menu`, so in OMZ the first
TAB lists and the menu *starts* on a later TAB. The niu owner rule is
explicitly different: **default TAB is first-TAB select** — TAB #1 already
splices candidate 0 and paints it selected. That is intentional. niu's
menu-entry state matches zsh `mselect = first gnum`; only the trigger timing
is the product overlay. The matrix therefore scores `TAB #1 selects` against
the owner rule, not against OMZ's auto_menu timing.

## Matrix result (master `78c815a`, #778 merged)

| Case | Expected (zsh + owner overlay) | Actual | Status |
| --- | --- | --- | --- |
| `defaults/select_tab1_first_candidate` | TAB #1 `cd cmp-aa/`, first cell reversed | `cd cmp-aa/`, reversed | PASS |
| `defaults/select_tab2_cycles` | TAB #2 `cd cmp-bb/`, second cell reversed | `cd cmp-bb/`, reversed | PASS |
| `select/shift_tab_previous` | Shift-TAB wraps to `cmp-ff/`, reversed | `cmp-ff/`, reversed | PASS |
| `select/down_one_row` | Down moves one row, `cmp-bb/` | `cmp-bb/`, reversed | PASS |
| `select/up_one_row_wraps` | Up from first row wraps to `cmp-ff/` | `cmp-bb/` (moved down) | **FAIL (#778)** |
| `select/right_one_column` | Right steps `rows` candidates, `cmp-cc/` | `cmp-cc/`, reversed | PASS |
| `select/left_one_column_wraps` | Left wraps to `cmp-ee/` | `cmp-ee/`, reversed | PASS |
| `select/enter_accept_runs` | Enter runs `cd cmp-aa/` | runs `cmp-aa/` | PASS |
| `select/esc_rollback` | Esc restores `cd cmp-` | restored | PASS |
| `select/lone_candidate` | lone match completes outright | `cd only-dir/` | PASS |
| `select/bare_tab_indent` | blank-line TAB inserts four spaces, no grid | four spaces, no grid (owner overlay, not zsh) | PASS |
| `select/grid_column_major` | 3x2 column-major grid at 40 cols | `cmp-aa cmp-cc cmp-ee` / `cmp-bb cmp-dd cmp-ff` | PASS |
| `select/lscolors` | `di=01;35` paints directory cells | SGR `1;35` present | PASS |
| `select/page_keys_unclaimed` | PageDown changes neither line nor selection | line `cd cmp-aa/` and `cmp-aa` before/after | PASS |
| `cycle/tab1_first_candidate` | TAB #1 first candidate + selected | `cd cmp-aa/`, reversed | PASS |
| `cycle/tab2_second` | second candidate | `cd cmp-bb/`, reversed | PASS |
| `cycle/shift_tab_wraps_last` | wrap backwards to last | `cd cmp-ff/`, reversed | PASS |
| `cycle/enter_accept_runs` | Enter runs the candidate | `cmp-aa` pwd | PASS |
| `cycle/esc_rollback` | Esc restores snapshot | `cd cmp-` | PASS |
| `inline/cycle_first_match_then_next` | inline style inserts first match, TAB cycles | TAB1 `cmp-aa/`, TAB2 `cmp-bb/` | PASS |
| `styles/select_inline_cycles` | inline presentation on the select axis | TAB1 `cmp-aa/`, TAB2 `cmp-bb/` | PASS |

### Remaining FAIL

`select/up_one_row_wraps` is the only failure: from the TAB-#1 highlight
(index 0), Arrow Up should wrap to the last candidate but moves down one row
to `cmp-bb/`. The dispatch is in `crates/niubash-runtime/src/repl.rs:2715-2720`
(`Up => CycleMove::Grid(GridMove::Up)`) and the virtual-cursor math in
`crates/niubash-runtime/src/completion/cycle_menu.rs` (`grid_move`). This is
#778's follow-up, not a harness expectation error.

`select/page_keys_unclaimed` was a harness bug fixed on this branch: it
captured `before` after TAB (already `cd cmp-aa/`) and asserted the
post-PageDown line equalled the pre-TAB prefix. It now compares
`after == before` and the highlighted candidate before/after. No product
PageDown behavior changed.
