# Windows Path Contract

Niubash uses a real Windows directory tree for its Unix-shaped shell paths.
This is path spelling support, not MSYS, WSL, Cygwin, a POSIX runtime, or a
filesystem overlay.

## Ownership

| Layer | Responsibility |
| --- | --- |
| Niubash | Select one `winuxcmd.exe`, derive its installation root, create the real tree, and configure the shell session. |
| Rubash | Interpret `/`, `/bin`, `/usr/bin`, `/etc`, `/tmp`, `cd`, `source`, glob, redirects, tests, and command lookup. |
| WinuxCmd | Implement external commands and native Windows filesystem, process, handle, and device operations. |
| WPM | Install package payloads and command links inside the selected installation root. |

## Installation Root

For an installed executable such as:

```text
C:/Users/Administrator/AppData/Local/Programs/Niubash/winuxcmd/usr/bin/winuxcmd.exe
```

the shell root is:

```text
C:/Users/Administrator/AppData/Local/Programs/Niubash/winuxcmd/
  usr/bin/
  bin/
  usr/local/bin/
  etc/
  var/
  tmp/
  dev/
  .wpm/
```

These are ordinary Windows directories. `usr/bin` is canonical for
`winuxcmd.exe`, `wpm.exe`, command links, and filename-only WPM targets.
Explicit WPM targets under `bin`, `usr/bin`, or `usr/local/bin` remain in that
exact directory. `.wpm` is private package state and is never a command path.

Niubash passes this exact root to Rubash as `NIU_ROOT`. Rubash maps paths
lexically below it:

```text
/             -> <root>
/usr/bin/tool -> <root>/usr/bin/tool
/bin/tool    -> <root>/bin/tool
/etc/config  -> <root>/etc/config
/tmp/file    -> <root>/tmp/file
```

Command lookup and native child `PATH` use these real directories directly.
Rubash does not merge a second provider directory, and WinuxCmd coreutils do
not inspect Niubash variables. Existing flat installations remain usable when
their directory is explicitly present on `PATH`; new installs use the tree
above.

## Dispatcher Selection

`WINUXCMD_PATH` selects one exact dispatcher executable for the session.
Niubash resolves it, prepends that installation's `usr/local/bin`, `usr/bin`,
and `bin` directories to the native `PATH`, and passes the exact executable to
Rubash. Rubash does not discover another dispatcher from `PATH`.

The dispatcher is only a fallback when a command is absent from the real tree.
It must not implement Rubash builtins such as `cd`, `export`, `set`, `read`,
`jobs`, or `trap`.

## Special Paths

`~` is `USERPROFILE`, the same directory used by PowerShell. Windows paths
such as `C:/work/file` and `/c/work/file` remain host paths.

The only device spelling currently supported is `/dev/null`, mapped to the
native `NUL` endpoint. Other `/dev` entries do not become ordinary files until
their fd or terminal capability is implemented.
