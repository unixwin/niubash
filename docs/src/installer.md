# Installer and Self-Update

Niubash ships two Windows package shapes:

- `niubash-v<version>-win-<arch>-setup.exe` for normal users.
- `niubash-v<version>-win-<arch>.zip` for portable, agent, or scripted use.

The installer is built with Inno Setup and installs per user by default under:

```text
%LOCALAPPDATA%\Programs\Niubash
```

It does not require administrator privileges. The default installer tasks:

- add the install directory to the user's `PATH`;
- add or update a Windows Terminal profile named `Niubash`;
- set that profile's command line to the installed `niu.exe`;
- set that profile's starting directory to `%USERPROFILE%`;
- point the Windows Terminal profile icon at the installed PNG asset.

The Windows Terminal profile is installed by running:

```sh
niu --install-wt-profile --quiet
```

Users can run this command again after moving an install. To also set Niubash as
the Windows Terminal default profile, run:

```sh
niu --install-wt-profile --set-default
```

Self-update uses Windows WinHTTP directly to follow the GitHub Release
`releases/latest` redirect, download the latest installer for the current
architecture, and start it silently. It does not depend on the GitHub REST API.

```sh
niu --self-update
```

Inside an interactive Niubash REPL, use:

```sh
self-update
```

The REPL command hands the update to a child process and exits the current
shell so the installer can replace `niu.exe`.

Useful dry-run modes:

```sh
niu --self-update --check
niu --self-update --dry-run
```

Interactive shells check for updates at most once per day. The check is
best-effort and silent on network failures; when a newer release exists, Niubash
prints a short hint to run `self-update` in the REPL or
`niu --self-update` outside it. Set
`NIU_UPDATE_CHECK=0` or `NIU_NO_UPDATE_CHECK=1` to disable the reminder.

The installer invokes `winuxcmd.exe wpm links rebuild --root ... --force` after
copying the files, so the bundled commands are materialized immediately. On NTFS,
WPM creates hard links to the installed `winuxcmd.exe`. The portable zip keeps the
first-start fallback: if command links are missing, Niubash runs
`winuxcmd/activate-winuxcmd.sh` once from the bundle.

The bundled package shims (`usr\bin\gawk.exe`, `awk.exe`) sit outside that
rebuild: wpm only reconciles names recorded in `.wpm` state, which the
product deliberately does not ship, so an upgrade that replaces
`winuxcmd.exe` under its own name would leave those links on the previous
image (niubash#468). The installer therefore runs
`niu --relink-bundled-shims --force --quiet` right after the rebuild, and
startup activation runs the same reconcile: every declared shim name is
brought back onto the current `winuxcmd.exe` image — relinked when stale or
missing, hardlinked when the payload delivered only a copy, and never
touched when the machine's own `.wpm` state owns the name.

## Updating WinuxCmd with WPM

The Unix command set (`ls`, `cat`, `grep`, `sed`, ...) is delivered by
WinuxCmd and managed separately from the Niubash binary. Winux Package
Manager (`wpm`) handles it:

```sh
wpm update winuxcmd          # update WinuxCmd from the local index
wpm index status             # inspect the local index state
wpm list                     # indexed packages and install state
```

`wpm update winuxcmd` refreshes the command set in place; command links are
rebuilt automatically. Run `wpm --help` for the full surface (`index`,
`source`, `search`, `info`, `install`, `links`).

So the update story has two parts:

```sh
niu --self-update        # the shell itself
wpm update winuxcmd          # the Unix commands it ships with
```

## Bash And sh Command Links

`bash.exe` and `sh.exe` are **the shell itself under another name**: where
hardlinks are available they are hardlinks to `niu.exe`, and the engine takes
the face from `argv[0]` (`$0`, `BASH_VERSION`, and POSIX mode for `sh`/`ash`).
That is worth real time - a hardlink costs nothing, while a forwarding launcher
runs one extra process on every `bash -c` / `sh -c` (measured at ~13 ms per
call, unixwin/niubash#377).

Run this after laying the tree down, and again after any update that replaces
`niu.exe` (a replaced binary leaves the names pointing at the previous inode):

```sh
niu --relink-shells --force
```

It links `bash.exe` and `sh.exe` in `<root>\bin`, `<root>\winuxcmd\bin` and
`<root>\winuxcmd\usr\bin`, skips directories that are not present, is
idempotent, and parks an existing name out of the way before replacing it, so a
shell that is running right now keeps its bytes. The Niubash installer does
this from `[Run]`.

Where hardlinks are unavailable (exFAT, a network share) the shipped forwarding
shim stays the fallback. It sets `NIU_INVOKED_AS=bash` / `NIU_INVOKED_AS=sh`
and re-executes `niu`, which produces exactly the identity the `argv[0]` path
provides - keep both working instead of assuming either:

```text
NIU_INVOKED_AS=bash
NIU_INVOKED_AS=sh
NIU_INVOKED_AS=ash
```

Niubash consumes that value before constructing Rubash; a plain `niu.exe`
launch must leave it unset, and a launcher must preserve all original argv
values rather than parsing shell options of its own.

Installer acceptance tests for either provider:

```sh
bash.exe -c 'test -z "$POSIXLY_CORRECT"'
sh.exe -c 'set -o | grep posix'
ash.exe -c 'set -o | grep posix'
```

This repository does not create the links inside a WinuxCmd tree by itself; the
installation provider calls `niu --relink-shells` (or implements the
`NIU_INVOKED_AS` contract above).

## The PATH Entry Directory Contract

Only two directories of the installation ever appear on the process `PATH`:
`{app}` itself (so `niu` resolves) and `<root>\winuxcmd\bin` — the single
*command* directory. The contract for what may live in the command directory
(niubash#376):

- **Hardlinks to the payload only, never forwarding launchers.** A hardlink is
  the same file — the spawn cost equals the payload's (`git.exe` there measured
  12.7 ms against a forwarding wrapper's 32.2 ms, unixwin/niu-git#8). A
  launcher re-executes the real binary and pays a full process per call, which
  removes the entire reason the entry exists.
- **`usr\bin` never goes on `PATH`.** It holds the whole Unix command set;
  putting it there shadows `git`, `cygpath`, `find`, `sort`, `sed` and friends
  in Git for Windows sessions and every GUI process.
- **The entry set is declared, not accumulated.** `wpm links rebuild`
  (installer `[Run]`) materializes the package-declared links, and
  `niu --relink-shells` keeps the shell faces on the current `niu.exe` inode.
  An undeclared link survives as a stale binary while the payload moves on.
- **Dead payloads are swept, not kept.** Retired shapes — `*.bak-*` and
  `*.old-*` left by earlier updaters, and `*.niu-old-*`, the relinker's park
  name when a crash lands between park and drop — are removed by the
  installer's `[InstallDelete]`/`[UninstallDelete]` sweeps; a directory on
  `PATH` must stay auditable. The sweeps match only these generated shapes
  inside the two app-owned directories; a user file that borrows one of these
  suffixes there would be removed with them, so keep user files out of the
  installation tree.

Accepted limitation: in a GUI-launched process `bash` still resolves to the
WSL launcher (`System32` and `WindowsApps` sit ahead of the user `PATH`);
`sh` and `git` resolve to the entry directory. Winning the `bash` name would
need a machine-scope `PATH` entry placed before `System32`, which a per-user
install does not do.

## The Git Bash Slot Is Never Claimed

`C:\Program Files\Git\bin\bash.exe` is a convention slot that some IDEs may
resolve. A machine was found with an unowned, unsigned .NET shim parked there
forwarding into Niubash (niubash#398) — not placed by this installer and not
rebuildable from any repository. The ruling (PR #414, now pinned by
`tests/installer_surface.rs` and `scripts/installer-slot-probe.py`): the
install tree does not claim the slot.

- The installer runs with `PrivilegesRequired=lowest` and never elevates, so
  it cannot write into an admin-walled directory like `C:\Program Files\Git\bin`.
- `installer/niubash.iss` may not name an absolute foreign path at all; every
  write, delete, shortcut and `[Run]` surface stays inside `{app}` and
  Inno-managed shell folders, and the only registry value written is
  `HKCU\Environment\Path`.
- A full install/uninstall cycle must leave every foreign slot byte-identical
  and leave no face name outside the removed tree. The cycle probe replays
  the executable installer steps in a user-directory sandbox and checks
  exactly that, so the ruling is a tested invariant rather than a promise.

Revisit only with evidence that an editor actually resolves that exact path
and an accepted opt-in elevated task — the same stale-link class niubash#376
rule 5 bans otherwise.

## Plugin Ecosystem In Packages

Release packages ship no plugin content. The built-in plugin/theme stack —
including the `oh-my-niu` bundle — is retired (niubash#145), and the plugin
verbs hard-bail on bundles, so staging one would lay down a tree the product
refuses to use (niubash#161). Packages contain the shell, the WinuxCmd
command set, and the command links.

The external bash ecosystem (oh-my-bash, bash-it, bash-completion, wild
plugins) installs on explicit user command — `niu plugin add <id>` clones
from upstream into the user profile, under the trust protocol. There is
nothing to pre-stage and nothing to update with the shell.
