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
