#!/usr/bin/env python3
"""Lane wt82-L02 — domain L02: wizard fresh-install full walk (P0 lens).

Walks the ENTIRE wizard decision tree on fresh sandboxes — every question
x {answer, Esc-default, Ctrl+C at each step} x re-run — asserting after
each step:

- rc content correct for the choices,
- journal / wizard-answers / spec record the run honestly,
- undo receipts are complete AND each printed undo line actually works,
- the "nothing was written" promise holds at every pre-Apply step,
- the post-Apply Ctrl+C state shape (documented, not hidden),
- theme-not-installed fallback (deleted tree after a pick).

The journey's J1/J2 covers the one happy path; this gate covers the REST.
Offline: collection clones travel through the lane's local fixture mirror
(fixtures.py), so results are deterministic and network-independent.

Menu digit semantics (interactive_menu key_action): digit N jumps to
option N (1-based) but does NOT confirm — Enter confirms. Esc on an
ordinary question = UseDefault (fast-forward); Esc on the Apply gate =
Cancel; Ctrl+C = Abort.

Usage: python scripts/audit/wt82-L02/run_l02.py <niu.exe> [--artifacts DIR]
       [--only NAME]  (scenario filter, repeatable)
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

from conpty_lib import (CTRL_C, ENTER, ESC, Session, StepRecorder, Verdict,
                        build_env, new_sandbox, now_utc, run_cli,
                        state_diff_label, state_snapshot)
from fixtures import build_fixtures, write_mirrors

CLONE_TIMEOUT = 180


def open_first_contact(exe, home, env, verdict, label, session_label):
    """Spawn bare `niu` on a fresh sandbox and wait for the wizard front
    matter (welcome banner, environment panel, empty-gallery note)."""
    session = Session([str(exe)], home, env,
                      raw_log=verdict.transcripts / f"{label}.raw.ansi",
                      label=session_label,
                      delivery_log=verdict.delivery_events)
    session.wait_for("Welcome to Niubash", timeout=90)
    session.wait_for("Environment", timeout=30)
    session.wait_for("No external themes installed yet", timeout=30)
    verdict.capture(f"{label}-contact", session)
    return session


def fresh_state(home: Path) -> dict:
    return state_snapshot(home)


def assert_untouched(step: StepRecorder, before: dict, home: Path,
                     when: str) -> dict:
    """The nothing-written oracle: rc, marker, wizard-answers, journal,
    spec, registry, sources, backups — all unchanged from before."""
    after = state_snapshot(home)
    changed = [key for key in after if before.get(key) != after.get(key)]
    step.check(
        f"{when}: nothing was written (rc/marker/answers/journal/spec/"
        "registry/sources/backups unchanged)",
        not changed,
        f"changed: {changed}" if changed else state_diff_label(before, after))
    return after


def wizard_answer_set(session: Session, collection: str, niu_git: str):
    """Drive collection + niu-git menus. collection: skip|minimal|
    recommended|full; niu_git: skip|install|never|none (none = the
    question is suppressed by a lasting answer)."""
    session.wait_for("Plugin collection?", timeout=60)
    col_digit = {"skip": "1", "minimal": "2", "recommended": "3",
                 "full": "4"}[collection]
    session.answer(f"{col_digit}\r")
    if niu_git == "none":
        return
    session.wait_for("Windows-native git experience", timeout=60)
    git_digit = {"skip": "1", "install": "2", "never": "3"}[niu_git]
    session.answer(f"{git_digit}\r")
    session.wait_for("Apply this configuration?", timeout=60)


def journal_has(text: str, key: str, value: str) -> bool:
    """True when `key = <value>` appears in a TOML-ish durable file in
    either quote form (the journal shell-quotes with SINGLE quotes; the
    wizard-answers file writes double quotes)."""
    return (f"{key} = '{value}'" in text
            or f'{key} = "{value}"' in text)


def extract_undo_commands(text: str) -> list:
    """The undo receipts exactly as a finish screen printed them:
    command text before the '#', whitespace-collapsed."""
    commands = []
    in_block = False
    for line in text.splitlines():
        stripped = line.strip()
        if not in_block:
            if "Undo this run" in stripped:
                in_block = True
            continue
        if "Change things later" in stripped:
            break
        if not stripped:
            continue
        if stripped.startswith(("│", "|")):
            stripped = stripped.lstrip("│|").strip()
        command = stripped.split("#", 1)[0]
        command = re.sub(r"\s+", " ", command).strip()
        if command:
            commands.append(command)
    return commands


def journal_undo_commands(journal_text: str, home: Path) -> list:
    """The undo receipts a finish screen renders for this journal, derived
    from the durable journal itself (the setup_undo_lines contract:
    `cp <rc_backup> <rc>` when a backup exists). Same command the screen
    prints, reconstructible because the rendering is a pure function of
    the journal."""
    commands = []
    for line in journal_text.splitlines():
        line = line.strip()
        if line.startswith("rc_backup"):
            _, _, value = line.partition("=")
            path = value.strip().strip("'").strip('"')
            if path:
                commands.append(f"cp {path} {home / '.niubashrc'}")
    return commands


def run_undo_line(exe: Path, home: Path, env: dict, command: str):
    """Run one printed undo command (bounded). `niu ...` lines run the
    tested binary directly; non-niu lines (`cp ...`) go through the
    engine's command-string route (`niu -c`), because bare-argv fallback
    is SCRIPT mode (`niu cp x y` would try to run cp as a script file —
    the same thing GNU bash does with a binary) and the receipt is a
    shell line, not an argv."""
    args = command.split()
    if args and args[0] == "niu":
        return run_cli(exe, home, args[1:], env)
    return run_cli(exe, home, ["-c", command], env)


# ── m01: the full answer walk with every receipt exercised ──────────────────

def m01_full_trust_pick(exe: Path, root: Path, mirror_base: str):
    artifacts = root / "m01-full-trust-pick"
    verdict = Verdict(artifacts, "l02-m01 full -> trust -> pick -> undo")
    home = sandbox_for(root, "m01") / "home"
    home.mkdir(parents=True, exist_ok=True)
    mirrors = home / ".niubash" / "mirrors.toml"
    write_mirrors(mirrors, mirror_base)
    env = build_env(home, exe, mirrors_file=mirrors)
    before = fresh_state(home)

    step = verdict.step("m01", "full collection -> trust both -> pick "
                               "robbyrussell -> receipts honored")
    session = open_first_contact(exe, home, env, verdict, "m01", "m01")
    try:
        wizard_answer_set(session, "full", "skip")
        body = session.raw_stripped()
        step.check("collection full option wording keeps the retraction "
                   "promise (niu downloads nothing)",
                   "niu downloads nothing" in body)
        step.check("summary printed with untouched-row honesty",
                   "Summary" in body and "everything else stays untouched"
                   in body)
        verdict.capture("m01-summary", session)
        session.answer(ENTER)  # Apply is the highlighted default
        session.wait_for("Cloning into", timeout=CLONE_TIMEOUT)
        step.check("collection apply clones on the terminal", True)
        session.wait_for("to list their themes?", timeout=CLONE_TIMEOUT,
                         abort_on=("press any key to continue",))
        trust_body = session.raw_stripped()
        step.check("trust question names BOTH theme-bearing sources",
                   "oh-my-bash" in trust_body and "bash-it" in trust_body)
        verdict.capture("m01-trust-question", session)
        session.answer("2\r")  # Trust now
        session.wait_for("trusted 'oh-my-bash'", timeout=120)
        session.wait_for("trusted 'bash-it'", timeout=120)
        step.check("both sources trusted (wizard question IS the trust "
                   "verb)", True)

        session.wait_for("Pick a theme", timeout=120)
        time.sleep(0.8)
        gallery = session.text()
        verdict.capture("m01-post-install-gallery", session)
        demox_rows = [line for line in gallery.splitlines()
                      if "demox" in line and ")" in line]
        step.check("same-name demox listed exactly once (dedupe)",
                   len(demox_rows) == 1, f"{len(demox_rows)} demox rows")
        # The preview renders for the HIGHLIGHTED row; the initial
        # highlight is Skip ("current look unchanged"). Jump to
        # robbyrussell (digit 5, no Enter) and assert the preview truth.
        session.answer("5")
        time.sleep(0.8)
        preview = session.text()
        verdict.capture("m01-preview-robbyrussell", session)
        step.check("gallery preview names the highlighted theme + adapter",
                   "robbyrussell - oh-my-bash theme (external source)"
                   in preview, preview[-600:])
        step.check("gallery preview explains the render channel",
                   "renders via the bash-compatible PS1 channel" in preview)
        # Options: Skip, agnoster, demox, powerline-multiline,
        # robbyrussell -> robbyrussell is digit 5. Confirm it.
        session.answer("\r")
        session.wait_for("Undo this run", timeout=120)
        time.sleep(1.0)
        finish = session.raw_stripped()
        verdict.save_text("m01-finish-screen.txt", finish)

        after = state_snapshot(home)
        rc = after["rc"]
        step.check("rc written", after["rc_exists"])
        step.check("rc activates the picked theme via OSH_THEME",
                   "OSH_THEME='robbyrussell'" in rc, rc[:400])
        step.check("rc loader is the oh-my-bash guarded block",
                   "oh-my-bash.sh" in rc)
        step.check("rc note names the pick's actual source",
                   "# Prompt owned by the oh-my-bash theme 'robbyrussell'"
                   in rc)
        step.check("rc theme guard is an existence check (fallback-safe)",
                   "[ -r " in rc)
        step.check("rc carries the bootstrap line",
                   "niu plugin sync --bootstrap" in rc)
        step.check("no BASH_IT_THEME leak for an omb pick",
                   "BASH_IT_THEME" not in rc)

        journal = after["journal"]
        step.check("journal records collection = full",
                   journal_has(journal, "collection", "full"))
        step.check("journal records every installed source",
                   all(src in journal for src in
                       ("oh-my-bash", "bash-it", "bash-completion",
                        "bash-preexec")), journal)
        step.check("journal records the theme + its source",
                   journal_has(journal, "theme", "robbyrussell")
                   and journal_has(journal, "theme_source", "oh-my-bash"))
        step.check("journal records no failure (fixtures install clean)",
                   "collection_failed" not in journal, journal)
        step.check("journal keeps the transient niu-git Skip out "
                   "(no niu_git key)", "niu_git" not in journal, journal)
        step.check("transient answers leave NO wizard-answers.toml",
                   after["wizard_answers"] == "")
        step.check("spec adopted (the run ends spec-managed)",
                   "oh-my-bash" in after["spec"]
                   and "bash-preexec" in after["spec"])

        undo = extract_undo_commands(finish)
        for needle in ("niu plugin disable robbyrussell",
                       "niu plugin source remove oh-my-bash",
                       "niu plugin source remove bash-it",
                       "niu plugin source remove bash-completion",
                       "niu plugin source remove bash-preexec"):
            step.check(f"undo receipt printed: {needle}",
                       any(cmd.startswith(needle) for cmd in undo),
                       f"receipts: {undo}")
        step.check("no spurious backup-restore receipt on a fresh install "
                   "(no previous rc existed)",
                   not any(cmd.startswith("cp ") for cmd in undo), str(undo))

        try:
            session.wait_for("press any key to continue", timeout=60)
            session.answer(" ")
        except TimeoutError:
            pass
        session.send_line("echo M01_ALIVE")
        try:
            session.wait_for("M01_ALIVE", timeout=60)
            step.check("REPL alive after the wizard run", True)
        except TimeoutError as err:
            step.check("REPL alive after the wizard run", False, str(err))
        verdict.capture("m01-repl", session)
    finally:
        session.close()
    step.finish()

    # Run every printed undo line, verbatim, and verify each effect.
    # Dedupe preserving order: the theme pick's "optional: also delete
    # the tree" receipt and the collection's "drop the source" receipt
    # are the SAME command for oh-my-bash; a user runs it once.
    step = verdict.step("m01-undo", "every printed undo line works")
    finish_text = (verdict.transcripts / "m01-finish-screen.txt")
    undo = list(dict.fromkeys(
        extract_undo_commands(finish_text.read_text(encoding="utf-8")
                              if finish_text.is_file() else "")))
    results = []
    for command in undo:
        proc = run_undo_line(exe, home, env, command)
        out = ((proc.stdout or "") + (proc.stderr or "")).strip()
        results.append((command, proc.returncode, out))
        step.check(f"undo line exits 0: {command}", proc.returncode == 0,
                   out[:300])
    after = state_snapshot(home)
    step.check("disable removed the theme block from rc",
               "OSH_THEME" not in after["rc"], after["rc"][-500:])
    step.check("source removes emptied the registry",
               "oh-my-bash" not in after["registry"], after["registry"])
    step.check("source removes deleted every source tree",
               [name for name in after["sources"]
                if name != "registry.toml"] == [],
               str(after["sources"]))
    verdict.save_text("m01-undo-run.txt", "\n".join(
        f"rc={code}\n$ {cmd}\n{out}\n" for cmd, code, out in results))
    step.finish()
    verdict.seal()
    return verdict


# ── m02: Skip collection + Don't-ask-again + re-run ─────────────────────────

def m02_skip_never_rerun(exe: Path, root: Path, mirror_base: str):
    artifacts = root / "m02-skip-never-rerun"
    verdict = Verdict(artifacts, "l02-m02 skip + never + re-run")
    home = sandbox_for(root, "m02") / "home"
    home.mkdir(parents=True, exist_ok=True)
    env = build_env(home, exe)
    before = fresh_state(home)

    step = verdict.step("m02", "Skip collection, Don't ask again, re-run")
    session = open_first_contact(exe, home, env, verdict, "m02-run1",
                                 "m02-1")
    try:
        wizard_answer_set(session, "skip", "never")
        session.answer(ENTER)  # Apply
        session.wait_for("Change things later", timeout=90)
        time.sleep(1.0)
        finish = session.raw_stripped()
        verdict.save_text("m02-finish-run1.txt", finish)
        step.check("no clone happened for Skip",
                   "Cloning into" not in session.raw_text())
        after = state_snapshot(home)
        step.check("rc written (theme-less)",
                   after["rc_exists"] and "OSH_THEME" not in after["rc"])
        step.check("wizard-answers.toml records the lasting answer",
                   journal_has(after["wizard_answers"], "niu_git",
                               "never"),
                   after["wizard_answers"])
        step.check("journal records niu_git = never, no collection",
                   journal_has(after["journal"], "niu_git", "never")
                   and "collection" not in after["journal"],
                   after["journal"])
        step.check("no spec materialized without a collection",
                   after["spec"] == "")
        undo = extract_undo_commands(finish)
        step.check("undo receipts empty on a fresh theme-less run "
                   "(nothing to restore, nothing to remove)",
                   undo == [], str(undo))
        try:
            session.wait_for("press any key to continue", timeout=30)
            session.answer(" ")
        except TimeoutError:
            pass
        session.send_line("echo M02_ALIVE")
        try:
            session.wait_for("M02_ALIVE", timeout=60)
            step.check("REPL alive (Skip run)", True)
        except TimeoutError as err:
            step.check("REPL alive (Skip run)", False, str(err))
    finally:
        session.close()
    step.finish()

    step = verdict.step("m02-rerun", "re-run: never honored, backup + "
                                     "restore receipt")
    marker = "USER MARKER do not lose\n"
    rc_path = home / ".niubashrc"
    rc_path.write_text(rc_path.read_text(encoding="utf-8") + marker,
                       encoding="utf-8", newline="\n")
    pre_rerun_rc = rc_path.read_text(encoding="utf-8")
    env2 = build_env(home, exe)
    s2 = Session([str(exe), "setup"], home, env2,
                 raw_log=verdict.transcripts / "m02-run2.raw.ansi",
                 label="m02-2", delivery_log=verdict.delivery_events)
    try:
        s2.wait_for("Reconfigure your interactive prompt", timeout=90)
        s2.wait_for("Plugin collection?", timeout=60)
        step.check("collection question returns while the ecosystem is "
                   "still empty (Skip is transient)", True)
        s2.answer("1\r")  # Skip
        try:
            s2.wait_for("Windows-native git experience", timeout=8)
            step.check("niu-git question suppressed after Don't ask again",
                       False, "question re-appeared")
            s2.answer("1\r")
        except TimeoutError:
            step.check("niu-git question suppressed after Don't ask again",
                       True)
        s2.wait_for("Apply this configuration?", timeout=60)
        s2.answer(ENTER)
        # NOTE (channel artifact, wt82-L02 control experiment): the setup
        # verb EXITS right after the finish screen, and the ConPTY/pywinpty
        # channel drops output written in the final moments before process
        # exit (a trivial 12-line child lost its last 4 lines at exit —
        # see conpty-exit-flush-control artifacts). The undo receipts of a
        # `niu setup` run are therefore asserted from the durable state
        # (journal + backup), which the finish screen renders verbatim;
        # first-contact runs (REPL stays alive) assert the on-screen
        # receipts (m01, m02 run-1).
        for _ in range(120):
            if not s2.proc.isalive():
                break
            time.sleep(0.5)
        step.check("setup verb exits after Apply", not s2.proc.isalive())
        after = state_snapshot(home)
        step.check("re-run backed up the existing rc",
                   bool(after["backups"]), str(after["backups"]))
        step.check("journal records the backup for undo",
                   "rc_backup" in after["journal"], after["journal"])
        step.check("backup content is the pre-run rc byte-identical",
                   any((home / ".niubash" / "backups" / name)
                       .read_text(encoding="utf-8", errors="replace")
                       == pre_rerun_rc
                       for name in after["backups"]),
                   str(after["backups"]))
        step.check("re-run rc is REGENERATED (foreign marker only in the "
                   "backup — the documented backup-and-rewrite contract)",
                   marker not in after["rc"])
        step.check("wizard-answers still honest after re-run",
                   journal_has(after["wizard_answers"], "niu_git",
                               "never"))
        undo2 = journal_undo_commands(after["journal"], home)
        step.check("journal yields exactly one cp undo receipt",
                   len(undo2) == 1, str(undo2))
        if undo2:
            proc = run_undo_line(exe, home, env2, undo2[0])
            step.check("cp undo line exits 0", proc.returncode == 0,
                       (proc.stderr or "")[:300])
            restored = rc_path.read_text(encoding="utf-8")
            step.check("cp undo line restores the pre-run rc "
                       "byte-identical", restored == pre_rerun_rc)
    finally:
        s2.close()
    step.finish()
    verdict.seal()
    return verdict


# ── m03: Esc fast-forward at the FIRST question ─────────────────────────────

def m03_esc_fast_forward(exe: Path, root: Path, mirror_base: str):
    artifacts = root / "m03-esc-fast-forward"
    verdict = Verdict(artifacts, "l02-m03 Esc fast-forward")
    home = sandbox_for(root, "m03") / "home"
    home.mkdir(parents=True, exist_ok=True)
    env = build_env(home, exe)
    before = fresh_state(home)

    step = verdict.step("m03", "Esc at collection: fast-forward to Apply, "
                               "coherent rc")
    session = open_first_contact(exe, home, env, verdict, "m03", "m03")
    try:
        session.wait_for("Plugin collection?", timeout=60)
        session.press(ESC)
        step.check("Esc prints the fast-forward line (the default taken is "
                   "SHOWN, not invisible)", "→" in session.text(),
                   session.text()[-400:])
        time.sleep(1.0)
        step.check("niu-git question silently skipped by fast-forward",
                   "Windows-native git experience"
                   not in session.raw_stripped())
        session.wait_for("Apply this configuration?", timeout=30)
        step.check("fast-forward still lands on the explicit Apply gate",
                   True)
        verdict.capture("m03-apply-gate", session)
        session.answer(ENTER)  # Apply
        session.wait_for("Change things later", timeout=90)
        time.sleep(1.0)
        after = state_snapshot(home)
        step.check("fast-forward rc is coherent (no theme, no plugins)",
                   after["rc_exists"] and "OSH_THEME" not in after["rc"]
                   and after["spec"] == "")
        step.check("journal: no collection, no lasting niu-git",
                   "collection" not in after["journal"]
                   and "niu_git" not in after["journal"],
                   after["journal"])
        try:
            session.wait_for("press any key to continue", timeout=30)
            session.answer(" ")
        except TimeoutError:
            pass
        session.send_line("echo M03_ALIVE")
        try:
            session.wait_for("M03_ALIVE", timeout=60)
            step.check("REPL alive after the Esc fast-forward run", True)
        except TimeoutError as err:
            step.check("REPL alive after the Esc fast-forward run", False,
                       str(err))
    finally:
        session.close()
    step.finish()
    verdict.seal()
    return verdict


# ── m04/m05: Esc at each pre-Apply question; m06: the Ctrl+C channel ────────

def m04_esc_collection(exe: Path, root: Path, mirror_base: str):
    """Esc at the FIRST question, then Esc at the Apply gate: the whole
    flow exits without writing anything (Esc-Esc is the reachable
    'abort everything' path — see m06 for why Ctrl+C itself cannot be
    injected through the ConPTY byte bridge)."""
    artifacts = root / "m04-esc-collection"
    verdict = Verdict(artifacts, "l02-m04 Esc at collection + Esc at Apply")
    home = sandbox_for(root, "m04") / "home"
    home.mkdir(parents=True, exist_ok=True)
    env = build_env(home, exe)
    before = fresh_state(home)

    step = verdict.step("m04", "Esc at collection, Esc at Apply: nothing "
                               "written, everything asked again")
    session = open_first_contact(exe, home, env, verdict, "m04", "m04")
    try:
        session.wait_for("Plugin collection?", timeout=60)
        session.press(ESC)
        step.check("Esc at collection prints the fast-forward line "
                   "(default taken is SHOWN)", "→" in session.raw_stripped())
        time.sleep(1.0)
        step.check("niu-git question silently skipped by fast-forward",
                   "Windows-native git experience"
                   not in session.raw_stripped())
        session.wait_for("Apply this configuration?", timeout=30)
        session.press(ESC)  # Esc ON the gate = Cancel (documented)
        session.wait_for("Nothing was written.", timeout=30)
        step.check("'Nothing was written.' printed after Esc-Esc", True)
        time.sleep(1.0)
        after = assert_untouched(step, before, home, "after Esc-Esc")
        step.check("REPL alive after the Esc-Esc exit", _repl_alive(
            session, "M04_ALIVE"))
        verdict.capture("m04-after", session)
    finally:
        session.close()
    step.finish()

    step = verdict.step("m04-rerun", "re-run after Esc-Esc: fresh wizard")
    s2 = Session([str(exe), "setup"], home, build_env(home, exe),
                 raw_log=verdict.transcripts / "m04-rerun.raw.ansi",
                 label="m04-2", delivery_log=verdict.delivery_events)
    try:
        s2.wait_for("Plugin collection?", timeout=90)
        step.check("collection asked again (nothing was persisted)", True)
    finally:
        s2.close()
    step.finish()
    verdict.seal()
    return verdict


def m05_esc_niugit(exe: Path, root: Path, mirror_base: str):
    """Esc at the niu-git question only: collection answered Skip first,
    then Esc fast-forwards the REST (Apply gate still explicit). Apply
    must produce a coherent theme-less rc with no lasting answers."""
    artifacts = root / "m05-esc-niugit"
    verdict = Verdict(artifacts, "l02-m05 Esc at niu-git -> fast-forward")
    home = sandbox_for(root, "m05") / "home"
    home.mkdir(parents=True, exist_ok=True)
    env = build_env(home, exe)
    before = fresh_state(home)

    step = verdict.step("m05", "Esc at niu-git: defaults for the rest, "
                               "coherent rc")
    session = open_first_contact(exe, home, env, verdict, "m05", "m05")
    try:
        session.wait_for("Plugin collection?", timeout=60)
        session.answer("1\r")  # Skip collection (a real answer)
        session.wait_for("Windows-native git experience", timeout=60)
        session.press(ESC)
        step.check("Esc at niu-git prints its fast-forward line",
                   "→" in session.raw_stripped()
                   and "niu-git" in session.raw_stripped())
        session.wait_for("Apply this configuration?", timeout=30)
        session.answer(ENTER)  # Apply
        session.wait_for("Change things later", timeout=90)
        time.sleep(1.0)
        after = state_snapshot(home)
        step.check("rc written and theme-less", after["rc_exists"]
                   and "OSH_THEME" not in after["rc"])
        step.check("journal: no collection, no lasting niu_git",
                   "collection" not in after["journal"]
                   and "niu_git" not in after["journal"],
                   after["journal"])
        step.check("no wizard-answers.toml (Esc defaults are transient)",
                   after["wizard_answers"] == "")
        step.check("REPL alive after the fast-forwarded run", _repl_alive(
            session, "M05_ALIVE"))
        verdict.capture("m05-after", session)
    finally:
        session.close()
    step.finish()
    verdict.seal()
    return verdict


def _repl_alive(session: Session, marker: str, timeout=60) -> bool:
    """echo <marker> comes back on the live REPL (tour keypress first if
    the about-tour is showing)."""
    try:
        session.wait_for("press any key to continue", timeout=10)
        session.answer(" ")
    except TimeoutError:
        pass
    try:
        session.send_line(f"echo {marker}")
        session.wait_for(marker, timeout=timeout)
        return True
    except (TimeoutError, RuntimeError) as err:
        session.delivery_log.append(
            {"utc": now_utc(), "session": session.label, "kind":
             "repl-alive", "keys": marker, "attempt": 1,
             "reason": str(err), "action": "failed"})
        return False


def m06_ctrlc_channel_evidence(exe: Path, root: Path, mirror_base: str):
    """The Ctrl+C abort paths cannot be exercised from this lane: the
    pywinpty byte bridge does not deliver 0x03 as a Ctrl+C key event to a
    raw-mode menu. This probe pins down what the bridge DOES deliver, so
    the finding is a channel limitation with evidence — never silently
    'covered'. Everything still reachable pre-Apply is asserted
    untouched."""
    artifacts = root / "m06-ctrlc-channel"
    verdict = Verdict(artifacts, "l02-m06 Ctrl+C channel evidence")
    home = sandbox_for(root, "m06") / "home"
    home.mkdir(parents=True, exist_ok=True)
    env = build_env(home, exe)
    before = fresh_state(home)

    step = verdict.step(
        "m06", "Ctrl+C at a raw-mode menu: what the ConPTY byte bridge "
               "actually delivers")
    session = open_first_contact(exe, home, env, verdict, "m06", "m06")
    try:
        session.wait_for("Plugin collection?", timeout=60)
        n0 = len(session.raw_chunks)
        session.proc.write(CTRL_C)
        time.sleep(3.0)
        raw = "".join(session.raw_chunks[n0:])
        body = session.raw_stripped()
        verdict.save_text("m06-ctrlc-raw.txt", repr(raw))
        # Observed channel behavior (2026-10-04, release build): the menu
        # is CONFIRMED with its highlighted default and the flow advances
        # to the next question — no 'Setup cancelled' note, no
        # fast-forward line. Arrows, digits, Enter and Esc all deliver
        # correctly through the same bridge (golden-journey heritage +
        # m03/m04/m05), so this is specific to 0x03 at a raw-mode menu.
        step.check(
            "CHANNEL EVIDENCE (not a product assertion): 0x03 at a "
            "raw-mode menu behaves as Enter-equivalent — no cancelled "
            "note, no fast-forward line, flow advances",
            "Setup cancelled" not in body and
            "Windows-native git experience" in body,
            body[-400:])
        step.check("even a misdelivered Ctrl+C wrote nothing (still "
                   "pre-Apply)",
                   state_snapshot(home) == before or
                   all(before.get(k) == state_snapshot(home).get(k)
                       for k in before),
                   state_diff_label(before, state_snapshot(home)))
        n1 = len(session.raw_chunks)
        session.proc.write("\x18")  # Ctrl+X: no menu action bound
        time.sleep(2.0)
        step.check("CHANNEL EVIDENCE: 0x18 (Ctrl+X) at the niu-git menu "
                   "produces no output and no state change",
                   len(session.raw_chunks) == n1)
        verdict.capture("m06-ctrlc-channel", session)
    finally:
        session.close()
    step.finish()
    verdict.seal()
    return verdict


# ── m07/m08: Cancel + Esc at the Apply gate ─────────────────────────────────

def _applygate_scenario(exe: Path, root: Path, name: str, gate_answer):
    how = ("Cancel answer" if gate_answer
           else "Esc (Esc on Apply means Cancel)")
    artifacts = root / name
    verdict = Verdict(artifacts, f"l02-{name} {how}")
    home = sandbox_for(root, name) / "home"
    home.mkdir(parents=True, exist_ok=True)
    env = build_env(home, exe)
    before = fresh_state(home)

    step = verdict.step(name, f"Apply gate via {how}: nothing written, "
                              "pick-then-cancel forgets everything")
    session = open_first_contact(exe, home, env, verdict, name, name)
    try:
        session.wait_for("Plugin collection?", timeout=60)
        session.answer("4\r")  # full — a real pick BEFORE the cancel
        session.wait_for("Windows-native git experience", timeout=60)
        session.answer("3\r")  # Don't ask again — lasting pick BEFORE
        session.wait_for("Apply this configuration?", timeout=60)
        if gate_answer:
            session.answer(gate_answer)
        else:
            session.press(ESC)
        session.wait_for("Nothing was written.", timeout=30)
        step.check("'Nothing was written.' printed", True)
        time.sleep(1.0)
        after = assert_untouched(step, before, home, f"after {how}")
        step.check("the pre-Cancel collection pick installed NOTHING",
                   after["sources"] == [], str(after["sources"]))
        step.check("the pre-Cancel Don't-ask-again is NOT persisted "
                   "(nothing was written means nothing)",
                   after["wizard_answers"] == "")
        verdict.capture(f"{name}-after", session)
    finally:
        session.close()
    step.finish()

    step = verdict.step(f"{name}-rerun",
                        "re-run after cancel: everything asked again")
    s2 = Session([str(exe), "setup"], home, build_env(home, exe),
                 raw_log=verdict.transcripts / f"{name}-rerun.raw.ansi",
                 label=f"{name}-2", delivery_log=verdict.delivery_events)
    try:
        s2.wait_for("Plugin collection?", timeout=90)
        step.check("collection asked again", True)
        s2.answer("1\r")
        try:
            s2.wait_for("Windows-native git experience", timeout=8)
            step.check("niu-git asked again (the cancelled run persisted "
                       "no lasting answer)", True)
        except TimeoutError:
            step.check("niu-git asked again (the cancelled run persisted "
                       "no lasting answer)", False,
                       "question was suppressed")
    finally:
        s2.close()
    step.finish()
    verdict.seal()
    return verdict


def m07_cancel_at_apply(exe, root, mirror_base):
    return _applygate_scenario(exe, root, "m07-cancel-apply", "2\r")


def m08_esc_at_apply(exe, root, mirror_base):
    return _applygate_scenario(exe, root, "m08-esc-apply", None)


# ── m09: Ctrl+C mid-clone (post-Apply state shape) ──────────────────────────

def m09_ctrlc_mid_clone(exe: Path, root: Path, mirror_base: str):
    artifacts = root / "m09-ctrlc-mid-clone"
    verdict = Verdict(artifacts, "l02-m09 Ctrl+C mid-clone (post-Apply)")
    home = sandbox_for(root, "m09") / "home"
    home.mkdir(parents=True, exist_ok=True)
    mirrors = home / ".niubash" / "mirrors.toml"
    write_mirrors(mirrors, mirror_base)
    env = build_env(home, exe, mirrors_file=mirrors)

    step = verdict.step("m09", "Ctrl+C mid-clone: post-Apply state shape")
    session = open_first_contact(exe, home, env, verdict, "m09", "m09")
    try:
        wizard_answer_set(session, "full", "skip")
        session.answer(ENTER)  # Apply — state IS written from here on
        session.wait_for("Cloning into", timeout=CLONE_TIMEOUT)
        # Raw kill, no settle: the whole point is to land DURING the
        # apply. Delivery is verified by the process/filesystem state
        # below, not by a screen change.
        session.proc.write(CTRL_C)
        time.sleep(3.0)
        alive = session.proc.isalive()
        step.note(f"process alive 3s after Ctrl+C mid-apply: {alive}")
        session.close()
        after = state_snapshot(home)
        verdict.save_text("m09-state.json", json.dumps(
            after, indent=2, ensure_ascii=False))
        step.check("rc WAS written (Ctrl+C after Apply does not unwrite)",
                   after["rc_exists"])
        step.check("setup marker written (the run got past Apply)",
                   after["setup_done"])
        step.check("spec NOT yet adopted (the killed run never reached "
                   "the adoption step)", after["spec"] == "")
        step.check("journal NOT yet written (no finish screen happened)",
                   after["journal"] == "")
        step.check("sources root holds the interrupted apply's debris "
                   "(partial trees / staging dirs — visible, not hidden)",
                   after["sources"] != [], str(after["sources"]))
        clean = run_cli(exe, home, ["plugin", "clean"], env, timeout=120)
        step.check("niu plugin clean exits 0 after the interrupted run",
                   clean.returncode == 0,
                   ((clean.stdout or "") + (clean.stderr or ""))[:300])
        after_clean = state_snapshot(home)
        step.check("clean left no .staging-* debris",
                   not any(name.startswith(".staging-")
                           for name in after_clean["sources"]),
                   str(after_clean["sources"]))
        s2 = Session([str(exe)], home, build_env(home, exe),
                     raw_log=verdict.transcripts / "m09-restart.raw.ansi",
                     label="m09-restart",
                     delivery_log=verdict.delivery_events)
        try:
            s2.wait_for("Niubash", timeout=120)
            s2.send_line("echo M09_RESTART_ALIVE")
            try:
                s2.wait_for("M09_RESTART_ALIVE", timeout=60)
                step.check("fresh terminal after the interrupted run: "
                           "prompt alive", True)
            except TimeoutError as err:
                step.check("fresh terminal after the interrupted run: "
                           "prompt alive", False, str(err))
            body = s2.raw_stripped()
            step.check("no re-clone on restart (nothing was declared)",
                       "Cloning into" not in body)
            verdict.capture("m09-restart", s2)
        finally:
            s2.close()
    finally:
        pass
    step.finish()
    verdict.seal()
    return verdict


# ── m11: theme tree deleted after a pick — fallback must hold ───────────────

def m11_theme_fallback(exe: Path, root: Path, mirror_base: str):
    artifacts = root / "m11-theme-fallback"
    verdict = Verdict(artifacts, "l02-m11 deleted-theme-tree fallback")
    home = sandbox_for(root, "m11") / "home"
    home.mkdir(parents=True, exist_ok=True)
    env = build_env(home, exe)
    omb_fixture = (root / "fixtures" / "gh" / "github.com" / "ohmybash"
                   / "oh-my-bash")

    step = verdict.step("m11", "pick a theme, delete its tree, boot: "
                               "silent fallback")
    add = run_cli(exe, home, ["plugin", "add", "oh-my-bash", "--path",
                              str(omb_fixture)], env, timeout=120)
    step.check("fixture source add --path exits 0", add.returncode == 0,
               ((add.stdout or "") + (add.stderr or ""))[:300])
    trust = run_cli(exe, home, ["plugin", "trust", "oh-my-bash"], env,
                    timeout=120)
    step.check("fixture trust exits 0", trust.returncode == 0,
               ((trust.stdout or "") + (trust.stderr or ""))[:300])

    s1 = Session([str(exe), "setup"], home, build_env(home, exe),
                 raw_log=verdict.transcripts / "m11-setup.raw.ansi",
                 label="m11", delivery_log=verdict.delivery_events)
    try:
        s1.wait_for("Pick a theme", timeout=120)
        # Options: Skip, agnoster, demox, robbyrussell -> demox = 3.
        s1.answer("3\r")
        s1.wait_for("Windows-native git experience", timeout=30)
        s1.answer("1\r")
        s1.wait_for("Apply this configuration?", timeout=30)
        s1.answer(ENTER)
        s1.wait_for("Undo this run", timeout=90)
        time.sleep(1.0)
        after = state_snapshot(home)
        step.check("rc carries the Q1 pick (OSH_THEME='demox')",
                   "OSH_THEME='demox'" in after["rc"], after["rc"][:500])
        step.check("journal records the Q1 theme",
                   journal_has(after["journal"], "theme", "demox"),
                   after["journal"])
        verdict.capture("m11-after-setup", s1)
    finally:
        s1.close()

    # Delete the tree out from under the rc's guarded block.
    tree = home / ".niubash" / "sources" / "oh-my-bash"
    shutil.rmtree(tree, ignore_errors=True)
    step.check("source tree deleted", not tree.exists())

    s2 = Session([str(exe)], home, build_env(home, exe),
                 raw_log=verdict.transcripts / "m11-boot.raw.ansi",
                 label="m11-boot", delivery_log=verdict.delivery_events)
    try:
        s2.wait_for("Niubash", timeout=120)
        time.sleep(1.5)
        body = s2.raw_stripped()
        verdict.save_text("m11-boot-screen.txt", body)
        bad = [line for line in body.splitlines()
               if re.search(r"\b(error|not found|No such file)\b", line,
                            re.IGNORECASE)]
        step.check("no error line from the missing theme tree", not bad,
                   "; ".join(bad[:3]))
        s2.send_line("echo M11_ALIVE")
        try:
            s2.wait_for("M11_ALIVE", timeout=60)
            step.check("prompt alive with the theme tree gone", True)
        except TimeoutError as err:
            step.check("prompt alive with the theme tree gone", False,
                       str(err))
        step.check("fallback look: the deleted theme's PS1 never rendered",
                   "[omb:demox]" not in s2.text())
    finally:
        s2.close()
    step.finish()
    verdict.seal()
    return verdict


def sandbox_for(lane_root: Path, name: str) -> Path:
    """Sandbox on the system temp drive (D: was at 100% during the run —
    the lane's durable evidence stays under lane_root on D:, the heavy
    re-creatable sandbox state goes to the temp drive)."""
    root = Path(tempfile.gettempdir()) / "wt82-L02-sandboxes"
    return new_sandbox(root, name)


SCENARIOS = {
    "m01": m01_full_trust_pick,
    "m02": m02_skip_never_rerun,
    "m03": m03_esc_fast_forward,
    "m04": m04_esc_collection,
    "m05": m05_esc_niugit,
    "m06": m06_ctrlc_channel_evidence,
    "m07": m07_cancel_at_apply,
    "m08": m08_esc_at_apply,
    "m09": m09_ctrlc_mid_clone,
    "m11": m11_theme_fallback,
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
    (lane_root / "run.json").write_text(json.dumps({
        "niu": str(exe), "started_utc": now_utc(),
        "mirror_base": mirror_base}, indent=2) + "\n",
        encoding="utf-8", newline="\n")

    names = args.only or list(SCENARIOS)
    results = {}
    sandbox_root = Path(tempfile.gettempdir()) / "wt82-L02-sandboxes"
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
        # Sandbox hygiene: green scenarios leave nothing heavy behind
        # (all durable evidence already lives under lane_root); a red or
        # crashed scenario keeps its sandbox for diagnosis.
        result = None
        if results[name] is not None:
            result = json.loads((results[name].artifacts / "verdict.json")
                                .read_text(encoding="utf-8"))["result"]
        if result == "pass":
            shutil.rmtree(sandbox_root / name, ignore_errors=True)
    print("\n===== L02 SUMMARY =====")
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
