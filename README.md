<p align="center">
  <img src="assets/niubash-banner.svg" alt="niubash — real Bash, native on Windows, Linux, and macOS." />
</p>

# Niubash — native Bash for Windows (Git Bash alternative)

> **Real Bash, native on Windows, Linux, and macOS.** "Windows-only" is the
> old story: five platforms build from this tree now, and Windows remains
> the flagship — no WSL. No VM. No `/mnt/c`. No cmdlet dialect. One binary:
> the shell your fingers already know, and the one your AI agent actually
> speaks.

<div align="center">

[English](README.md) · [中文](README-zh.md)

Docs & downloads: [unixwin.dev](https://unixwin.dev)

[![niubash CI](https://github.com/unixwin/niubash/actions/workflows/ci.yml/badge.svg)](https://github.com/unixwin/niubash/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/unixwin/niubash)](https://github.com/unixwin/niubash/releases)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-blue)](https://github.com/unixwin/niubash)
[![Rust](https://img.shields.io/badge/rust-1.95%2B-orange)](https://github.com/unixwin/niubash)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Stars](https://img.shields.io/github/stars/unixwin/niubash)](https://github.com/unixwin/niubash/stargazers)

</div>

**niubash** is a Bash-compatible shell implemented natively in Rust, a Git Bash alternative. On
Windows, the flagship platform, one `niu.exe` bundles everything: the
[rubash](https://github.com/unixwin/rubash) language engine, real Unix
commands from [winuxcmd](https://github.com/unixwin/winuxcmd), a git-aware
prompt, and a permission-modeled plugin system. Git comes from
[niu-git](https://github.com/unixwin/niu-git): the real git-for-windows C
source built with MSVC, so the `git` on PATH *is* the git binary — no MSYS
runtime, no launcher hop, no bundled perl. `git --version` starts in **~14
ms** (13.7 ms avg / 11.5 ms min measured; `git status` 22.6 ms) where Git
Bash's `git` takes 45-103 ms
([niu-git#12](https://github.com/unixwin/niu-git/pull/12),
[niubash#374](https://github.com/unixwin/niubash/pull/374)), and the
shell-script parts of
git (hooks, `git bisect`, `git mergetool`) run on niubash's own `sh`. Agents
launched from a niu session inherit it as their shell through `SHELL`; see
[Coding agents on Windows](docs/src/agents.md) for which shell each agent runs
and how to point the rest at niu. The
Linux and macOS builds
are portable `niu` binaries that use your system's own tools. niu is not
MSYS2, Cygwin, Git Bash, or WSL — there is no POSIX emulation layer and no
path-conversion heuristics. It is a from-scratch replacement for Git Bash: a native Windows binary with no POSIX emulation. The full per-platform contract, including the
honest status of every target, lives in
[platform-support.md](docs/src/platform-support.md).

## Quick start

All downloads start at [unixwin.dev](https://unixwin.dev). Latest release:
**v1.4.4**.

Windows: grab `niubash-v*-win-*-setup.exe` from
[Releases](https://github.com/unixwin/niubash/releases) and run it — no
admin rights; PATH and a Windows Terminal profile are set up for you.
Prefer portable? Take the `.7z` — it preserves the bundled git/awk
payload's hardlinks, so it stays small. The `.tar.gz` works too
(`tar -xf niubash-v*-win-x64.tar.gz`, built into Windows 10+). Avoid the
`.zip`: it cannot represent hardlinks, so the payload is stored
duplicated and the archive is much larger — it stays only for tooling
that speaks nothing but zip.

*winget:* the `Unixwin.Niubash` package is submitted and currently in
Microsoft review ([microsoft/winget-pkgs#460976](https://github.com/microsoft/winget-pkgs/pull/460976))
— until it clears, use the installer above.

Linux (x86_64, aarch64; glibc 2.35+) and macOS (aarch64, x86_64): take the
portable tarball, untar, run:

```sh
tar -xzf niubash-v*-linux-x86_64.tar.gz && niubash-v*-linux-x86_64/niu
```

Android (aarch64, armv7): release `.zip` assets ship since v1.4.3 and run
on real devices (Termux) — see [Platforms](#platforms) for the honest
per-platform status.

From source (Rust 1.95+):
`git clone https://github.com/unixwin/niubash.git && cd niubash && cargo build --release`

Configuration is ONE file, `~/.niubashrc`, in plain Bash syntax — it is
the **only** startup file niu reads (`~/.bashrc` is never sourced; run
`niu plugin migrate-rc --diff` to see exactly what is worth carrying
over) — start with
[Getting started](docs/src/getting-started.md).

## Niubash vs Git Bash

| | **Niubash** | **Git Bash** |
| --- | --- | --- |
| Engine | Bash semantics reimplemented natively in Rust ([rubash](https://github.com/unixwin/rubash)); no emulation layer | GNU Bash ported onto the MSYS2 POSIX emulation runtime |
| Bash language level | **GNU Bash 5.3** semantics — including 5.3-only syntax such as no-fork command substitution `${ cmd; }` ([rubash#195](https://github.com/unixwin/rubash/issues/195)) | GNU Bash **5.2** (the version Git for Windows bundles) |
| Path handling | Windows paths are the native representation; argument translation is a deterministic, documented contract — no guessing | Heuristic path conversion of arguments, with `MSYS_NO_PATHCONV` as the documented opt-out |
| `git` | Real git-for-windows C source built with MSVC ([niu-git](https://github.com/unixwin/niu-git)); `git --version` ~14 ms | `git` launched through the MSYS runtime; 45–103 ms for the same probe ([niu-git#12](https://github.com/unixwin/niu-git/pull/12), [niubash#374](https://github.com/unixwin/niubash/pull/374)) |
| Cold start to prompt | **~170 ms** | ~1 s |
| Subshells | In-process — `( )` and `$( )` create no processes | `fork()` emulation |
| Console devices | `/dev/tty` redirects behave like GNU — the engine splits `CON` into `CONIN$`/`CONOUT$` (see [Quick answers](#quick-answers)) | Console device opens hit Windows' device restrictions |

The long version — including same-command-line side-by-sides of what
actually reaches a native process — is
[Why niubash](docs/src/why-niubash.md).

## Why not Git Bash / MSYS2 / Cygwin — and why not a "lightweight replacement" shell

**Git Bash** calls itself, verbatim, "a BASH emulation used to run Git from the
command line" — an emulation built to run Git. The costs are structural: console
device redirects such as `echo >/dev/tty` fail outright with `Invalid argument`
in its MSYS layer (niubash matches GNU; project-measured); its bundled bash is on
the 5.2 line, so bash 5.3's `${ cmd; }` nofork substitution doesn't exist; every
prompt spawns git for the branch segment (~100–300 ms), while niu reads `.git/HEAD`
directly and never spawns git (39 ms cold, p50 2.5 ms warm; project-measured, as
of 2026-10-25); a real install is 440 MB plus a 20 MB simulation DLL, versus
niu at ≈16 MB with zero DLLs.

**MSYS2**'s own documentation concedes that arguments resembling Unix paths are
automatically rewritten — and that the rewriting "also isn't perfect," hitting
arguments that aren't paths at all. niubash keeps translation deliberately
narrow: quoted arguments are never rewritten, and the only device translation is
the exact token `/dev/null` → `nul`; every overreach ever found became a public
issue and a pinned regression test. **Cygwin** states on its own site that apps
must be rebuilt from source to run in its world — the simulation layer is its
official definition. niubash is a native build: no simulation DLL.

**The new "bash replacement" shells** (e.g. brush, in Rust) sell lightness.
Lightweight is no excuse for missing limbs: complete bash semantics re-implemented
path by path without fork, plus cross-platform itself, is what 474,985 lines
(rubash 385,618 + niubash 89,367; static-audit caliber, 2026-10-25) buys. We
measured brush's v0.4.0 release with a 306-case differential corpus
(byte-compared against GNU bash 5.3.20): 181/306; a source build of its latest
master commit re-measures at 183/306 — the same failure profile — a wrong alias timing model (6 substantive divergences in 20
probes), `printf x y` looping forever and hanging the script, `wait $!`
unimplemented in scripts, non-recursive globstar, and nameref `unset` reporting
"not yet implemented." Such implementations can be neither a bash replacement
nor a reliable login shell (support chain in the campaign evidence files).

**niubash's ledger**: 244/246 byte-identical to GNU on our 246-case differential
suite; measured against GNU's own 83-suite corpus; nvm / bats-core / bash-it /
oh-my-bash run for real, with every divergence filed as a public issue and pinned
as a regression test (as of 2026-10-25: rubash 1,050 issues filed, 1,039 closed;
niubash 292 filed, 285 closed). The bundled command layer, WinuxCmd, carries
176 commands / 1,924 options designed for shell use — against Microsoft
coreutils' own README, which lists no `/dev/null`, no POSIX signals, and
twenty-plus commands (`chmod`, `id`, `tty`, …) dropped wholesale.

## Built for AI agents

Models are trained on Bash; on a Windows machine their commands deserve
a shell that executes them the way they were written.

- **Agents inherit niu as their shell.** Anything launched from a niu
  session gets it through `SHELL`, and
  [Coding agents on Windows](docs/src/agents.md) documents, agent by
  agent, which shell each one runs and how to point the rest at niu.
- **Deterministic non-interactive behavior.** `niu -c` and script runs
  are quiet and stable: no banners, stable stdout/stderr, exact
  exit-code propagation — the properties an agent's tool loop depends
  on.
- **No path roulette.** There is no `MSYS_NO_PATHCONV` to remember and
  no heuristic rewriting arguments behind the child's back — `/flags`
  stay `/flags`, and path-shaped operands are translated by one
  documented contract ([windows-path-contract.md](docs/src/windows-path-contract.md)).
- **An agent skill ships with every release**: the
  `niubash-skill-v*.zip` asset on the
  [Releases](https://github.com/unixwin/niubash/releases) page packages
  the shell's documented behavior for agent consumption, and the engine
  repo carries its own disclosure in `SKILL.md` (including the platform
  identity semantics).

The casualty list that motivates this — real agent failures on
PowerShell and emulation shells, with receipts — is in
[Why niubash](docs/src/why-niubash.md#agent-casualties-with-receipts).

## Platforms

| Platform | Status | Ships as |
|---|---|---|
| Windows x64 / ARM64 | Released; smoke-gated every release | installer `.exe` + portable `.tar.gz`/`.7z`/`.zip`, Unix commands bundled |
| Linux x86_64 / aarch64 (glibc 2.35+) | Released; smoke-gated every release | portable `.tar.gz`, uses system tools |
| macOS aarch64 / x86_64 | Released; smoke-gated every release | portable `.tar.gz`, uses system tools |
| Android aarch64 / armv7 | Shipped since v1.4.3; runs on real devices (Termux), release smoke gate not yet wired in CI | NDK-linked `.zip` release asset |
| OpenHarmony (`aarch64-unknown-linux-ohos`) | CI check only, no release | kept compiling by CI, never run on a device |

"Smoke-gated" means the artifact is built, then run on its own OS against
four checks (`--version`, `-c` execution, a pipeline through native
coreutils, the offline plugin stack) before upload — the gate lives in
`release.yml` ("no smoke, no artifact": Linux legs run it natively on
`ubuntu-22.04`/`ubuntu-22.04-arm`, the macOS x86_64 leg under Rosetta 2),
and the release as a whole additionally blocks on the golden user journey
and the per-asset timing budget gates. OpenHarmony has not passed that
gate (compile-checked only, never run on a device). Android ships release
`.zip` assets since v1.4.3 and runs on real devices (Termux aarch64/armv7,
daily-driven with on-device benchmark baselines); what it lacks is the
automated per-release smoke leg in CI, which is wired for Windows/Linux/
macOS only. The
per-platform contract and the full status matrix live in
[platform-support.md](docs/src/platform-support.md).

## Quick answers

- **Does niu require WSL?** No. niu is a native build on every platform it
  ships for: on Windows there is no Linux VM, no emulation layer, no
  `/mnt/c`.
- **What platforms does niu run on?** Released builds cover Windows (x64,
  ARM64), Linux (x86_64, aarch64; glibc 2.35+), and macOS (aarch64,
  x86_64). OpenHarmony (aarch64) builds in CI and has no release yet;
  Android (aarch64, armv7) ships release `.zip` assets since v1.4.3 and
  runs on real devices (Termux). Android additionally carries a measured
  performance baseline (see [Performance](#performance)).
- **Does niu run real Bash scripts?** Yes. The rubash engine is gated on
  GNU Bash's own upstream test suites; the record lives in the
  [compatibility matrix](docs/src/rubash-bash-compat-matrix.md).
- **Beyond Git Bash compatibility.** Console device redirects work where
  even Git Bash fails: `echo >/dev/tty`, `exec 3<>/dev/tty`, and
  `test -e /dev/tty` all behave like GNU — the engine maps `/dev/tty` to
  the console and splits `CON` into its `CONIN$`/`CONOUT$` halves instead
  of hitting Windows' device-open restrictions (`FILE_APPEND_DATA` and
  read-write opens are rejected on `CON`). This is what keeps `git`/`ssh`
  askpass prompts working. (Engine trail:
  [rubash#1312](https://github.com/unixwin/rubash/issues/1312),
  [rubash#1314](https://github.com/unixwin/rubash/issues/1314),
  [niubash#519](https://github.com/unixwin/niubash/issues/519).)
- **Can I install niu on Android or HarmonyOS today?** Android yes:
  release `.zip` assets ship since v1.4.3 and run on real devices
  (Termux aarch64/armv7). HarmonyOS not yet: it compiles in CI but has
  no installable artifact.

## Compatibility & testing

niu's language semantics are the rubash engine's, and they are gated
mechanically, not anecdotally:

- **GNU Bash's own 83-suite upstream corpus: 83/83 pass** (2026-10-12
  full re-run; 62 suites byte-identical stdout on Windows, 20
  environment-attributed, the one nameref residual proven to be GNU's
  own race) — the ledger lives in the
  [rubash README](https://github.com/unixwin/rubash#compatibility-at-a-glance),
  and rubash CI replays the corpus on Linux on every push.
- **An independent 246-case differential suite** against a locally built
  GNU Bash 5.3.20: **244/246 byte-identical** on current master
  (222/246 at the first audit, 2026-10-22).
- **Every fixed issue lands with a regression test** named for the
  issue, asserting the GNU behavior — on both the engine and the host
  side (`tests/compat.rs`, `tests/host_contract.rs`, and the per-capability
  fixtures in the
  [compatibility matrix](docs/src/rubash-bash-compat-matrix.md)).
- **Releases are smoke-gated and budget-gated**: no artifact ships
  unless it runs on its own OS and stays inside the per-asset timing
  budgets (see [Receipts](#receipts)).

## Receipts

Each claim below links to the artifact that produced it: a committed log, a
baseline file, a CI gate.

**6,336 real-world shell assets, harvested and replayed one by one.** The
pipeline pulls shell scripts, frameworks, themes, and dotfiles from GitHub
and sources each one under niu, recording a per-asset verdict: 4,891 OK,
350 slow, 224 that GNU Bash also fails, 101 hangs, 698 that never fetched.
The full log is committed
([eco-harvest.py](scripts/harvest/eco-harvest.py),
[eco-test.py](scripts/harvest/eco-test.py), snapshot
[wt92-local-20261005](scripts/harvest/snapshots/wt92-local-20261005/)).

**Framework assets are sourced and timed.** The
timing baseline loads 437 real oh-my-bash and bash-it assets (166 themes
plus plugins, completions, aliases) and measures three phases per asset
(source, first prompt, re-render) as a median of three runs. That includes
the nvm assets of both frameworks, and real users' dotfiles that load
nvm.sh go through the same harness with per-phase timings on record
([baseline](scripts/perf/baselines/asset-timing-baseline.json),
[snapshot](scripts/harvest/snapshots/wt92-local-20261005/)).

**A release cannot ship slow.** Every asset in
[budgets.toml](scripts/perf/budgets.toml) carries a measured budget
(healthy themes re-render in 11-35 ms; the budget line is 120 ms; 5x over
budget is a hard fail). The
[release pipeline](.github/workflows/release.yml) runs the `perfbudget`
gate and fails the release on a breach; a nightly run watches the trend.

The GNU Bash upstream golden-suite record lives with the engine: the
[rubash README](https://github.com/unixwin/rubash#readme) carries the
ledger (83/83 upstream suites on the 2026-10-12 full re-run), and rubash
CI replays the upstream corpus on Linux on every push (the "Bash upstream
test progress" job, `scripts/run-bash-upstream-tests.sh`).

## Performance

Every number below is pinned to the issue or PR that measured it, and the
tests or gates that keep it honest are named.

**Windows spawn cost — one hidden console per session, not per child.** A
console-less session (direct `niu -c`, detached, CI) used to pay a hidden
console allocation on every external spawn, ~22 ms per child (design and
shape in [niubash#462](https://github.com/unixwin/niubash/issues/462)).
The fix landed engine-side
([rubash#913](https://github.com/unixwin/rubash/pull/913)): a windowless
console is pre-allocated once on Windows 11 24H2+/Server 2025
(`AllocConsoleWithOptions`), or adopted from the first `CREATE_NO_WINDOW`
child (`AttachConsole`) on older builds, so an entire `niu -c 'a; b; c'`
tree pays at most one allocation and later spawns inherit it. 14
regression tests pin zero visible windows and the scaling behavior
(rubash `tests/console_window_gate.rs`).

**`$(tput …)` terminal queries answered in-process.** The inline tput
whitelist arm ([rubash#884](https://github.com/unixwin/rubash/pull/884))
answers admitted TERM×capability shapes in-process, byte-equal to GNU
ncurses (269-row differential matrix, diff = 0; 8 regression tests). On
the ticket probe — 64 tput calls through command substitution — wall time
fell 1165 ms → 47 ms (~25x, zero spawns). The winuxcmd-side twin removed
the ~17 ms per-call i18n overhead in the real binary (~0.4 ms in-process
after the fix; 3208-row replay byte-identical:
[WinuxCmd#1269](https://github.com/unixwin/WinuxCmd/pull/1269)). The other
harvest SLOW face — `read` on EOF, ~4.8 s/call — is still open on
[rubash#800](https://github.com/unixwin/rubash/issues/800).

**Engine hot paths** (the embedded [rubash](https://github.com/unixwin/rubash)
engine; each fix carries its measured delta and GNU differential battery):

- Multi-word literal command fast path: `[ 1 -eq 2 ]` ×20k ≈177→155 ms
  ([rubash#967](https://github.com/unixwin/rubash/pull/967)).
- `[[ ]]` direct numeric leaf + decimal fast path: 50k `[[ 1 -eq 2 ]]`
  1.41x→1.10x vs GNU
  ([rubash#966](https://github.com/unixwin/rubash/pull/966)).
- Whitelist-skip of the procsub metadata scan: 1.56x→1.39x (~11%)
  ([rubash#974](https://github.com/unixwin/rubash/pull/974)).
- Decimal-subscript array write lane: interleaved 8k 3.65x→3.16x; also
  fixes a real double side-effect in `a[$((i++))]=v`
  ([rubash#959](https://github.com/unixwin/rubash/pull/959)).
- Case: head-only `$BASH_COMMAND` render: 20-arm ×10k −10%, plus the
  divergence fix ([rubash#958](https://github.com/unixwin/rubash/pull/958)).

The interpreter triage behind that wave is closed with numbers: scalar
assignment ~1.06x GNU (rubash#609 → #672/#678), arithmetic loops and
parameter expansion via rubash#607/#608 (#665/#671/#679, #666), and the
O(n²) array element write became O(1) amortized (rubash#618/#616:
associative 5000-key writes 445x→9.9x vs GNU, indexed 736x→3.8x; the
ledger lives in rubash's `docs/performance.md`). The export/unset
fast-lane family has landed (#917/#918/#920, reworked for correctness in
#943); [rubash#916](https://github.com/unixwin/rubash/issues/916) stays
open on the residual.

**Android baseline — measured, honest, not yet re-measured.** On a Termux
aarch64 tablet, niubash 1.4.0 vs GNU bash 5.3.20
([niubash#352](https://github.com/unixwin/niubash/issues/352), full
method and matrix in the issue): interpreter-bound workloads 2.3-3.7x
slower (up to 7.3x on `${x^^}` itself), `niu -c true` 21.1 ms vs bash's
8.3 ms (median of 30), peak RSS 2.2x — while fork-bound workloads run up
to 5.2x faster and a real mkdir/write/read-back script 2.1x faster. The
engine-side root causes were fixed and closed (rubash#607-609) and the
wrapper's one-shot startup gap closed too
([niubash#351](https://github.com/unixwin/niubash/issues/351):
`niu -c true` 15 ms vs bash 13 ms after the fix, zero tput spawns).
[niubash#349](https://github.com/unixwin/niubash/issues/349) is the
original ~2.1x arithmetic/function datapoint. No post-fix re-measure on
Android hardware exists yet — #352 stays open for that re-run, and the
Android leg still ships build-verified only.

## Documentation

[Docs site](https://unixwin.github.io/niubash/) ·
[Why niubash](docs/src/why-niubash.md) (the long version, with the agent
casualties) · [Platform support](docs/src/platform-support.md) ·
[Bash compatibility matrix](docs/src/rubash-bash-compat-matrix.md) ·
[Roadmap](docs/src/niubash-roadmap.md)

On the engine side, two items are **in design** (not shipped): the
**1.5.0 embeddable engine API**
([rubash#595](https://github.com/unixwin/rubash/issues/595), RFC draft in
the rubash repo) and a **native script debugger** — `rubash --debug` for
humans and `--dap` for editors, two front ends over one debug session
([rubash#497](https://github.com/unixwin/rubash/issues/497), M0 design in
the rubash repo).

---

If niu saved you a Linux VM, a mangled quote, or an argument PowerShell
ate, [star the repo](https://github.com/unixwin/niubash) and tell a
developer. ★

## License

MIT. See [LICENSE](LICENSE).
