# Completion matrix: baseline after the PageDown assertion fix

Branch: `fix/completion-matrix-page-keys`, based on master `78c815a` (which
already merges the style-shape and insert-stop fixes and #778).
Binary: `target/debug/niu`, built in this worktree with cargo's default jobs.

| Run | PASS | FAIL | N/A | Total |
| --- | --- | --- | --- | --- |
| before this branch (master `78c815a`) | 54 | 2 | 2 | 58 |
| after this branch | **55** | **1** | **2** | 58 |

## Harness fix: `select/page_keys_unclaimed`

The case captured `before` *after* TAB — with #778 merged that is
`cd cmp-aa/`, not the typed prefix — then pressed PageDown and asserted
`after == "cd cmp-"`. That made a correct product behavior fail.

The intended contract is that PageDown is unclaimed: it must neither splice
nor move the selection. The assertion is now:

```python
before = s.input_line()
before_highlight = s.highlighted(CANDIDATES)
s.key(PGDN)
after = s.input_line()
after_highlight = s.highlighted(CANDIDATES)
ok = after == before and after_highlight == before_highlight
```

Result on `78c815a`: before/after both `cd cmp-aa/`, highlighted
`['cmp-aa']` both times. **PASS.** No product PageDown behavior changed.

## Remaining FAIL: `select/up_one_row_wraps` (#778)

Repro: `cmp-aa … cmp-ff` cwd, 40 columns, default `column`+`select`, type
`cd cmp-<TAB>`, then Arrow Up.

* Expected: wrap from candidate 0 (top row, first column) to the last
  candidate `cmp-ff/`, highlighted.
* Actual: `cd cmp-bb/`, highlighted `cmp-bb` — it behaves like Down.

This is the only remaining matrix failure. Dispatch is
`crates/niubash-runtime/src/repl.rs:2715-2720`
(`Up => CycleMove::Grid(GridMove::Up)`); the virtual cursor/edge-wrap math
is `grid_move` in `crates/niubash-runtime/src/completion/cycle_menu.rs`.
Owned by #778; not changed here.

## N/A: internal-only knobs (2)

* `knobs/page_size_not_an_option` — fixed `MenuConfig::completion_page_size`
  (10); no resolver/env/config key.
* `knobs/completion_behavior_not_an_option` — internal `CompletionBehavior`;
  no resolver/env/config key.

No env names were invented.

## Everything else green

The style-presentation cases (column description column, ide popup, list
vertical), the insert first-TAB cases, all aliases, precedence/invalid
fallback, config-file migration notice, bare-TAB indent, Enter/Esc,
cycle/inline, grid geometry, LS_COLORS, and all `NIU_COMPDEFS`/compspec
layers pass. `select/page_keys_unclaimed` now passes for the corrected
reason.
