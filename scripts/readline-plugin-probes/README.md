# readline plugin acceptance probes

Repeatable, CI-able probes for the in-process readline ABI and the bash
plugin surfaces that ride it.  Trackers:
[ruline#7](https://github.com/unixwin/ruline/issues/7) (plugin acceptance) and
[niubash#775](https://github.com/unixwin/niubash/issues/775) (host export /
routing).

This is deliberately a **script**, not part of `cargo test`.  PTY probes are
flaky by nature; `cargo test` keeps its deterministic non-PTY suite.  Run the
harness explicitly when changing `scripts/readline-abi-exports.txt`,
`build.rs`, the `readline-abi` pin, `bind -x`, inputrc handling, completion,
or the loadable-builtin path.

## Quick start

```sh
# Existing target/debug/niu, no rebuild:
sh scripts/readline-plugin-probes/run.sh

# Build niu first, keep logs/transcripts/.so for inspection:
sh scripts/readline-plugin-probes/run.sh --build --keep

# One probe:
sh scripts/readline-plugin-probes/run.sh --probe enable-f --keep

# List probe names:
sh scripts/readline-plugin-probes/run.sh --list
```

Requires a POSIX `sh`.  Optional per-host tools degrade to `SKIP` with a
reason: `cc` (C fixture), `python3` (PTY scenarios), `bash` (GNU oracle),
`nm`/`readelf`/`dumpbin` (export audit).  Nothing needs the network at run
time; `cargo build` is only attempted when `--build` is passed or no
`target/debug/niu` exists, and normally uses the local Cargo cache.

Exit status is `0` when every probe is `PASS` or an environment-only `SKIP`,
`1` when any probe `FAIL`s.  A missing feature of ours is a `FAIL`, never a
`SKIP`; `SKIP` is only for a genuinely absent external tool.  The reason is
printed in the matrix.  Artifacts are kept automatically on failure and with
`--keep` (default work dir: `$TMPDIR/niu-readline-probes.XXXXXX`).

## Matrix

| Probe | What it proves | Needs |
|---|---|---|
| `niu-build` | binary under test exists / `--build` succeeds | `cargo` (or prebuilt) |
| `readline-abi-exports` | every symbol in `scripts/readline-abi-exports.txt` (~274) is in the **dynamic** symbol table (`scripts/check-readline-abi-exports.sh`) | `nm` or `readelf`, or `dumpbin` on Windows |
| `loadable-builtin-build` | `loadable_builtin.c` compiles as a shared object | `cc` (`CC` overrides) |
| `loadable-builtin-gnu-bash` | the same `.so` loads in interactive GNU bash and reads/writes the **live** `rl_line_buffer`/`rl_point` | `bash`, ideally `python3` |
| `enable-f` | niu's `enable -f` can dlopen the fixture (currently refused by rubash) | `niu` |
| `loadable-builtin-niu` | the fixture runs against niu and reports complete ABI fields (currently depends on `enable-f`) | `niu` |
| `fzf-bindx-register` | `bind -x` registration survives `bind -X` listing | `niu` |
| `fzf-bindx-roundtrip` | PTY Ctrl-R runs a fake fzf, reads `READLINE_LINE`, rewrites it, and honors `READLINE_POINT` | `niu`, `python3` |
| `bash-completion` | local `complete -F` fixture: `complete -p`, `COMPREPLY`, `compgen`, `COMP_WORDBREAKS` match GNU bash | `niu`, `bash` |
| `inputrc-startup` | `set` vars, `"key": function`, a macro, and `$include` apply at startup | `niu` |
| `inputrc-reread` | `C-x C-r` re-reads `.niubashrc` bind lines; checks whether `$INPUTRC` itself is re-read (known gap) | `niu`, `python3` |

The C fixture (`loadable_builtin.c` + `build-builtin.sh`) is a probe, not a
plugin: every symbol is resolved with `dlsym(RTLD_DEFAULT)`, so a missing
symbol is reported (`missing`) instead of preventing `dlopen`.  That is what
lets the runner distinguish "host does not export the ABI yet" from "fixture
is broken".  It calls:

* `rl_readline_version` / `rl_library_version` (data symbols),
* `rl_add_defun` and `rl_bind_key` (registered function `rl-probe-defun`),
* `rl_line_buffer` (reads the exported buffer, flips and restores its first
  byte; standalone ABI state until live-editor routing lands),
* `rl_point` (reads, bumps, restores).

One machine-readable line is printed:

```text
RL_PROBE version=0x0803 library=8.3 header=0x0803 add_defun=ok bind_key=ok \
  line_buffer=ok line_mutated=ok point=ok point_mutated=ok result=ok
```

`result=missing-abi` means the version/add_defun/bind_key core is incomplete;
`line_buffer=absent` is expected for a non-interactive `bash -c` and is *not*
treated as an ABI failure (the GNU oracle runs under a PTY to observe it).

## Observed baseline

Run on 2026-11-01, Android/Termux aarch64, `sh` = dash, after rubash
`bbc084e7` and the `re-read-init-file` inputrc fix:

| Tool | Version |
|---|---|
| niu | `Niubash 1.4.4`, local rubash `bbc084e7` |
| GNU bash | `5.3.20(1)-release` with readline `8.3` |
| fzf | `0.74 (devel)` (deterministic fixture used) |
| C compiler | clang `21.1.8` |
| python3 | `3.14.6` |
| bash-completion | framework absent; local fixture used |

Result: **PASS 10 / FAIL 1 / SKIP 0**.

| Probe | Baseline | Why |
|---|---|---|
| `niu-build` | PASS | existing `target/debug/niu` |
| `readline-abi-exports` | PASS | version symbols exported |
| `loadable-builtin-build` | PASS | `.so` built |
| `loadable-builtin-gnu-bash` | PASS | live buffer: `line_buffer=ok line_mutated=ok point=ok point_mutated=ok result=ok` |
| `enable-f` | **PASS** | rubash `enable -f` now dlopens and runs `rl_probe` |
| `loadable-builtin-niu` | **FAIL** | the builtin loads but the full `rl_*` surface is not exported by niu yet — `add_defun=missing bind_key=missing line_buffer=absent` (niubash#775) |
| `fzf-bindx-register` | PASS | `"\C-r" "__probe_fzf"` listed by `bind -X` |
| `fzf-bindx-roundtrip` | PASS | `Xpicked:typed` from point insertion + empty-buffer `picked:` |
| `bash-completion` | PASS | byte-identical to GNU bash for the fixture |
| `inputrc-startup` | PASS | all four fixture features applied |
| `inputrc-reread` | **PASS** | `C-x C-r` re-reads both rc bind lines and the `$include`d `$INPUTRC` binding |

Remaining blocker: the in-process ABI export is still the two-symbol probe
(niubash#775). `enable -f` and the loadable-builtin loader are now real
(rubash#2440), so the remaining failure is the host-side full-surface export
and live-editor routing, not rubash.

`loadable-builtin-gnu-bash` is the fixture self-check: when `enable -f` is
fixed, a failing GNU-bash oracle means the fixture—not niu—is at fault.

## Files

```text
scripts/readline-plugin-probes/
├── README.md                  this file
├── run.sh                     the single POSIX-sh runner/matrix
├── pty_probes.py              three explicit PTY scenarios (fzf/inputrc/builtin)
├── loadable_builtin.c         bash loadable builtin fixture
├── build-builtin.sh           cc build script; prints the .so path
└── fixtures/
    ├── fzf                    deterministic fzf stand-in (stdin -> picked:<line>)
    ├── fzf.rc                 bind -x / READLINE_LINE / READLINE_POINT fixture
    ├── inputrc                set vars + function + macro + $include (@EXTRA@)
    ├── extra.inputrc          the $include'd key binding
    └── completion.bash        representative complete -F definition
```

`run.sh` writes logs, transcripts, the built `.so`, and `results.tsv` under
`$TMPDIR/niu-readline-probes.XXXXXX` (kept on failure/`--keep`).

## CI notes

The runner has no network dependency and no hidden state.  A CI job can:

```sh
cargo build --locked --bin niu
sh scripts/readline-plugin-probes/run.sh --probe readline-abi-exports
sh scripts/readline-plugin-probes/run.sh --probe loadable-builtin-build
sh scripts/readline-plugin-probes/run.sh --probe bash-completion
sh scripts/readline-plugin-probes/run.sh --probe inputrc-startup
```

Keep the PTY probes on a self-hosted/known-PTY lane.  On hosts without a
usable pty or `python3`, they `SKIP` with that reason; on a host that claims
`NIU_INTERACTIVE_TESTS=1`-style support, a missing pty should be treated as a
lane failure outside this script.  The loadable-builtin probe expects a
dynamically linkable niu (not `crt-static`/musl-static), matching
`build.rs`'s export gate.

## Adding a probe

1. Put deterministic fixtures under `fixtures/` and take paths as arguments.
2. Record a specific reason for `PASS`, `FAIL`, and `SKIP`; a generic
   "failed" is not useful in the matrix.
3. Only `FAIL` on behavior that should work today.  A known acceptance gap
   is `SKIP` with the tracker and the exact observed command/diagnostic.
4. Never change production behavior in this harness to turn a probe green.
