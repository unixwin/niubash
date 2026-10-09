# Release pipeline

How `.github/workflows/release.yml` turns a `vX.Y.Z` tag into release
artifacts. This is the contract every future change to the pipeline must
preserve: **no platform ships binaries that were not run on that platform
first.**

## Trigger and pinning (all platforms)

- Trigger: push of a `v*` tag, or `workflow_dispatch` with an explicit tag.
- The tag must match `^v\d+\.\d+\.\d+$`, must exist as a git tag, and the
  `version` in `Cargo.toml` at that tag must equal the tag's version. Any
  mismatch fails the build.
- Every build job clones rubash `master` as a sibling (`../rubash`, the
  `[patch]` path override), pins its revision into job outputs, and fails if
  that checkout is dirty. `build.rs` embeds the same revision into the
  binary, so `niu --version` names the engine the binary actually contains.
- The `release` job refuses to publish if the three platform jobs did not
  compile the same rubash revision (a master commit landing between two
  jobs' clones would otherwise ship mixed engines).

## Artifact matrix

| Job | Runner | Rust target | Artifacts | Contents |
| --- | --- | --- | --- | --- |
| `build-windows` (x64) | `windows-2025` | `x86_64-pc-windows-msvc` (crt-static) | `niubash-v{ver}-win-x64.zip`, `-setup.exe`, `niubash-win-x64.*` aliases | `niu.exe`, WinuxCmd, bash/sh shims, icons |
| `build-windows` (arm64) | `windows-2025` | `aarch64-pc-windows-msvc` (crt-static) | same, `-arm64` | same |
| `build-linux` (x86_64) | `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | `niubash-v{ver}-linux-x86_64.tar.gz` | `niu`, `README.md`, `LICENSE` |
| `build-linux` (aarch64) | `ubuntu-22.04-arm` | `aarch64-unknown-linux-gnu` | `niubash-v{ver}-linux-aarch64.tar.gz` | same |
| `build-macos` (aarch64) | `macos-latest` (arm64) | `aarch64-apple-darwin` | `niubash-v{ver}-macos-aarch64.tar.gz` | same |
| `build-macos` (x86_64) | `macos-latest` (arm64) | `x86_64-apple-darwin` (cross) | `niubash-v{ver}-macos-x86_64.tar.gz` | same |

Each tarball unpacks to a single `niubash-v{ver}-{os}-{arch}/` directory;
`./niu` runs from anywhere. The `release` job attaches every artifact
(`*.zip`, `*.exe`, `*.tar.gz`) with `fail_on_unmatched_files: true`.

## Unix decisions (recorded)

- **gnu, not musl (released); musl now smoke-gated** — the Unix release
  still ships `*-linux-gnu`, for the glibc floor below. The original hold
  was that "musl has never been compiled in this repo, so a musl leg would
  ship an untested libc surface; add musl legs only after they carry the
  same smoke gate green." Both halves are now discharged:
  - `cross-target-check` compiles the workspace for
    `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-ohos` (the
    OpenHarmony target, HarmonyOS PC) — this caught rubash's glibc-only
    `__rlimit_resource_t` typedef, which broke both targets while the
    glibc-only `cross-check` stayed green (rubash#449).
  - `cross-target-smoke` builds the musl binary `--release` and runs the
    **same four checks as the glibc legs** (see "The smoke gate" below)
    against it, natively — a static musl x86_64 binary runs on the glibc
    runner, so unlike the ohos target it can carry the real gate, not just
    a compile check. It first asserts the binary is actually static (`ldd`
    must report `statically linked`), so a silent re-link against the
    runner's glibc cannot pass.

  So musl is compiled *and* smoked on every CI run; the released tarballs
  remain gnu because the artifact matrix below is unchanged. A musl
  *release* artifact (Alpine / static redistribution) is now a delivery
  decision, not a test-coverage gap — flip it on by adding a leg to the
  `build-linux` matrix when a consumer needs it.
- **glibc floor: 2.35** (the ubuntu-22.04 build image). Covers Ubuntu 22.04+,
  Debian 12+. When the ubuntu-22.04 image retires, move BOTH Linux legs
  forward together and update this floor — never leave the two legs on
  different images.
- **Native arm64 runner, not cross/qemu** — `ubuntu-22.04-arm` is free for
  public repos and runs the smoke gate on the real target CPU. `cross`/qemu
  builds cannot execute the binary natively without extra emulation, which
  weakens the honest gate.
- **macOS shape** — `macos-13` (the last Intel runner) is retired, so both
  macOS targets build on the arm64 runner: `aarch64-apple-darwin` natively,
  `x86_64-apple-darwin` as a first-class Apple cross-compile (same SDK).
  The x86_64 smoke gate runs under Rosetta 2 (`arch -x86_64`), installed on
  demand by the job. The effective macOS deployment floor is whatever the
  runner's SDK + rustc defaults produce; treat the smoke-verified host
  (printed by `sw_vers` in the job log) as the tested configuration.
- **No bundling on Unix** — Unix tarballs carry only `niu` + `README.md` +
  `LICENSE`. Native system tools (coreutils etc.) are used as-is. WinuxCmd
  and the wpm package-manager surface are Windows-only by compile-time cfg;
  that red line must not be weakened (no `wpm` strings on non-Windows
  paths).

## The smoke gate (contract)

Each build job runs, **on the OS it built on, against the exact binary it
uploads, before anything is uploaded**:

1. `niu --version` — must print `Niubash <tag-version> ` and
   `rubash   git <pinned-revision>`. The substrings are pinned by
   `tests/rubash_revision_banner.rs::version_banner_lines_match_release_smoke_greps`.
2. `niu -c 'echo ok'` — must print exactly `ok` (engine execution).
3. `printf 'a\nb\n' | niu -c 'sort | head -1'` — must print exactly `a`
   (external commands + pipelines through the platform's native coreutils).
4. Offline plugin-stack check — with an isolated `HOME` and an empty
   `NIU_PLUGIN_SPEC`, `niu plugin sync --bootstrap` must exit 0 with zero
   output and touch no network.

Any failure fails the job, so no artifact can leave it. The macOS x86_64 leg
runs all four through Rosetta 2. The release body tells users every artifact
was "smoke-verified on each build OS" — that sentence is only true while
this gate stays in the workflow between build and upload.

What the gate does **not** cover: the interactive journey (prompt, readline,
TTY behavior). The ConPTY journey harness is Windows-specific; a Unix
interactive journey harness (expectrl family) is future work and must not be
faked — until it exists, interactive-mode claims on Unix stay out of the
release notes.

## Windows behavior (unchanged)

`build-windows` keeps its shape: package-release.ps1 + Inno Setup
installers, WinuxCmd bundling from a matched release, and `niubash-win-*`
latest-compatible aliases. Unix jobs deliberately do not add such aliases.

One stage was added between WinuxCmd staging and packaging
(niubash#189): **manifest-driven pre-install**. The workflow reads
`scripts/release/preinstall.json`, installs each named package into a
staged WinuxCmd root with `winuxcmd wpm install <pkg> --root <root>
--yes` (wpm ships inside winuxcmd.exe — the binary the download step
extracted is the package manager), materializes the manifest's command
shims as winuxcmd.exe hardlinks, and package-release.ps1 copies the
root's `usr\bin` shims plus the whole `opt\` payload tree into the
package (the .iss `recursesubdirs` copy carries them into the installer
for free). First entry: gawk 5.4.1 with an `awk` shim — bash-it plugins
and completions call plain `awk`, and wpm's gawk package registers only
`gawk`. The exclusions (compression tools, `goawk`, and `link` —
forbidden forever, MSVC collision) are recorded policy in the manifest
and pinned by `tests/preinstall_manifest.rs`. Installs are **fail-open**:
three attempts with backoff (the local proxy `http://127.0.0.1:7897` as
the final-attempt fallback, same convention as the perfbudget gate), and
a failure warns (`::warning` + step summary + release-notes caveat)
instead of blocking the release — currently that only bites the arm64
legs, whose wpm index has no artifact. Whatever DID install is
hard-verified in the staged package before upload (`--version` per shim
plus a plugin-shaped `awk '{print $1}'` pipeline through the packaged
niu.exe); a package that installed but does not run FAILS the job.

## Dispatching a dry run (captain)

The workflow validates `tag` against `^v\d+\.\d+\.\d+$` and against the
tagged commit's `Cargo.toml` version, so a dry run needs a real tag whose
commit carries the matching version (e.g. bump `version` to `1.3.2` on a
scratch commit). Suggested sequence:

```sh
# 1. scratch commit with Cargo.toml version = "1.3.2" on any branch
git tag v1.3.2 <that-commit>
git push origin <that-branch> v1.3.2

# 2. run the workflow from the branch that carries the new jobs
gh workflow run release.yml --repo unixwin/niubash \
  --ref <branch-with-this-workflow> -f tag=v1.3.2
gh run watch --repo unixwin/niubash "$(gh run list --repo unixwin/niubash --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')"

# 3. inspect the Niubash v1.3.2 release, download the tarballs onto real
#    Linux/macOS machines, then clean up:
gh release delete v1.3.2 --repo unixwin/niubash --yes --cleanup-tag
```

Only the first real dispatch run can prove the unix legs end-to-end
(runner labels, cross-link, Rosetta, tar packaging) — local validation
before merge covers YAML shape, job graph, cross `cargo check`, and the
Windows build/test suite.
