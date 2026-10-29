# Performance

Measured, not claimed. Every number on this page is pinned to the issue or PR
that produced it, and the gates that keep it honest are named. The engine-side
ledger lives in rubash's
[`docs/performance.md`](https://github.com/unixwin/rubash/blob/master/docs/performance.md);
this page is the host-facing summary, kept in step with the README's
Performance section.

## Windows spawn cost — one hidden console per session

A console-less session (direct `niu -c`, detached, CI) used to pay a hidden
console allocation on **every** external spawn, ~22 ms per child (design and
shape in [niubash#462](https://github.com/unixwin/niubash/issues/462)). The
fix landed engine-side
([rubash#913](https://github.com/unixwin/rubash/pull/913), shipped in v1.4.2):
a windowless console is pre-allocated once on Windows 11 24H2+/Server 2025
(`AllocConsoleWithOptions`), or adopted from the first `CREATE_NO_WINDOW`
child (`AttachConsole`) on older builds — an entire `niu -c 'a; b; c'` tree
pays at most one allocation and later spawns inherit it. 14 regression tests
pin zero visible windows and the scaling behaviour (rubash
`tests/console_window_gate.rs`). The same release closed the per-command
console flash through launchers (niubash#379 → rubash#668, WinuxCmd#1282) that
the [agents page](agents.md) names as the Claude Code failure.

## `$(tput …)` terminal queries answered in-process

The inline tput whitelist arm
([rubash#884](https://github.com/unixwin/rubash/pull/884)) answers admitted
TERM×capability shapes in-process, byte-equal to GNU ncurses (269-row
differential matrix, diff = 0; 8 regression tests). On the ticket probe — 64
tput calls through command substitution — wall time fell **1165 ms → 47 ms
(~25x, zero spawns)**. The winuxcmd-side twin removed the ~17 ms per-call i18n
overhead in the real binary (~0.4 ms in-process after the fix; 3208-row replay
byte-identical:
[WinuxCmd#1269](https://github.com/unixwin/WinuxCmd/pull/1269)). The other
harvest SLOW face — `read` on EOF, ~4.8 s/call — stays open on
[rubash#800](https://github.com/unixwin/rubash/issues/800).

## Engine hot paths

The embedded [rubash](https://github.com/unixwin/rubash) engine; each fix
carries its measured delta and a GNU differential battery:

- Multi-word literal command fast path: `[ 1 -eq 2 ]` ×20k ≈177→155 ms
  ([rubash#967](https://github.com/unixwin/rubash/pull/967)).
- `[[ ]]` direct numeric leaf + decimal fast path: 50k `[[ 1 -eq 2 ]]`
  1.41x→1.10x vs GNU
  ([rubash#966](https://github.com/unixwin/rubash/pull/966)).
- Whitelist-skip of the procsub metadata scan: 1.56x→1.39x (~11%)
  ([rubash#974](https://github.com/unixwin/rubash/pull/974)).
- Decimal-subscript array write lane: interleaved 8k 3.65x→3.16x; also fixes a
  real double side-effect in `a[$((i++))]=v`
  ([rubash#959](https://github.com/unixwin/rubash/pull/959)).
- Case: head-only `$BASH_COMMAND` render: 20-arm ×10k −10%, plus the
  divergence fix ([rubash#958](https://github.com/unixwin/rubash/pull/958)).

The interpreter triage behind that wave is closed with numbers: scalar
assignment ~1.06x GNU (rubash#609 → #672/#678), arithmetic loops and parameter
expansion via rubash#607/#608 (#665/#671/#679, #666 — `${s^}` alone ~6x
faster), and the O(n²) array element write became O(1) amortized
(rubash#618/#616: associative 5000-key writes **445x→9.9x** vs GNU, indexed
**736x→3.8x**). The export/unset fast-lane family landed
(#917/#918/#920, reworked for correctness in #943 — the per-op reduction the
v1.4.3 changelog notes);
[rubash#916](https://github.com/unixwin/rubash/issues/916) stays open on the
residual.

## Startup — the 2026-10-28 perf round (Linux, same machine)

The 2026-10-28 perf round re-ran the startup adjudication: 50-sample
interleaved runs of release builds on one Linux machine (sampling gate:
load1 ≤ 2.0, no other cargo/rustc), niu at master `c06b2a08` embedding
rubash `231ffe1a`, against a self-compiled GNU bash 5.3.20 oracle.
These are Linux same-machine numbers; they say nothing about Windows
magnitudes.

| shape | niu | rubash | GNU bash 5.3.20 |
|---|---|---|---|
| bare run (p50) | **3.71 ms** | 3.28 ms | 2.60 ms |
| `-c 'true'` (p50) | 4.14 ms | 3.59 ms | 2.67 ms |

The headline is the bare run: **16.98 ms in the 2026-10-25 baseline →
3.71 ms** — the non-interactive fast path
([niubash#562](https://github.com/unixwin/niubash/issues/562)) works as
designed, and niu bare startup now sits at the bare engine's level. The
`-c 'true'` row is ≈ baseline for all three shells. Engine-side detail
from the same round — the 30-case A-table, the pattern-op and
element-write fixes — lives in rubash's
[`docs/performance.md`](https://github.com/unixwin/rubash/blob/master/docs/performance.md);
the corpus and raw results live in
[shellbench](https://github.com/unixwin/shellbench).

## Residual round close-out — 2026-10-29 (Linux, same machine)

The perf round's residual batch has closed out (the fixes land
engine-side in rubash; niu inherits them with the engine). Same caliber
family as above: release builds, Linux same-machine interleaved
sampling — the residual batch sampled in same-window pairs under
ambient load, so ratios and growth factors are the verdict basis.

- Associative-array element reads no longer scale with table size: a
  `${m[key]}` miss at 10,000 entries measures 18.3 µs against GNU's
  13.4 µs, flat in n; the length form `${#m[key]}` had its growth
  factor cut from ×10.67 to ×1.08 on misses. The mechanism is a
  settled read on the combined table — the typed cell mirror answers
  hits and misses authoritatively (rubash PR #1919, merged; the length
  legs in the residual batch). Full account in the engine ledger.
- Command-completion candidates are cached: `compgen -c` on a fat
  PATH (~3,100 candidates) fell from 110.6 ms to 13.6 ms per call
  (GNU measures 277.6 ms in the same window); the walk re-runs only
  when the PATH/cwd fingerprint changes.
- Large grouped scripts load faster: an 8,000-line
  function-definition group — the grouped shape heavy startup scripts
  such as nvm.sh exercise — reads in at 0.533 s, down from 3.248 s;
  the grouped reader no longer re-tokenizes the accumulated logical
  line per physical line.
- Honest floor, unchanged: plain command dispatch still carries the
  ~2.83–2.95× A-table floor tax against GNU (tracked in
  [rubash#682](https://github.com/unixwin/rubash/issues/682)). The
  filed "procsub read at 68×" figure did not reproduce on current
  master: draining a procsub costs 1.3 µs/line, and the while-read
  residual prices the same across file and pipe sources — it is this
  floor tax, not a procsub defect.

## Android baseline — measured, honest, not yet re-measured

On a Termux aarch64 tablet, niubash 1.4.0 vs GNU bash 5.3.20
([niubash#352](https://github.com/unixwin/niubash/issues/352) — full method
and matrix in the issue: same script file per shell, interleaved runs,
best-of-3; startup rusage median of 20-30 warm runs):

| Workload | niu vs GNU bash 5.3.20 |
|---|---|
| Interpreter-bound (arith-for, string expansion, scalar assignment) | 2.3–3.7x slower (up to **7.3x** on `${x^^}` itself) |
| `niu -c true` startup | 21.1 ms vs 8.3 ms (median of 30) |
| Peak RSS (`VmHWM`, plain shell) | 10.2 MB vs 4.6 MB (**2.2x**) |
| Fork-bound (command substitution, pipelines) | up to **5.2x faster** |
| Real script (500× mkdir/write + read-back) | **2.1x faster** |

Both halves of the attributed gap are closed since the measurement: the
engine-side root causes (rubash#607-609, above) and the wrapper's one-shot
startup gap ([niubash#351](https://github.com/unixwin/niubash/issues/351) —
`niu -c` was building a never-rendered prompt whose width query spawned two
`tput` processes on non-tty paths; after the fix `niu -c true` measures
**15 ms vs bash 13 ms**, zero tput spawns).
[niubash#349](https://github.com/unixwin/niubash/issues/349) is the original
~2.1x arithmetic/function datapoint this pass grew out of.

**No post-fix re-measure on Android hardware exists yet** — #352 stays open
for that re-run, and the Android leg still ships build-verified only (see
[Platform Support](platform-support.md#android)).

## What keeps these numbers honest

- The release pipeline runs the `perfbudget` gate — every asset in
  `scripts/perf/budgets.toml` carries a measured budget (healthy themes
  re-render in 11-35 ms; the budget line is 120 ms; 5x over is a hard fail),
  and a breach fails the release. A nightly run watches the trend.
- Perf canaries ride the engine's CI regression suite
  (`cargo test --test regression`), GNU-differential batteries ship with each
  fix, and the harness discipline (min-of-N, interleaved runs, same-tree
  release binaries) is written into rubash's ledger.
