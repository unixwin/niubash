# Installer and Self-Update

Winuxsh ships two Windows package shapes:

- `winuxsh-v<version>-win-<arch>-setup.exe` for normal users.
- `winuxsh-v<version>-win-<arch>.zip` for portable, agent, or scripted use.

The installer is built with Inno Setup and installs per user by default under:

```text
%LOCALAPPDATA%\Programs\Winuxsh
```

It does not require administrator privileges. The default installer tasks:

- add the install directory to the user's `PATH`;
- add or update a Windows Terminal profile named `Winuxsh`;
- set that profile's command line to the installed `winuxsh.exe`;
- set that profile's starting directory to `%USERPROFILE%`;
- point the Windows Terminal profile icon at the installed PNG asset.

The Windows Terminal profile is installed by running:

```sh
winuxsh --install-wt-profile --quiet
```

Users can run this command again after moving an install. To also set Winuxsh as
the Windows Terminal default profile, run:

```sh
winuxsh --install-wt-profile --set-default
```

Self-update uses Windows WinHTTP directly to follow the GitHub Release
`releases/latest` redirect, download the latest installer for the current
architecture, and start it silently. It does not depend on the GitHub REST API.

```sh
winuxsh --self-update
```

Inside an interactive Winuxsh REPL, use:

```sh
self-update
```

The REPL command hands the update to a child process and exits the current
shell so the installer can replace `winuxsh.exe`.

Useful dry-run modes:

```sh
winuxsh --self-update --check
winuxsh --self-update --dry-run
```

Interactive shells check for updates at most once per day. The check is
best-effort and silent on network failures; when a newer release exists, Winuxsh
prints a short hint to run `self-update` in the REPL or
`winuxsh --self-update` outside it. Set
`WINUXSH_UPDATE_CHECK=0` or `WINUXSH_NO_UPDATE_CHECK=1` to disable the reminder.

The installer invokes `winuxcmd.exe wpm links rebuild --root ... --force` after
copying the files, so the bundled commands are materialized immediately. On NTFS,
WPM creates hard links to the installed `winuxcmd.exe`. The portable zip keeps the
first-start fallback: if command links are missing, Winuxsh runs
`winuxcmd/activate-winuxcmd.sh` once from the bundle.

## Updating WinuxCmd with WPM

The Unix command set (`ls`, `cat`, `grep`, `sed`, ...) is delivered by
WinuxCmd and managed separately from the Winuxsh binary. Winux Package
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
winuxsh --self-update        # the shell itself
wpm update winuxcmd          # the Unix commands it ships with
```

## Bash And sh Command Links

When the WinuxCmd installer creates `bash.exe`, `sh.exe`, or `ash.exe` command
links to Winuxsh, the link launcher must pass the invocation identity without
relying on the resolved executable path:

```text
WINUXSH_INVOKED_AS=bash
WINUXSH_INVOKED_AS=sh
WINUXSH_INVOKED_AS=ash
```

Winuxsh already consumes this value before constructing Rubash. `sh` and `ash`
select POSIX mode; `bash` keeps Bash mode. A plain `winuxsh.exe` launch must
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
`current_exe()` heuristic in Winuxsh.

## Bundled Plugin Baseline

Release packages also stage the official `oh-my-winuxsh` bundle under:

```text
bundles\oh-my-winuxsh
```

The runtime checks that app-bundled path after user-managed bundle locations:

```text
%LOCALAPPDATA%\Winuxsh\bundles\oh-my-winuxsh\current
%LOCALAPPDATA%\Winuxsh\bundles\oh-my-winuxsh\<version>
bundles\oh-my-winuxsh
```

Fresh offline installs can still list and use official plugins, while
`winuxsh plugin update oh-my-winuxsh ...` can replace the baseline without
rewriting the application install directory.
