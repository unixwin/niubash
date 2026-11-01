# Readline keyboard oracle (GNU bash vs niu)

A PTY differential test for **actual key behavior**. It compares GNU bash
5.3.20 / readline against the pinned niu build and reports every case as
`PASS`, `FAIL`, `KNOWN`, or `SKIP`. Expected values always come from GNU in
the same run; niu is never the reference.

This is not the default-keymap gap test. A real terminal is attached to
each shell, the key sequence is typed as bytes, and a `bind -x` hook dumps
`READLINE_LINE` and `READLINE_POINT` afterwards.

## Files

| File | Purpose |
| --- | --- |
| `run.sh` | Entry point; resolves `bash`/`niu`, calls the Python driver, exits nonzero on any FAIL |
| `pty_compare.py` | Case matrix, PTY sessions, dump hook, byte comparison, reports |
| `fixtures.sh` | Deterministic per-case `HOME`, `INPUTRC`, history, cwd, and completion files |
| `baseline/results.json` | Machine-readable baseline for the current niu pin |
| `baseline/summary.md` | Human-readable baseline (all mismatches and passes) |
| `select_first_tab_frame.py` | Focused frame-level regression for niubash#778: TAB #1 and grid arrow navigation (`NIU_BIN=… python3 …`) |
| `transcripts/latest/` | Raw PTY bytes for the latest run (gitignored) |

## How a case runs

Each case gets a fresh PTY and a fresh fixture tree. Fresh shells are
intentional: kill-ring entries, undo state, macros, and completion menus
otherwise leak between cases.

Controlled environment for both shells:

```text
HOME=<case>/home          INPUTRC=<case>/inputrc
HISTFILE=<case>/home/.history   HISTSIZE=100
TERM=xterm-256color       COLUMNS=80  LINES=24
PS1='__RLP__> '           LC_ALL=C.UTF-8
```

`fixtures.sh` creates `alpha.txt`, `alpine.txt`, and `beta.txt` in the
case cwd, and the history entries `alpha one`, `beta two`, `gamma three`.
The driver installs:

```sh
__rl_dump() {
    printf '%s' "$READLINE_LINE"  >"$DUMP_LINE"
    printf '%s' "$READLINE_POINT" >"$DUMP_POINT"
}
bind -x '"\C-x\C-d": __rl_dump'
```

The test text is typed, the key sequence is sent, the editor is allowed to
settle, then `C-x C-d` writes the two dump files. The run waits for both
files, reads them twice until stable, and compares GNU and niu raw bytes.
`C-c` clears the line before the session closes. A dump timeout is a FAIL,
not a SKIP.

The `bind -x` hook itself exposes a niu defect: because niu's cursor
read-back reports `len-1` at end of buffer, invoking a `bind -x` command
that leaves the line unchanged also moves niu's cursor back one character.
That is why every case dumps exactly once, at the end, and why the next
case starts from a fresh process.

### niu completion mode

All GNU-parity completion cases run niu with `NIU_COMPLETION_STYLE=gnu`.
TAB completion itself is intentionally zsh-style (owner ruling,
niubash#342/#778): the default `column` style opens the candidate grid on
the first TAB and splices the FIRST candidate (zsh menu-select). That is a
product rule, not a compatibility bug, so `complete/default_style_tab`
runs with `NIU_COMPLETION_STYLE=column` and asserts niu's documented result
(`cat alpha.txt ` at point 14, first candidate highlighted). GNU's
common-prefix result is still
captured and reported as the oracle reference, but it is not the
pass/fail target for that case. This keeps a product rule from either
hiding real GNU mismatches or being mislabeled as one.

## Matrix coverage

50 cases:

* **text** — insertion, Left/Right, Home/End, `C-a`/`C-e`, `C-b`/`C-f`,
  `M-b`/`M-f`, `C-w`, `C-u`, `C-k`, `C-y`, `M-y`, `C-t`, `M-t`,
  `M-u`/`M-l`/`M-c`, `C-@` + `M-f`, `C-x C-x`, `C-_` undo, `C-x C-u`
  undo, `C-g`, `C-g` followed by a key, bracketed paste.
* **history** — Up/Down, `C-r` incremental search, `M-p`/`M-n`
  non-incremental search, `C-o` operate-and-get-next.
* **completion** — TAB (common prefix, single match, double TAB), the
  zsh-default product case, `M-?`/`M-=` (list-only, with a
  candidate-list assertion), `M-*`, `M-/`, `M-~`, `M-@`, `M-{`, `M-g`.
* **macro** — `C-x (`, `C-x )`, `C-x e`.

GNU source anchors used in the case comments and driver:

* `emacs_keymap.c` — default emacs bindings.
* `text.c` — motion, word rubout, kill/yank/yank-pop, transposition,
  case conversion, mark exchange, undo, operate-and-get-next.
* `search.c` — `C-r` isearch and its `C-j`/`C-g` termination, `M-p`/`M-n`.
* `complete.c` — `rl_complete`, `rl_possible_completions`,
  `rl_insert_completions`, filename/username/hostname completion,
  `rl_complete_into_braces`, `rl_glob_complete_word`.
* `bashline.c` — `bash_execute_unix_command` and the
  `READLINE_LINE`/`READLINE_POINT` protocol (`bashline.c:4540`,
  `maybe_make_readline_line`).
* `macro.c` — keyboard macro record/end/call.
* `readline.c` — bracketed paste.

## Baseline: GNU bash 5.3.20 vs niu 1.4.4 (master 288b6b5, rubash c9604443, ruline edc490d)

Run on this host with the branch's debug build:

```text
PASS 50 / FAIL 0 / KNOWN 0 / SKIP 0   (50 cases)
```

`complete/default_style_tab` is an explicit product-divergence case, not a
KNOWN: under the owner ruling it asserts niu's documented zsh menu-select
result (TAB #1 splices `cat alpha.txt ` at point 14 with the first
candidate highlighted) and records GNU's common-prefix splice (`cat alp`,
point 7) as the reference. The rest of the matrix runs
with `NIU_COMPLETION_STYLE=gnu`. `M-?`/`M-=` are list-only and match GNU
buffer/point; their candidate lists are asserted in the transcripts.

The previous baseline on the merge pin reported the end-of-line point
off-by-one, `M-c`/`C-r` point placement, `M-t`/`C-_`/`C-g X`, `M-*`,
`M-g`, and the `C-x e` macro replay as failures. Those are closed: the
`bind -x` read-back reports `len`, `M-*`/`M-g` install their GNU default
bindings (glob-completion through `Session::glob_filename`), and a
chord's `C-x` prefix no longer leaks into a keyboard-macro recording.

`complete/m_tilde` has no password database on this host, so both sides
leave `~ro` unchanged; `history/m_n` deliberately searches forward from
the oldest entry, and both sides find `beta two`.

## Run it

```sh
cd /data/data/com.termux/files/home/wt/niu-keyboard

# full matrix against the pinned debug binary
scripts/readline-keyboard-oracle/run.sh

# one group / one case
scripts/readline-keyboard-oracle/run.sh --filter complete
scripts/readline-keyboard-oracle/run.sh --filter text/m_c

# list cases
scripts/readline-keyboard-oracle/run.sh --list

# refresh the committed baseline
scripts/readline-keyboard-oracle/run.sh --record-baseline
```

Useful overrides:

```sh
BASH_BIN=/path/to/bash NIU_BIN=/path/to/niu \
  scripts/readline-keyboard-oracle/run.sh --record-baseline
```

Exit status is `0` when every case is PASS/KNOWN/SKIP, `1` on any FAIL,
`2` on a harness invocation error (for example no matching `--filter`, or
`python3` missing). A behavior mismatch is always FAIL; SKIP is used only
when `bash`, `niu`, or `sh` is absent.

No network, no fzf, no ble.sh, no Rust build. The harness is Python +
POSIX `sh` only. If a new pin must be built, build it outside the harness
(one build, `CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0`), then point
`NIU_BIN` at it.

### After the next pin

```sh
NIU_BIN=/path/to/new/niu \
  scripts/readline-keyboard-oracle/run.sh --record-baseline

git diff -- scripts/readline-keyboard-oracle/baseline
```

The diff is the keyboard-parity delta. Re-run without
`--record-baseline` first if you only want to inspect the new status; raw
PTY transcripts for each case are under `transcripts/latest/`.

## Determinism notes

* Fixed per-case scratch layout under `.rl-oracle-work/`; deleted at the
  start of every full run (use `--keep-work` to preserve it).
* Fixture files, history, inputrc, prompt, terminal size, and locale are
  identical for GNU and niu.
* No host `HOME`, `INPUTRC`, `HISTFILE`, `PROMPT_COMMAND`, `BASH_ENV`,
  `CDPATH`, or `GLOBIGNORE` leaks into either shell.
* The only host-coupled completion data is `/etc/hosts` for `M-@` (same
  host for both sides) and the username database for `M-~` (empty here).
* `--settle` (default 0.40s) is the post-key sequence delay. niu needs a
  settle before the dump after a ding; a shorter value produces dump
  timeouts, which are FAILs.
