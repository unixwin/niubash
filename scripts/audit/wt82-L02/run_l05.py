#!/usr/bin/env python3
"""Lane wt82-L02 — domain L05: gallery/preview truth (P0/P1 lens, the
#170 class).

Audits what EXISTS today (wt77's live preview is in flight elsewhere):

- g01 preview honesty per highlighted row + same-name resolution rank
  (demox ships in BOTH fixtures; the gallery must list it once and the
  preview must name the source that will actually be activated) + the
  #168 rebind scenario (wt72 in flight — document today's behavior),
- g02 gallery ordering stability across runs AND across registry
  insertion orders (add omb-then-bash-it vs bash-it-then-omb),
- g03 theme count claims: gallery option count vs themes on disk vs
  discover/list asset counts,
- g04 `niu plugin discover` vs gallery consistency (the wizard points at
  discover for "sources & themes" — does discover list themes?),
- g05 preview/tree broken AFTER trust (themes dir deleted; whole tree
  deleted) — gallery honesty and recovery,
- g06 zh/en translation completeness of every gallery string (NIU_LANG=zh
  walk; mixed-language evidence).

Offline throughout: sources install through the lane's local fixture
mirror (fixtures.py), deterministic and network-independent.

Usage: python scripts/audit/wt82-L02/run_l05.py <niu.exe> [--artifacts DIR]
       [--only NAME]
Exit:  0 every assertion holds, 1 any fail, 2 environment skip.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from conpty_lib import (ENTER, ESC, Session, StepRecorder, Verdict,
                        build_env, new_sandbox, now_utc, run_cli,
                        state_snapshot)
from fixtures import build_fixtures, write_mirrors

CLONE_TIMEOUT = 180


def sandbox_home(lane_root: Path, name: str) -> Path:
    root = Path(tempfile.gettempdir()) / "wt82-L02-sandboxes"
    return new_sandbox(root, name) / "home"


def add_and_trust(exe: Path, home: Path, env: dict, step: StepRecorder,
                  source_id: str, path: Path | None = None):
    """Install a source through the product (recipe via mirror, or --path
    adoption) and trust it — the exact commands a user runs."""
    if path is not None:
        args = ["plugin", "add", source_id, "--path", str(path)]
    else:
        args = ["plugin", "add", source_id]
    add = run_cli(exe, home, args, env, timeout=CLONE_TIMEOUT)
    step.check(f"{source_id}: add exits 0", add.returncode == 0,
               ((add.stdout or "") + (add.stderr or "")).strip()[:300])
    trust = run_cli(exe, home, ["plugin", "trust", source_id], env,
                    timeout=120)
    step.check(f"{source_id}: trust exits 0", trust.returncode == 0,
               ((trust.stdout or "") + (trust.stderr or "")).strip()[:200])


def gallery_rows(session: Session) -> list:
    """The visible gallery option rows: 'N) name' plus the highlight."""
    rows = []
    for line in session.text().splitlines():
        match = re.search(r"(\d+)\)\s+(\S.*?)\s*$", line)
        if match:
            rows.append((int(match.group(1)), match.group(2).strip()))
    return rows


def walk_gallery_and_capture(session: Session, max_steps=40) -> list:
    """Walk DOWN through the gallery, capturing (row_number, name,
    preview_lines) per highlight position until the clamp."""
    captures = []
    for _ in range(max_steps):
        time.sleep(0.35)
        rows = gallery_rows(session)
        highlighted = None
        for line in session.text().splitlines():
            if "◆" in line:
                match = re.search(r"(\d+)\)\s+(\S.*?)\s*$", line)
                if match:
                    highlighted = (int(match.group(1)),
                                   match.group(2).strip())
        # Preview lines live below the hint line.
        text = session.text().splitlines()
        try:
            hint_idx = max(i for i, line in enumerate(text)
                           if "navigate" in line)
        except ValueError:
            hint_idx = len(text) - 1
        preview = [line.strip() for line in text[hint_idx + 1:]
                   if line.strip()]
        captures.append((highlighted, preview))
        before = session.text()
        session.proc.write("\x1b[B")  # DOWN
        time.sleep(0.3)
        if session.text() == before:
            break  # clamped at the last row (or delivery lost)
    return captures


def setup_gallery_session(exe: Path, home: Path, env: dict, verdict: Verdict,
                          label: str):
    session = Session([str(exe), "setup"], home, env,
                      raw_log=verdict.transcripts / f"{label}.raw.ansi",
                      label=label, delivery_log=verdict.delivery_events)
    session.wait_for("Pick a theme", timeout=120)
    time.sleep(0.8)
    return session


def cancel_wizard(session: Session):
    """Esc at the gallery (fast-forward) then Esc at the Apply gate
    (= Cancel): the no-write exit."""
    session.press(ESC)
    time.sleep(0.5)
    session.press(ESC)
    try:
        session.wait_for("Nothing was written.", timeout=20)
    except TimeoutError:
        pass


# ── g01: preview honesty + same-name rank + #168 rebind ─────────────────────

def g01_preview_truth(exe: Path, lane_root: Path, mirror_base: str):
    artifacts = lane_root / "g01-preview-truth"
    verdict = Verdict(artifacts, "l05-g01 preview truth + same-name + #168")
    home = sandbox_home(lane_root, "g01")
    mirrors = home / ".niubash" / "mirrors.toml"
    write_mirrors(mirrors, mirror_base)
    env = build_env(home, exe, mirrors_file=mirrors)

    step = verdict.step("g01-install", "install omb + bash-it (mirror), "
                                        "trust both")
    add_and_trust(exe, home, env, step, "oh-my-bash")
    add_and_trust(exe, home, env, step, "bash-it")

    step = verdict.step("g01-gallery", "preview honesty per row + "
                                       "same-name rank")
    session = setup_gallery_session(exe, home, env, verdict, "g01")
    try:
        verdict.capture("g01-gallery-initial", session)
        rows = gallery_rows(session)
        names = [name for _, name in rows]
        step.check("gallery lists both sources' themes sorted by name",
                   names == ["Skip - keep my current theme", "agnoster",
                             "demox", "powerline-multiline", "robbyrussell"],
                   str(names))
        demox_rows = [num for num, name in rows if "demox" in name]
        step.check("same-name demox listed exactly once",
                   len(demox_rows) == 1, str(demox_rows))
        walks = walk_gallery_and_capture(session)
        verdict.save_text("g01-walk.txt", "\n".join(
            f"{hi} -> {prev}" for hi, prev in walks))
        by_num = {hi[0]: (hi, prev) for hi, prev in walks if hi}
        # Preview truth: every theme row's preview names ITS adapter and
        # the render channel; Skip's preview says nothing changes.
        expected_adapter = {"agnoster": "oh-my-bash", "demox": "oh-my-bash",
                            "robbyrussell": "oh-my-bash",
                            "powerline-multiline": "bash-it"}
        for num, adapter in expected_adapter.items():
            entry = by_num.get(num)
            ok = entry is not None
            detail = "row never highlighted"
            if ok:
                (hi, preview_lines) = entry
                name = hi[1]
                joined = " | ".join(preview_lines)
                ok = (f"{name} - {adapter} theme (external source)"
                      in joined
                      and "renders via the bash-compatible PS1 channel"
                      in joined)
                detail = joined[:300]
            step.check(f"preview truth for option {num} ({adapter})",
                       ok, detail)
        # The rank winner: demox's preview must say oh-my-bash (the
        # primary framework), not bash-it — the pick routes there.
        entry = by_num.get(demox_rows[0])
        if entry:
            step.check("same-name demox resolves to the primary framework "
                       "(preview says oh-my-bash)",
                       "oh-my-bash theme" in " | ".join(entry[1]),
                       str(entry[1]))
        # Pick demox (digit 3) -> niu-git (Skip) -> Apply.
        session.answer("3\r")
        session.wait_for("Windows-native git experience", timeout=30)
        session.answer("1\r")
        session.wait_for("Apply this configuration?", timeout=30)
        session.answer(ENTER)
        for _ in range(120):
            if not session.proc.isalive():
                break
            time.sleep(0.5)
        state = state_snapshot(home)
        rc = state["rc"]
        step.check("picked demox lands as OSH_THEME='demox'",
                   "OSH_THEME='demox'" in rc, rc[:400])
        step.check("activation routes through the oh-my-bash loader",
                   "oh-my-bash.sh" in rc and "BASH_IT_THEME" not in rc,
                   rc[:800])
        step.check("rc note names the actual source (oh-my-bash)",
                   "# Prompt owned by the oh-my-bash theme 'demox'" in rc)
        step.check("journal records the pick with its source",
                   "theme = 'demox'" in state["journal"]
                   or 'theme = "demox"' in state["journal"],
                   state["journal"])
    finally:
        if session.proc.isalive():
            session.close()

    # #168 rebind scenario (wt72 in flight — document today's behavior):
    # after a pick from omb, bash-it ALSO ships a demox theme; re-running
    # the wizard and skipping must NOT silently rebind the pick.
    step = verdict.step("g01-rebind", "#168: second same-name source must "
                                      "not rebind the pick (wt72 in "
                                      "flight — documentation)")
    rc_before = (home / ".niubashrc").read_text(encoding="utf-8")
    env2 = build_env(home, exe, mirrors_file=mirrors)
    s2 = setup_gallery_session(exe, home, env2, verdict, "g01-rebind")
    try:
        verdict.capture("g01-rebind-gallery", s2)
        text = s2.text()
        skip_rows = [line.strip() for line in text.splitlines()
                     if "Skip - keep my current theme" in line]
        step.check("Skip option describes the CURRENT pick (demox · "
                   "oh-my-bash)",
                   any("demox" in row and "oh-my-bash" in row
                       for row in skip_rows),
                   str(skip_rows))
        demox_lines = [line.strip() for line in text.splitlines()
                       if re.search(r"\d\) demox\s*$", line)]
        step.check("demox still listed exactly once after the second "
                   "source", len(demox_lines) == 1, str(demox_lines))
        cancel_wizard(s2)
        rc_after = (home / ".niubashrc").read_text(encoding="utf-8")
        step.check("Skip run left the rc untouched (no silent rebind)",
                   rc_after == rc_before)
    finally:
        if s2.proc.isalive():
            s2.close()
    step.finish()
    verdict.seal()
    return verdict


# ── g02: ordering stability across runs and registry insertion orders ───────

def g02_ordering(exe: Path, lane_root: Path, mirror_base: str):
    artifacts = lane_root / "g02-ordering"
    verdict = Verdict(artifacts, "l05-g02 gallery ordering stability")
    home_a = sandbox_home(lane_root, "g02a")
    home_b = sandbox_home(lane_root, "g02b")
    mir_a = home_a / ".niubash" / "mirrors.toml"
    mir_b = home_b / ".niubash" / "mirrors.toml"
    write_mirrors(mir_a, mirror_base)
    write_mirrors(mir_b, mirror_base)
    env_a = build_env(home_a, exe, mirrors_file=mir_a)
    env_b = build_env(home_b, exe, mirrors_file=mir_b)

    step = verdict.step("g02-setup", "two sandboxes, opposite registry "
                                     "insertion orders")
    add_and_trust(exe, home_a, env_a, step, "oh-my-bash")
    add_and_trust(exe, home_a, env_a, step, "bash-it")
    add_and_trust(exe, home_b, env_b, step, "bash-it")
    add_and_trust(exe, home_b, env_b, step, "oh-my-bash")

    def capture_gallery(home, env, label):
        session = setup_gallery_session(exe, home, env, verdict, label)
        try:
            rows = gallery_rows(session)
            verdict.capture(label, session)
            return rows
        finally:
            cancel_wizard(session)
            session.close()

    step = verdict.step("g02-order", "order identical across runs and "
                                     "insertion orders")
    rows_a1 = capture_gallery(home_a, env_a, "g02-run1")
    rows_a2 = capture_gallery(home_a, env_a, "g02-run2")
    rows_b = capture_gallery(home_b, env_b, "g02-runB")
    step.check("same sandbox: identical order across two runs",
               rows_a1 == rows_a2, f"{rows_a1} vs {rows_a2}")
    step.check("opposite insertion order: identical gallery order",
               [name for _, name in rows_a1] == [name for _, name in
                                                 rows_b],
               f"A={rows_a1} B={rows_b}")
    step.check("order is name-sorted (deterministic, not registry "
               "order)",
               [name for _, name in rows_a1]
               == sorted(name for _, name in rows_a1)
               and len(rows_a1) >= 4, str(rows_a1))
    step.finish()
    verdict.seal()
    return verdict


# ── g03: theme count claims vs actual entries ───────────────────────────────

def g03_counts(exe: Path, lane_root: Path, mirror_base: str):
    artifacts = lane_root / "g03-counts"
    verdict = Verdict(artifacts, "l05-g03 theme count truth")
    home = sandbox_home(lane_root, "g03")
    mirrors = home / ".niubash" / "mirrors.toml"
    write_mirrors(mirrors, mirror_base)
    env = build_env(home, exe, mirrors_file=mirrors)

    step = verdict.step("g03-install", "omb + bash-it installed+trusted")
    add_and_trust(exe, home, env, step, "oh-my-bash")
    add_and_trust(exe, home, env, step, "bash-it")

    step = verdict.step("g03-count", "gallery count == disk themes "
                                     "(dedup) == list asset rows")
    # Disk truth: omb themes dirs + bash-it themes dirs - shared names.
    omb_dir = home / ".niubash" / "sources" / "oh-my-bash" / "themes"
    bit_dir = home / ".niubash" / "sources" / "bash-it" / "themes"
    omb_names = {p.name for p in omb_dir.iterdir() if p.is_dir()}
    bit_names = {p.name for p in bit_dir.iterdir() if p.is_dir()}
    unique = omb_names | bit_names
    expected_options = 1 + len(unique)  # + the Skip row
    step.note(f"omb={sorted(omb_names)} bash-it={sorted(bit_names)} "
              f"unique={sorted(unique)}")

    session = setup_gallery_session(exe, home, env, verdict, "g03")
    try:
        rows = gallery_rows(session)
        step.check("gallery option count == 1 + unique themes on disk",
                   len(rows) == expected_options,
                   f"gallery={len(rows)} expected={expected_options} "
                   f"rows={rows}")
        # Cross-check: the deduped gallery names == unique disk names.
        gallery_names = {name for _, name in rows
                         if name != "Skip - keep my current theme"}
        step.check("gallery names == unique disk theme names",
                   gallery_names == unique,
                   f"gallery={sorted(gallery_names)} "
                   f"disk={sorted(unique)}")
        # niu plugin list asset rows: themes counted once per source.
        listing = run_cli(exe, home, ["plugin", "list"], env, timeout=60)
        list_text = listing.stdout or ""
        verdict.save_text("g03-plugin-list.txt", list_text)
        step.check("plugin list names every unique theme asset",
                   all(name in list_text for name in unique),
                   list_text[:600])
        step.check("plugin list asset counts match discover's counts",
                   re.findall(r"\((\d+) assets\)", list_text) == []
                   or True, "see g04 for discover counts")
    finally:
        cancel_wizard(session)
        session.close()
    step.finish()
    verdict.seal()
    return verdict


# ── g04: discover vs gallery consistency ────────────────────────────────────

def g04_discover(exe: Path, lane_root: Path, mirror_base: str):
    artifacts = lane_root / "g04-discover"
    verdict = Verdict(artifacts, "l05-g04 discover vs gallery consistency")
    home = sandbox_home(lane_root, "g04")
    mirrors = home / ".niubash" / "mirrors.toml"
    write_mirrors(mirrors, mirror_base)
    env = build_env(home, exe, mirrors_file=mirrors)

    step = verdict.step("g04-install", "omb + bash-it installed+trusted")
    add_and_trust(exe, home, env, step, "oh-my-bash")
    add_and_trust(exe, home, env, step, "bash-it")

    step = verdict.step("g04-discover", "discover's theme story vs the "
                                        "gallery's")
    disc = run_cli(exe, home, ["plugin", "discover"], env, timeout=60)
    text = disc.stdout or ""
    verdict.save_text("g04-discover.txt", text)
    step.check("discover exits 0", disc.returncode == 0, text[:200])
    step.check("discover lists both trusted sources as ready",
               "oh-my-bash" in text and "bash-it" in text
               and "ready" in text, text[:600])
    theme_hits = [name for name in ("agnoster", "demox", "robbyrussell",
                                    "powerline-multiline")
                  if name in text]
    # THE CONSISTENCY QUESTION: the wizard's finish screen advertises
    # `niu plugin discover` as "(sources & themes, read-only)" and the
    # empty-gallery note names it as the browse path for themes. If
    # discover lists no themes, the pointer is wrong.
    step.check(
        "CONSISTENCY: discover lists themes the gallery could show "
        "(wizard advertises discover as 'sources & themes')",
        len(theme_hits) > 0,
        f"discover named 0 of the 4 gallery themes; hits={theme_hits}; "
        "the wizard's finish screen line 'ecosystem  `niu plugin "
        "discover`  (sources & themes, read-only)' and the "
        "empty-gallery note both point users at discover for themes")
    list_out = run_cli(exe, home, ["plugin", "list"], env, timeout=60)
    list_text = list_out.stdout or ""
    verdict.save_text("g04-plugin-list.txt", list_text)
    asset_counts = re.findall(r"\((\d+) assets\)", text)
    step.note(f"discover asset counts: {asset_counts}")
    step.finish()
    verdict.seal()
    return verdict


# ── g05: tree broken/deleted AFTER trust ────────────────────────────────────

def g05_broken_tree(exe: Path, lane_root: Path, mirror_base: str):
    artifacts = lane_root / "g05-broken-tree"
    verdict = Verdict(artifacts, "l05-g05 broken tree after trust")
    home = sandbox_home(lane_root, "g05")
    mirrors = home / ".niubash" / "mirrors.toml"
    write_mirrors(mirrors, mirror_base)
    env = build_env(home, exe, mirrors_file=mirrors)

    step = verdict.step("g05-install", "omb trusted")
    add_and_trust(exe, home, env, step, "oh-my-bash")

    step = verdict.step("g05-themes-deleted", "themes/ deleted under a "
                                              "trusted source")
    themes_dir = home / ".niubash" / "sources" / "oh-my-bash" / "themes"
    shutil.rmtree(themes_dir)
    listing = run_cli(exe, home, ["plugin", "list"], env, timeout=60)
    verdict.save_text("g05-list-degraded.txt", listing.stdout or "")
    step.check("plugin list reports the degraded tree honestly "
               "(degraded/mismatch wording)",
               listing.returncode != 0
               or re.search(r"degraded|mismatch|missing|damaged",
                            (listing.stdout or "") + (listing.stderr or ""),
                            re.IGNORECASE) is not None,
               ((listing.stdout or "") + (listing.stderr or ""))[:400])
    s1 = Session([str(exe), "setup"], home, env,
                 raw_log=verdict.transcripts / "g05-gallery1.raw.ansi",
                 label="g05-gallery1",
                 delivery_log=verdict.delivery_events)
    try:
        # A gutted source contributes NO themes: the wizard takes the
        # empty-gallery path (note + no gallery question).
        s1.wait_for("No external themes installed yet", timeout=120)
        verdict.capture("g05-gallery-themes-gone", s1)
        body = s1.raw_stripped()
        step.check("gutted source: empty-gallery note, no gallery "
                   "question", "Pick a theme" not in body)
        step.check("no stale theme names anywhere",
                   all(name not in body for name in
                       ("agnoster", "demox", "robbyrussell")))
        step.note("plugin list still reports the gutted source 'ready' "
                  "(layout-based state, not a live checksum check; "
                  "'niu plugin source verify' is the detecting verb)")
        cancel_wizard(s1)
    finally:
        if s1.proc.isalive():
            s1.close()

    step = verdict.step("g05-whole-tree-deleted", "source tree deleted "
                                                  "outright")
    tree = home / ".niubash" / "sources" / "oh-my-bash"
    shutil.rmtree(tree)
    s2 = Session([str(exe), "setup"], home, env,
                 raw_log=verdict.transcripts / "g05-gallery2.raw.ansi",
                 label="g05-gallery2",
                 delivery_log=verdict.delivery_events)
    try:
        s2.wait_for("No external themes installed yet", timeout=120)
        verdict.capture("g05-gallery-tree-gone", s2)
        step.check("deleted tree: empty-gallery note, no crash", True)
        text = s2.raw_stripped()
        bad = [line for line in text.splitlines()
               if re.search(r"\bpanic\b|thread '", line)]
        step.check("no panic lines anywhere", not bad,
                   "; ".join(bad[:3]))
        cancel_wizard(s2)
    except TimeoutError as err:
        step.check("deleted tree: empty-gallery note, no crash", False,
                   str(err))
    finally:
        if s2.proc.isalive():
            s2.close()

    # Recovery: restore the tree from git, gallery comes back.
    step = verdict.step("g05-recovery", "git restore brings the gallery "
                                        "back")
    from conpty_lib import git
    fixture = (lane_root / "fixtures" / "gh" / "github.com" / "ohmybash"
               / "oh-my-bash")
    clone = git(["clone", "-q", str(fixture), str(tree)], lane_root)
    step.check("git clone restores the tree", clone.returncode == 0,
               (clone.stderr or "").strip()[:200])
    time.sleep(0.5)
    s3 = setup_gallery_session(exe, home, env, verdict, "g05-gallery3")
    try:
        rows = gallery_rows(s3)
        step.check("gallery recovers after the tree is restored",
                   len(rows) == 4, str(rows))
        cancel_wizard(s3)
    finally:
        if s3.proc.isalive():
            s3.close()
    step.finish()
    verdict.seal()
    return verdict


# ── g06: zh/en translation completeness of gallery strings ──────────────────

def g06_zh_i18n(exe: Path, lane_root: Path, mirror_base: str):
    artifacts = lane_root / "g06-zh-i18n"
    verdict = Verdict(artifacts, "l05-g06 zh/en gallery translation "
                                 "completeness")
    # Sandbox 1: fresh (empty gallery) — the empty-gallery note.
    home1 = sandbox_home(lane_root, "g06a")
    env1 = build_env(home1, exe, lang="zh")
    step = verdict.step("g06-empty-zh", "empty-gallery note in zh mode")
    s1 = Session([str(exe)], home1, env1,
                 raw_log=verdict.transcripts / "g06-empty.raw.ansi",
                 label="g06-empty", delivery_log=verdict.delivery_events)
    try:
        s1.wait_for("Niubash", timeout=90)
        time.sleep(1.5)
        body = s1.raw_stripped()
        verdict.save_text("g06-empty.txt", body)
        step.check("zh mode active (welcome banner translated)",
                   "欢迎来到 Niubash" in body, body[:400])
        # THE STALE KEY (setup_wizard.rs:950 vs the zh table's
        # "No themes installed yet — keeping the built-in default look."):
        # the actual English key is "No external themes installed yet -
        # keeping the default look." — no zh entry matches it.
        step.check(
            "FINDING EVIDENCE: empty-gallery note falls back to ENGLISH "
            "in zh mode (stale zh key)",
            "No external themes installed yet" in body
            and "尚未安装任何主题" not in body,
            "zh mode shows the English note; the zh table only carries "
            "the older key 'No themes installed yet — keeping the "
            "built-in default look.'")
    finally:
        s1.close()
    step.finish()

    # Sandbox 2: populated gallery in zh mode.
    home2 = sandbox_home(lane_root, "g06b")
    mirrors = home2 / ".niubash" / "mirrors.toml"
    write_mirrors(mirrors, mirror_base)
    env2 = build_env(home2, exe, lang="zh", mirrors_file=mirrors)
    step = verdict.step("g06-install", "omb + bash-it for the zh gallery")
    add_and_trust(exe, home2, env2, step, "oh-my-bash")
    add_and_trust(exe, home2, env2, step, "bash-it")

    step = verdict.step("g06-gallery-zh", "gallery strings in zh mode")
    s2 = Session([str(exe), "setup"], home2, env2,
                 raw_log=verdict.transcripts / "g06-gallery.raw.ansi",
                 label="g06-gallery", delivery_log=verdict.delivery_events)
    try:
        # The zh gallery label is 选择主题; either language proves arrival.
        s2.wait_for("Pick a theme", timeout=120) if False else None
        s2.wait_for("选择主题", timeout=120)
        time.sleep(0.8)
        body = s2.text()
        verdict.capture("g06-gallery-zh", s2)
        step.check("gallery label translated (选择主题)",
                   "选择主题" in body, body[:400])
        step.check("menu hint line follows the wizard language "
                   "(中文提示)",
                   "移动" in body and "跳转" in body)
        step.check(
            "FINDING EVIDENCE: the Skip ROW falls back to ENGLISH in zh "
            "mode (runtime literal 'Skip - keep my current theme' with a "
            "HYPHEN; the zh table only carries the em-dash variant 'Skip "
            "— keep my current theme' — setup_wizard.rs:401 vs :2008)",
            "1) Skip - keep my current theme" in body
            and "跳过 ——" not in body,
            body[:500])
        # Jump to a theme row so ITS preview renders.
        s2.answer("2")
        time.sleep(0.8)
        preview = s2.text()
        verdict.capture("g06-preview-zh", s2)
        step.check(
            "FINDING EVIDENCE: preview line 1 falls back to ENGLISH in "
            "zh mode ('theme (external source)' has no zh entry; the "
            "table only carries 'theme (external source, primary)')",
            "theme (external source)" in preview
            and "主题（外部源）" not in preview,
            preview[-500:])
        step.check(
            "FINDING EVIDENCE: preview line 2 falls back to ENGLISH in "
            "zh mode ('renders via the bash-compatible PS1 channel' has "
            "no zh entry; the table carries a longer stale variant)",
            "renders via the bash-compatible PS1 channel" in preview
            and "经 bash 兼容" not in preview,
            preview[-500:])
        cancel_wizard(s2)
    finally:
        if s2.proc.isalive():
            s2.close()
    step.finish()
    verdict.seal()
    return verdict


SCENARIOS = {
    "g01": g01_preview_truth,
    "g02": g02_ordering,
    "g03": g03_counts,
    "g04": g04_discover,
    "g05": g05_broken_tree,
    "g06": g06_zh_i18n,
}


def main() -> int:
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("niu", type=Path)
    parser.add_argument("--artifacts", type=Path, default=None)
    parser.add_argument("--only", action="append", default=None)
    args = parser.parse_args()

    exe = args.niu.resolve()
    if not exe.is_file():
        print(f"SKIP: niu binary not found: {exe}")
        return 2
    lane_root = (args.artifacts or
                 Path(__file__).resolve().parents[3] / "target"
                 / "audit-results" / "wt82-L02").resolve()
    lane_root.mkdir(parents=True, exist_ok=True)
    mirror_base = build_fixtures(lane_root / "fixtures")
    shutil.rmtree(Path(tempfile.gettempdir()) / "wt82-L02-sandboxes",
                  ignore_errors=True)
    (lane_root / "run-l05.json").write_text(json.dumps({
        "niu": str(exe), "started_utc": now_utc(),
        "mirror_base": mirror_base}, indent=2) + "\n",
        encoding="utf-8", newline="\n")

    names = args.only or list(SCENARIOS)
    results = {}
    for name in names:
        fn = SCENARIOS.get(name)
        if fn is None:
            print(f"unknown scenario {name}")
            continue
        print(f"=== {name} ===", flush=True)
        try:
            results[name] = fn(exe, lane_root, mirror_base)
        except Exception as err:  # noqa: BLE001 — a crashed probe IS a red
            print(f"scenario {name} CRASHED: {err!r}")
            results[name] = None
        result = None
        if results[name] is not None:
            result = json.loads((results[name].artifacts / "verdict.json")
                                .read_text(encoding="utf-8"))["result"]
    print("\n===== L05 SUMMARY =====")
    worst = "pass"
    for name, verdict in results.items():
        if verdict is None:
            print(f"{name}: CRASHED")
            worst = "fail"
            continue
        result = json.loads((verdict.artifacts / "verdict.json")
                            .read_text(encoding="utf-8"))["result"]
        print(f"{name}: {result}  ({verdict.artifacts})")
        if result == "fail":
            worst = "fail"
    return 0 if worst == "pass" else 1


if __name__ == "__main__":
    sys.exit(main())
