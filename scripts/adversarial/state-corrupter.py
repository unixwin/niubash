#!/usr/bin/env python3
"""state-corrupter.py -- injects aged, drifted, hand-edited and corrupted
niubash state into a sandbox HOME (adversarial-matrix lanes AM02, AM08,
AM11).

The golden journey always meets a just-written install; every P0 that
shipped lately lived in WEEK-OLD state instead (owner post-mortem, axes 2
and 8). This tool fast-forwards time: it writes the shapes a real week of
use produces -- hand-edited rc drift, mixed CRLF, duplicated managed
blocks, marker typos, stale bootstrap ledgers, damaged trees, flipped
registry attributions, pre-1.3.1 imperative-era state -- using the REAL
file formats (markers, spec schema, registry schema) read from the
product source, so lanes can assert against them honestly.

Every mutation is sandbox-only: you point it at a fake HOME
(HOME/USERPROFILE redirected), never at a real profile. Each file it
touches is backed up to <name>.pre-corrupt first; `restore` undoes the
last round.

Ops (composable, order = call order):
  crlf-mix                 rewrite ~/.niubashrc with mixed CRLF/LF lines
  duplicate-block          append a second copy of the oh-my-bash managed block
  approximate-marker       begin marker loses its closing '>>>' (the
                           second-unmanaged-block shape of niu#176)
  hand-line-in-block       a user alias line inside the managed block
                           (the silent-drop shape of niu#176)
  truncate-rc              cut the last 4 lines of the rc
  corrupt-spec-toml        append broken content to plugins.json
                           (invalid JSON; op name kept for matrix compat)
  stale-bootstrap-failure  seed bootstrap-failures.toml (real schema) with
                           an old failed-fetch memo (the F5 shape)
  rename-tree-file         oh-my-bash.sh -> oh-my-bash.sh.bak (damaged tree)
  delete-tree-git          remove the source tree's .git (fingerprint
                           reject -> re-fetch churn, the 6s-source class)
  delete-tree-partial      remove themes/ only (partially-deleted tree)
  registry-attribution-flip  record's adapter flips oh-my-bash -> bash-it
                           (the #168 suspected mechanism)
  registry-truncate        cut the registry mid-record (broken JSON)
  foreign-ps1              Git-Bash-shaped PS1/PROMPT_COMMAND planted in
                           the rc user area (the niu#117 class)
  imperative-era           delete plugins.json, keep the registry
                           (pre-1.3.1 shape, journey P9-S1)
  age-all                  curated week-old drift: crlf-mix +
                           hand-line-in-block + stale-bootstrap-failure +
                           foreign-ps1 + rename-tree-file
  restore                  roll every mutated file back to .pre-corrupt

Usage:
    python scripts/adversarial/state-corrupter.py --home <sandbox> --init-demo
    python scripts/adversarial/state-corrupter.py --home <sandbox> --op age-all
    python scripts/adversarial/state-corrupter.py --home <sandbox> --list
"""

from __future__ import annotations

import argparse
import shutil
import sys
import time
from pathlib import Path

SPEC_SCHEMA = "niubash:plugin-spec@0.1.0"
REGISTRY_SCHEMA = "niubash:plugin-source-registry@0.3.0"
BOOTSTRAP_SCHEMA = "niubash:plugin-bootstrap-failures@1"

BEGIN_MARKER = (
    "# >>> niu source oh-my-bash (managed by `niu plugin enable/disable`) >>>"
)
END_MARKER = "# <<< niu source oh-my-bash <<<"

BLOCK_BODY = """\
export OSH='$HOME/.niubash/sources/oh-my-bash'
export OSH_THEME='powerline-multiline'
source "$OSH/oh-my-bash.sh"
"""

RC_TEMPLATE = f"""\
# ~/.niubashrc -- written by niu setup, kept by niu
# user lines live outside the managed markers
alias gs='git status'

{BEGIN_MARKER}
{BLOCK_BODY}{END_MARKER}

# foot: PATH block managed elsewhere
export PATH="$HOME/bin:$PATH"
"""

SPEC_TEMPLATE = f"""\
schema = "{SPEC_SCHEMA}"

[[sources]]
target = "oh-my-bash"
id = "oh-my-bash"
theme = "powerline-multiline"
enable = ["themes/powerline-multiline.themes.sh"]

[[sources]]
target = "rhysd/bash-completion"
id = "bash-completion"
"""

REGISTRY_TEMPLATE = f"""\
schema = "{REGISTRY_SCHEMA}"

[[sources]]
adapter = "oh-my-bash"
checksum_sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
id = "oh-my-bash"
installed_at = "1767139200"
license = "MIT"
path = "~/.niubash/sources/oh-my-bash"
ref = "master"
trusted = true
url = "https://github.com/ohmybash/oh-my-bash"
version = "oh-my-bash master"

[[sources]]
adapter = "wild"
checksum_sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
id = "bash-completion"
installed_at = "1767139200"
license = "GPL-2.0"
path = "~/.niubash/sources/bash-completion"
ref = "main"
trusted = false
url = "https://github.com/rhysd/bash-completion"
version = "bash-completion main"
"""

THEME_FILE = """\
# powerline-multiline (fixture excerpt for the sandbox tree)
function __niu_prompt_segment { :; }
PS1=" \\[\\e[36m\\]\\t \\[\\e[0m\\]\\n❯ "
"""


def rc_path(home: Path) -> Path:
    return home / ".niubashrc"


def niu_dir(home: Path) -> Path:
    return home / ".niubash"


def spec_path(home: Path) -> Path:
    return niu_dir(home) / "plugins.json"


def sources_root(home: Path) -> Path:
    return niu_dir(home) / "sources"


def registry_path(home: Path) -> Path:
    return sources_root(home) / "registry.json"


def omb_tree(home: Path) -> Path:
    return sources_root(home) / "oh-my-bash"


def backup(path: Path) -> None:
    if path.exists() and not Path(str(path) + ".pre-corrupt").exists():
        shutil.copy2(path, str(path) + ".pre-corrupt")


def write_text(path: Path, text: str) -> None:
    backup(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="") as fh:
        fh.write(text)


def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8")


# ── ops ──────────────────────────────────────────────────────────────────

def op_crlf_mix(home: Path, report) -> None:
    """Mixed line endings: a real Windows-editor artifact, niu#176's
    'CRLF whole-file rewrite' shape lives here."""
    path = rc_path(home)
    lines = read_text(path).splitlines()
    out = []
    for i, line in enumerate(lines):
        out.append(line + ("\r\n" if i % 3 == 1 else "\n"))
    write_text(path, "".join(out))
    report("crlf-mix", f"{path} now mixes LF and CRLF (lines 2,5,8... CRLF)")


def op_duplicate_block(home: Path, report) -> None:
    path = rc_path(home)
    text = read_text(path)
    block = f"{BEGIN_MARKER}\n{BLOCK_BODY}{END_MARKER}\n"
    write_text(path, text.rstrip("\n") + "\n\n" + block)
    report("duplicate-block", f"second oh-my-bash managed block appended to {path}")


def op_approximate_marker(home: Path, report) -> None:
    path = rc_path(home)
    text = read_text(path)
    broken = BEGIN_MARKER[:-3]  # trailing '>>>' gone
    write_text(path, text.replace(BEGIN_MARKER, broken, 1))
    report(
        "approximate-marker",
        f"begin marker truncated to '{broken}' (unmanaged second block shape)",
    )


def op_hand_line_in_block(home: Path, report) -> None:
    path = rc_path(home)
    text = read_text(path)
    pos = text.index(END_MARKER)
    write_text(
        path,
        text[:pos] + "alias ll='ls -l'   # mine, keep this\n" + text[pos:],
    )
    report("hand-line-in-block", "user alias line injected inside the managed block")


def op_truncate_rc(home: Path, report) -> None:
    path = rc_path(home)
    lines = read_text(path).splitlines()
    write_text(path, "\n".join(lines[:-4]) + "\n")
    report("truncate-rc", f"{path} cut mid-file (last 4 lines gone)")


def op_corrupt_spec(home: Path, report) -> None:
    path = spec_path(home)
    write_text(path, read_text(path) + '\n{"sources": [ {"target": "oh-my-bash"\n')
    report("corrupt-spec-toml", f"{path} given an unclosed JSON fragment")


def op_stale_bootstrap(home: Path, report) -> None:
    path = sources_root(home) / "bootstrap-failures.toml"
    old = int(time.time()) - 7 * 24 * 3600
    body = (
        f'schema = "{BOOTSTRAP_SCHEMA}"\n\n[[failure]]\n'
        'target = "https://github.com/ohmybash/oh-my-bash"\n'
        'ref = "master"\n'
        'error = "transport failed: schannel: next InitializeSecurityContext '
        'failed: Unknown error (0x80092012) - the revocation function was '
        'unable to check revocation"\n'
        f'at = "{old}"\n'
    )
    write_text(path, body)
    report("stale-bootstrap-failure", f"F5 memo seeded at {path} (dated a week ago)")


def op_rename_tree_file(home: Path, report) -> None:
    src = omb_tree(home) / "oh-my-bash.sh"
    dst = omb_tree(home) / "oh-my-bash.sh.bak"
    if src.exists():
        backup(src)  # restore() can then recreate the original file
        src.rename(dst)
        report("rename-tree-file", f"{src} -> {dst.name} (trusted tree damaged)")
    else:
        report("rename-tree-file", f"SKIP: {src} missing (run --init-demo first)")


def op_delete_tree_git(home: Path, report) -> None:
    git_dir = omb_tree(home) / ".git"
    if git_dir.exists():
        shutil.rmtree(git_dir)
        report("delete-tree-git", f"{git_dir} removed (fingerprint reject shape)")
    else:
        report("delete-tree-git", f"SKIP: {git_dir} already gone")


def op_delete_tree_partial(home: Path, report) -> None:
    themes = omb_tree(home) / "themes"
    if themes.exists():
        shutil.rmtree(themes)
        report("delete-tree-partial", f"{themes} removed (partially-deleted tree)")
    else:
        report("delete-tree-partial", f"SKIP: {themes} already gone")


def op_registry_flip(home: Path, report) -> None:
    path = registry_path(home)
    text = read_text(path)
    flipped = text.replace('adapter = "oh-my-bash"', 'adapter = "bash-it"', 1)
    write_text(path, flipped)
    report(
        "registry-attribution-flip",
        "oh-my-bash record now claims adapter=bash-it (the #168 mechanism)",
    )


def op_registry_truncate(home: Path, report) -> None:
    path = registry_path(home)
    text = read_text(path)
    cut = text.rindex("[[sources]]")
    write_text(path, text[:cut] + "[[sources]]\nadapter = \"wil")
    report("registry-truncate", f"{path} cut mid-record (broken JSON)")


def op_foreign_ps1(home: Path, report) -> None:
    path = rc_path(home)
    poison = (
        "\n# from git-bash (a real user's terminal carries these)\n"
        "PS1='\\[\\e]0;\\u@\\h: \\w\\a\\]$?\\[\\e[32m\\]\\u@\\h\\[\\e[33m\\] \\w'\n"
        "PROMPT_COMMAND='__vte_prompt_command'\n"
    )
    write_text(path, read_text(path) + poison)
    report("foreign-ps1", "Git-Bash PS1 + PROMPT_COMMAND planted in the rc user area")


def op_imperative_era(home: Path, report) -> None:
    path = spec_path(home)
    if path.exists():
        backup(path)
        path.unlink()
        report("imperative-era", f"{path} deleted, registry kept (pre-1.3.1 shape)")
    else:
        report("imperative-era", f"SKIP: {path} already absent")


OPS = {
    "crlf-mix": op_crlf_mix,
    "duplicate-block": op_duplicate_block,
    "approximate-marker": op_approximate_marker,
    "hand-line-in-block": op_hand_line_in_block,
    "truncate-rc": op_truncate_rc,
    "corrupt-spec-toml": op_corrupt_spec,
    "stale-bootstrap-failure": op_stale_bootstrap,
    "rename-tree-file": op_rename_tree_file,
    "delete-tree-git": op_delete_tree_git,
    "delete-tree-partial": op_delete_tree_partial,
    "registry-attribution-flip": op_registry_flip,
    "registry-truncate": op_registry_truncate,
    "foreign-ps1": op_foreign_ps1,
    "imperative-era": op_imperative_era,
}

AGE_ALL = [
    "crlf-mix",
    "hand-line-in-block",
    "stale-bootstrap-failure",
    "foreign-ps1",
    "rename-tree-file",
]


def init_demo(home: Path, report) -> None:
    """Seed a realistic week-old dual-source install (sandbox-only)."""
    write_text(rc_path(home), RC_TEMPLATE)
    write_text(spec_path(home), SPEC_TEMPLATE)
    write_text(registry_path(home), REGISTRY_TEMPLATE)
    tree = omb_tree(home)
    (tree / "themes").mkdir(parents=True, exist_ok=True)
    (tree / "oh-my-bash.sh").write_text("#!/usr/bin/env bash\n# fixture\n", encoding="utf-8")
    (tree / "themes" / "powerline-multiline.themes.sh").write_text(
        THEME_FILE, encoding="utf-8"
    )
    git_dir = tree / ".git"
    git_dir.mkdir(exist_ok=True)
    (git_dir / "HEAD").write_text("ref: refs/heads/master\n", encoding="utf-8")
    tree2 = sources_root(home) / "bash-completion"
    tree2.mkdir(parents=True, exist_ok=True)
    (tree2 / "bash_completion").write_text("# fixture\n", encoding="utf-8")
    report("init-demo", f"week-old dual-source install seeded under {home}")


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--home", required=True, help="sandbox HOME (never a real profile)")
    p.add_argument("--op", action="append", default=[], help="repeatable; see --list")
    p.add_argument("--init-demo", action="store_true")
    p.add_argument("--list", action="store_true")
    args = p.parse_args(argv)

    home = Path(args.home).expanduser().resolve()
    if str(home) in ("", str(Path.home())):
        p.error("refusing to run against the real profile home")

    def report(op: str, msg: str) -> None:
        print(f"[{op}] {msg}")

    if args.list:
        for name in list(OPS) + ["age-all", "restore"]:
            print(name)
        return 0

    if not any([args.init_demo, args.op]):
        p.error("nothing to do: pass --init-demo and/or --op (see --list)")

    if args.init_demo:
        init_demo(home, report)

    for op in args.op:
        if op == "age-all":
            for sub in AGE_ALL:
                OPS[sub](home, report)
            report("age-all", f"curated week-old drift applied: {', '.join(AGE_ALL)}")
        elif op == "restore":
            restored = 0
            for bak in sorted(home.rglob("*.pre-corrupt")):
                orig = Path(str(bak)[: -len(".pre-corrupt")])
                shutil.copy2(bak, orig)
                bak.unlink()
                restored += 1
            report("restore", f"{restored} file(s) rolled back")
        elif op in OPS:
            OPS[op](home, report)
        else:
            p.error(f"unknown op '{op}' (see --list)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
