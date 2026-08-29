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
niubash --install-wt-profile --quiet
```

Users can run this command again after moving an install. To also set Niubash as
the Windows Terminal default profile, run:

```sh
niubash --install-wt-profile --set-default
```

Self-update uses Windows WinHTTP directly to follow the GitHub Release
`releases/latest` redirect, download the latest installer for the current
architecture, and start it silently. It does not depend on the GitHub REST API.

```sh
niubash --self-update
```

Inside an interactive Niubash REPL, use:

```sh
self-update
```

The REPL command hands the update to a child process and exits the current
shell so the installer can replace `niu.exe`.

Useful dry-run modes:

```sh
niubash --self-update --check
niubash --self-update --dry-run
```

Interactive shells check for updates at most once per day. The check is
best-effort and silent on network failures; when a newer release exists, Niubash
prints a short hint to run `self-update` in the REPL or
`niubash --self-update` outside it. Set
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
niubash --self-update        # the shell itself
wpm update winuxcmd          # the Unix commands it ships with
```

## Bash And sh Command Links

When the WinuxCmd installer creates `bash.exe`, `sh.exe`, or `ash.exe` command
links to Niubash, the link launcher must pass the invocation identity without
relying on the resolved executable path:

```text
NIU_INVOKED_AS=bash
NIU_INVOKED_AS=sh
NIU_INVOKED_AS=ash
```

Niubash already consumes this value before constructing Rubash. `sh` and `ash`
select POSIX mode; `bash` keeps Bash mode. A plain `niu.exe` launch must
leave the variable unset. The launcher must preserve all original argv values
and must not implement a second shell-option parser.

Installer acceptance tests for the WinuxCmd link provider:

```sh
bash.exe -c 'test -z "$POSIXLY_CORRECT"'
sh.exe -c 'set -o | grep posix'
ash.exe -c 'set -o | grep posix'
```

This repository does not create the command links. The installation provider
currently needs to implement this contract; do not replace it with a
`current_exe()` heuristic in Niubash.

## Bundled Plugin Baseline

Release packages also stage the official `oh-my-niu` bundle under:

```text
bundles\oh-my-niu
```

The runtime checks that app-bundled path after user-managed bundle locations:

```text
%LOCALAPPDATA%\Niubash\bundles\oh-my-niu\current
%LOCALAPPDATA%\Niubash\bundles\oh-my-niu\<version>
bundles\oh-my-niu
```

Fresh offline installs can still list and use official plugins, while
`niu plugin update oh-my-niu ...` can replace the baseline without
rewriting the application install directory.
