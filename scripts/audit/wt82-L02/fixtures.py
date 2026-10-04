#!/usr/bin/env python3
"""Local offline fixture ecosystem for lane wt82-L02.

The wizard's collection apply and the theme gallery need plugin-manager
sources. GitHub was unreachable at audit time (TLS failure to
https://github.com — recorded as environment, per AUDIT-PLAN rule 10), so
the lane pins the git transport to LOCAL fixture repos through the
product's own mirror channel (§14.8 insteadOf, transport-only): a
hand-written mirrors.toml with `git_instead_of = <fixtures>/gh/github.com/`.

The registry/spec still record canonical GitHub URLs (the iron invariant),
so every verb behaves exactly as it would online — only the bytes travel
from localhost. Fixture layouts follow crates/niubash-runtime/src/plugins/
descriptors.rs fingerprints exactly:

- oh-my-bash:  oh-my-bash.sh + themes/<name>/<name>.theme.sh (+ plugins,
  aliases, completions)
- bash-it:     bash_it.sh + lib/composure.bash + themes/<n>/<n>.theme.bash
- bash-completion: root file `bash_completion` + completions/*.sh
- bash-preexec: root file `bash-preexec.sh` (the recipe's entry)

Theme stubs set distinct PS1s so "which theme is actually active" is
observable in ConPTY text.
"""

from __future__ import annotations

from pathlib import Path

from conpty_lib import git

OMB_LOADER = """\
# fixture oh-my-bash loader (audit wt82-L02)
if [ -n "${OSH_THEME:-}" ] && [ -r "$OSH/themes/$OSH_THEME/$OSH_THEME.theme.sh" ]; then
  . "$OSH/themes/$OSH_THEME/$OSH_THEME.theme.sh"
fi
"""

BASH_IT_LOADER = """\
# fixture bash-it loader (audit wt82-L02)
if [ -n "${BASH_IT_THEME:-}" ] && [ -r "$BASH_IT/themes/$BASH_IT_THEME/$BASH_IT_THEME.theme.bash" ]; then
  . "$BASH_IT/themes/$BASH_IT_THEME/$BASH_IT_THEME.theme.bash"
fi
"""

BC_STUB = """\
# fixture bash-completion root entry (audit wt82-L02)
__niu_fixture_bash_completion_loaded=1
"""

PREEXEC_STUB = """\
# fixture bash-preexec (audit wt82-L02)
__niu_fixture_bash_preexec_loaded=1
"""

# name -> PS1 the theme stub sets (activation is observable in the REPL)
OMB_THEMES = {
    "robbyrussell": "[robby:rb] ",
    "demox": "[omb:demox] ",
    "agnoster": "[omb:agnoster] ",
}
BASH_IT_THEMES = {
    "demox": "[bit:demox] ",
    "powerline-multiline": "[bit:plml] ",
}


def _write(root: Path, rel: str, text: str):
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8", newline="\n")


def _commit(repo: Path, message: str):
    for args in (
        ["init", "-q"],
        ["config", "user.email", "audit@example.invalid"],
        ["config", "user.name", "audit-fixtures"],
        ["add", "-A"],
        ["commit", "-q", "-m", message],
    ):
        res = git(args, repo)
        if res.returncode != 0:
            # Rebuild over an existing fixture repo: "nothing to commit"
            # is the success case (the content is already committed).
            if args[0] == "commit" and "nothing to commit" in (
                    res.stdout + res.stderr):
                return
            raise RuntimeError(
                f"git {args[0]} failed in {repo}: "
                f"{(res.stderr or res.stdout).strip()}")


def _mirror_spellings(repo: Path):
    """Recipe/adapter origins mostly end in `.git`; the insteadOf rewrite
    is pure prefix replacement, so the fixture must answer BOTH spellings
    (`<name>` and `<name>.git`). A plain clone of the fixture serves as
    the second spelling (repos are tiny)."""
    twin = repo.parent / (repo.name + ".git")
    if not (twin / "HEAD").exists() and not (twin / ".git").exists():
        res = git(["clone", "-q", str(repo), str(twin)], repo.parent)
        if res.returncode != 0:
            raise RuntimeError(
                f"fixture twin clone failed for {repo}: "
                f"{(res.stderr or '').strip()}")


def build_fixtures(fixtures_root: Path) -> Path:
    """Create the four fixture repos; returns the insteadOf base
    (<fixtures_root>/gh/github.com, WITH trailing slash)."""
    gh = fixtures_root / "gh" / "github.com"
    omb = gh / "ohmybash" / "oh-my-bash"
    _write(omb, "oh-my-bash.sh", OMB_LOADER)
    for name, ps1 in OMB_THEMES.items():
        _write(omb, f"themes/{name}/{name}.theme.sh",
               f"# fixture omb theme {name}\nPS1='{ps1}'\n")
    _write(omb, "plugins/git/git.plugin.sh",
           "# fixture omb plugin\n__niu_fixture_omb_git=1\n")
    _write(omb, "aliases/git.aliases.sh",
           "# fixture omb alias\nalias __niu_fixture_omb_alias='true'\n")
    _write(omb, "completions/git.completion.sh",
           "# fixture omb completion\n__niu_fixture_omb_comp=1\n")
    _commit(omb, "fixture oh-my-bash")

    bashit = gh / "Bash-it" / "bash-it"
    _write(bashit, "bash_it.sh", BASH_IT_LOADER)
    _write(bashit, "lib/composure.bash", "# fixture composure\n")
    for name, ps1 in BASH_IT_THEMES.items():
        _write(bashit, f"themes/{name}/{name}.theme.bash",
               f"# fixture bash-it theme {name}\nPS1='{ps1}'\n")
    _write(bashit, "aliases/available/git.aliases.bash",
           "# fixture bash-it alias\nalias __niu_fixture_bit_alias='true'\n")
    _commit(bashit, "fixture bash-it")

    bc = gh / "scop" / "bash-completion"
    _write(bc, "bash_completion", BC_STUB)
    _write(bc, "completions/git.sh", "# fixture completion\n")
    _commit(bc, "fixture bash-completion")

    preexec = gh / "rcaloras" / "bash-preexec"
    _write(preexec, "bash-preexec.sh", PREEXEC_STUB)
    _commit(preexec, "fixture bash-preexec")

    for repo in (omb, bashit, bc, preexec):
        _mirror_spellings(repo)

    # POSIX slashes: this base lands inside a TOML basic string
    # (mirrors.toml git_instead_of) where backslashes are escape
    # sequences — the wt82-L02 m01 lesson (the malformed file degraded to
    # direct connection, exactly as the degrade discipline requires).
    return gh.as_posix() + "/"


def write_mirrors(mirrors_file: Path, base: str):
    """The transport-only insteadOf mirror file (NIU_MIRRORS target)."""
    mirrors_file.parent.mkdir(parents=True, exist_ok=True)
    mirrors_file.write_text(
        f'schema = "niubash:mirrors@0.1.0"\n'
        f'active = "custom"\n'
        f"\n[github]\n"
        f'git_instead_of = "{base}"\n',
        encoding="utf-8", newline="\n")
