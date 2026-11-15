# Lane Q3 final report — niubash#731 residual funmap names

Authoritative residual set: **62 names** (issue text's "58" ≈ the ~50-strong vi
family; the mechanical diff against GNU `funmap.c` + `bashline.c` registrations
yields 60 + `bash-vi-complete` + `vi-edit-and-execute-command`).

## PRs

| Repo | PR | Status |
| --- | --- | --- |
| unixwin/ruline | https://github.com/unixwin/ruline/pull/63 | **merged** (a68ff11) |
| unixwin/niubash | https://github.com/unixwin/niubash/pull/1022 | **merged** |

Issue #731 updated with `[WIN-AGENT-FIX]` batch-1 ledger; stays open
until the residual names land.

## Batch 1 landed (~50 names)

Ruline (PR 63): `ReedlineEvent::ViKeys` command-grammar injection seam
(`vi_command_char`/`vi_command_pending`/`vi_command_abort`), engine standby
routing, `MotionTarget::ViMark` + engine mark table, `FetchHistory`
(`rl_fetch_history`, misc.c:722; explicit arg via `WithArg`).

Niubash (PR 1022): `native_widget_event` wiring —

- ViKeys single-key faces: vi-append-mode, vi-append-eol, vi-back-to-indent,
  vi-backward-word/-bigword, vi-bword/-bWord, vi-prev-word, vi-forward-word/
  -bigword, vi-fword/-fWord, vi-next-word, vi-end-word/-bigword, vi-eword/-eWord,
  vi-change-case, vi-change-char, vi-change-to, vi-column, vi-delete,
  vi-delete-to, vi-fetch-history, vi-first-print, vi-goto-mark, vi-insert-beg,
  vi-insertion-mode, vi-match, vi-overstrike, vi-put, vi-replace, vi-rubout,
  vi-set-mark, vi-subst, vi-undo, vi-yank-to.
- Bound-key faces (ViKeys of the pressed char): vi-arg-digit, vi-char-search,
  vi-search, vi-search-again.
- Direct faces: vi-movement-mode (backspace + command map), vi-eof-maybe (CtrlD
  contract), fetch-history, vi-edit-and-execute-command (OpenEditor),
  vi-yank-arg, vi-yank-pop, vi-redo, vi-unix-word-rubout,
  vi-overstrike-delete.
- Completion faces: vi-complete, bash-vi-complete, old-menu-complete → host
  completion engine.

## Residual for the next batch (issue stays open)

- Byte/input layer: forward-byte, backward-byte,
  forward-backward-delete-char, arrow-key-prefix, skip-csi-sequence
  (needs ruline input-layer consume events).
- forward-search-history (direction-aware isearch).
- Host/session names: tty-status, export-completions, vi-tilde-expand.

## Verification

- ruline: `cargo test --lib` 2514/2514; clippy clean; CI failures identical
  to main's pre-existing red (rustfmt baseline drift + readline-abi +
  conformance + PR-only typos job on untouched files).
- niubash: `cargo test -p niubash-runtime --lib` 1067/1068 —
  `bash_ps0_runs_before_interactive_command` fails identically on the
  baseline (stash-verified, unrelated env/rubash state); clippy + fmt clean.
