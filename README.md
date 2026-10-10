<p align="center">
  <img src="assets/niubash-banner.svg" alt="niubash — real Bash, native on Windows, Linux, and macOS." />
</p>

> **Real Bash, native on Windows, Linux, and macOS.** "Windows-only" is the
> old story: five platforms build from this tree now, and Windows remains
> the flagship — no WSL. No VM. No `/mnt/c`. No cmdlet dialect. One binary:
> the shell your fingers already know, and the one your AI agent actually
> speaks.

<div align="center">

[English](README.md) · [中文](README-zh.md)

[![niubash CI](https://github.com/unixwin/niubash/actions/workflows/ci.yml/badge.svg)](https://github.com/unixwin/niubash/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/unixwin/niubash)](https://github.com/unixwin/niubash/releases)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-blue)](https://github.com/unixwin/niubash)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange)](https://github.com/unixwin/niubash)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Stars](https://img.shields.io/github/stars/unixwin/niubash)](https://github.com/unixwin/niubash/stargazers)

</div>

**niubash** is a Bash-compatible shell implemented natively in Rust. On
Windows, the flagship platform, one `niu.exe` bundles everything: the
[rubash](https://github.com/unixwin/rubash) language engine, real Unix
commands from [winuxcmd](https://github.com/unixwin/winuxcmd), a git-aware
prompt, and a permission-modeled plugin system. The Linux and macOS builds
are portable `niu` binaries that use your system's own tools. niu is not
MSYS2, Cygwin, Git Bash, or WSL — there is no POSIX emulation layer and no
path-translation machinery. The full per-platform contract, including the
honest status of every target, lives in
[platform-support.md](docs/src/platform-support.md).

## Platforms

| Platform | Status | Ships as |
|---|---|---|
| Windows x64 / ARM64 | Released; smoke-gated every release | installer `.exe` + `.zip`, Unix commands bundled |
| Linux x86_64 / aarch64 (glibc 2.35+) | Released; smoke-gated every release | portable `.tar.gz`, uses system tools |
| macOS aarch64 / x86_64 | Released; smoke-gated every release | portable `.tar.gz`, uses system tools |
| Android aarch64 / armv7 | CI build only, no release | NDK-linked zip on workflow runs, not smoke-tested |
| OpenHarmony (`aarch64-unknown-linux-ohos`) | CI check only, no release | kept compiling by CI, never run on a device |

"Smoke-gated" means the artifact is built, then run on its own OS against
four checks (`--version`, `-c` execution, a pipeline through native
coreutils, the offline plugin stack) before upload. Android and OpenHarmony
have not passed that gate: Android artifacts are build-verified only,
OpenHarmony is compile-checked only, and neither claims device readiness.
The per-platform contract and the full status matrix live in
[platform-support.md](docs/src/platform-support.md).

Quick answers:

- **Does niu require WSL?** No. niu is a native build on every platform it
  ships for: on Windows there is no Linux VM, no emulation layer, no
  `/mnt/c`.
- **What platforms does niu run on?** Released builds cover Windows (x64,
  ARM64), Linux (x86_64, aarch64; glibc 2.35+), and macOS (aarch64,
  x86_64). Android (aarch64, armv7) and OpenHarmony (aarch64) build in CI
  and have no release yet.
- **Does niu run real Bash scripts?** Yes. The rubash engine is gated on
  GNU Bash's own upstream test suites; the record lives in the
  [compatibility matrix](docs/src/rubash-bash-compat-matrix.md).
- **Can I install niu on Android or HarmonyOS today?** Not yet. Both
  targets compile in CI, neither has passed a runtime smoke test, so there
  is nothing to install.

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

**Framework assets are sourced and timed, not just "supported."** The
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

The GNU Bash upstream golden-suite record belongs here too; it returns
once the fix currently in flight lands.

## Quick start

Windows: grab `niubash-v*-win-*-setup.exe` from
[Releases](https://github.com/unixwin/niubash/releases) and run it — no
admin rights; PATH and a Windows Terminal profile are set up for you.
Prefer portable? Take the `.zip`.

Linux (x86_64, aarch64; glibc 2.35+) and macOS (aarch64, x86_64): take the
portable tarball, untar, run:

```sh
tar -xzf niubash-v*-linux-x86_64.tar.gz && niubash-v*-linux-x86_64/niu
```

From source (Rust 1.70+):
`git clone https://github.com/unixwin/niubash.git && cd niubash && cargo build --release`

Configuration is one file, `~/.niubashrc`, in plain Bash syntax — start
with [Getting started](docs/src/getting-started.md).

## Documentation

[Docs site](https://unixwin.github.io/niubash/) ·
[Why niubash](docs/src/why-niubash.md) (the long version, with the agent
casualties) · [Platform support](docs/src/platform-support.md) ·
[Bash compatibility matrix](docs/src/rubash-bash-compat-matrix.md) ·
[Roadmap](docs/src/niubash-roadmap.md)

---

If niu saved you a Linux VM, a mangled quote, or an argument PowerShell
ate, [star the repo](https://github.com/unixwin/niubash) and tell a
developer. ★

## License

MIT. See [LICENSE](LICENSE).
