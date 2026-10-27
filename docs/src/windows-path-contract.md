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
```

`/tmp` is deliberately NOT part of this tree: it is a per-user temporary
namespace resolved to the real Windows temp directory (`TMPDIR`, else
`%TEMP%`), so it stays writable under any install location
(unixwin/niubash#94). `/var/tmp` resolves beside it as
`<real temp>/var/tmp`. For every child that is not a bundled WinuxCmd applet,
external command arguments are translated by the Rubash executor with the same
rules — including `/mnt/X` drive spellings — so there is
exactly one path-semantics owner and no host-side AST rewrite. Device spellings
such as `/dev/null` are not part of that set; as arguments they reach the child
verbatim. One child class is exempt from the drive-form rewrite, described under
Shell Namespace Ownership.

Command lookup and native child `PATH` use these real directories directly.
Rubash does not merge a second provider directory, and WinuxCmd coreutils do
not inspect Niubash variables. Existing flat installations remain usable when
their directory is explicitly present on `PATH`; new installs use the tree
above.

## Shell Namespace Ownership

A child process either resolves the shell namespace itself or receives native
paths — never both, and never a guess about the child's capabilities:

- **Bundled WinuxCmd applets own the shell tree** (unixwin/niubash#346). The
  applets selected by the dispatcher route, the configured root, and the
  `winuxcmd.exe` tree marker receive `/`, `/usr/bin`, `/etc`, `/tmp`,
  `/home`, `/mnt/X`, the bare `/`, and `/dev/*` VERBATIM, and resolve them
  through WinuxCmd's own shell-tree layer, which mirrors Rubash's root map.
  They therefore render the POSIX face (`ls -d /usr/bin`, `realpath /etc`,
  `readlink -f /tmp`) and stay consistent with the spelling-preserving `pwd`.
  The executable-leaf convention is theirs too: `/usr/bin/seq` resolves to
  the real `seq.exe` while `ls -d` keeps the operand spelling, so
  `head -1 /usr/bin/seq` and `test -f /usr/bin/seq` agree (niubash#177).
- **Every other child — a native Windows program, or a foreign POSIX runtime
  such as MSYS2 or Cygwin — receives native Windows paths.** There is
  no per-child capability probe and no `MSYS_NO_PATHCONV`-style switch; a
  foreign runtime is treated exactly like a native program.
- **One exception, and it is not a capability probe: `cmd.exe`.** A
  single-letter drive form such as `/d/repo` or `/c/Users` reaches `cmd.exe`
  verbatim, because `/d` and `/c` are indistinguishable from `cmd`'s own
  switches. Root forms and `/mnt/X` spellings are still translated, so
  `cmd /c echo /usr/bin` and `cmd /c echo /mnt/c/Users` arrive native while
  `cmd /c echo /d/repo` does not. The exemption is pinned in Rubash
  (`is_windows_command_processor`, and the
  `windows_cmd_switches_are_not_mapped_into_shell_root` test) and applies to
  `cmd.exe` alone: a native program that is not `cmd` receives `/d/repo` as
  `D:\repo`.
- **Namespace bridges stand outside the translation boundary entirely:
  `wsl` and the OpenSSH client family (`ssh`, `scp`, `sftp`).** Their
  arguments are not paths on this machine at all — they are interpreted
  inside another filesystem namespace (a WSL guest's tree, an SSH remote's
  tree) — so the Rubash executor hands every argument to them byte-for-byte
  verbatim: no root map, no home/temp map, no drive-form rewrite, no
  `/dev/null` rewrite. Membership in the class is the platform property of
  being a namespace bridge; the set is the platform's fixed bridges, pinned
  in Rubash (`is_namespace_bridge_program` and the `pathmatrix_bridge_*`
  tests). A bridge's remote words therefore survive exactly as typed:
  `ssh host cat /var/log/x` and `wsl cat /home/user/file` name the
  remote/guest files the user spelled, never local ones.

The boundary is structural rather than a rename of the "MSYS NO CONV"
problem: the shell never asks a child what it can understand. The shipped
toolchain's own resolution decides, and the engine compensates only for the
child classes that cannot resolve the tree.

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

The only device spelling currently supported is `/dev/null`. As a redirect
target it is mapped to the native `NUL` endpoint; as an external-command
argument it reaches the child verbatim, like the other `/dev` entries, which do
not become ordinary files until their fd or terminal capability is implemented.
