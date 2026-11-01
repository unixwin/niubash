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

All completion cases run niu with `NIU_COMPLETION_STYLE=gnu`, the
documented GNU readline parity mode. niu's default `column` style is a
deliberate product behavior; the single `complete/default_style_tab` case
exercises it and is marked `KNOWN` with the docs link:

<https://github.com/unixwin/niubash/blob/master/docs/src/completion.md#completion-styles>

This keeps a product-specific menu style from hiding real GNU completion
mismatches.

## Matrix coverage

48 cases:

* **text** — insertion, Left/Right, Home/End, `C-a`/`C-e`, `C-b`/`C-f`,
  `M-b`/`M-f`, `C-w`, `C-u`, `C-k`, `C-y`, `M-y`, `C-t`, `M-t`,
  `M-u`/`M-l`/`M-c`, `C-@` + `M-f`, `C-x C-x`, `C-_` undo, `C-x C-u`
  undo, `C-g`, `C-g` followed by a key, bracketed paste.
* **history** — Up/Down, `C-r` incremental search, `M-p`/`M-n`
  non-incremental search, `C-o` operate-and-get-next.
* **completion** — TAB (common prefix, single match, double TAB), `M-?`,
  `M-*`, `M-/`, `M-~`, `M-@`, `M-{`, `M-g`, plus the default-style
  `KNOWN` case.
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

## Baseline: GNU bash 5.3.20 vs niu 1.4.4 (master a883cde, ruline 0bfc785)

Run on this host with the prebuilt
`/data/data/com.termux/files/home/projects/niubash/target/debug/niu`:

```text
PASS 20 / FAIL 27 / KNOWN 1 / SKIP 0   (48 cases)
```

The 27 FAILs are:

**19 × end-of-line point off-by-one** — same buffer, niu point is `len-1`.
This is the `bind -x` read-back defect:

`text/chars`, `text/end`, `text/c_e`, `text/c_w`, `text/c_k_mid`,
`text/c_y`, `text/m_y`, `text/c_g`, `text/bracketed_paste`,
`history/up`, `history/c_o`, `complete/tab_common_prefix`,
`complete/tab_single`, `complete/tab_double`, `complete/m_question`,
`complete/m_slash`, `complete/m_tilde`, `complete/m_at`,
`complete/m_lbrace`.

Expected `point == len(line)`; niu reports `len(line)-1` because
`Shell::run_bind_x_command` computes `safe_cursor` from
`char_indices().filter(|off| *off <= cursor_byte).max()`. There is no
`char_indices()` entry at `buffer.len()`, so a cursor at the end maps to
the last character start. GNU `bashline.c:4540` reports the character
offset, which is `len`.

**2 × other point mismatches**

| Case | Keys | GNU | niu | Meaning |
| --- | --- | --- | --- | --- |
| `text/m_c` | `C-a M-c` | `Hello world` / 5 | `Hello world` / 1 | Capitalized word is right; point jumps to the first letter instead of the end of the word. |
| `history/c_r` | `C-r beta C-j` | `beta two` / 0 | `beta two` / 7 | GNU isearch leaves point at the match start; niu leaves it at the end of the recalled line. |

**6 × buffer mismatches**

| Case | Keys | GNU | niu | Meaning |
| --- | --- | --- | --- | --- |
| `text/m_t` | `C-e M-t` | `one three two` | `one two three` | `M-t` at end-of-line is a no-op in niu. |
| `text/undo` | `C-_` | `` (empty) | `abc` | `C-_` undo is a no-op in niu. `C-x C-u` works and is a PASS. |
| `text/c_g_then_key` | `C-g X` | `abcX` | `abc` | niu flushes a key queued in the same burst behind `C-g`; GNU keeps it. A key sent after a pause is inserted normally. |
| `complete/m_star` | `M-*` | `cat alpha.txt alpine.txt ` | `cat al` | `insert-completions` is a no-op in niu. |
| `complete/m_g` | `M-g` | `cat alp` | `cat al*` | GNU glob-completes `al*` and consumes the star; niu does nothing. |
| `macro/record_replay` | `C-x ( ab C-x ) C-x e` | `aba` | `ab` | Macro recording works, but `C-x e` does not replay the macro. |

**1 × KNOWN**

`complete/default_style_tab` (TAB, default column style): GNU inserts the
common prefix `alp`; niu opens the candidate grid and leaves `cat al`.
Documented product behavior, linked above; the completion-style knob is
tracked in <https://github.com/unixwin/niubash/issues/342>.

The other 20 cases PASS, including `C-a`/`C-b`/`C-f`/`M-b`/`M-f`,
`C-u`, `C-t`, `M-u`/`M-l`, `C-@` + `M-f`, both `C-x C-x` cases,
`C-x C-u`, Down, `M-p`/`M-n`, and `C-r` abort. The GNU-mode TAB and
`M-/` buffers match GNU; those cases are listed under the EOL point
failures only.

`complete/m_tilde` is a parity PASS on the buffer but still FAILs on the
EOL point: this host has no password database, so both sides leave `~ro`
unchanged. `history/m_n` deliberately searches forward from the oldest
entry; both sides find `beta two`.

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
