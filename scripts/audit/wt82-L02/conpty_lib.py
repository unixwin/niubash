#!/usr/bin/env python3
"""Shared ConPTY driver for audit lane wt82-L02 (domains L02 + L05).

Adapted from scripts/journey/golden-journey.py (the proven Session +
settle/answer/anchor + Verdict pattern). Differences from the journey:

- per-scenario sandboxes kept under target/audit-results/wt82-L02/ (the
  lane's evidence dir — raw transcripts are the audit trail, never thrown
  away),
- a `press()` primitive for single keys the journey never sends (Esc =
  fast-forward/Cancel, Ctrl+C = abort) with the same delivery
  verification (a delivered key always advances the screen),
- a durable `state_snapshot()` of the sandbox rc/spec/journal/registry
  files so "nothing was written" claims are checked on disk, not on a
  screen tail.

Every probe has a timeout; no unbounded waits. Sandboxed HOME always:
USERPROFILE and HOME both point into the sandbox.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import tempfile
import threading
import time
from datetime import datetime, timezone
from pathlib import Path

if os.name != "nt":  # pragma: no cover - ConPTY is Windows-only
    raise RuntimeError("ConPTY probes are Windows-only")

import pyte
from winpty import PtyProcess

# Timing discipline (golden-journey constants).
SETTLE_SECONDS = 0.6
ENTER_GAP_SECONDS = 0.2
NAV_GAP_SECONDS = 0.12
QUIESCE_POLL_SECONDS = 0.15
QUIESCE_TIMEOUT_SECONDS = 15.0
ECHO_TIMEOUT_SECONDS = 10.0
ENTER_ACK_SECONDS = 3.0
WAKE_GAP_SECONDS = 0.15

ENTER = "\r"
DOWN = "\x1b[B"
UP = "\x1b[A"
ESC = "\x1b"
CTRL_C = "\x03"
KILL_LINE = "\x15"

COLS, ROWS = 120, 36
STARTUP_TIMEOUT = 120

PROMPTISH_LAST_ROW = re.compile(
    r"(?:.*[$#%>❯➜▶►»❮]\s*$|\s*[❯➜▶►»➤⮞❮])")


def now_utc() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def render_history_line(line, columns: int) -> str:
    if isinstance(line, str):
        return line.rstrip()
    chars = []
    for column in range(columns):
        char = line.get(column)
        data = getattr(char, "data", " ") or " "
        chars.append(data)
    return "".join(chars).rstrip()


def make_history_screen(cols: int, rows: int):
    try:
        return pyte.HistoryScreen(cols, rows, history=900)
    except TypeError:  # older pyte
        return pyte.HistoryScreen(cols, rows, top=900)


class Session:
    """A niu.exe (or other argv) under ConPTY with a pyte screen."""

    def __init__(self, argv, cwd, env, cols=COLS, rows=ROWS, raw_log=None,
                 label="session", delivery_log=None):
        self.label = label
        self.delivery_log = delivery_log if delivery_log is not None else []
        self.proc = PtyProcess.spawn(argv, cwd=str(cwd), env=env,
                                     dimensions=(rows, cols))
        self.screen = make_history_screen(cols, rows)
        self.stream = pyte.Stream(self.screen)
        self.raw_chunks = []
        self._lock = threading.Lock()
        self._raw_file = open(raw_log, "wb") if raw_log else None
        self._reader = threading.Thread(target=self._pump, daemon=True)
        self._reader.start()

    def _pump(self):
        while self.proc.isalive():
            try:
                data = self.proc.read()
            except Exception:
                break
            if data:
                with self._lock:
                    self.raw_chunks.append(data)
                    if self._raw_file is not None:
                        try:
                            self._raw_file.write(
                                data.encode("utf-8", errors="replace"))
                        except OSError:
                            # A full disk must not kill the reader thread
                            # (that freezes pyte and makes a healthy
                            # session look stalled — the wt82-L02 m02
                            # lesson). Drop the raw log, keep the lane
                            # evidence that lives in memory.
                            self._delivery_event(
                                "raw-log", None, 1,
                                "raw log write failed (OSError) — raw "
                                "capture disabled, session continues",
                                "raw-log-disabled")
                            try:
                                self._raw_file.close()
                            except Exception:
                                pass
                            self._raw_file = None
                self.stream.feed(data)

    def text(self) -> str:
        return "\n".join(self.screen.display)

    def raw_text(self) -> str:
        with self._lock:
            return "".join(self.raw_chunks)

    def transcript(self) -> str:
        history = [render_history_line(line, self.screen.columns)
                   for line in self.screen.history.top]
        return "\n".join(history + self.screen.display)

    def raw_stripped(self) -> str:
        raw = self.raw_text()
        raw = re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]", "", raw)
        raw = raw.replace("\r\n", "\n").replace("\r", "")
        return raw

    def wait_for(self, *needles, timeout=60, abort_on=()):
        deadline = time.time() + timeout
        while time.time() < deadline:
            body = self.text()
            for needle in needles:
                if needle in body or needle in self.raw_text():
                    return needle
            for gone in abort_on:
                if gone in body:
                    raise TimeoutError(
                        f"flow moved on (saw {gone!r}) while waiting for "
                        f"{needles}; screen:\n{body}")
            time.sleep(0.05)
        raise TimeoutError(
            f"timed out ({timeout}s) waiting for {needles}; screen:\n"
            f"{self.text()}")

    def _raw_pulse(self) -> int:
        with self._lock:
            return len(self.raw_chunks)

    def last_nonempty_row(self) -> str:
        for row in reversed(self.screen.display):
            if row.strip():
                return row.rstrip()
        return ""

    def wait_quiescent(self, timeout=QUIESCE_TIMEOUT_SECONDS) -> str:
        deadline = time.time() + timeout
        prev_screen = None
        prev_pulse = None
        while time.time() < deadline:
            screen = self.text()
            pulse = self._raw_pulse()
            if (prev_pulse and prev_screen is not None
                    and screen == prev_screen and pulse == prev_pulse):
                return "quiescent"
            if PROMPTISH_LAST_ROW.match(self.last_nonempty_row()):
                return "prompt"
            prev_screen, prev_pulse = screen, pulse
            time.sleep(QUIESCE_POLL_SECONDS)
        return "timeout"

    def _delivery_event(self, kind, keys, attempt, reason, action):
        self.delivery_log.append({
            "utc": now_utc(),
            "session": self.label,
            "kind": kind,
            "keys": keys,
            "attempt": attempt,
            "reason": reason,
            "action": action,
        })

    def _await_change(self, before, timeout=ECHO_TIMEOUT_SECONDS) -> bool:
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.text() != before:
                return True
            time.sleep(0.1)
        return False

    def answer(self, keys):
        """Settle out the menu draw, then press; digit and Enter split.

        Delivery-verified: menus give no echo, so the signal is that the
        screen CHANGED; one resend when nothing moved."""
        for attempt in (1, 2):
            settle = self.wait_quiescent()
            before = self.text()
            if keys.endswith(ENTER) and len(keys) > 1:
                self.proc.write(keys[:-1])
                time.sleep(ENTER_GAP_SECONDS)
                self.proc.write(ENTER)
            else:
                self.proc.write(keys)
            if self._await_change(before):
                return True
            if attempt == 1:
                self._delivery_event(
                    "answer", keys, 2,
                    f"screen unchanged {ECHO_TIMEOUT_SECONDS}s after the "
                    f"press (settle={settle}) — resending once", "resend")
            else:
                self._delivery_event(
                    "answer", keys, 2,
                    "screen still unchanged after the resend — proceeding",
                    "undelivered")
        return False

    def press(self, key, expect_change=True, timeout=ECHO_TIMEOUT_SECONDS):
        """Send one raw key (Esc / Ctrl+C) with delivery verification.

        A delivered Esc or Ctrl+C always advances the screen (next
        question, summary, cancelled note). One resend when nothing
        moved — Ctrl+C on ConPTY is the key class a lost byte hurts
        most, so the retry is ledgered like every other delivery."""
        for attempt in (1, 2):
            settle = self.wait_quiescent()
            before = self.text()
            self.proc.write(key)
            if self._await_change(before, timeout=timeout):
                return True
            self._delivery_event(
                "press", repr(key), 2,
                f"screen unchanged after key (settle={settle}) — "
                "resending once" if attempt == 1 else
                "screen still unchanged after the resend — proceeding",
                "resend" if attempt == 1 else "undelivered")
        return False

    def send_line(self, line, anchor_timeout=30.0):
        """Type a REPL line (kill-line wake first) and wait for the
        post-execution prompt (output-anchored, the J6 gluing guard).

        MENU GUARD (wt82-L02 m04 lesson): if a menu hint is on screen the
        wizard is blocked in a raw-mode menu — typed text lands IN the
        menu (letters are ignored but DIGITS jump and Enter confirms, so
        "echo M04_ALIVE" selected option 4 and confirmed it). Refuse
        instead of poisoning the flow."""
        self.wait_quiescent()
        if "navigate" in self.text():
            raise RuntimeError(
                "send_line refused: a menu is on screen (hint line "
                "visible) — drive it with answer()/press(), not typed "
                "text; screen tail:\n"
                + "\n".join(self.text().splitlines()[-6:]))
        for attempt in (1, 2):
            before = self.text()
            self.proc.write(KILL_LINE)
            time.sleep(WAKE_GAP_SECONDS)
            self.proc.write(line)
            deadline = time.time() + ECHO_TIMEOUT_SECONDS
            echoed = False
            while time.time() < deadline:
                body = self.text()
                if line in body and line not in before:
                    echoed = True
                    break
                time.sleep(0.1)
            if echoed:
                break
            if attempt == 1:
                self.wait_quiescent(timeout=5.0)
                self._delivery_event(
                    "send_line", line, 2, "no echo — resending once",
                    "resend")
            else:
                self._delivery_event(
                    "send_line", line, 2, "echo never appeared",
                    "undelivered")
        time.sleep(ENTER_GAP_SECONDS)
        before = self.text()
        input_row = self.last_nonempty_row()
        self.proc.write(ENTER)
        if not self._await_change(before, timeout=ENTER_ACK_SECONDS):
            self.proc.write(ENTER)
        started = time.time()
        deadline = started + anchor_timeout
        while time.time() < deadline:
            row = self.last_nonempty_row()
            if (row and row != input_row and line not in row
                    and PROMPTISH_LAST_ROW.match(row)):
                return True
            time.sleep(0.1)
        self._delivery_event(
            "anchor-wait", line, 1,
            f"no post-execution prompt within {anchor_timeout}s",
            "anchor-timeout")
        return False

    def close(self):
        try:
            self.proc.terminate(force=True)
        except Exception:
            pass
        if self._raw_file:
            try:
                self._raw_file.close()
            except Exception:
                pass


class Verdict:
    """Per-step assertions + transcripts -> verdict.{json,txt}."""

    def __init__(self, artifacts: Path, gate: str):
        self.artifacts = artifacts
        self.transcripts = artifacts / "transcripts"
        self.transcripts.mkdir(parents=True, exist_ok=True)
        self.gate = gate
        self.steps = []
        self.delivery_events = []

    def step(self, step_id, title):
        return StepRecorder(self, step_id, title)

    def capture(self, name: str, session: Session):
        (self.transcripts / f"{name}.txt").write_text(
            f"=== {name} @ {now_utc()} ===\n{session.transcript()}\n",
            encoding="utf-8", newline="\n")

    def save_text(self, name: str, text: str):
        (self.transcripts / name).write_text(
            text, encoding="utf-8", newline="\n")

    def seal(self) -> str:
        worst = "pass"
        for step in self.steps:
            if step["status"] == "fail":
                worst = "fail"
                break
            if step["status"] == "known-fail":
                worst = "known-fail"
        summary = {
            "gate": self.gate,
            "finished_utc": now_utc(),
            "result": worst,
            "steps": self.steps,
            "delivery": self.delivery_events,
        }
        (self.artifacts / "verdict.json").write_text(
            json.dumps(summary, indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8", newline="\n")
        lines = [f"{self.gate.upper()} VERDICT: {worst.upper()}", ""]
        for step in self.steps:
            mark = {"pass": "PASS", "fail": "FAIL", "known-fail":
                    "KNOWN-FAIL", "blocked": "BLOCKED", "skip": "SKIP"}[
                        step["status"]]
            lines.append(f"[{mark}] {step['id']} {step['title']}")
            for assertion in step["assertions"]:
                flag = "ok " if assertion["ok"] else "BAD"
                detail = (f" — {assertion['detail']}"
                          if assertion.get("detail") else "")
                lines.append(f"    {flag}  {assertion['name']}{detail}")
            for note in step.get("notes", []):
                lines.append(f"    ·    {note}")
        (self.artifacts / "verdict.txt").write_text(
            "\n".join(lines) + "\n", encoding="utf-8", newline="\n")
        return worst


class StepRecorder:
    def __init__(self, verdict: Verdict, step_id: str, title: str):
        self.verdict = verdict
        self.record = {
            "id": step_id,
            "title": title,
            "started": now_utc(),
            "status": "pass",
            "assertions": [],
            "notes": [],
        }
        self._failed = False

    def check(self, name: str, ok, detail: str = "") -> bool:
        ok = bool(ok)
        self.record["assertions"].append(
            {"name": name, "ok": ok, "detail": detail})
        if not ok:
            self._failed = True
        return ok

    def note(self, text: str):
        self.record["notes"].append(text)

    def finish(self, status: str = None):
        if status:
            self.record["status"] = status
        elif self._failed:
            self.record["status"] = "fail"
        self.record["finished"] = now_utc()
        self.verdict.steps.append(self.record)


# ── Sandbox + environment ───────────────────────────────────────────────────

ENV_POP = ("NIU_PLUGIN_SOURCES_ROOT", "NIU_PLUGIN_SPEC", "NIU_MIRRORS",
           "NIU_PLUGIN_BOOTSTRAP", "NIU_REPL_STARTUP", "BASH_ENV",
           "WINUXSH_ROOT", "NIU_THEME", "OSH_THEME", "BASH_IT_THEME",
           "MSYSTEM", "WT_SESSION", "NIU_SETUP_TIMEOUT")


def build_env(home: Path, exe: Path, lang: str = "en",
              mirrors_file: Path | None = None) -> dict:
    """Sandboxed environment: USERPROFILE and HOME both redirected (the
    USERPROFILE-wins pitfall), the tested exe FIRST on PATH (a stale PATH
    niu would poison the rc bootstrap line), git available for fixtures."""
    env = dict(os.environ)
    system_root = env.get("SystemRoot", r"C:\Windows")
    env.update({
        "SystemRoot": system_root,
        "COMSPEC": system_root + r"\System32\cmd.exe",
        "HOME": str(home),
        "USERPROFILE": str(home),
        "LOCALAPPDATA": str(home / "AppData" / "Local"),
        "APPDATA": str(home / "AppData" / "Roaming"),
        "TEMP": str(home / "tmp"),
        "TMP": str(home / "tmp"),
        "TERM": "xterm",
        "NIU_LANG": lang,
        "NIU_NO_UPDATE_CHECK": "1",
    })
    for key in ENV_POP:
        env.pop(key, None)
    if mirrors_file is not None:
        env["NIU_MIRRORS"] = str(mirrors_file)
    path_parts = [str(exe.parent)]
    if shutil.which("git") is None:
        for guess in (r"C:\Program Files\Git\cmd",
                      r"C:\Program Files\Git\bin"):
            if Path(guess).joinpath("git.exe").is_file():
                path_parts.append(guess)
                break
    path_parts.append(env.get("PATH", ""))
    env["PATH"] = os.pathsep.join(path_parts)
    (home / "tmp").mkdir(parents=True, exist_ok=True)
    (home / "AppData" / "Local").mkdir(parents=True, exist_ok=True)
    (home / "AppData" / "Roaming").mkdir(parents=True, exist_ok=True)
    return env


def new_sandbox(scenarios_root: Path, name: str) -> Path:
    scenarios_root.mkdir(parents=True, exist_ok=True)
    sandbox = (scenarios_root / name).resolve()
    if sandbox.exists():
        shutil.rmtree(sandbox, ignore_errors=True)
    (sandbox / "home").mkdir(parents=True)
    return sandbox


# ── Durable on-disk state (the "nothing was written" oracle) ────────────────

def state_snapshot(home: Path) -> dict:
    """Existence + salient content of every file a wizard run may write."""
    niu_dir = home / ".niubash"

    def read(path: Path) -> str:
        try:
            return path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return ""

    sources = []
    sources_dir = niu_dir / "sources"
    if sources_dir.is_dir():
        sources = sorted(child.name for child in sources_dir.iterdir())
    backups = []
    backups_dir = niu_dir / "backups"
    if backups_dir.is_dir():
        backups = sorted(child.name for child in backups_dir.iterdir())
    return {
        "rc": read(home / ".niubashrc"),
        "rc_exists": (home / ".niubashrc").is_file(),
        "compat_rc_exists": (home / ".winuxshrc").is_file(),
        "setup_done": (niu_dir / ".setup-done").is_file(),
        "wizard_answers": read(niu_dir / "wizard-answers.toml"),
        "journal": read(niu_dir / "setup-journal.toml"),
        "spec": read(niu_dir / "plugins.toml"),
        "mirrors": read(niu_dir / "mirrors.toml"),
        "registry": read(niu_dir / "registry.toml"),
        "sources": sources,
        "backups": backups,
        "niu_dir_exists": niu_dir.is_dir(),
    }


def state_diff_label(before: dict, after: dict) -> str:
    """Human one-line description of what changed between snapshots."""
    changed = []
    for key in after:
        if before.get(key) != after.get(key):
            changed.append(key)
    return ",".join(changed) if changed else "(no state key changed)"


def run_cli(exe: Path, home: Path, args: list, env: dict, timeout=120):
    """Bounded non-interactive CLI probe (niu -c / plugin verbs)."""
    proc = subprocess.run(
        [str(exe), *args], env=env, cwd=str(home), timeout=timeout,
        capture_output=True, text=True, encoding="utf-8", errors="replace")
    return proc


def git(args: list, cwd: Path, timeout=60):
    return subprocess.run(
        ["git", *args], cwd=str(cwd), timeout=timeout, capture_output=True,
        text=True, encoding="utf-8", errors="replace")
