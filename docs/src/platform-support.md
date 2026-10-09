---
tags: [niubash, platforms, unix, harmonyos]
created: 2026-10-09
status: active
---

# Platform Support

Which platforms Niubash runs on, what "runs" means for each, and the parts
of the surface that are deliberately different per platform. This page is
the contract; if a platform behaves differently from a claim here, that is
a bug.

## Status matrix

| Platform | Build | Smoke-gated | Released | Notes |
| --- | --- | --- | --- | --- |
| Windows x64 | ✅ `windows-2025` | ✅ release gate | ✅ `.zip` + `.exe` | primary target |
| Windows arm64 | ✅ `windows-2025` | ✅ release gate | ✅ | |
| Linux x86_64 (glibc) | ✅ `ubuntu-22.04` | ✅ release gate | ✅ `.tar.gz` | glibc ≥ 2.35 |
| Linux aarch64 (glibc) | ✅ `ubuntu-22.04-arm` | ✅ release gate | ✅ | native arm64 runner |
| macOS aarch64 | ✅ `macos-latest` | ✅ release gate | ✅ | |
| macOS x86_64 | ✅ `macos-latest` | ✅ (Rosetta 2) | ✅ | Apple cross-compile |
| Linux x86_64 (musl) | ✅ `ubuntu-latest` | ✅ `cross-target-smoke` | ⬜ | static; see below |
| HarmonyOS PC (`aarch64-linux-ohos`) | ✅ `ubuntu-latest` | ⬜ | ⬜ | see below |

"Smoke-gated" means the artifact is built, then run on its own OS, against
the four checks in [release.md](../release.md) before it is published —
`--version` banner, `-c` execution, a pipeline through native coreutils,
and the offline plugin stack. No smoke, no artifact.

### musl

Compiled by `cross-target-check` and **smoke-tested** by
`cross-target-smoke`, which builds the static musl binary and runs the same
four checks natively (a static musl x86_64 binary runs on the glibc
runner). The job asserts `ldd` reports `statically linked` first, so a
silent re-link against the runner's glibc cannot pass as a musl build. No
`musl-tools` package is needed: rustc links against the toolchain's own
self-contained musl crt/libc through the host `cc`.

No musl **release** artifact ships yet. That is a delivery decision, not a
coverage gap — flip it on by adding `x86_64-unknown-linux-musl` to the
`build-linux` matrix in `release.yml`.

## HarmonyOS

HarmonyOS is a "de-GNU'd Linux distribution": musl libc, `mksh` as the
default shell (not bash), and `toybox` built in (not GNU coreutils). That
gap — a bash-compatible runtime plus a GNU toolset on a non-GNU userland —
is exactly what Niubash provides, which is why the port is cheap.

The Rust target is `aarch64-unknown-linux-ohos`. Two facts make it mostly
free:

- `rustc --print cfg --target aarch64-unknown-linux-ohos` reports
  `target_os = "linux"` and `target_family = "unix"`. So Niubash's existing
  unix leg and rubash's existing Linux leg apply nearly unchanged, and
  `fd/mod.rs`'s `#[cfg(unix)]` arm is taken as-is.
- The target is **Tier 2 with host tools** in Rust, so a check-only build
  needs no external OpenHarmony SDK or clang wrapper.

The one compile blocker found so far was rubash's `ulimit`, which typed its
resource selector as `libc::__rlimit_resource_t` — a typedef only glibc
defines. musl and the OpenHarmony libc take a plain `c_int`. Fixed by
aliasing on `target_env` (rubash#449). With that, the whole Niubash
workspace compiles for `aarch64-unknown-linux-ohos`.

`cross-target-check` (in `ci.yml`) keeps it compiling. It is check-only
because there is no HarmonyOS runtime available on GitHub runners, so the
gate is "it compiles", not "it runs" — the ohos column of the status matrix
stays ⬜ until a real device runs the smoke gate.

### `uname` on HarmonyOS

`uname`/`arch` do **not** hard-code a platform name. On unix,
`crates`-side `identity.rs` reads `uname(2)` and prints `utsname` verbatim,
the same way coreutils `uname(1)` does. So the reported values are whatever
the running kernel says:

| Field | `uname` flag | OpenHarmony (Linux kernel) | HarmonyOS (Huawei kernel) |
| --- | --- | --- | --- |
| sysname | `-s` | `Linux` | the Huawei kernel's own name |
| nodename | `-n` | the host name | the host name |
| release | `-r` | the kernel release | the kernel release |
| machine | `-m` | `aarch64` | `aarch64` |

There is nothing to special-case here, and that is deliberate: the honest
source is the kernel, not a compile-time constant. Scripts that branch on
`uname -s` will see `Linux` on OpenHarmony and the Huawei name on HarmonyOS
— a real divergence, but the correct one. Do not add a HarmonyOS-specific
`sysname()` branch unless a concrete consumer needs it.

`$BASH_VERSION` reports Niubash's Bash-compatibility level (e.g.
`5.3.0(1)-release`) on every platform, HarmonyOS included.

### HarmonyOS PC vs HarmonyOS phone

These are two different products, not one port.

| | HarmonyOS PC | HarmonyOS phone |
| --- | --- | --- |
| Userland | full | full |
| `fork`/`exec` of new processes | unrestricted | **restricted** — ordinary apps cannot create processes directly; the app must declare each one in its config and pass per-process review at listing time |
| Packaging | tarball, same shape as the Linux leg | HAP bundle |
| Distribution | direct download | app-store listing + review |
| Status | the target this page describes | a new form factor; not in scope yet |

The fork restriction is the load-bearing difference. Huawei's own FAQ
(`faqs-ability-112`) states that on phones the system restricts and manages
`fork` for ordinary apps, and community reports add that PC does not
restrict it. Niubash is built on a fork+exec model on unix (see
`rubash`'s `fd/unix.rs`: `Command::new` + `Stdio::from_raw_fd` +
`pre_exec`), so the PC form factor is a plain port while the phone form
factor needs the process-declaration path to be sustainable before it is
worth building.

**Do not treat "HarmonyOS support" as one item.** A PC tarball is close; a
phone HAP is a separate project with an approval gate Niubash does not
control.

## What is Windows-only

By compile-time cfg, the following never appear on non-Windows paths and
must not be weakened (no `wpm` strings off Windows):

- WinuxCmd command links and the `wpm` package-manager surface.
- The ConPTY interactive journey harness.
- Windows path translation (`/mnt/X` drive argv rewriting) and the Windows
  signal bridge.

Unix tarballs carry only `niu` + `README.md` + `LICENSE`; native system
tools are used as-is rather than bundled.
