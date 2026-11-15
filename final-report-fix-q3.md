# Lane Q3 final report — niubash#731 residual funmap names

Authoritative residual set: **62 names** (issue text's "58" ≈ the ~50-strong vi
family; the mechanical diff against GNU `funmap.c` + `bashline.c` registrations
yields 60 + `bash-vi-complete` + `vi-edit-and-execute-command`).

## PRs

| Repo | PR | Status |
| --- | --- | --- |
| unixwin/ruline | https://github.com/unixwin/ruline/pull/63 | **merged** (a68ff11) |
| unixwin/niubash | https://github.com/unixwin/niubash/pull/1022 | **merged** |
| unixwin/ruline | https://github.com/unixwin/ruline/pull/64 | **merged** (52f3449) |
| unixwin/niubash | https://github.com/unixwin/niubash/pull/1024 | **merged** (7bcbfd1) |

Issue #731 updated with `[WIN-AGENT-FIX]` batch ledgers; all names landed,
issue closed.

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

## Batch 2 landed (9 names — the residual tail)

Ruline (PR 64): `MotionTarget::Byte` (forward-byte/backward-byte,
text.c:264/394; grapheme-edge-safe landing), `EditCommand::RuboutOrDelete`
(forward-backward-delete-char, text.c:1331), `ReedlineEvent::ArrowKeyPrefix`
+ `SkipCsiSequence` (text.c:653/639 — pending input consumers fed by the
input loop), `ReedlineEvent::ForwardSearchHistory` (isearch.c:141 —
`isearch_direction` field makes the one isearch machine direction-aware;
forward retargets seed from the last match or `history_recall_pos`),
`Reedline::switch_edit_mode` (host-channel mode entry for
`rl_vi_start_inserting`).

Niubash (PR 1024): `native_widget_event` wiring for all six ruline faces
plus the host trio — `tty-status` (util.c:134 `#else rl_ding`, no TIOCSTAT
on Windows), `export-completions` (`_rl_export_completions` printout:
count incl. common-prefix element, text, `start:end`, one match per line),
`vi-tilde-expand` (shared `tilde_expand_at_point` +
`switch_edit_mode(Vi(Insert))`). `\C-s` left
`KNOWN_UNMAPPED_GNU_DEFAULTS`, which is now **empty** — the whole GNU
default bind table mirrors.

## Residual

None — all 62 authoritative residual names are delivered across the two
repos. Issue #731 closed on merge of PR 1024; final ledger posted.

## Verification

- ruline: `cargo test --lib` 2519/2519 (5 new `q3_*` probes: byte
  step/clamp, rubout-or-delete by cursor position, arrow dispatch + ding,
  CSI tail consumption, forward isearch); clippy clean; CI failures
  identical to main's pre-existing red (rustfmt baseline drift +
  readline-abi + conformance + PR-only typos job on untouched files;
  msrv green after a dropped doc comment was restored).
- niubash: `cargo test -p niubash-runtime --lib` 1068/1069 —
  `bash_ps0_runs_before_interactive_command` fails identically on the
  baseline (stash-verified, unrelated env/rubash state);
  `gnu_default_bind_table_mirrors_except_known_gaps` updated to zero
  gaps and passes; clippy + fmt clean.
