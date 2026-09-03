# WPM

Use the directly linked `wpm` command for WinuxCmd package discovery and
management.

- `wpm links list`: inspect command links.
- `wpm links rebuild`: create or repair links in a selected WinuxCmd root.
- `wpm list`, `wpm search`, `wpm info`: inspect packages.
- `wpm install`: install a missing command or package.
- `wpm index status`: inspect package-index state.
- `wpm update winuxcmd`: update WinuxCmd itself.

Discover what the user actually has installed before assuming a tool is
present — run these directly in the current Niubash session:

```bash
winuxcmd --version     # WinuxCmd command-link runtime version
wpm installed          # packages present in this WinuxCmd root
wpm list               # full catalog with install state
```

WPM is the package manager for GNU/POSIX command packages (`rg`, `fd`,
`bat`, `jq`, `node`, ...); WinuxCmd owns the real `/usr/bin` tree that
routes them. `niubash --self-update` updates Niubash and is separate from
`wpm` updates.

Check the active command before changing anything:

```bash
command -v wpm
wpm --version
wpm links list
wpm index status
wpm list
```

For a missing command:

```bash
wpm search NAME
wpm info NAME
wpm install NAME
```

If the package exists but its hard links are missing:

```bash
wpm links rebuild --force
```

For access-denied or locked-file errors, request approved elevation. `niubash
--self-update` updates Niubash and is separate from WPM updates.
