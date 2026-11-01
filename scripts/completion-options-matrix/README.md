# Completion options matrix

A deterministic PTY matrix for **every completion knob niubash documents**:
the five presentation styles, the three interaction modes, environment vs
config-file precedence, aliases, invalid-value fallback, page sizing, and the
`NIU_COMPDEFS`/compspec layers.

The harness answers cursor-position requests from its own VT model, captures
frames, and asserts the *screen contract* — input line, grid geometry, which
cell is reverse-video, whether descriptions render — not just the edit buffer.
A real terminal is attached to every case; each case gets a fresh HOME,
INPUTRC, history, cwd and window size.

## Run it

```sh
# current branch build
scripts/completion-options-matrix/run.sh

# one group
scripts/completion-options-matrix/run.sh --filter select

# refresh baseline/results.json + baseline/summary.md
scripts/completion-options-matrix/run.sh --record-baseline
```

`NIU_BIN=/path/to/niu` overrides the binary. The harness never builds; build
under the global lock first:

```sh
flock /data/data/com.termux/files/home/.ruline-build.lock sh -c \
  'cd /data/data/com.termux/files/home/wt/niu-completion-matrix && \
   CARGO_PROFILE_DEV_DEBUG=0 cargo build --locked --bin niu'
```

Exit status is `0` when every case is PASS/MISSING/SKIP and `1` on any FAIL.
Run one matrix at a time: the scratch root (`.work/`) is shared by default;
use `--work <dir>` if two runs must overlap.

## What is asserted

| Group | Contract |
| --- | --- |
| `defaults` | built-in `column` + `select`; first TAB selects the first candidate and paints it selected |
| `select` | zsh menu-select bindings: Tab/BackTab, Up/Down, Left/Right, Enter, Esc, lone candidate, bare-TAB indent, column-major grid, LS_COLORS |
| `cycle` | `cycle` interaction: TAB #1 first candidate + grid, TAB/Shift-TAB cycle, Enter runs, Esc rolls back |
| `inline` | `inline` style = first match inserted, TAB cycles (bash `menu-complete`) |
| `insert` | first TAB splices the common prefix and stops; second TAB lists; no candidate on the first TAB |
| `styles` | each style's documented shape under the default interaction: column grid, ide popup + descriptions, list vertical + descriptions, inline in-place, gnu printed list with no selection |
| `aliases` | `readline→gnu`, `col/grid→column`, `vertical→list`, `cycle/menu→inline`; the code-only `smart/popup→ide` aliases are asserted too |
| `precedence` | env > default; invalid values warn once and fall back; `config.toml` is not read (1.5.0) and prints the migration notice |
| `knobs` | `completion.page_size` and `completion.behavior`: documented/claimed or not exposed |
| `compdefs` | `NIU_COMPDEFS` beats static tables; a compspec owns the command over static tables; compdef + compspec merge; external `.toml`/`.bash` definition directories |

The zsh menu-select expectations cite the local zsh 5.9.2 source:

* `Src/Zle/complist.c` `menuselect_bindings()` — Tab=`complete-word`,
  Enter/CR=`accept-line`, arrows=`up/down-line-or-history` /
  `forward-char`/`backward-char`.
* `Src/Zle/complist.c` `domenuselect()` — `mselect =
  (*(minfo.cur))->gnum`, i.e. the first match starts selected.
* `Doc/Zsh/mod_complist.yo` — "All movement functions wrap around at the
  edges."
* `refs/oh-my-zsh/lib/completion.zsh` L1–14 — `unsetopt menu_complete`,
  `setopt auto_menu`, `zstyle ':completion:*' menu select`, and
  `^o`=`accept-and-infer-next-history`.

**Product overlay (owner rule):** niu's default TAB is *first-TAB select*
(candidate 0 spliced and highlighted), not OMZ's `auto_menu` timing where
the first TAB only lists. That difference is intentional and is called out
in `zsh-parity.md`; it is not scored as a zsh deviation.

## Files

| File | Purpose |
| --- | --- |
| `matrix.py` | case inventory + assertions + report writer |
| `session.py` | PTY session, isolated env, CPR answering, settle discipline |
| `vt.py` | small VT screen model (cursor motion, SGR, scroll, UTF-8 widths) |
| `run.sh` | entry point |
| `baseline/results.json` | machine-readable baseline for the pinned binary |
| `baseline/summary.md` | human-readable PASS/FAIL/MISSING report |
| `zsh-parity.md` | source-cited zsh menu-select evidence and product overlay |

Scratch trees live under `.work/` (deleted at the start of every run).
