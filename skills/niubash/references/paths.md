# Path contract

Niubash's path model in one sentence: **the Windows-native path is the
first-class internal representation; POSIX- and WSL-style spellings are
input dialects.** At the process boundary the executor applies one
deterministic translation contract (stated in full in
`docs/src/windows-path-contract.md`): classified path spellings are
translated for native children, and everything else — data, the bare
`/`, drive-root forms — goes verbatim. There is no heuristic converter,
so there is nothing that can mis-convert data — and no opt-out switch.

## The contract

| Input spelling | Meaning | Resolves to |
|---|---|---|
| `C:/Users/me`, `C:\Users\me` | native Windows | itself — always safe |
| `/c/Users/me` | POSIX display dialect | `C:\Users\me` |
| `/mnt/c/Users/me` | WSL dialect | `C:\Users\me` |
| `/usr/bin`, `/bin`, `/etc/...`, `/var/...` | install-tree dialect | real dirs in the WinuxCmd installation root |
| `/tmp/x`, `/var/tmp/x` | per-user temp namespace | the real Windows temp dir (`TMPDIR`, else `%TEMP%`); `/var/tmp` → `<temp>/var/tmp` |
| `/home/me` | users dialect | real users directory (`C:/Users`) |
| `/dev/null` | null device | Windows `NUL` |
| `~` | home | `USERPROFILE` (kept consistent with `HOME`) |
| `D:` (bare) | drive designator | the drive's root — `D:` and `cd D:` land on `/d/` |

At the boundary, argv tokens are sorted by shape. Classified path
spellings (the dialects above, `/mnt/X`, the exact token `/dev/null`)
are **translated** to native Windows paths for native children.
Everything else goes **verbatim**: literal data (even path-shaped data),
the bare `/`, drive-root forms `/d` and `/d/`, attached values
(`--opt=/d/x`, `FOO=/d/x`), and namespace bridges (`ssh`, `wsl`), whose
arguments name files in another filesystem namespace. Bundled WinuxCmd
applets are the second child class: they receive the shell spellings
verbatim and resolve them through their own tree layer.

Key facts for agents:

- Bare drive designators are the cmd-style drive switch: `D:` or `cd D:`
  jumps to that drive's root, and the behavior is identical across
  `niu -c`, brace groups/subshells, and piped `niu -i` (the engine owns
  the dispatch on every route). `D:rel\path` drive-relative spellings are
  not a dialect — use `D:/rel/path` or `/d/rel/path`.

- `> /tmp/f` and `cat /tmp/f` agree: both hit the real Windows temp
  directory. The install tree ships **no** `tmp/` directory at all — there
  is exactly one `/tmp` and it is per-user.
- `printf '%s\n' /tmp/x` prints `/tmp/x` verbatim (GNU bash behavior):
  literal data arguments are never rewritten.
- What a native child process receives for a classified path spelling is
  the native Windows path — `/d/repo` arrives at `python.exe` as
  `D:\repo`, and `cat /etc/hosts` (the applet resolving its own spelling)
  opens the real installed file.

## Contrast with MSYS-family environments

| | MSYS2 / Git Bash | Niubash |
|---|---|---|
| Model | POSIX emulation over a Unix-looking root | native shell, dialects as input |
| Path handling | heuristic conversion of arguments | deterministic translation of classified spellings for native children; data and boundary forms verbatim |
| Escape hatch | `MSYS_NO_PATHCONV`, `MSYS2_ARG_CONV_EXCL` required when the heuristic breaks | nothing — there is no heuristic converter, and no opt-out switch |
| Failure mode | silent argument mangling (e.g. `git grep "/pattern"` rewritten) | data is never mangled (verbatim by contract); classified spellings follow the one documented rule |

Rule of thumb: if you find yourself reaching for a `NO_PATH_CONV`-style
variable, stop — in Niubash that variable does not exist because the
heuristic converter it works around does not exist. If a path misbehaves
against the contract above, it is a bug: report it
to `unixwin/rubash` (shell semantics) or `unixwin/winuxcmd` (command
implementation).
