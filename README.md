<p align="center">
  <img src="assets/niubash-banner.svg" alt="niubash — real Bash, native on Windows, Linux, and macOS." />
</p>

# Niubash — native Bash for Windows (Git Bash alternative)

> **Real Bash, native on Windows, Linux, and macOS.** No WSL. No VM. No `/mnt/c`. No cmdlet dialect. One binary: the shell your fingers already know — and the one your AI agent actually speaks.

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

**niubash** is a Bash-compatible shell implemented natively in Rust — a Git Bash alternative. On Windows, the flagship platform, one `niu.exe` bundles everything: the [rubash](https://github.com/unixwin/rubash) language engine, real Unix commands from [WinuxCmd](https://github.com/unixwin/winuxcmd), a git-aware prompt, and a permission-modeled plugin system. Git comes from [niu-git](https://github.com/unixwin/niu-git): the real git-for-windows C source built with MSVC, so the `git` on PATH *is* the git binary — no MSYS runtime, no launcher hop, no bundled perl — and git's shell-script parts (hooks, `git bisect`, `git mergetool`) run on niubash's own `sh`. Agents launched from a niu session inherit it as their shell through `SHELL`; see [Coding agents on Windows](docs/src/agents.md) for which shell each agent runs and how to point the rest at niu. The Linux and macOS builds are portable `niu` binaries that use your system's own tools.

niu is not MSYS2, Cygwin, Git Bash, or WSL — there is no POSIX emulation layer and no path-conversion heuristics. It is a from-scratch replacement for Git Bash: a native Windows binary with no POSIX emulation. The full per-platform contract, including the honest status of every target, lives in [platform-support.md](docs/src/platform-support.md).

<p align="center">
  <img src="assets/demo-gitbash.gif" alt="The same commands under Git Bash and under niubash" width="880" />
</p>

*The same commands, two outcomes — Git Bash's MSYS layer vs niu, side by side (`assets/demo-gitbash.gif`).*

## Quick start

All downloads start at [unixwin.dev](https://unixwin.dev). Latest release: **v1.4.4** (as of 2026-10-30; the [releases page](https://github.com/unixwin/niubash/releases) always carries the newest build).

Windows via winget (installs **1.4.4** today):

```powershell
winget install --id Unixwin.Niubash
```

Windows via installer: grab `niubash-v*-win-*-setup.exe` from [Releases](https://github.com/unixwin/niubash/releases) and run it — no admin rights; PATH and a Windows Terminal profile are set up for you. Prefer portable? Take the `.7z` — it preserves the bundled git/awk payload's hardlinks, so it stays small. The `.tar.gz` works too (`tar -xf niubash-v*-win-x64.tar.gz`, built into Windows 10+). Avoid the `.zip`: it cannot represent hardlinks, so the payload is stored duplicated and the archive is much larger — it stays only for tooling that speaks nothing but zip.

Linux (x86_64, aarch64; glibc 2.35+) and macOS (aarch64, x86_64): take the portable tarball, untar, run:

```sh
tar -xzf niubash-v*-linux-x86_64.tar.gz && niubash-v*-linux-x86_64/niu
```

Android (aarch64, armv7): release `.zip` assets ship since v1.4.3 and run on real devices (Termux) — see [Platforms](#platforms) for the honest per-platform status.

From source (Rust 1.95+):
`git clone https://github.com/unixwin/niubash.git && cd niubash && cargo build --release`

Configuration is ONE file, `~/.niubashrc`, in plain Bash syntax — it is the **only** startup file niu reads (`~/.bashrc` is never sourced; run `niu plugin migrate-rc --diff` to see exactly what is worth carrying over) — start with [Getting started](docs/src/getting-started.md).

## Why not Git Bash / MSYS2 / Cygwin

**Git Bash** calls itself, verbatim, "a BASH emulation used to run Git from the command line" — an emulation built to run Git. The costs are structural. Console device redirects such as `echo >/dev/tty` fail outright with `Invalid argument` in its MSYS layer, where niubash matches GNU. Its bundled bash is on the 5.2 line, so bash 5.3's `${ cmd; }` nofork substitution does not exist. Its prompt runs `__git_ps1` — a git process — on every draw; niu's git segment asks your own git at most once per prompt pass — one bounded `status --porcelain=v2 --branch` query (2 s timeout) — and repaints within the pass reuse that result instead of spawning again (measured steady state inside a repository: 1.0 spawn per pass, p50 26 ms per pass; Windows, same machine). And a real Git for Windows install is ≈440 MB plus a 20 MB simulation DLL, versus niu at ≈16 MB with zero DLLs (project-measured, as of 2026-10-25).

**MSYS2**'s own documentation concedes that arguments resembling Unix paths are automatically rewritten before being passed to native programs — and that the rewriting "isn't perfect," hitting arguments that aren't paths, or path lists that aren't. niubash keeps translation deliberately narrow: quoted arguments are never rewritten, and the only device translation is the exact token `/dev/null` → `nul`; every overreach ever found became a public issue and a pinned regression test.

**Cygwin** states on its own site that its core is a DLL providing POSIX API functionality, and that applications must be rebuilt from source to run in its world. The simulation layer is its official definition. niubash is a native build: no simulation DLL.

## Niubash vs Git Bash

| | **Niubash** | **Git Bash** |
| --- | --- | --- |
| Engine | Bash semantics reimplemented natively in Rust ([rubash](https://github.com/unixwin/rubash)); no emulation layer | GNU Bash ported onto the MSYS2 POSIX emulation runtime |
| Bash language level | **GNU Bash 5.3** semantics — including 5.3-only syntax such as no-fork command substitution `${ cmd; }` ([rubash#195](https://github.com/unixwin/rubash/issues/195)) | GNU Bash **5.2** (the version Git for Windows bundles) |
| Path handling | Windows paths are the native representation; argument translation is a deterministic, documented contract — quoted arguments never rewritten, no guessing | Heuristic path conversion of arguments, with `MSYS_NO_PATHCONV` as the documented opt-out |
| `git` | Real git-for-windows C source built with MSVC ([niu-git](https://github.com/unixwin/niu-git)) — the binary itself; no MSYS runtime, no launcher hop | `git` launched through the MSYS runtime |
| Prompt git segment | Your own git, at most once per prompt pass (bounded porcelain query); repaints reuse the result | Runs `__git_ps1` — spawns git on every prompt |
| Subshells | In-process — `( )` and `$( )` create no processes | `fork()` emulation |
| Console devices | `/dev/tty` redirects behave like GNU — the engine splits `CON` into `CONIN$`/`CONOUT$` (see [Quick answers](#quick-answers)) | Console device redirects fail outright in the MSYS layer |
| Install footprint | ≈16 MB, zero DLLs | ≈440 MB + 20 MB `msys-2.0.dll` (project-measured, real installs, as of 2026-10-25) |

The long version — including same-command-line side-by-sides of what actually reaches a native process — is [Why niubash](docs/src/why-niubash.md).

## The command layer: built for a shell

The bundled commands are **WinuxCmd**: 176 commands / 1,924 options, native Windows builds designed for shell use and evolved in lockstep with this shell — path forms, argument passing, and shell interplay are designed against shell semantics. The obvious alternative, Microsoft's own coreutils package, is an upstream uutils build *packaged* for PowerShell/CMD (its repo describes itself as "Installer & Packaging", and the package is in preview). Its README states its own limits: **no `/dev/null`**, **no POSIX signals**, `kill` and `timeout` not shipped, and twenty-plus commands — `chmod`, `chown`, `id`, `tty`, `mkfifo`, `nohup`, `stty`, … — dropped wholesale as "POSIX-only concepts". A command layer for a shell cannot start by discarding the concepts a shell lives on.

## And the "lightweight replacement" shells

The new Rust shells sell lightness. Of the peers, hush is a Lua-based language that never claimed to replace bash, and Oils/OSH calls itself an "upgrade path from bash", not a drop-in. **brush** claims the drop-in: "Run it as your everyday shell", "Production-ready for most use cases". Measured with a 306-case differential corpus (byte-compared against GNU Bash 5.3.20): its shipped v0.4.0 scores 181/306, and a source build of its latest master commit re-measures at 183/306 — the same failure profile — against rubash's 304/306 on the same ruler. The misses are foundation mechanisms, not edge cases: an alias timing model reversed from GNU's (6 substantive divergences in 20 probes), `printf x y` looping forever and hanging the script in the release (fixed on master), `wait $!` unimplemented in scripts, globstar that does not recurse while listed as "Fully Supported", and nameref `unset` reporting "not yet implemented" — which a real oh-my-bash load trips over. Its Windows support is self-described "Preview", and v0.4.0 ships no Windows binary at all.

Lightweight is no excuse for missing limbs. Complete bash semantics re-implemented path by path without fork, plus cross-platform itself, is what 474,985 lines (rubash 385,618 + niubash 89,367; static-audit caliber, 2026-10-25) buys. An implementation that misses these mechanisms can be neither a bash replacement nor a reliable login shell.

## Built for AI agents

Models are trained on Bash; on a Windows machine their commands deserve a shell that executes them the way they were written.

- **Agents inherit niu as their shell.** Anything launched from a niu session gets it through `SHELL`, and [Coding agents on Windows](docs/src/agents.md) documents, agent by agent, which shell each one runs and how to point the rest at niu.
- **Real agent usage came first.** OpenCode users run `niu.exe` as their agent's shell and drove the environment-driven init work ([#82](https://github.com/unixwin/niubash/issues/82)); a Hermes integration failure — literal quotes leaking into external-command arguments — was filed and fixed ([#124](https://github.com/unixwin/niubash/issues/124)); and after three false-baseline incidents in a single day, `--version` now carries build metadata, so an agent can always tell exactly which build it is talking to ([rubash#488](https://github.com/unixwin/rubash/issues/488)).
- **Deterministic non-interactive behavior.** `niu -c` and script runs are quiet and stable: no banners, stable stdout/stderr, exact exit-code propagation — the properties an agent's tool loop depends on.
- **No path roulette.** There is no `MSYS_NO_PATHCONV` to remember and no heuristic rewriting arguments behind the child's back — `/flags` stay `/flags`, and path-shaped operands are translated by one documented contract ([windows-path-contract.md](docs/src/windows-path-contract.md)).
- **An agent skill ships with every release**: the `niubash-skill-v*.zip` asset on the [Releases](https://github.com/unixwin/niubash/releases) page packages the shell's documented behavior for agent consumption ([#188](https://github.com/unixwin/niubash/issues/188)), and the engine repo carries its own disclosure in `SKILL.md` (including the platform identity semantics).

The casualty list that motivates this — real agent failures on PowerShell and emulation shells, with receipts — is in [Why niubash](docs/src/why-niubash.md#agent-casualties-with-receipts).

## Compatibility & testing

niu's language semantics are the rubash engine's, and they are gated mechanically, not anecdotally:

- **GNU's own 83-suite upstream corpus is the yardstick.** The engine's ledger (2026-10-12 full re-run): 62 suites byte-identical stdout on Windows, 20 more differing only on environment-attributed lines, and the one nameref residual proven to be GNU's own reap-timing race — the full ledger lives in the [rubash README](https://github.com/unixwin/rubash#compatibility-at-a-glance), and rubash CI replays the corpus on Linux on every push.
- **An independent 246-case differential suite** against a locally built GNU Bash 5.3.20: **244/246 byte-identical** on current master (222/246 at the first audit, 2026-10-22).
- **A 306-case differential corpus**: rubash scores **304/306** (see the peer measurements above).
- **Every fixed issue lands with a regression test** named for the issue, asserting the GNU behavior — on both the engine and the host side (`tests/compat.rs`, `tests/host_contract.rs`, and the per-capability fixtures in the [compatibility matrix](docs/src/rubash-bash-compat-matrix.md)).
- **Releases are smoke-gated and budget-gated**: no artifact ships unless it runs on its own OS and stays inside the per-asset timing budgets (see [Receipts](#receipts)).

As of 2026-10-30 the trackers carry the receipts: rubash 1,440 issues filed / 1,408 closed; niubash 373 filed / 354 closed.

## Receipts

Each claim below links to the artifact that produced it: a committed log, a baseline file, a CI gate.

**6,336 real-world shell assets, harvested and replayed one by one.** The pipeline pulls shell scripts, frameworks, themes, and dotfiles from GitHub and sources each one under niu, recording a per-asset verdict: 4,891 OK, 350 slow, 224 that GNU Bash also fails, 101 hangs, 698 that never fetched. The full log is committed ([eco-harvest.py](scripts/harvest/eco-harvest.py), [eco-test.py](scripts/harvest/eco-test.py), snapshot [wt92-local-20261005](scripts/harvest/snapshots/wt92-local-20261005/)).

**Framework assets are sourced and timed.** The timing baseline loads 437 real oh-my-bash and bash-it assets (166 themes plus plugins, completions, aliases) and measures three phases per asset (source, first prompt, re-render) as a median of three runs. That includes the nvm assets of both frameworks, and real users' dotfiles that load nvm.sh go through the same harness with per-phase timings on record ([baseline](scripts/perf/baselines/asset-timing-baseline.json), [snapshot](scripts/harvest/snapshots/wt92-local-20261005/)).

**A release cannot ship slow.** Every asset in [budgets.json](scripts/perf/budgets.json) carries a measured budget (healthy themes re-render in 11–35 ms and the budget line is 120 ms on the Windows reference baseline; 5× over budget is a hard fail). The [release pipeline](.github/workflows/release.yml) runs the `perfbudget` gate and fails the release on a breach; a nightly run watches the trend.

## Performance

Every number below comes from one published protocol — the 2026-10-25 baseline (PERF-BASELINE-2026-10-25, in the public [shellbench](https://github.com/unixwin/shellbench) repo): release builds, Linux, against a self-built GNU Bash 5.3.20.

**The honest headline.** The embedded rubash engine pays a roughly uniform **~3× floor tax** on interpreter-bound micro-work (arithmetic, function calls, assignment, read loops at 2.8–3.2× per-op). Beating GNU across the full table is the stated goal and the flagship release's hard gate; the current status is published, not hidden. Where the cost is process creation, the model inverts: pipeline stages and command substitution run in-process, at **0.05×** (pipe) and **0.11×** (command-substitution loop) of GNU's time.

**Startup — the one quotable set.** `-c 'true'`, p50 of 50 interleaved samples, release builds, Linux: **niu 3.96 ms, rubash 3.43 ms, GNU Bash 5.3.20 2.59 ms**. No Windows startup figures are quoted until re-measured on real Windows hardware under the same protocol.

**Windows process economics.** A console-less session used to pay a hidden-console allocation on *every* external spawn. The fix is one hidden console per session — pre-allocated windowless on Windows 11 24H2+/Server 2025, adopted from the first `CREATE_NO_WINDOW` child elsewhere — pinned by 14 regression tests (design in [#462](https://github.com/unixwin/niubash/issues/462), landed engine-side in [rubash#913](https://github.com/unixwin/rubash/pull/913)). And `$(tput …)` terminal queries are answered in-process for admitted TERM×capability shapes, byte-equal to GNU ncurses on a 269-row differential matrix, with zero spawns ([rubash#884](https://github.com/unixwin/rubash/pull/884)).

**Loop scripts — one Windows machine, measured.** Same-machine Windows measurement (niubash 1.4.4): a 50-iteration loop over a `sed|grep|awk` pipeline — 150 external command launches in total — takes 1.502s in niubash and 27.256s in Git Bash, roughly 18×. The gap is not in the commands; it is Windows process-startup cost: no fork, no POSIX emulation layer, a startup surface more than an order of magnitude smaller. Original record: [niubash #567](https://github.com/unixwin/niubash/issues/567).

**Android, stated plainly.** An on-device Android baseline exists ([#352](https://github.com/unixwin/niubash/issues/352)); its engine-side root causes were found and closed (rubash#607–#609), fork-bound workloads already ran faster there, and a post-fix on-device re-measure is still owed — we say so instead of quoting the stale pre-fix numbers.

## Platforms

| Platform | Status | Ships as |
|---|---|---|
| Windows x64 / ARM64 | Released; smoke-gated every release | installer `.exe` + portable `.tar.gz`/`.7z`/`.zip`, Unix commands bundled |
| Linux x86_64 / aarch64 (glibc 2.35+) | Released; smoke-gated every release | portable `.tar.gz`, uses system tools |
| macOS aarch64 / x86_64 | Released; smoke-gated every release | portable `.tar.gz`, uses system tools |
| Android aarch64 / armv7 | Shipped since v1.4.3; runs on real devices (Termux), release smoke gate not yet wired in CI | NDK-linked `.zip` release asset |
| OpenHarmony (`aarch64-unknown-linux-ohos`) | CI check only, no release | kept compiling by CI, never run on a device |

"Smoke-gated" means the artifact is built, then run on its own OS against four checks (`--version`, `-c` execution, a pipeline through native coreutils, the offline plugin stack) before upload — the gate lives in `release.yml` ("no smoke, no artifact": Linux legs run it natively on `ubuntu-22.04`/`ubuntu-22.04-arm`, the macOS x86_64 leg under Rosetta 2), and the release as a whole additionally blocks on the golden user journey and the per-asset timing budget gates. OpenHarmony has not passed that gate (compile-checked only, never run on a device). Android ships release `.zip` assets since v1.4.3 and runs on real devices (Termux aarch64/armv7, daily-driven with on-device benchmark baselines); what it lacks is the automated per-release smoke leg in CI, which is wired for Windows/Linux/macOS only. The per-platform contract and the full status matrix live in [platform-support.md](docs/src/platform-support.md).

## Quick answers

- **Does niu require WSL?** No. niu is a native build on every platform it ships for: on Windows there is no Linux VM, no emulation layer, no `/mnt/c`.
- **What platforms does niu run on?** Released builds cover Windows (x64, ARM64), Linux (x86_64, aarch64; glibc 2.35+), macOS (aarch64, x86_64), and Android (aarch64, armv7; `.zip` assets since v1.4.3, running on real devices). OpenHarmony (aarch64) builds in CI and has no release yet.
- **Does niu run real Bash scripts?** Yes. The rubash engine is gated on GNU Bash's own upstream test suites; the record lives in the [compatibility matrix](docs/src/rubash-bash-compat-matrix.md).
- **Beyond Git Bash compatibility.** Console device redirects work where even Git Bash fails: `echo >/dev/tty`, `exec 3<>/dev/tty`, and `test -e /dev/tty` all behave like GNU — the engine maps `/dev/tty` to the console and splits `CON` into its `CONIN$`/`CONOUT$` halves instead of hitting Windows' device-open restrictions (`FILE_APPEND_DATA` and read-write opens are rejected on `CON`). This is what keeps `git`/`ssh` askpass prompts working. (Engine trail: [rubash#1312](https://github.com/unixwin/rubash/issues/1312), [rubash#1314](https://github.com/unixwin/rubash/issues/1314), [niubash#519](https://github.com/unixwin/niubash/issues/519).)
- **Can I install niu on Android or HarmonyOS today?** Android yes: release `.zip` assets ship since v1.4.3 and run on real devices (Termux aarch64/armv7). HarmonyOS not yet: it compiles in CI but has no installable artifact.

## Known limitations, stated plainly

- **The installer is not code-signed yet.** Windows SmartScreen will warn on first run ([#511](https://github.com/unixwin/niubash/issues/511) — a SignPath signing route is set up; its production release-signing approval is still pending). Verify downloads against the published SHA-256 sums in the meantime.
- **Interpreter speed is a measured, published gap** (see [Performance](#performance)) — not a hidden one. Process-bound workloads already lead; interpreter-bound micro-work is the flagship release's hard gate.

## Documentation

[Docs site](https://unixwin.github.io/niubash/) · [Why niubash](docs/src/why-niubash.md) (the long version, with the agent casualties) · [Platform support](docs/src/platform-support.md) · [Bash compatibility matrix](docs/src/rubash-bash-compat-matrix.md) · [Roadmap](docs/src/niubash-roadmap.md)

On the engine side, 1.5.0 lands the **embeddable engine API** ([rubash#595](https://github.com/unixwin/rubash/issues/595)) and a **native script debugger** — `rubash --debug` for humans and `--dap` for editors, two front ends over one debug session ([rubash#497](https://github.com/unixwin/rubash/issues/497)).

---

If niu saved you a Linux VM, a mangled quote, or an argument PowerShell ate, [star the repo](https://github.com/unixwin/niubash) and tell a developer. ★

## License

MIT. See [LICENSE](LICENSE).
