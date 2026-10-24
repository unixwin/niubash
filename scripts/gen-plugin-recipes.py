#!/usr/bin/env python3
"""Generate the niu plugin recipe seed index (recipes.toml).

The recipe registry is pure data (mason-registry pattern: one registry file,
one row per asset, no behavior code). This script derives the bulk of the
seed — oh-my-bash and bash-it asset inventories — from the corpus
checkouts, plus a hand-curated standalone set (managers, prompts, tools).

Usage:
    python scripts/gen-plugin-recipes.py \
        --corpus D:/repo/rubash/target-ecosys/repos \
        --out crates/niubash-runtime/assets/plugins/recipes.toml

Re-run when the corpus moves; the committed TOML is the artifact.
"""

from __future__ import annotations

import argparse
import re
from pathlib import Path

OMB_REPO = "https://github.com/ohmybash/oh-my-bash"
BASHIT_REPO = "https://github.com/Bash-it/bash-it"

ABOUT_ALIAS = re.compile(r"^about-alias ['\"](.+?)['\"]")
ABOUT_PLUGIN = re.compile(r"^about-plugin ['\"](.+?)['\"]")
ABOUT_COMPLETION = re.compile(r"^about-completion ['\"](.+?)['\"]")

# Hand-curated standalone recipes: the popular bash-family assets that are
# not inside a manager tree. Driver is one of: git (tree source via the
# named adapter kind) or download (direct binary via plugins::download).
STANDALONE = [
    # id, category, summary, license, url, driver, extra
    ("bash-preexec", "plugin",
     "precmd/preexec hook layer for bash (the zsh-preeq equivalent)",
     "MIT", "https://github.com/rcaloras/bash-preexec",
     # Upstream root ships `bash-preexec.sh` (audited 2026-10-02 via the
     # GitHub contents API, wt61/journeyfix G1): the bare name was a seed
     # drift that failed every `distro apply full` at entry validation.
     {"driver": "git", "kind": "generic", "entry": "bash-preexec.sh"}),
    ("liquidprompt", "prompt",
     "adaptive prompt: git/virtualenv/weather-aware, bash 3.2+",
     "AGPL-3.0-only", "https://github.com/liquidprompt/liquidprompt",
     {"driver": "git", "kind": "generic", "entry": "liquidprompt"}),
    ("bash-git-prompt", "prompt",
     "git status prompt line with themes (source gitprompt.sh)",
     "BSD-2-Clause", "https://github.com/magicmonty/bash-git-prompt",
     {"driver": "git", "kind": "generic", "entry": "gitprompt.sh"}),
    ("sexy-bash-prompt", "prompt",
     "minimal colored prompt (`make install` model; info-only in niu)",
     "MIT", "https://github.com/twolfson/sexy-bash-prompt",
     None),
    ("bpkg", "manager",
     "lightweight bash package manager (bpkg install owner/pkg)",
     "MIT", "https://github.com/bpkg/bpkg",
     {"driver": "git", "kind": "generic", "entry": "bpkg.sh"}),
    # niubash#171: curated independent recipes carried by the built-in
    # collections. Every entry audited against its upstream root via the
    # GitHub contents API (wt61 pattern, 2026-10-02); the file-adapter
    # detection needs at least one *.sh/*.bash in the tree, which each
    # origin satisfies (complete-alias through its tests/*.sh).
    ("complete-alias", "completion",
     "automagical alias completion: completes what your aliases expand to (needs bash-completion)",
     "GPL-3.0-only", "https://github.com/cykerway/complete-alias",
     {"driver": "git", "kind": "generic", "entry": "complete_alias"}),
    ("fzf-git.sh", "plugin",
     "fzf key bindings for git objects (files, branches, commits); script only — needs the fzf binary (package-manager install)",
     "MIT", "https://github.com/junegunn/fzf-git.sh",
     {"driver": "git", "kind": "generic", "entry": "fzf-git.sh"}),
    ("bash-sensible", "plugin",
     "sane bash defaults in one sourceable file (vim-sensible for bash)",
     "MIT", "https://github.com/mrzool/bash-sensible",
     {"driver": "git", "kind": "generic", "entry": "sensible.bash"}),
    ("git-flow-completion", "completion",
     "bash completion for git-flow (feature/hotfix/release/support commands)",
     "MIT", "https://github.com/bobthecow/git-flow-completion",
     {"driver": "git", "kind": "generic", "entry": "git-flow-completion.bash"}),
    ("basher", "manager",
     "package manager with per-package bins (PATH + `basher init` model; info-only in niu)",
     "MIT", "https://github.com/basherpm/basher",
     None),
    ("ble.sh", "plugin",
     "line editor: syntax highlight, autosuggest, vim mode for bash (build from source)",
     "BSD-3-Clause", "https://github.com/akinomyoga/ble.sh",
     None),
    ("fzf", "plugin",
     "fuzzy finder with official bash key-bindings and completion",
     "MIT", "https://github.com/junegunn/fzf",
     {"driver": "download", "version": "v0.74.4", "bins": ["fzf"],
      "assets": {
          "windows-x64": ("https://github.com/junegunn/fzf/releases/download/v0.74.4/fzf-0.74.4-windows_amd64.zip",
                          "zip", "5e63c0e798406fcb9c51a9fed4988398e25fdabf8c670e32c16caf7b4a7ed02d"),
          "linux-x64": ("https://github.com/junegunn/fzf/releases/download/v0.74.4/fzf-0.74.4-linux_amd64.tar.gz",
                        "tar-gzip", "05e6813a337cc722c3ed07e54a764b75cc5d671e2e60459db0ba696ee5fa7504"),
      }}),
    ("starship", "prompt",
     "cross-shell prompt: fast, configurable, git/k8s/module aware",
     "ISC", "https://github.com/starship/starship",
     {"driver": "download", "version": "v1.26.0", "bins": ["starship"],
      "assets": {
          "windows-x64": ("https://github.com/starship/starship/releases/download/v1.26.0/starship-x86_64-pc-windows-msvc.zip",
                          "zip", "690021e22f2bf2c57d5867dc0fababe5e55e714c26942bd909ffddb6c5df42f2"),
          "linux-x64": ("https://github.com/starship/starship/releases/download/v1.26.0/starship-x86_64-unknown-linux-gnu.tar.gz",
                        "tar-gzip", "321f0dd7af8340a5f2e6a8fec6538a04f617486f9ec70d878f91c09cd8deef22"),
          "linux-arm64": ("https://github.com/starship/starship/releases/download/v1.26.0/starship-aarch64-unknown-linux-musl.tar.gz",
                          "tar-gzip", "dc30189378d2f2e287384e8a692d3f95ad1df64cf0e8c36aa9201516028aed6b"),
      }}),
    # Application-tool rows (owner ruling 2026-10-03, wpm retraction): the
    # download driver is the only executable-tool install entry, so the
    # PROBED_TOOLS application class gets recipe rows. Digests are measured
    # from the exact release assets (windows rows cross-checked against the
    # WinuxCmd wpm official index); bare-executable upstream releases use
    # archive = "raw".
    ("ripgrep", "plugin",
     "fast recursive grep (rg) with regex defaults and .gitignore respect",
     "MIT OR Unlicense", "https://github.com/BurntSushi/ripgrep",
     {"driver": "download", "version": "15.2.0", "bins": ["rg"],
      "assets": {
          "windows-x64": ("https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-x86_64-pc-windows-msvc.zip",
                          "zip", "71b2fef860abe467217a538ff31de02f5258807c0129f771846f87bd029aafc5", ["ripgrep-15.2.0-x86_64-pc-windows-msvc/rg.exe"]),
          "linux-x64": ("https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-x86_64-unknown-linux-musl.tar.gz",
                        "tar-gzip", "33e15bcf1624b25cdd2a55813a47a2f95dbe126268203e76aa6a585d1e7b149c", ["ripgrep-15.2.0-x86_64-unknown-linux-musl/rg"]),
      }}),
    ("fd", "plugin",
     "fast, user-friendly find (fd) with sane defaults and color output",
     "MIT OR Apache-2.0", "https://github.com/sharkdp/fd",
     {"driver": "download", "version": "v10.4.2", "bins": ["fd"],
      "assets": {
          "windows-x64": ("https://github.com/sharkdp/fd/releases/download/v10.4.2/fd-v10.4.2-x86_64-pc-windows-msvc.zip",
                          "zip", "b2816e506390a89941c63c9187d58a3cc10e9a55f2ef0685f9ea0eccaf7c98c8", ["fd-v10.4.2-x86_64-pc-windows-msvc/fd.exe"]),
          "linux-x64": ("https://github.com/sharkdp/fd/releases/download/v10.4.2/fd-v10.4.2-x86_64-unknown-linux-gnu.tar.gz",
                        "tar-gzip", "def59805cd14b5651b68990855f426ad087f3b96881296d963910431ba3143c8", ["fd-v10.4.2-x86_64-unknown-linux-gnu/fd"]),
      }}),
    ("bat", "plugin",
     "cat(1) clone with syntax highlighting and git integration",
     "MIT OR Apache-2.0", "https://github.com/sharkdp/bat",
     {"driver": "download", "version": "v0.26.1", "bins": ["bat"],
      "assets": {
          "windows-x64": ("https://github.com/sharkdp/bat/releases/download/v0.26.1/bat-v0.26.1-x86_64-pc-windows-msvc.zip",
                          "zip", "0f729b4b6f5f28d395c641eacc2e9ff68d0096b85aa0eec344aa62425144b69b", ["bat-v0.26.1-x86_64-pc-windows-msvc/bat.exe"]),
          "linux-x64": ("https://github.com/sharkdp/bat/releases/download/v0.26.1/bat-v0.26.1-x86_64-unknown-linux-gnu.tar.gz",
                        "tar-gzip", "726f04c8f576a7fd18b7634f1bbf2f915c43494c1c0f013baa3287edb0d5a2a3", ["bat-v0.26.1-x86_64-unknown-linux-gnu/bat"]),
      }}),
    ("eza", "plugin",
     "modern ls replacement with icons, git status, and tree view",
     "MIT", "https://github.com/eza-community/eza",
     {"driver": "download", "version": "v0.23.5", "bins": ["eza"],
      "assets": {
          "windows-x64": ("https://github.com/eza-community/eza/releases/download/v0.23.5/eza.exe_x86_64-pc-windows-gnu.zip",
                          "zip", "c830638c844a5b89d39ba662b5549903a71fa539018e813880f5b8afa77bac2e", ["eza.exe"]),
          "linux-x64": ("https://github.com/eza-community/eza/releases/download/v0.23.5/eza_x86_64-unknown-linux-gnu.tar.gz",
                        "tar-gzip", "35c70c5c43c29108075e58b893234c67ef585f0b53a7eaf8e9e7d4eec9f339b4", ["eza"]),
      }}),
    ("zoxide", "plugin",
     "smarter cd that learns your habits (z/zi with fzf integration)",
     "MIT", "https://github.com/ajeetdsouza/zoxide",
     {"driver": "download", "version": "v0.10.0", "bins": ["zoxide"],
      "assets": {
          "windows-x64": ("https://github.com/ajeetdsouza/zoxide/releases/download/v0.10.0/zoxide-0.10.0-x86_64-pc-windows-msvc.zip",
                          "zip", "f465ae548f8754c8e7edbc60b45fbf58c92bfe123db83d790252d6810fa5daf1", ["zoxide.exe"]),
          "linux-x64": ("https://github.com/ajeetdsouza/zoxide/releases/download/v0.10.0/zoxide-0.10.0-x86_64-unknown-linux-musl.tar.gz",
                        "tar-gzip", "2d93385b99f3e82cf2701609a1bffcad863fbeb75aa3fe7eb6be4d29be68b1ae", ["zoxide"]),
      }}),
    ("dust", "plugin",
     "du + tree: intuitive disk usage analyzer",
     "Apache-2.0", "https://github.com/bootandy/dust",
     {"driver": "download", "version": "v1.2.4", "bins": ["dust"],
      "assets": {
          "windows-x64": ("https://github.com/bootandy/dust/releases/download/v1.2.4/dust-v1.2.4-x86_64-pc-windows-msvc.zip",
                          "zip", "eb08d642f016787bb9fc918a4dc5f34665463657fddf83a40f2441cbf020fb4c", ["dust-v1.2.4-x86_64-pc-windows-msvc/dust.exe"]),
          "linux-x64": ("https://github.com/bootandy/dust/releases/download/v1.2.4/dust-v1.2.4-x86_64-unknown-linux-gnu.tar.gz",
                        "tar-gzip", "707cfdbfb9d2dc536f8c3853815bbe98a01012f2772463835edae06816551160", ["dust-v1.2.4-x86_64-unknown-linux-gnu/dust"]),
      }}),
    ("duf", "plugin",
     "disk usage/free utility with a clean tabular display",
     "MIT", "https://github.com/muesli/duf",
     {"driver": "download", "version": "v0.9.1", "bins": ["duf"],
      "assets": {
          "windows-x64": ("https://github.com/muesli/duf/releases/download/v0.9.1/duf_0.9.1_windows_x86_64.zip",
                          "zip", "503934be81f847d9ddb1b739834217480633435ad16515dd199e372c0b2e1afc", ["duf.exe"]),
          "linux-x64": ("https://github.com/muesli/duf/releases/download/v0.9.1/duf_0.9.1_linux_x86_64.tar.gz",
                        "tar-gzip", "5add851e7062c5e56939abb664705e4d14fa2d06289490aff31d51f153832de7", ["duf"]),
      }}),
    ("erdtree", "plugin",
     "modern tree/disk-usage hybrid (erd) with parallel traversal",
     "MIT", "https://github.com/solidiquis/erdtree",
     {"driver": "download", "version": "v3.1.2", "bins": ["erd"],
      "assets": {
          "windows-x64": ("https://github.com/solidiquis/erdtree/releases/download/v3.1.2/erd-v3.1.2-x86_64-pc-windows-msvc.exe",
                          "raw", "df359e20e5a38384c27b98667d780c5bd5ab1ce4bac739a5fa9c5ed25511a0a1", ["erd.exe"]),
          "linux-x64": ("https://github.com/solidiquis/erdtree/releases/download/v3.1.2/erd-v3.1.2-x86_64-unknown-linux-gnu.tar.gz",
                        "tar-gzip", "9354667bc1ef744cb363604d4eb5b6784205b7fb1c283f4c0f9d78e3ad07e42f", ["erd"]),
      }}),
    ("direnv", "plugin",
     "per-directory environment loader (un/ad-hoc env on cd)",
     "MIT", "https://github.com/direnv/direnv",
     {"driver": "download", "version": "v2.37.1", "bins": ["direnv"],
      "assets": {
          "windows-x64": ("https://github.com/direnv/direnv/releases/download/v2.37.1/direnv.windows-amd64",
                          "raw", "d96fc8b7cf020c2d4c1dbbc2ccec5fd1cab05b51c491f02c8527a7fa6c50a1cd", ["direnv.exe"]),
          "linux-x64": ("https://github.com/direnv/direnv/releases/download/v2.37.1/direnv.linux-amd64",
                        "raw", "1f1b93dd6f38523fde26dfac96151ef9d31a374e3005cd3345fb93555ae0c9b5", ["direnv"]),
      }}),
    ("niugit", "plugin",
     "native Windows git without MSYS (niu-git build); the setup wizard's git pick",
     "GPL-2.0-only", "https://github.com/unixwin/niu-git",
     {"driver": "download", "version": "v2.56.0.2", "bins": ["git.exe"],
      "assets": {
          "windows-x64": ("https://github.com/unixwin/niu-git/releases/download/v2.56.0.2/niu-git-2.56.0.windows.3-x64.zip",
                          "zip", "fce603d3c9a3e8119a3df1c559322651e53185eecce4c90a8efe36844bd268dc", ["git.exe"]),
      }}),
    ("thefuck", "plugin",
     "fixes your previous command (thefuck); no binary release — install via pip or a package manager",
     "MIT", "https://github.com/nvbn/thefuck",
     None),
]


def toml_str(value: str) -> str:
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def first_summary(path: Path, pattern: re.Pattern) -> str | None:
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    for line in text.splitlines():
        line = line.strip()
        match = pattern.match(line)
        if match:
            return match.group(1)
    return None


def emit(out, recipe: dict) -> None:
    out.append("[[recipe]]")
    for key in ("id", "category", "summary", "license", "url"):
        if key in recipe:
            out.append(f"{key} = {toml_str(recipe[key])}")
    if recipe.get("driver"):
        out.append(f"driver = {toml_str(recipe['driver'])}")
    if recipe.get("kind"):
        out.append(f"kind = {toml_str(recipe['kind'])}")
    if recipe.get("entry"):
        out.append(f"entry = {toml_str(recipe['entry'])}")
    if recipe.get("version"):
        out.append(f"version = {toml_str(recipe['version'])}")
    if recipe.get("manager"):
        out.append(f"manager = {toml_str(recipe['manager'])}")
    if recipe.get("asset_kind"):
        out.append(f"asset_kind = {toml_str(recipe['asset_kind'])}")
    if recipe.get("asset"):
        out.append(f"asset = {toml_str(recipe['asset'])}")
    if recipe.get("origin"):
        out.append(f"origin = {toml_str(recipe['origin'])}")
    bins = recipe.get("bins") or []
    if bins:
        out.append("bins = [" + ", ".join(toml_str(b) for b in bins) + "]")
    # Asset rows: (url, archive, sha256) or (url, archive, sha256, bins) —
    # per-platform bins override the recipe-level `bins` when archives nest
    # the binary under different directory names per platform (ripgrep,
    # fd, bat, dust) or ship bare executables (raw assets).
    for platform, asset in (recipe.get("assets") or {}).items():
        url, archive, sha = asset[0], asset[1], asset[2]
        asset_bins = list(asset[3]) if len(asset) > 3 else None
        out.append(f"[recipe.downloads.{platform}]")
        out.append(f"url = {toml_str(url)}")
        out.append(f"archive = {toml_str(archive)}")
        if sha:
            out.append(f"sha256 = {toml_str(sha)}")
        if asset_bins:
            out.append("bins = [" + ", ".join(toml_str(b) for b in asset_bins) + "]")
    out.append("")


def omb_assets(corpus: Path, out: list[str]) -> tuple[int, set[str]]:
    root = corpus / "oh-my-bash"
    count = 0
    seen: set[str] = set()
    kinds = (
        ("themes", "theme", lambda d, n: d / f"{n}.theme.sh",
         lambda p, n: f"oh-my-bash theme {n}"),
        ("plugins", "plugin", lambda d, n: d / f"{n}.plugin.sh",
         lambda p, n: first_summary(p, ABOUT_PLUGIN) or f"oh-my-bash plugin {n}"),
        ("aliases", "alias", None, None),
        ("completions", "completion", None, None),
    )
    for subdir, kind, script_path, summarize in kinds:
        directory = root / subdir
        if not directory.is_dir():
            continue
        for entry in sorted(directory.iterdir()):
            name = None
            path = None
            if kind == "theme":
                if entry.is_dir() and (entry / f"{entry.name}.theme.sh").is_file():
                    name, path = entry.name, entry / f"{entry.name}.theme.sh"
            elif kind == "plugin":
                if entry.is_dir() and (entry / f"{entry.name}.plugin.sh").is_file():
                    name, path = entry.name, entry / f"{entry.name}.plugin.sh"
            elif kind == "alias":
                if entry.is_file():
                    stem = entry.name
                    for suffix in (".aliases.sh", ".aliases.bash"):
                        if stem.endswith(suffix):
                            name, path = stem[: -len(suffix)], entry
                            break
            elif kind == "completion":
                if entry.is_file():
                    stem = entry.name
                    for suffix in (".completion.sh", ".completion.bash"):
                        if stem.endswith(suffix):
                            name, path = stem[: -len(suffix)], entry
                            break
            if not name or not path:
                continue
            recipe_id = f"omb-{kind}-{name}"
            if recipe_id in seen:
                continue
            seen.add(recipe_id)
            summary = summarize(path, name) if summarize else f"oh-my-bash {kind} {name}"
            emit(out, {
                "id": recipe_id,
                "category": kind,
                "summary": summary,
                "license": "MIT",
                "url": f"{OMB_REPO}/tree/master/{subdir}/{name}",
                "manager": "oh-my-bash",
                "asset_kind": kind,
                "asset": name,
            })
            count += 1
    return count, seen


def bash_it_assets(corpus: Path, out: list[str]) -> tuple[int, set[str]]:
    root = corpus / "bash-it"
    count = 0
    seen: set[str] = set()
    # aliases / plugins / completion: available/<name>.<kind>.bash
    table = (
        ("aliases", "alias", ".aliases.bash", ABOUT_ALIAS),
        ("plugins", "plugin", ".plugin.bash", ABOUT_PLUGIN),
        ("completion", "completion", ".completion.bash", ABOUT_COMPLETION),
    )
    for subdir, kind, suffix, pattern in table:
        directory = root / subdir / "available"
        if not directory.is_dir():
            continue
        for entry in sorted(directory.iterdir()):
            if not entry.is_file():
                continue
            stem = entry.name
            if not stem.endswith(suffix):
                continue
            name = stem[: -len(suffix)]
            recipe_id = f"bashit-{kind}-{name}"
            if recipe_id in seen:
                continue
            seen.add(recipe_id)
            summary = first_summary(entry, pattern) or f"bash-it {kind} {name}"
            emit(out, {
                "id": recipe_id,
                "category": kind,
                "summary": summary,
                "license": "MIT",
                "url": f"{BASHIT_REPO}/tree/master/{subdir}/available/{entry.name}",
                "manager": "bash-it",
                "asset_kind": kind,
                "asset": name,
            })
            count += 1
    # themes: themes/<name>/<name>.theme.bash
    directory = root / "themes"
    if directory.is_dir():
        for entry in sorted(directory.iterdir()):
            if entry.is_dir() and (entry / f"{entry.name}.theme.bash").is_file():
                recipe_id = f"bashit-theme-{entry.name}"
                if recipe_id in seen:
                    continue
                seen.add(recipe_id)
                emit(out, {
                    "id": recipe_id,
                    "category": "theme",
                    "summary": f"bash-it theme {entry.name}",
                    "license": "MIT",
                    "url": f"{BASHIT_REPO}/tree/master/themes/{entry.name}",
                    "manager": "bash-it",
                    "asset_kind": "theme",
                    "asset": entry.name,
                })
                count += 1
    return count, seen


def standalone(out: list[str]) -> int:
    count = 0
    for recipe_id, category, summary, license_, url, extra in STANDALONE:
        recipe = {
            "id": recipe_id,
            "category": category,
            "summary": summary,
            "license": license_,
            "url": url,
        }
        if extra:
            for key in ("driver", "kind", "entry", "version", "bins", "assets"):
                if key in extra:
                    recipe[key] = extra[key]
            if extra.get("driver") == "git":
                recipe["origin"] = (url + ".git") if "github.com" in url else url
        emit(out, recipe)
        count += 1
    return count


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    header = [
        "# niubash plugin recipe seed index (generated — do not hand-edit bulk rows).",
        "# Regenerate: python scripts/gen-plugin-recipes.py --corpus <ecosys-repos> --out <this file>",
        "#",
        "# Data shape (mason-registry pattern; see docs/planning/lazy-family-source-study.md §6):",
        "#   id          unique recipe id (`niu plugin add <id>`)",
        "#   category    manager | theme | plugin | alias | completion | prompt",
        "#   driver      git (tree source) | download (direct binary) | absent (info-only)",
        "#   manager/asset_kind/asset  asset recipes riding on a manager source",
        "#   kind/entry  source adapter kind + entry file (generic sources)",
        "#   downloads.<platform>  per-platform download rows (url/archive/sha256)",
        "#",
        "# Manager rows for oh-my-bash / bash-it / bash-completion are derived from",
        "# the compiled-in adapters at runtime (single source of truth); this file",
        "# only carries their per-asset rows and standalone ecosystem entries.",
        "",
    ]

    out: list[str] = list(header)
    omb_count, _ = omb_assets(args.corpus, out)
    bashit_count, _ = bash_it_assets(args.corpus, out)
    standalone_count = standalone(out)

    text = "\n".join(out).rstrip() + "\n"
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(text, encoding="utf-8", newline="\n")
    print(
        f"wrote {args.out}: {omb_count} oh-my-bash, {bashit_count} bash-it, "
        f"{standalone_count} standalone recipes"
    )


if __name__ == "__main__":
    main()
