# Using Git With Niubash

Niubash ships a real git — [niu-git](https://github.com/unixwin/niu-git), the
git-for-windows C source rebuilt with MSVC — so a fresh machine has a working
`git` with zero setup. Two questions come up once you already have another git
(Git for Windows, a scoop git) or want niu's git from outside the shell:

- [Which git does niu use?](#which-git-does-niu-use) — yours, when you have one.
- [How do I use niu's git from other programs?](#how-do-i-use-nius-git-from-other-programs)
  — one PATH entry, never `usr\bin`.

## Which git does niu use?

`git` inside niu resolves through `PATH`, like every external command. The
resolution order is:

1. **Your own git wins.** At startup niu injects the bundled command
   directories ahead of the `PATH` it inherited, then promotes the directory of
   the first git that came from *your* `PATH` — a scoop shims dir, Git for
   Windows' `bin`, a custom install — back to the front. `which git` inside niu
   therefore names the same git you installed, with its own configuration
   (`credential.helper`, `core.editor`, GPG, SSH keys all behave as outside).
   niubash#1050. Promotion is whole-directory: only the first user git
   directory in your own `PATH` order is moved, the relative order of
   everything else is unchanged, and directories inside the niubash
   installation never count as "your" git.
2. **The bundled niugit is the fallback.** On a machine with no other git on
   `PATH`, `git` resolves to `<root>\winuxcmd\usr\bin\git.exe` (niu-git) and
   everything works out of the box — that is the point of bundling it
   (niubash#209).

Check which one you are on:

```sh
which git          # your install's path, or ...\winuxcmd\usr\bin\git.exe
git --version      # niugit builds self-identify as ...windows.N.niugit
```

There is nothing to configure and no setting to flip: to use your own git,
have it on `PATH` before niu starts, as you already would. To use the bundled
one on a machine where another git exists, run that session with the other
git's directory removed from `PATH`.

## How do I use niu's git from other programs?

The install exposes exactly one directory to the process `PATH`:
`<root>\winuxcmd\bin` — the *PATH entry directory*
([installer.md](installer.md#the-path-entry-directory-contract), niubash#376).
Everything a package installs has a hardlink there, and `git` is no exception:

- **From v1.5.1** the package ships the entry links themselves, so a fresh
  install already has `<root>\winuxcmd\bin\git.exe` (niubash#1052 — the 1.5.0
  packager dropped them; `wpm install niugit` inside niu produces the same
  links on 1.5.0 and earlier).
- Point other programs at that directory and they resolve niu's git directly:
  add `%LOCALAPPDATA%\Programs\Niubash\winuxcmd\bin` (per-user install) to the
  **user** `PATH`, or configure your IDE/editor's git path to
  `<install>\winuxcmd\bin\git.exe`. The entry link *is* the git binary — a
  hardlink, not a launcher — so calling it from anywhere costs the same as
  calling `git.exe` itself (12.7 ms measured vs 32.2 ms through a forwarding
  wrapper, unixwin/niu-git#8).

**Do not put `winuxcmd\usr\bin` on `PATH`.** It holds the whole Unix command
set; on a machine that also has Git for Windows, its `find`, `sort`, `sed`,
`cygpath` would be shadowed in every Git Bash session and GUI process — the
exact collision the entry-directory contract exists to prevent
([installer.md](installer.md#the-path-entry-directory-contract)).

Keeping both gits is also fine and needs no decisions: leave your Git for
Windows (or scoop git) directory wherever it is on `PATH` for other programs,
and niu will use that same one inside the shell (see
[above](#which-git-does-niu-use)). The bundled niugit stays as the fallback
for machines without any other install, and `wpm` keeps it up to date with
`wpm update winuxcmd`.

Accepted limitation (same as the installer contract): in a GUI-launched
process `bash` still resolves to the WSL launcher, because `System32` sits
ahead of the user `PATH`; `git` resolves to whichever directory you put on
`PATH` as above.
