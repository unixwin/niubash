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
| Android (`aarch64-linux-android`, `armv7-linux-androideabi`) | ✅ release `build-android` | ⬜ | ⬜ | incubating: NDK-linked zip, no runtime smoke; see below |
| HarmonyOS PC (`aarch64-linux-ohos`) | ✅ `ubuntu-latest` check | ⬜ | ⬜ | see below |

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

### Android

The `build-android` legs in `release.yml` build `aarch64-linux-android` and
`armv7-linux-androideabi` with a real bionic link through the NDK
(`nttld/setup-ndk`, API 24 clang drivers). The bionic cfg surface was fixed
upstream in rubash#456 (`AT_EACCESS`, `confstr`, `sa_restorer`, 32-bit
`off_t`/`S_IFMT` widths). The legs are incubating: `continue-on-error`, not
in the release `needs`, artifacts go to the workflow run only. The runner
cannot execute bionic binaries, so the four-check smoke gate has not run
for these — a zip from this leg is build-verified, not smoke-verified.

## HarmonyOS

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

### What HarmonyOS PC actually ships

An earlier revision of this page described HarmonyOS as "a de-GNU'd Linux
distribution — musl libc, `mksh` as the default shell, `toybox` built in".
**That is wrong for HarmonyOS PC.** It came from OpenHarmony's dev-board
userland and was applied to the whole platform. The two are different
products and the difference changes how much Niubash has to supply.

**HarmonyOS PC is not stripped down.** A measured install (HarmonyOS
1.12.0, aarch64, kernel `HongMeng Kernel 1.12.0`) runs **bash** as the
shell (`/data/service/hnp/bin/bash`) and carries **454 commands** under
`/data/service/hnp/bin` — about **93%** of a standard Linux command
surface, and **100%** of text processing:

```
grep sed awk cut sort uniq diff diff3 comm cmp head tail wc fold column
```

Also present: `clang`/`gcc` (multi-arch: aarch64 / armv7 / loongarch64 /
x86_64), `cmake`, `make`, `ninja`, `autoconf`, `bison`, `flex`, `gdb`,
`lldb`, `git` (+ `git-lfs`, `git-svn`), `python3.12`, `node`, full JDK,
`ruby`, `vim`/`nvim`/`nano`. What is genuinely missing is **system
management**, not text processing:

| Missing / restricted | Note |
| --- | --- |
| `apt` / `yum` / `dnf` | no classic package manager; use `hnpcli` or preinstalled |
| `systemctl` | no systemd; HarmonyOS has its own service management |
| `iptables` / `nftables` | restricted |
| `docker` | not preinstalled |
| `snap` / `flatpak` | unsupported |
| `ifconfig` / `netstat` | present but partly restricted (`ip`, `ss` preferred) |

### The HarmonyOS bash is stock GNU bash

It is worth pinning down *what* that bash is, because "it has bash" could
mean a reimplementation. It is not. It is upstream GNU bash, cross-compiled
against musl:

- `bash --version` on a HarmonyOS PC prints
  `GNU bash, version 5.1.16(1)-release` — the GPL banner is GNU's own.
- The public HNP build (Termony, `build-hnp/bash/Makefile`) pulls
  `$(GNU_MIRROR)/gnu/bash/bash-5.3.tar.gz` and configures it with
  `--host aarch64-unknown-linux-musl --without-bash-malloc --disable-nls`.
  `--without-bash-malloc` matters: it drops bash's bundled allocator so the
  musl allocator is used instead.
- The build emits `libsh.a`, `libhistory.a` and the `loadables`
  (`recho`, `printf`, `basename`, …) — artifacts unique to the GNU bash
  source tree.

**No Huawei-specific shell patches.** It is an ordinary musl-linked GNU
bash, which is a useful data point for us: the userland under HarmonyOS is
a musl + Linux-ABI stack, not a bespoke one. That is *why* Niubash's
`target_os = "linux"` cfgs all hit and why the port was two lines.

Note the version split when quoting numbers: the measured machine ran
**5.1.16**, while the public build recipe fetches **5.3**. These are
different devices at different times — do not present either as "the"
HarmonyOS bash version.

### Two userland caveats the existing ports hit

Both are worth knowing before a first device run, because neither is a
documentation problem — they are things that actually broke people.

**1. A third-party port still needed to *bundle* a shell toolset.**
Termony (a "Termux for HarmonyOS PC") ships 42 packages, and its list is
`aria2 bash binutils *busybox* ... gcc gdb gettext git ...` — **bash yes,
coreutils no, busybox instead**. So even with 454 commands present system-
wide, an independent porter concluded the stock surface was not enough and
vendored `busybox` to fill it. That is not proof the 93% number is wrong;
it is evidence that "the tools exist" and "the tools are dependable" are
different claims — the same distinction the argument below turns on.

**2. Prefix mismatch inside the built-in Terminal.**
Termony's README warns:

> you can use these utilities in the builtin Terminal app under
> `/data/service/hnp`: **Although some paths might get wrong due to prefix
> set to `/data/app/base.org/base_1.0`** … You can override them like:
> `LD_LIBRARY_PATH=/data/service/hnp/base.org/base_1.0/lib
> TERMINFO=/data/service/hnp/base.org/base_1.0/share/terminfo fish`

This does not contradict the PATH finding below — `PATH` resolution and
runtime prefix resolution are separate mechanisms. It does mean that a
first device run should check more than "is it on PATH": also confirm that
a resolved binary can actually find its own `lib` and data dirs. On
HarmonyOS 6.0+ the README adds that **`sudo` is available and system
environment variables are editable**, which is how the overrides are meant
to be persisted.

### The OpenHarmony dev boards are the limited case

There the userland is `toybox`, and Huawei's own documentation says it is
trimmed per device:

> In the current version, different devices support different toybox
> commands. You can run the `toybox` command to obtain the full list.

Community reports go further: `toybox: Unknown command diff` **even though
`diff.c` exists in `third_party/toybox/toys/pending` and is listed in
`BUILD.gn`**. The workaround people use is *"选择使用 busybox 代替 toybox"*.
The third-party `pkgsrc` bootstrap lists `gawk` and `grep` as **required
dependencies**, which implies they are absent by default.

The honest split:

| | HarmonyOS PC | OpenHarmony dev boards |
| --- | --- | --- |
| Shell | **bash** | `mksh` |
| Command set | **454 commands** at `/data/service/hnp/bin` (~93%) | `toybox`, per-device trimmed |
| Text processing | **fully covered** | gaps; `awk`/`grep` absent, `diff` may be "unknown" though the source exists |
| What Niubash adds | a **consistent** bash + GNU-toolset baseline across OS versions, plus the Bash semantics | the missing tools, not just the semantics |

State the argument plainly, because it changed: on HarmonyOS PC the value
is **not** "the commands are missing". It is that the toolset is not
guaranteed uniform across device revisions, and that a bash-compatible
runtime is still what you want when you do not control the host. Both are
real, but weaker than "there is nothing here", and this page should not
overstate them.

### Packaging: HarmonyOS PC requires HNP

HarmonyOS PC does **not** accept the tarball layout the Linux legs ship.
The constraint is documented for native ports and it is absolute:

> 在鸿蒙PC上，由于系统安全规格限制等原因，**暂不支持通过"解压 + 配 PATH"的方式直接使用 tar.gz 包**
> ❌ 不能直接解压 tar.gz 包到任意目录
> ❌ 不能通过设置 PATH 环境变量来使用
> ✅ **必须打包成 HNP（HarmonyOS Native Package）格式才能正常使用**

**HNP = HarmonyOS Native Package**, the platform's native package format —
the analogue of MSI on Windows or deb/rpm on Linux. Packed with `hnpcli`
(which ships in the OpenHarmony SDK):

```bash
hnpcli pack -i <pkg-dir> [-o <out-dir>]              # with hnp.json present
hnpcli pack -i <pkg-dir> -o <out-dir> -n <name> -v <ver>   # without
```

The payload is a directory with `bin/`, optional `cfg/` and `lib/`, and a
required `hnp.json`:

```json
{
  "type": "hnp-config",
  "name": "hnpsample",
  "version": "1.1",
  "install": {
    "links": [
      { "source": "/bin/hnpsample", "target": "hnpsample" }
    ]
  }
}
```

`source` is relative to the package root and `target` becomes a symlink
name in the `bin` directory. If `links` is omitted, **every** binary under
`bin/` gets a symlink by default. Constraints: no spaces or special
characters in `name`/`version`, no non-ASCII directory names, package
≤ 4 GB. Binaries that need their own libraries should carry an rpath
(`-Wl,-rpath=${ORIGIN}/../lib`).

**The distribution path is the part that matters.** An `.hnp` is **not
installed standalone**. It is embedded in a **HAP** — an app bundle —
declared in `module.json5` as `hnpPackages` with `"type": "public"` or
`"private"`, and the HAP is signed and distributed through AppGallery:

```
HAP project root
└── hnp/                    # hnp root, as passed to --hnp-path
    └── arm64-v8a/          # device ABI dir
        ├── python.hnp
        └── sub_dir/test.hnp
```

So shipping Niubash to HarmonyOS PC is **not** "publish a `.hnp` and let
people install it". It means publishing **a HAP on the app store** whose
payload is Niubash's binaries. That is an app-store listing, a signature,
and a review — the same shape of gate as the phone form factor, just with
a looser runtime policy. Anyone who thought "HarmonyOS PC is a tarball"
should recalibrate: the packaging is cheap, the **distribution** is a
store release.

A public build framework exists —
[`gitcode.com/OpenHarmonyPCDeveloper/build`](https://gitcode.com/OpenHarmonyPCDeveloper/build)
— which cross-compiles Linux/Unix CLI tools to `aarch64-linux-ohos` and
packs them as HNP; it consumes `OHOS_SDK`, drives everything through
`build.sh --sdk <path>`, and registers components in a `dependency.json`.
Precedent components include `tree` and `ninja`.

**The "same shape as the Linux leg" claim was wrong**, twice over: the
artifact is different (`niu-<ver>-ohos.hnp`, inside a HAP), and the
delivery channel is different (store, not direct download).

### Where HNP installs, and whether it lands on `PATH`

Two install roots, both of which do put their `bin` on `PATH`:

| Kind | Install path | Binaries symlinked into | On `PATH`? |
| --- | --- | --- | --- |
| Public HNP | `/data/service/hnp/<name>_<version>/` (`HNP_PUBLIC_HOME=/data/service/hnp`) | `/data/service/hnp/bin` | ✅ **yes** |
| Private HNP | `/data/app/<name>_<version>/` (`HNP_PRIVATE_HOME=/data/app`) | `/data/app/bin` | ✅ **yes** |

The official guide is explicit: *"公有hnp包二进制软链接路径：`/data/service/hnp/bin`（已加入环境变量）"*, and the same for
`/data/app/bin`. Where the two collide, `HNP_PRIVATE_HOME` wins — a
same-named private binary shadows the public one.

So the open question from the previous revision is **closed**: a HNP does
put its commands on `PATH`, and `/data/service/hnp/bin` — the same
directory the 454-command measurement counted — *is* that symlink
directory. Tools that install and then say "安装后找不到 ninja 命令？" and
ask you to `export PATH=...` by hand have gone a manual route; that is not
what the packaging system does by itself.

This is good news for Niubash: **no Unix command-directory discovery
mechanism is needed.** Once the HAP is installed, the host looks like any
other Unix box and Niubash's existing `PATH`-based discovery works
unchanged. A real device should still run the smoke gate — but this
particular unknown does not need one.

The build-framework side still uses `HNP_PUBLIC_PATH` when cross-compiling
on a dev host (e.g. `export HNP_PUBLIC_PATH=~/HarmonyOSPC/data/service/hnp`)
so `hnpcli` writes into the right tree; that is a build-host variable, not a
runtime one.

### `uname` on HarmonyOS

`uname`/`arch` do **not** hard-code a platform name. On unix,
`crates`-side `identity.rs` reads `uname(2)` and prints `utsname` verbatim,
the same way coreutils `uname(1)` does. So the reported values are whatever
the running kernel says. Measured on real devices:

| Field | Flag | OpenHarmony (Linux kernel) | HarmonyOS (Huawei kernel) |
| --- | --- | --- | --- |
| sysname | `-s` | `Linux` | **`HarmonyOS`** |
| nodename | `-n` | the host name | the host name |
| release | `-r` | the kernel release | **`HongMeng Kernel 1.13.0`** |
| version | `-v` | the build stamp | `#1 SMP Sat Aug 15 11:19:26 UTC 2026` |
| machine | `-m` | `aarch64` | `aarch64` |

`-s` being `HarmonyOS` (not `Linux`) is the one that bites. `config.guess`
did not know it, fell through to *"unable to guess system type"*, and
exited non-zero — which breaks every autoconf-based configure, including
LLVM's `GetHostTriple.cmake`. A patch proposing `aarch64-unknown-linux-ohos`
for `uname -s = HarmonyOS` was filed upstream in 2026-08.

`-r` is a **string with spaces**. Code that splices `uname -r` into a
compiler flag without quoting produces `-D__HarmonyOS_HongMeng Kernel
1_12_0`, which the shell splits into several arguments and `Kernel` is
mistaken for an input file. That is a real bug in the wild (BitMagic
7.13.4), and the fix was to override the platform label, not to change the
kernel.

There is nothing to special-case in Niubash, and that is deliberate: the
honest source is the kernel, not a compile-time constant. **Do not add a
HarmonyOS-specific `sysname()` branch** unless a concrete consumer needs
it — and if something does need to branch, it should match `HarmonyOS`
exactly, not `Linux`.

`$BASH_VERSION` reports Niubash's Bash-compatibility level (e.g.
`5.3.0(1)-release`) on every platform, HarmonyOS included.

### Sandbox quirks worth knowing on HarmonyOS

Beyond the kernel name, HarmonyOS restricts the filesystem in ways a
portable shell has to survive. Measured on HarmonyOS 7 (aarch64):
`/tmp` is read-only and `$HOME` is not writable, so a probe that opens a
temp file gets `Permission denied`. That is why a naive `config.guess`
fails twice on the same box — once on the temp file, and again on the
unrecognised OS name. Applications also face binary **code-signing
verification**, which is why third-party executables on HarmonyOS PC
frequently hit `Permission denied` on launch.

A shell that assumes a writable `$TMPDIR` or a writable `$HOME` will
misbehave here. Niubash should be checked against both on the first real
device run; nothing in the build changes.

None of this changes the build. It is listed here so nobody spends a day
re-deriving it.

### HarmonyOS PC vs HarmonyOS phone

These are two different products, not one port.

| | HarmonyOS PC | HarmonyOS phone |
| --- | --- | --- |
| Userland | full (bash + 454 commands) | full |
| `fork`/`exec` of new processes | unrestricted | **restricted** — ordinary apps cannot create processes directly; the app must declare each one in its config and pass per-process review at listing time |
| Packaging | **HNP** (tarball + PATH is not supported) | HNP, same format |
| Distribution | **HAP on AppGallery** — an HNP is embedded in a HAP, not installed standalone | HAP on AppGallery |
| Status | the target this page describes | a new form factor; not in scope yet |

The fork restriction is the load-bearing **runtime** difference. Huawei's
own FAQ (`faqs-ability-112`) states that on phones the system restricts and
manages `fork` for ordinary apps, and community reports add that PC does
not restrict it. Niubash is built on a fork+exec model on unix (see
`rubash`'s `fd/unix.rs`: `Command::new` + `Stdio::from_raw_fd` +
`pre_exec`), so PC is a plain port while the phone form factor needs the
process-declaration path to be sustainable before it is worth building.

But note what the **distribution** column now says: both are HAP. The
packaging and store-release work is largely the same for the two form
factors; what differs is the runtime process policy. That is a different
split from the earlier "PC is a tarball / phone is an app" framing, and it
is the accurate one.

**Do not treat "HarmonyOS support" as one item.** The store-release work is
shared, but the phone form factor adds a per-process declaration and review
gate that PC does not — and that gate is not something Niubash controls.

## What niu is not: no MSYS runtime

On Windows niu prints MSYS labels: `uname -s` reports `MSYS_NT-…`,
`$OSTYPE` is `msys`, `$MACHTYPE` is `x86_64-pc-msys`. Those labels are a
**compatibility persona** ([rubash#154](https://github.com/unixwin/rubash/issues/154)),
not a runtime. Ecosystem scripts routinely gate on `uname -s` matching
`MINGW*|MSYS*|CYGWIN*` or on `OSTYPE = msys` before they run; the persona
exists so those gates open. It is a default, not a fact —
`RUBASH_IDENTITY=native` switches the builtins to the native face
(`uname -s` → `Windows_NT`, `$MACHTYPE` → `x86_64-pc-windows`).

Under the labels there is **no MSYS2 runtime**: no `msys-2.0.dll`, no
`cygwin1.dll`, and no POSIX→Windows argument-conversion layer. niu is a
pure native Windows build; every process it starts is an ordinary Win32
process. So the `uname` story on this page splits by platform: on unix
the builtin reports the kernel verbatim (see
[`uname` on HarmonyOS](#uname-on-harmonyos)); on Windows it reports
persona labels that name a runtime which is not there.

### The behavioral boundary (measured)

| A script written for real MSYS assumes | niu does |
| --- | --- |
| The runtime rewrites POSIX-form argv before a native child sees it. | Arguments arrive **verbatim** — `niu -c 'cmd /c echo /d/repo'` prints `/d/repo`. |
| `/d/...` reaches the child as `D:\...`. | `git -C /d/repo rev-parse --show-toplevel` fails with `cannot change to '/d/repo'` (exit 128). Pass `D:/repo`. |
| An MSYS2 root: `/etc/fstab`, `/etc/profile`, MSYS2's `/usr`. | `/` is the current drive's root (`ls /` lists `C:\`); the Unix-shaped `/etc` resolves to a real Windows directory holding only winuxcmd's `mtab` shim. |
| MSYS2's bundled perl/awk runtimes. | No perl at all; `awk` is winuxcmd's own native build, not MSYS2's. |

The first two rows are the ones that bite. The shell itself accepts
POSIX-drive spellings as input dialects — `cd /d/repo && pwd` prints
`D:/repo` — but that is niu's own path parsing (the
[Windows Path Contract](windows-path-contract.md)), not a conversion
layer in front of child processes. Scripts that depend on **argv being
rewritten on the way to a native program** therefore behave differently
under niu than under Git Bash, and there is deliberately no
`MSYS_NO_PATHCONV`-style escape hatch: nothing converts, so nothing
needs switching off.

One line of orientation: **Git Bash = MSYS2 runtime + GNU bash; niu =
native engine + compatibility persona.** The labels match so ecosystem
scripts run; the runtime they name is not present.

## What is Windows-only

By compile-time cfg, the following never appear on non-Windows paths and
must not be weakened (no `wpm` strings off Windows):

- WinuxCmd command links and the `wpm` package-manager surface.
- The ConPTY interactive journey harness.
- Windows path translation (`/mnt/X` drive argv rewriting) and the Windows
  signal bridge.

Unix tarballs carry only `niu` + `README.md` + `LICENSE`; native system
tools are used as-is rather than bundled.
