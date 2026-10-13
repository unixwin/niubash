#!/usr/bin/env python3
"""The Golden User Journey gate — the owner's 2026-10-03/04 transcripts as
an automated release blocker.

Owner directive (2026-10-04, verbatim intent): "你现在的测试逻辑完完全全
不是按照用户使用来的!交互式你根本不测!…必须改革测试方式" — tests must
follow REAL user journeys, interactive included, not synthetic shapes. This
script walks the exact journey a real user walked, over a real ConPTY
(pywinpty + pyte, the scripts/smoke-wizard-journey.py pattern), in a
sandboxed HOME (USERPROFILE and HOME both overridden — USERPROFILE wins in
this product, a known pitfall, so both point at the sandbox):

  J1 fresh-install first contact
     no ~/.niubash anything -> run `niu` -> the v-banner + wizard appear
     (welcome block, environment panel, "No external themes installed yet").
  J2 wizard full run
     collection "full" -> Apply -> the real `git clone`s progress on the
     terminal -> trust-now question -> "Trust now" (现在信任) -> theme
     gallery lists >60 themes -> pick `powerline-multiline` -> rc written,
     spec written, undo receipts listed on the finish screen -> the
     about-tour keypress -> the live REPL prompt.
  J3 activation
     source ~/.niubashrc in a live session -> ZERO syntax errors anywhere.
  J4 new-terminal x3
     each fresh `niu` session: no "Cloning into" (no re-downloads), no
     "not declared" nags, at most the documented per-source awaiting-trust
     notices, prompt renders.
  J5 daily battery (in the live session left open by J4)
     `ls | wc -l`, `echo hi | cat -n`, `cd ~ && pwd`,
     `[[ a != b ]] && echo ok` — each must produce output, not errors,
     with the prompt still alive after each command.
  J6 trust + activate bash-completion (the owner's exact flow)
     `niu plugin trust bash-completion` -> `source ~/.niubashrc` -> a
     fresh terminal -> ZERO syntax errors (bash_completion included).
  J7 gallery live preview (niubash#170)
     re-run `niu setup` -> the pane below the gallery highlight renders
     that theme's real PS1 (header follows the highlight; a hung theme
     degrades bounded instead of freezing); Esc fast-forwards, Cancel
     writes nothing.

Wave-1 persistence lane (wt79/jw1-persistence, journey-spec P3 + P8-S4 —
the two live P0s #168/#167 plus F4/F5 aged state). Registered as phases
P3 and P8 in PHASE_RUNNERS; like every phase they compose AFTER the base
gate on the same sandbox, walking on the installed state J1–J7 leave
(the theme-bearing source is still installed — J6 only trusts
bash-completion — and P8-S4 restores what it removes):

  P3-S1 reopen sandbox session (aged state)
     a fresh terminal after J5: prompt renders; the picked theme still
     owns the rc (`OSH_THEME`/`BASH_IT_THEME` line intact); the spec
     still declares the wizard's sources; the session reads back a
     marker file a PREVIOUS session wrote (the wake-flag capability:
     a session detects "a previous session mutated state"), proving the
     sandbox HOME wiring survives process boundaries.
  P3-S2 theme-block byte-stability + theme identity across terminals
  (niu#168)
     snapshot rc bytes after the wizard; `source ~/.niubashrc` in a live
     session; two fresh terminals; snapshot after each. (a) the THEME
     block bytes (the oh-my-bash managed guard region — the #168
     battleground) identical after every open, present exactly once,
     with no activated/deactivated flip of the theme source; the
     WHOLE-rc comparison stays as an informational note only, because
     other managed blocks may legally materialize between snapshots once
     their spec declaration goes trusted/declared (release run
     37218948432: the bash-completion block J6 trusted) — materializing
     it is sync's job, not a stability break; (b) the picked theme's
     variable still present, unchanged, on the same framework; (c) the
     string `selection materialized` (the #168 rewrite tell) never
     appears during an unchanged-spec source/startup; (d) terminal 2's
     first prompt row equals terminal 1's modulo clock digits. Green
     expected since wt72/themeback landed (release 1.3.4): the retired
     `wt73-168-theme-rebound` label no longer swallows this class, so a
     red here is a plain #168 regression.
  P3-S3 first-key integrity as product behavior (niu#167 anti-masking)
     after the themed prompt idles >=1.2s (>=1 clock repaint), send
     `echo NIU167KEY` with the driver's Ctrl-U wake DISABLED for this
     one probe: the full word must execute, never `cho: command not
     found`. One labeled probe per session. When it passes the journey
     FLIPS the global wake off (`WAKE_ENABLED = False`): every later
     send runs wake-free, so the gate stops masking #167.
  P3-S4 aged state: damaged tree + bootstrap-failures ledger (F5)
     seed between sessions (the `seed` capability): rename a file
     inside the trusted oh-my-bash tree (the guarded loader must no-op
     silently) and hand-write a spec entry for a declared-but-missing
     origin plus its `bootstrap-failures.toml` memo. The next terminal
     reaches a prompt within 10s with at most the documented one-line
     notices (deferred / awaiting-trust); the explicit verbs repair
     (`niu plugin sync` retries the missing origin readably, `niu
     plugin restore` rebuilds the tree); after healing the spec, the
     final terminal is silent again.
  P8-S4 remove the source providing the ACTIVE theme (F4)
     `niu plugin source remove oh-my-bash` while its theme is applied:
     spec declaration drops, registry record and tree drop, fresh
     terminal(s) fall back to the floor prompt with no resurrection and
     no syntax-error storm; then re-add + trust + re-enable restores the
     theme, and the rc carries exactly one oh-my-bash block (no orphan
     blocks) at steady state. The theme under test is DERIVED from the
     live state at step start (whatever P4 left active — see the
     function docstring); the floor-terminal count is the
     NIU_JOURNEY_P8S4_FLOOR_TERMINALS matrix knob (default 1).

Driver capabilities landed with this lane (journey-steps.json
`driver_capabilities`, owner W1):

  wake-flag  `send_line(..., wake=False)` sends a line WITHOUT the
             sacrificial Ctrl-U byte, and never retries it (a retry
             would mask exactly the first-key behavior the probe
             exists to observe). Plus the previous-session marker
             file (`~/.journey-wake.flag`): a step's session writes
             it through the shell; the NEXT session's step reads it
             back — a session detects "a previous session mutated
             state" without any driver-internal channel.
  seed       the `SandboxSeed` helper: byte snapshots of the rc,
             spec `[[sources]]` stanza add/drop, a product-format
             `bootstrap-failures.toml` memo write, and tree-file
             damage — all under the sandbox home, between sessions.

`--phases` selects what runs (comma-separated): the base J1–J7 gate
ALWAYS runs first; then the registered phases. `all` (the DEFAULT) adds
every registered phase in spec order (P3, P4, P7, P8) — the gate
exercises everything unless a subset is asked for. `base` is the bare
release gate (J1–J7, no phases). The wt79 single-value `--phase`
vocabulary folds in: `full` = all, `gate` = base, `persist` = P3,P8.

Unlike scripts/smoke-wizard-journey.py (offline, mirror-seeded, one
question path), this is the ONLINE journey: the clones come from the real
canonical origins, exactly as the user's terminal did. CI and local runs
need network + git on PATH; `ls`/`cat` for the battery come from the real
PATH (WinuxCmd on a user machine, Git for Windows on a runner).

Known failures are told, not hidden: each syntax-error assertion checks
the registered KNOWN-FAIL list. A known-fail is still RED (exit 1) — the
gate's job is to tell the truth — but the verdict names the owning ticket
so the red is expected-red, not a mystery.

Spec phases (docs/journey-spec.md §3, lane split §7): wave lanes land
their phases as clearly-separated runner functions keyed by the spec's
phase ids (docs/journey-steps.json) — wt79/jw1-persistence owns P3/P8-S4,
wt80/jw2-wizardspec owns P4/P7. The DEFAULT run is the base gate (J1–J7)
followed by EVERY registered phase in spec order (P3, P4, P7, P8) — the
gate exercises everything; `--phases` selects subsets (`--phases base`
is the bare release gate). Phases compose AFTER the base gate on the
same sandbox — every phase walks on the installed state the base gate
leaves — so a lane's local run is `--phases base,P4,P7` and the full
walk is the default (or `--phases all`).

Exit codes: 0 every assertion holds, 1 any fail/known-fail, 2 skip
(missing python deps / not Windows / no niu.exe / no git / unknown phase
id).

Usage: python scripts/journey/golden-journey.py <niu.exe> [--artifacts DIR]
        [--phases all|base|P3,P4,P7,P8,...|full|gate|persist]
"""

import argparse
import difflib
import json
import os
import random
import re
import shutil
import sys
import tempfile
import threading
import time
import tomllib
from datetime import datetime, timezone
from pathlib import Path

if os.name != "nt":  # pragma: no cover - ConPTY is Windows-only
    print("SKIP: the golden journey drives ConPTY (Windows only)")
    sys.exit(2)

try:
    import pyte
    from winpty import PtyProcess
except ImportError as err:  # pragma: no cover - environment guard
    print(f"SKIP: python deps missing ({err}); needs pywinpty + pyte")
    sys.exit(2)

# ── Timing discipline (scripts/smoke-wizard-journey.py pattern) ─────────────
# Menus drop input queued while they draw, so every answer settles before
# pressing; the digit and the Enter go as separate writes (one burst can
# lose the trailing Enter on the ConPTY bridge).
SETTLE_SECONDS = 0.6
ENTER_GAP_SECONDS = 0.2
NAV_GAP_SECONDS = 0.12
ENTER = "\r"
DOWN = "\x1b[B"

# ── Input-delivery hardening (release run 37149660449, journey J6) ──────────
# On a slow runner the J6 keystrokes (`niu plugin trust bash-completion`,
# typed right after the prompt came up) never reached the ConPTY: the
# screen still showed the startup banner + awaiting-trust nags with NO
# echo of the command, and the 90s wait timed out. Same family as wt61:
# interactive screens drain console input queued while they draw — a slow
# runner widens every such window. Delivery is therefore VERIFIED, not
# assumed (bounded everywhere; the send always proceeds when a bound
# expires, and every retry is recorded as a delivery event):
#   settle — before every send, wait for a quiet screen (two polls with
#            no new bytes and no viewport change) or a prompt-ish last
#            non-empty row. Bounded by QUIESCE_TIMEOUT_SECONDS.
#   wake   — REPL lines are preceded by a kill-line (Ctrl-U): the themed
#            prompt's clock repaint eats the FIRST input byte after
#            idle, so the kill-line goes first as the sacrifice (and
#            clears stale input before a retry's retype).
#   echo   — after a REPL line, wait for the typed text to appear NEW in
#            the rendered viewport (the editor syntax-highlights input,
#            so raw bytes never hold the text contiguously; and
#            pre-existing screen text does not count); after a menu key,
#            wait for ANY viewport change (menus in raw mode give no
#            echo). Bounded by ECHO_TIMEOUT_SECONDS.
#   retry  — when the signal never came, resend ONCE. This retries
#            DELIVERY only: the expected-output waits (wait_for) never
#            retry, so a real product failure still fails the gate.
#   anchor — send_line RETURNS only after the command completed: a new
#            prompt-ish row below the typed line (see the ANCHOR_*
#            constants and await_output_anchor below). Sequencing, not
#            delivery: it closes the J6 gluing window where send N+1
#            landed while send N was still executing.
QUIESCE_POLL_SECONDS = 0.15
QUIESCE_TIMEOUT_SECONDS = 15.0
ECHO_TIMEOUT_SECONDS = 10.0
ENTER_ACK_SECONDS = 3.0
WAKE_GAP_SECONDS = 0.15
# ── Output-anchored sequencing (release run 37153503706, J6) ─────────────────
# `echo J6_ALIVE` and the NEXT line executed as ONE glued command
# (`❯ echo J6_ALIVEniu plugin trust bash-completion`): the second send
# landed while the first was still being processed, the wake Ctrl-U was
# the clock-repaint's eaten byte, and the text appended to the unsubmitted
# input line. The wt67 settle cannot close that window — the typed input
# line ITSELF starts with the prompt glyph, so PROMPTISH_LAST_ROW matches
# it during execution and `wait_quiescent` reports "prompt" before the
# prompt has returned. So between two consecutive REPL sends, send_line
# returns only when the command it typed has COMPLETED: a NEW prompt-ish
# last row that is neither the typed input line nor any row carrying the
# command text (anchor condition in await_output_anchor). Bounded like
# everything here; the known-long journey commands carry their own bound
# (trust 90s, source 60s) and an expiry is ledgered, never silent.
ANCHOR_POLL_SECONDS = 0.1
ANCHOR_TIMEOUT_SECONDS = 30.0


def _env_int(name: str, default: int) -> int:
    """Matrix-axis knob: an integer from the environment (a malformed or
    absent value keeps the default; the axis exists so a matrix lane can
    widen a step's cell coverage without editing step code)."""
    try:
        return int(os.environ.get(name, "").strip() or default)
    except (TypeError, ValueError):
        return default
# Stress harness (hidden --stress-delay-ms): a randomized pause before
# every send's settle check, simulating runner slowness — the shape that
# broke two release runs. 0 disables (normal mode).
STRESS_DELAY_MS = 0
# Readline kill-line: the REPL editor's first input byte after the themed
# prompt has been idle is EATEN (observed 2026-10-03, run drv-run1: `echo`
# renders as `❯ cho`, `niu` as `❯ iu`, `source` as `❯ ource` — the prompt's
# per-second clock repaint consumes one queued key). Ctrl-U is the
# sacrificial wake byte: eaten -> the line behind it lands intact; not
# eaten -> it clears the (empty or stale) input line and the line still
# lands intact. Verified against the release build: Ctrl-U + line executes
# clean; double Ctrl-U + line (the retry shape) also executes clean.
KILL_LINE = "\x15"
# The wake-flag capability (journey-spec W1, P3-S3 / niu#167): the gate's
# sends normally carry the sacrificial Ctrl-U above, which MASKS the
# product's first-key-eat behavior. P3-S3 runs one labeled no-wake probe;
# when the product passes it (the 1.3.3 typeahead guard), the probe flips
# this flag and every LATER send in the run goes wake-free too — the gate
# stops masking. When the probe fails, the flag stays True so one live
# product bug does not drown the rest of the journey in eaten bytes.
WAKE_ENABLED = True
# A REPL prompt is the classic "last non-empty row ends in a prompt
# glyph" (powerline tails, the default user@host:cwd# $ #) or the
# agnoster shape where the glyph leads the row (➜  dirname). An early
# exit here is only a heuristic miss on a lookalike help line — the
# echo/change confirmation behind it still decides whether the key was
# delivered.
PROMPTISH_LAST_ROW = re.compile(
    r"(?:.*[$#%>❯➜▶►»❮]\s*$|\s*[❯➜▶►»➤⮞❮])")

COLS, ROWS = 120, 36

# Network clones are the real thing; give them real time. The budget is
# sized for the `full` collection's SEQUENTIAL apply: since niubash#171 it
# clones eight sources (oh-my-bash, bash-it, bash-completion, bash-preexec,
# complete-alias, fzf-git.sh, bash-sensible, git-flow-completion) — on a
# slow link (~100-300 KiB/s, several MiB) that legitimately exceeds the old
# 900s budget sized for four clones (observed 2026-10-04: the trust wait
# expired mid-apply with the last clone still receiving). It is a cap, not
# a sleep — a healthy network returns as soon as the question appears, and
# a real apply failure still aborts on the tour/REPL marker.
CLONE_TIMEOUT = 1800
STARTUP_TIMEOUT = 120

# Registered known-fails. A match keeps the gate RED (the gate must tell
# the truth) but labels the verdict with the owning ticket so the failure
# is actionable rather than mysterious.
KNOWN_FAILS = [
    {
        "id": "wt56-bash-completion-syntax",
        "ticket": "lane wt56 (alias family)",
        "pattern": r"bash_completion[^\n]*line\s+\d+",
        "note": "syntax error while sourcing bash_completion (the owner hit "
                "bash_completion:1376 on 2026-10-03, from the alias family; "
                "gate-observed 2026-10-03: 'syntax error in conditional "
                "expression: unexpected token ../' at line 1376)",
    },
    {
        "id": "bash-preexec-recipe-entry",
        "ticket": "recipe seed: entry file vs upstream layout",
        "pattern": r"bash-preexec[^:\n]*: entry file 'bash-preexec' not found",
        "note": "the 'full' collection's bash-preexec recipe names entry "
                "'bash-preexec' but upstream rcaloras/bash-preexec ships "
                "'bash-preexec.sh' — the apply reports '1 entries failed'",
    },
    {
        "id": "wt80-undo-receipt-ambiguous-theme",
        "ticket": "niu (new finding, lane wt80/jw2-wizardspec, run "
                  "20261004-173006): the wizard's undo receipt "
                  "`niu plugin disable <theme>` fails when the theme name "
                  "exists in more than one installed source",
        "pattern": r"exists in multiple sources",
        "note": "the finish screen and setup-journal print the bare theme "
                "name, but resolution refuses ambiguous names ('pick one: "
                "oh-my-bash/powerline-multiline, bash-it/powerline-multiline') "
                "— the undo contract breaks in exactly the dual-framework "
                "state the 'full' collection itself creates (the same-name "
                "family of niu#168). expected-red until the receipt prints "
                "the qualified id or resolution prefers the theme's owning "
                "source. labeling, never waiving",
    },
]


def now_utc() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def stress_pause():
    """The hidden --stress-delay-ms harness: a randomized pause before a
    send's settle check, stretching exactly the timing window a slow
    runner stretches. No-op in normal mode (STRESS_DELAY_MS == 0)."""
    if STRESS_DELAY_MS > 0:
        time.sleep(random.uniform(0, STRESS_DELAY_MS) / 1000.0)


def render_history_line(line, columns: int) -> str:
    """One pyte history row to text. Newer pyte stores rows as
    char-maps (`StaticDefaultDict[int, Char]`), older as mappings too —
    either way `Char.data` is the glyph and the default is a space."""
    if isinstance(line, str):  # future pyte handing us rendered text
        return line.rstrip()
    chars = []
    for column in range(columns):
        char = line.get(column)
        data = getattr(char, "data", " ") or " "
        chars.append(data)
    return "".join(chars).rstrip()


def make_history_screen(cols: int, rows: int) -> "pyte.HistoryScreen":
    """pyte's HistoryScreen kwarg moved between versions (`top=` -> 
    `history=`); accept either so the gate runs on the pyte a machine
    actually has."""
    try:
        return pyte.HistoryScreen(cols, rows, history=600)
    except TypeError:  # older pyte: top/bottom deques
        return pyte.HistoryScreen(cols, rows, top=600)


# ── The ConPTY session (the proven Session pattern, plus full transcripts) ──
class Session:
    """A niu.exe under ConPTY with a pyte-parsed screen and a running
    capture of every raw chunk. The RAW stream is the truth: a screen
    clear (the about-tour's \\x1b[2J) erases unscrolled lines from pyte's
    view forever, so waits fall back to the raw stream and the artifacts
    keep the raw bytes alongside the rendered transcripts."""

    def __init__(self, argv, cwd, env, cols=COLS, rows=ROWS, raw_log=None,
                 label="session", delivery_log=None):
        self.label = label
        self.delivery_log = delivery_log if delivery_log is not None else []
        self.dead = False  # the child exited and took the pty with it
        self.proc = PtyProcess.spawn(argv, cwd=str(cwd), env=env,
                                     dimensions=(rows, cols))
        # HistoryScreen keeps scrolled-off lines so the artifacts hold FULL
        # captures, not just the final viewport.
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
                    if self._raw_file:
                        self._raw_file.write(data.encode("utf-8",
                                                         errors="replace"))
                self.stream.feed(data)
        self.dead = True

    def _write(self, data) -> bool:
        """One guarded ConPTY write. A child that dies mid-journey closes
        the pty out from under the driver (observed 2026-10-04 run 4:
        niu.exe children vanishing while sibling lanes ran their own
        cleanup — a by-name taskkill looks identical to a product crash
        here); the journey must then record an honest red and STILL seal
        a verdict, never crash unsealed."""
        if self.dead:
            return False
        try:
            self.proc.write(data)
            return True
        except (EOFError, OSError) as err:
            self.dead = True
            self._delivery_event(
                "pty-write", str(data)[:20], 1,
                f"pty closed (child exited): {err}", "child-exited")
            return False

    def text(self) -> str:
        """The current viewport (what a user sees right now)."""
        return "\n".join(self.screen.display)

    def raw_text(self) -> str:
        with self._lock:
            return "".join(self.raw_chunks)

    def transcript(self) -> str:
        """Scrollback plus viewport — the full story of the session."""
        history = [render_history_line(line, self.screen.columns)
                   for line in self.screen.history.top]
        return "\n".join(history + self.screen.display)

    def raw_stripped(self) -> str:
        """The raw byte stream with ANSI escape sequences removed — every
        line the terminal ever received, even ones a later screen clear
        erased from pyte's model."""
        raw = self.raw_text()
        raw = re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]", "", raw)
        raw = raw.replace("\r\n", "\n").replace("\r", "")
        return raw

    def wait_for(self, *needles, timeout=60, abort_on=()):
        """Wait until a needle shows on the viewport or in the raw stream
        (screen clears hide text from pyte, never from the bytes).
        `abort_on` needles (viewport only) fail fast: they mean the flow
        moved past what we were waiting for."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.dead:
                raise TimeoutError(
                    f"pty closed (child exited) while waiting for {needles};"
                    f" screen:\n{self.text()}")
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
            f"timed out ({timeout}s) waiting for {needles}; screen:\n{self.text()}"
        )

    def _raw_pulse(self) -> int:
        """How many chunks the terminal has emitted so far — a cheap
        'bytes are still arriving' signal for settle-before-send."""
        with self._lock:
            return len(self.raw_chunks)

    def last_nonempty_row(self) -> str:
        for row in reversed(self.screen.display):
            if row.strip():
                return row.rstrip()
        return ""

    def wait_quiescent(self, timeout=QUIESCE_TIMEOUT_SECONDS) -> str:
        """Settle-before-send. Returns 'quiescent' once two polls saw no
        viewport change and no new bytes, 'prompt' when the last
        non-empty row already looks like a REPL prompt (a themed prompt
        with a clock/spinner never goes fully quiet), or 'timeout' when
        the bound expired. Bounded and best-effort: the caller sends
        regardless, and the send's own echo/change confirmation is what
        decides whether delivery needs a retry.

        KNOWN HOLE this method cannot close on its own: the typed input
        line itself starts with the prompt glyph, so 'prompt' can fire
        while a command is still executing (release run 37153503706 —
        the J6 gluing). send_line closes it with await_output_anchor
        after its Enter; first-sends (no predecessor command) have no
        gluing to prevent."""
        stress_pause()
        deadline = time.time() + timeout
        prev_screen = None
        prev_pulse = None
        while time.time() < deadline:
            screen = self.text()
            pulse = self._raw_pulse()
            # prev_pulse must be nonzero: a session that has never emitted
            # a byte is not settled, it is not started yet — typing into
            # that window is exactly the startup-drain loss (37149660449).
            if (prev_pulse and prev_screen is not None
                    and screen == prev_screen and pulse == prev_pulse):
                return "quiescent"
            if PROMPTISH_LAST_ROW.match(self.last_nonempty_row()):
                return "prompt"
            prev_screen, prev_pulse = screen, pulse
            time.sleep(QUIESCE_POLL_SECONDS)
        return "timeout"

    def _delivery_event(self, kind, keys, attempt, reason, action):
        """Record a delivery retry (or a give-up) for the verdict, so a
        future flake is diagnosable from the artifacts alone."""
        self.delivery_log.append({
            "utc": now_utc(),
            "session": self.label,
            "kind": kind,
            "keys": keys,
            "attempt": attempt,
            "reason": reason,
            "action": action,
        })

    def _await_echo(self, line, before, timeout=ECHO_TIMEOUT_SECONDS) -> bool:
        """Wait for the typed text to appear in the RENDERED viewport.
        The viewport is the only reliable surface: the editor
        syntax-highlights the input line, so the raw bytes interleave SGR
        codes INSIDE the word (`\\x1b[36mecho\\x1b[0m DUM...`) and a raw
        substring test can never match; pyte's rendered row holds the
        glyphs contiguously. `before` is the pre-send viewport — a match
        that was already on screen is not an echo (the awaiting-trust
        nag spells `niu plugin trust bash-completion`; drv-run1's
        raw-stream false confirm hid the eaten first byte)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            body = self.text()
            if line in body and line not in before:
                return True
            time.sleep(0.1)
        return False

    def _await_change(self, before, timeout=ECHO_TIMEOUT_SECONDS) -> bool:
        """Wait for ANY viewport change — the delivery signal for menu
        keys, which read raw and never echo: a delivered answer always
        advances the screen (next question, gallery, clones)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            if self.text() != before:
                return True
            time.sleep(0.1)
        return False

    def await_output_anchor(self, line, input_row,
                            timeout=ANCHOR_TIMEOUT_SECONDS) -> bool:
        """Output-anchored sequencing: wait for the POST-EXECUTION prompt
        of a command send_line just submitted — a NEW prompt-ish last
        non-empty row that is neither the typed input line (`input_row`)
        nor any row carrying the command text (`line`). The input line
        itself starts with the prompt glyph, so PROMPTISH_LAST_ROW
        matches it DURING execution — that is exactly the J6 gluing
        (release run 37153503706: `echo J6_ALIVEniu plugin trust
        bash-completion` ran as ONE command because the next send landed
        before this command's prompt returned). What a human waits for —
        echo scrolled up, output printed, fresh prompt at the bottom —
        is what this waits for. The command-text guard also survives the
        theme clock's repaints (the repainted input line still carries
        the text, so it keeps being excluded until execution replaces
        it). Bounded: on expiry the caller proceeds anyway (the step's
        own expected-output wait rules) but the expiry is ledgered as an
        'anchor-wait' delivery event, never silent. Every anchor
        outcome is ledgered (with its elapsed time) — one line per REPL
        send is the sequencing trace a future CI flake needs."""
        started = time.time()
        deadline = started + timeout
        while time.time() < deadline:
            row = self.last_nonempty_row()
            if (row and row != input_row and line not in row
                    and PROMPTISH_LAST_ROW.match(row)):
                elapsed = time.time() - started
                self._delivery_event(
                    "anchor-wait", line, 1,
                    f"post-execution prompt for {line!r} anchored after "
                    f"{elapsed:.1f}s — safe to send the next line",
                    "anchored")
                return True
            time.sleep(ANCHOR_POLL_SECONDS)
        self._delivery_event(
            "anchor-wait", line, 1,
            f"no post-execution prompt within {timeout}s of {line!r} — "
            "proceeding; the step's own expected-output wait will rule",
            "anchor-timeout")
        return False

    def answer(self, keys):
        """Settle out the menu draw, then press; digit and Enter split.

        Delivery-verified like send_line, but menus give no echo, so the
        signal is that the screen CHANGED after the press; one resend
        when nothing moved. The gallery's DOWN walk deliberately does
        NOT go through this per key — a repaint can lag a delivered
        arrow, a resent arrow can overshoot the row the walk just
        verified, and the walk already self-heals by polling the
        highlight."""
        for attempt in (1, 2):
            settle = self.wait_quiescent()
            before = self.text()
            if keys.endswith(ENTER) and len(keys) > 1:
                if not self._write(keys[:-1]):
                    return
                time.sleep(ENTER_GAP_SECONDS)
                self._write(ENTER)
            else:
                if not self._write(keys):
                    return
            if self._await_change(before):
                return
            if attempt == 1:
                self._delivery_event(
                    "answer", keys, 2,
                    f"screen unchanged {ECHO_TIMEOUT_SECONDS}s after the "
                    f"press (settle={settle}) — resending once", "resend")
            else:
                self._delivery_event(
                    "answer", keys, 2,
                    "screen still unchanged after the resend — proceeding; "
                    "the step's own wait will rule",
                    "undelivered")

    def send_line(self, line, anchor_timeout=ANCHOR_TIMEOUT_SECONDS,
                  anchor=True, wake=None):
        """Type a whole command at the REPL prompt, then Enter — and do
        not return until the command has COMPLETED.

        Delivery-verified (release run 37149660449, J6: keystrokes typed
        while startup notices were still landing never reached the
        ConPTY — no echo, 90s timeout; and drv-run1: the themed prompt's
        clock repaint eats the FIRST input byte after idle). Protocol,
        per attempt: kill-line first (sacrificial wake byte + clear of
        any stale input), then the text, then wait for the text to come
        back as NEW bytes; a failed confirm resends ONCE — the retry's
        kill-line also wipes a late-landing first attempt, so a resend
        can no longer concatenate two half-lines. The Enter gets its own
        short confirm (its scroll is immediate). Delivery retries only —
        the expected-output wait that follows never retries.

        wake (the W1 wake-flag capability, journey-spec P3-S3 / niu#167):
        `wake=False` sends the line WITHOUT the sacrificial Ctrl-U and
        WITHOUT the one resend — the #167 probe must observe the
        product's raw first-key behavior (a retry, with or without a
        wake byte, would mask exactly what this send exists to test),
        and without the kill-line a resend could concatenate with a
        half-delivered first line. `wake=None` (default) follows the
        global WAKE_ENABLED, which P3-S3 flips off for the rest of the
        run when the product passes the probe.

        Output-anchored (release run 37153503706, J6): after the Enter,
        await_output_anchor holds this send until a NEW prompt-ish row
        sits below the typed line — so the NEXT send in this session can
        never land while this command is still executing (the gluing).
        `anchor_timeout` carries the known-long commands' own bounds
        (trust 90s, source 60s); the default covers everything else.

        `anchor=False` skips that post-execution anchor for commands
        whose completion is INTERACTIVE, not a returning prompt — the
        wizard re-runs (`niu setup`): the flow continues with menu keys
        gated on the wizard's own screens (wait_for), and the anchor's
        prompt-ish matcher must not decide when interaction may start.
        Delivery hardening (settle/wake/echo/retry) is identical."""
        use_wake = WAKE_ENABLED if wake is None else bool(wake)
        settle = self.wait_quiescent()
        for attempt in ((1,) if not use_wake else (1, 2)):
            before = self.text()
            if use_wake:
                self._write(KILL_LINE)
                time.sleep(WAKE_GAP_SECONDS)
            if not self._write(line):
                return
            if self._await_echo(line, before):
                break
            if not use_wake:
                self._delivery_event(
                    "send_line", line, 1,
                    f"no echo of the typed text within "
                    f"{ECHO_TIMEOUT_SECONDS}s (wake disabled, "
                    f"settle={settle}) — no retry by design (a no-wake "
                    "send must not mask the first-key behavior it "
                    "probes)", "no-wake-no-retry")
                break
            if attempt == 1:
                self.wait_quiescent(timeout=5.0)  # do not retype mid-drain
                self._delivery_event(
                    "send_line", line, 2,
                    f"no echo of the typed text within "
                    f"{ECHO_TIMEOUT_SECONDS}s (settle={settle}) — "
                    "resending once", "resend")
            else:
                self._delivery_event(
                    "send_line", line, 2,
                    "echo never appeared after the resend — proceeding; "
                    "the step's own wait will rule",
                    "undelivered")
        time.sleep(ENTER_GAP_SECONDS)
        before = self.text()
        # The input line is the bottom-most content while editing; this
        # snapshot is what the anchor must see REPLACED by a new prompt.
        input_row = self.last_nonempty_row()
        self._write(ENTER)
        if not self._await_change(before, timeout=ENTER_ACK_SECONDS):
            self._delivery_event(
                "send_line-enter", line, 2,
                f"screen did not advance within {ENTER_ACK_SECONDS}s of "
                "Enter — resending Enter once", "resend")
            self._write(ENTER)
        if anchor:
            self.await_output_anchor(line, input_row, timeout=anchor_timeout)

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


# ── Verdict recording ───────────────────────────────────────────────────────
class Verdict:
    """Per-step assertions + full screen transcripts -> verdict.{json,txt}."""

    def __init__(self, artifacts: Path):
        self.artifacts = artifacts
        self.transcripts = artifacts / "transcripts"
        self.transcripts.mkdir(parents=True, exist_ok=True)
        self.steps = []
        # Input-delivery retries from every Session, merged into the
        # verdict so a future ConPTY flake is diagnosable from the
        # artifacts alone (raw-stream format itself is unchanged).
        self.delivery_events = []

    def step(self, step_id, title):
        return StepRecorder(self, step_id, title)

    def seal(self) -> str:
        worst = "pass"
        for step in self.steps:
            if step["status"] == "fail":
                worst = "fail"
                break
            if step["status"] == "known-fail":
                worst = "known-fail"
        summary = {
            "gate": "golden-user-journey",
            "finished_utc": now_utc(),
            "result": worst,
            "steps": self.steps,
            # Input-delivery retry ledger: keys that had to be resent (or
            # never confirmed) on the ConPTY bridge. Delivery retries are
            # never a waiver — if the product failed, the step assertions
            # above still rule — but they separate "the keystroke never
            # landed" (a runner/window problem) from "the product said
            # so" (a real red).
            "delivery": self.delivery_events,
        }
        (self.artifacts / "verdict.json").write_text(
            json.dumps(summary, indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8", newline="\n")
        lines = [f"GOLDEN JOURNEY VERDICT: {worst.upper()}", ""]
        for step in self.steps:
            mark = {"pass": "PASS", "fail": "FAIL",
                    "known-fail": "KNOWN-FAIL",
                    "blocked": "BLOCKED", "skip": "SKIP"}[step["status"]]
            lines.append(f"[{mark}] {step['id']} {step['title']}")
            for assertion in step["assertions"]:
                flag = "ok " if assertion["ok"] else "BAD"
                detail = f" — {assertion['detail']}" if assertion.get("detail") else ""
                lines.append(f"    {flag}  {assertion['name']}{detail}")
            for note in step.get("notes", []):
                lines.append(f"    ·    {note}")
        if self.delivery_events:
            lines.append("")
            lines.append("INPUT DELIVERY EVENTS (ConPTY keystroke retries +")
            lines.append("anchor waits — delivery/sequencing only; "
                         "expected-output waits never retry):")
            for event in self.delivery_events:
                lines.append(
                    f"    [{event['action']}] {event['session']} "
                    f"{event['kind']} attempt {event['attempt']} "
                    f"{event['keys']!r} — {event['reason']}")
        (self.artifacts / "verdict.txt").write_text(
            "\n".join(lines) + "\n", encoding="utf-8", newline="\n")
        return worst

    def capture(self, name: str, session: Session):
        (self.transcripts / f"{name}.txt").write_text(
            f"=== {name} @ {now_utc()} ===\n{session.transcript()}\n",
            encoding="utf-8", newline="\n")


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
        self._known_failed = False

    def check(self, name: str, ok, detail: str = "", known=False) -> bool:
        """Record one assertion. `known=True` marks a registered
        known-fail: still RED (recorded ok=False) but labeled — and it
        does not BLOCK later journey steps the way an unknown fail does."""
        ok = bool(ok)
        self.record["assertions"].append(
            {"name": name, "ok": ok, "detail": detail, "known": known})
        if not ok:
            if known:
                self._known_failed = True
            else:
                self._failed = True
        return ok

    def note(self, text: str):
        self.record["notes"].append(text)

    def finish(self, status: str = None):
        if status:
            self.record["status"] = status
        elif self._failed:
            self.record["status"] = "fail"
        elif self._known_failed:
            # Red (the gate must tell the truth) but not blocking: the
            # journey continues past a labeled known-fail.
            self.record["status"] = "known-fail"
        else:
            self.record["status"] = "pass"
        self.record["finished"] = now_utc()
        self.verdict.steps.append(self.record)


def match_known_fail(text: str):
    """The first registered known-fail whose pattern matches, if any."""
    for known in KNOWN_FAILS:
        if re.search(known["pattern"], text, re.IGNORECASE):
            return known
    return None


def syntax_error_lines(body: str):
    return [line.strip() for line in body.splitlines()
            if "syntax error" in line.lower()]


def all_syntax_errors(session: Session):
    """Syntax-error lines from the rendered transcript AND the raw stream
    (a screen clear can erase an error from pyte's model before anyone
    reads it)."""
    seen = []
    for line in syntax_error_lines(session.transcript()) + \
            syntax_error_lines(session.raw_stripped()):
        if line not in seen:
            seen.append(line)
    return seen


def verdict_for_syntax_errors(recorder: StepRecorder, name: str, errors):
    """Record one syntax-error assertion: pass, known-fail, or fail."""
    if not errors:
        recorder.check(name, True)
    elif (known := match_known_fail("\n".join(errors))):
        recorder.check(name, False,
                       f"KNOWN-FAIL {known['id']} ({known['ticket']}): "
                       f"{errors[0]} — {known['note']}")
        recorder.note(f"expected-red until {known['ticket']} lands")
    else:
        recorder.check(name, False,
                       f"{len(errors)} syntax error line(s): {errors[:3]}")


def check_with_known_fail(recorder: StepRecorder, name: str, evidence: str):
    """Record an assertion whose absence-check FAILED: `evidence` is the
    offending text that was found (a failing install report, a syntax
    error, ...). This never records a pass — the caller records the pass
    when nothing was found. A registered known-fail turns the red into a
    labeled one; an unregistered one stays a plain fail."""
    lines = [line for line in evidence.splitlines() if line.strip()]
    known = match_known_fail(evidence)
    if known is not None:
        recorder.check(name, False,
                       f"KNOWN-FAIL {known['id']} ({known['ticket']}) — "
                       f"{known['note']}", known=True)
        recorder.note(f"expected-red until {known['ticket']} lands")
    else:
        recorder.check(name, False,
                       f"{len(lines)} offending line(s), e.g. "
                       f"{lines[0].strip()[:160] if lines else evidence[:160]}")


# ── The journey environment ─────────────────────────────────────────────────
def build_env(home: Path, exe: Path) -> dict:
    """The real user's terminal environment with state redirected into the
    sandbox: USERPROFILE and HOME both point there (USERPROFILE wins in
    this product — set both, leave no ambiguity; both must be ABSOLUTE —
    a relative home doubles inside niu's path joins), APPDATA/LOCALAPPDATA/
    TEMP/TMP too. PATH is the real user PATH with one journey fix: the
    tested exe's directory goes FIRST so `niu` resolves to the binary
    under test (a developer machine may carry an older niu on PATH, and
    the rc bootstrap line `niu plugin sync --bootstrap` must run THIS
    build). git (clones) and ls/cat (battery) come from the real PATH,
    exactly like the user's machine."""
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
        # Deterministic, assertable wizard language (the strings this gate
        # waits for are the English ones).
        "NIU_LANG": "en",
        # The update hint is environment noise, not part of the journey.
        "NIU_NO_UPDATE_CHECK": "1",
    })
    # Ambient overrides from a developer shell must not leak into the user
    # journey: niu state locations must resolve purely from HOME.
    for key in ("NIU_PLUGIN_SOURCES_ROOT", "NIU_PLUGIN_SPEC", "NIU_MIRRORS",
                "NIU_PLUGIN_BOOTSTRAP", "NIU_REPL_STARTUP", "BASH_ENV",
                "WINUXSH_ROOT", "NIU_THEME", "OSH_THEME", "BASH_IT_THEME"):
        env.pop(key, None)
    env["PATH"] = os.pathsep.join(
        [str(exe.parent)] + [ensure_tools_on_path(env.get("PATH", ""))])
    (home / "tmp").mkdir(parents=True, exist_ok=True)
    (home / "AppData" / "Local").mkdir(parents=True, exist_ok=True)
    (home / "AppData" / "Roaming").mkdir(parents=True, exist_ok=True)
    return env


def ensure_tools_on_path(path: str) -> str:
    """The journey needs `git` (clones) and `ls`/`cat` (battery) on PATH.
    On a runner only `Git\cmd` may be present; add the Git for Windows
    layout pieces that exist. A user machine has WinuxCmd and needs none
    of this."""
    extras = []
    if shutil.which("git") is None:
        for guess in (r"C:\Program Files\Git\cmd",
                      r"C:\Program Files\Git\bin"):
            if Path(guess).joinpath("git.exe").is_file():
                extras.append(guess)
                break
    if shutil.which("ls") is None:
        usr_bin = r"C:\Program Files\Git\usr\bin"
        if Path(usr_bin).joinpath("ls.exe").is_file():
            extras.append(usr_bin)
    return path + os.pathsep.join([""] + extras) if extras else path


def prompt_alive(session: Session, marker: str, timeout=STARTUP_TIMEOUT):
    """The prompt is alive: `echo <marker>` comes back as output."""
    if session.dead:
        return False
    session.send_line(f"echo {marker}")
    try:
        session.wait_for(marker, timeout=timeout)
        time.sleep(0.3)
        return True
    except TimeoutError:
        return False


def drain_notices(session: Session, seconds: float = 1.0):
    """Let late stderr (startup sync notices) land before reading."""
    time.sleep(seconds)


# ── W1 driver capabilities (journey-spec §4: wake-flag, seed) ────────────────
PROMPT_BLOCK_ROWS = 4


def prompt_block(session: Session):
    """The prompt BLOCK as the user sees it: the last non-empty viewport
    rows (a themed prompt paints several — powerline-multiline carries
    the clock on its top segment row). Returns the rows newest-last, or
    None before the first prompt has rendered."""
    if not (session._raw_pulse()
            and PROMPTISH_LAST_ROW.match(session.last_nonempty_row())):
        return None
    rows = [row for row in session.text().splitlines() if row.strip()]
    return rows[-PROMPT_BLOCK_ROWS:]


# The structural signature scan stops at the first row that is not prompt
# structure: startup-sync notices, the banner, and plain command output
# must never enter the comparison (the run 37224044818 artifact: the
# last-4-rows fingerprint carried a wrapped `completion`` notice fragment).
NOTICE_MARK = "niu plugin sync:"
POWERLINE_GLYPHS = "\ue0b0\ue0b2\ue0b3\ue0b6"


def themed_prompt_signature(session: Session):
    """The ACTIVE theme's prompt rows, digit-stripped, newest-last — the
    stable structural signature of whatever theme this session renders
    right now. Bottom-up from the input-glyph row, taking rows that carry
    prompt structure (a powerline segment glyph, or the prompt-ish input
    row) and stopping at the first notice/banner/plain-output row. Theme-
    independent by construction: compare a session against a reference
    captured from the SAME theme in the SAME run, never against a
    fingerprint of a different theme's shape (P4 legitimately re-picks).
    [] before the first prompt has rendered."""
    rows = [row for row in session.text().splitlines() if row.strip()]
    signature = []
    for row in reversed(rows):
        if NOTICE_MARK in row or re.match(r"Niubash \d+\.\d+\.\d+",
                                          row.strip()):
            break
        if (any(glyph in row for glyph in POWERLINE_GLYPHS)
                or PROMPTISH_LAST_ROW.match(row)):
            signature.append(normalize_prompt_row(row))
        else:
            break
    signature.reverse()
    return signature


def await_first_prompt_row(session: Session, timeout=STARTUP_TIMEOUT):
    """The terminal's FIRST prompt block, captured before anything is
    typed: poll until bytes have arrived and the last non-empty row looks
    like a prompt, let trailing startup notices land, then read it.
    Returns the digit-stripped fingerprint (the clock must not count) of
    the block joined with '|', or None on timeout."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        block = prompt_block(session)
        if block is not None:
            time.sleep(1.0)  # trailing startup notices, then re-read
            block = prompt_block(session)
            if block is not None:
                return "|".join(normalize_prompt_row(row)
                                for row in block)
        time.sleep(0.05)
    return None


def time_to_first_prompt(session: Session, timeout, started=None):
    """Seconds from `started` (default: now) to the first prompt-ish row —
    the P3-S4 bounded-startup measure (prompt within 10s). None on
    timeout."""
    start = started if started is not None else time.time()
    deadline = start + timeout
    while time.time() < deadline:
        if (session._raw_pulse()
                and PROMPTISH_LAST_ROW.match(session.last_nonempty_row())):
            return time.time() - start
        time.sleep(0.05)
    return None


def normalize_prompt_row(row):
    """A prompt row modulo its clock: the themed prompt repaints a
    per-second clock, so P3-S2(d) compares rows with every digit run
    stripped."""
    return re.sub(r"\d", "", row or "")


def await_bare_line(session: Session, text, timeout=30):
    """True when `text` arrives as an OUTPUT line (viewport or
    ANSI-stripped raw stream) within the bound. The typed input line also
    carries the text — behind the prompt glyph — so the match is a line
    that STARTS with the text: `echo X`'s output row and `cat`'s output
    both lead with the payload, while the input echo leads with the
    prompt glyph (the distinction the #167 probe lives on). Prefix (not
    full-line) matching on purpose: stray console noise (observed:
    tasklist's 'No Instance(s) Available.' from niu's own startup probes)
    can land on the same physical row right after the payload with no
    newline in between."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        for line in session.text().splitlines():
            if line.strip().startswith(text):
                return True
        for line in session.raw_stripped().splitlines():
            if line.strip().startswith(text):
                return True
        time.sleep(0.1)
    return False


def sync_row_keys(body: str, action: str):
    """Distinct source ids of `niu plugin sync: <action> <id> ...` rows in
    a mixed transcript+raw body. The same row appears twice (the pyte
    transcript AND the raw stream both carry it) and stray console noise
    can glue onto a physical line, so row identity is (action, id) —
    never the physical line count."""
    return {
        match.group(1)
        for match in re.finditer(
            rf"niu plugin sync: {re.escape(action)}\s+(\S+)", body)
    }


def untrusted_registry_ids(home: Path):
    """The source ids the sandbox registry still marks untrusted — the
    documented awaiting-trust bound. Since wt78 expanded the `full`
    collection to nine entries, a literal cap goes stale on every
    entry-set change (six-plus untrusted at J4 is the documented new
    normal), so the bound derives from the registry — the same state
    sync --bootstrap reads (the J4 derivation, shared with P3-S4)."""
    registry_path = home / ".niubash" / "sources" / "registry.toml"
    if not registry_path.is_file():
        return set()
    registry = tomllib.loads(registry_path.read_text(encoding="utf-8"))
    ids = {record.get("id", "")
           for record in registry.get("sources", [])
           if not record.get("trusted", False)}
    ids.discard("")
    return ids


def first_diff_lines(old: bytes, new: bytes, limit=8):
    """The first changed lines between two rc snapshots — the verdict
    detail for a P3-S2 byte-stability red."""
    old_text = old.decode("utf-8", errors="replace").splitlines()
    new_text = new.decode("utf-8", errors="replace").splitlines()
    diffs = [line for line in difflib.unified_diff(
        old_text, new_text, lineterm="", n=0)
        if line[:1] in "+-" and line[:3] not in ("+++", "---")]
    return diffs[:limit]


def check_theme_stability(recorder: "StepRecorder", name: str, ok: bool,
                          evidence_shape: str, detail: str = "") -> bool:
    """Record one P3-S2 assertion (the #168 assertion class: theme-block
    byte-stability, theme identity, no-rewrite-tell, prompt identity).
    A red carries the evidence shape and routes through the KNOWN-FAIL
    registry: a registered label keeps the red expected-red (labeling,
    never waiving). With wt72/themeback landed (release 1.3.4) the #168
    label is retired, so a red here is a plain regression red — the raw
    diff stays in the detail and the artifacts."""
    ok = bool(ok)
    if ok:
        recorder.check(name, True)
        return True
    evidence = f"theme rebound (#168): {evidence_shape}"
    if detail:
        evidence += f" — {detail}"
    known = match_known_fail(evidence)
    if known is not None:
        recorder.check(
            name, False,
            f"KNOWN-FAIL {known['id']} ({known['ticket']}): {detail or evidence}",
            known=True)
        recorder.note(f"expected-red until {known['ticket']} lands")
    else:
        recorder.check(name, False, detail or evidence)
    return False


class SandboxSeed:
    """The `seed` capability (journey-spec §4 `conpty+seed`, owner lane
    W1): aged-state writes under the sandbox home BETWEEN sessions, so a
    phase starts from state a PRIOR session produced (or damaged) — never
    from fresh-wizard state. Every path stays inside the sandbox; the
    product reads these files with its normal startup/sync machinery.

    The previous-session marker (`~/.journey-wake.flag`) also lives here:
    a step's session writes it THROUGH THE SHELL and the next session's
    step reads it back the same way — a session detects "a previous
    session mutated state" with no driver-internal channel, and the
    sandbox HOME wiring is proven end-to-end across process boundaries
    (the relative-home doubling pitfall)."""

    def __init__(self, home: Path):
        self.home = home

    # ── paths ──
    @property
    def rc_path(self) -> Path:
        return self.home / ".niubashrc"

    @property
    def spec_path(self) -> Path:
        return self.home / ".niubash" / "plugins.toml"

    @property
    def sources_root(self) -> Path:
        return self.home / ".niubash" / "sources"

    @property
    def ledger_path(self) -> Path:
        """The startup install-failure memo (`bootstrap-failures.toml`,
        changelog 1.3.1 F5) — under the sources root, the product's own
        `bootstrap_failure_path()` location."""
        return self.sources_root / "bootstrap-failures.toml"

    def marker_path(self) -> Path:
        return self.home / ".journey-wake.flag"

    # ── snapshots ──
    def rc_bytes(self) -> bytes:
        try:
            return self.rc_path.read_bytes()
        except OSError:
            return b""

    def spec_text(self) -> str:
        try:
            return self.spec_path.read_text(encoding="utf-8")
        except OSError:
            return ""

    # ── spec hand-editing (the documented workflow: edit + sync) ──
    def add_spec_source(self, stanza_body: str):
        """Append one `[[sources]]` stanza (body lines without the
        header) to the spec — the same shape `niu plugin add` writes."""
        text = self.spec_text()
        if text and not text.endswith("\n"):
            text += "\n"
        text += "\n[[sources]]\n" + stanza_body.rstrip("\n") + "\n"
        self.spec_path.parent.mkdir(parents=True, exist_ok=True)
        self.spec_path.write_text(text, encoding="utf-8", newline="\n")

    def drop_spec_source(self, id_or_target: str) -> bool:
        """Remove every `[[sources]]` stanza whose text names
        `id_or_target` (its `id =` or `target =` line). Returns True when
        a stanza was dropped."""
        text = self.spec_text()
        stanzas = text.split("[[sources]]")
        kept = [stanzas[0]]
        dropped = False
        for stanza in stanzas[1:]:
            if id_or_target in stanza:
                dropped = True
                continue
            kept.append("[[sources]]" + stanza)
        if dropped:
            self.spec_path.write_text("".join(kept), encoding="utf-8",
                                      newline="\n")
        return dropped

    # ── the F5 ledger, in the product's exact format ──
    def seed_bootstrap_failure(self, target: str, error: str,
                               ref_name=None):
        """Write (or merge into) `bootstrap-failures.toml` exactly the way
        `sync.rs write_bootstrap_failures` formats a startup failure for
        (target, ref) — the shape `bootstrap_failure_recorded` matches
        against the resolved origin of a spec entry."""
        entry = f"\n[[failure]]\ntarget = \"{target}\"\n"
        if ref_name:
            entry += f"ref = \"{ref_name}\"\n"
        entry += (f"error = \"{error}\"\n"
                  f"at = \"{datetime.now(timezone.utc).isoformat(timespec='seconds')}\"\n")
        path = self.ledger_path
        path.parent.mkdir(parents=True, exist_ok=True)
        if path.is_file():
            path.write_text(path.read_text(encoding="utf-8") + entry,
                            encoding="utf-8", newline="\n")
        else:
            path.write_text(
                "# Startup install-failure memo (niu plugin sync --bootstrap).\n"
                "# Cleared by an explicit `niu plugin sync` / `niu plugin add` retry.\n"
                "schema = \"niubash:plugin-bootstrap-failures@1\"\n" + entry,
                encoding="utf-8", newline="\n")

    # ── tree damage (the `tree missing` / guarded-loader contract) ──
    def damage_source_file(self, source_id: str, relative: str,
                           suffix=".journey-bak"):
        """Rename one file inside an installed source tree (aged-state
        damage a real user's disk can produce). Returns (old, new) paths;
        raises when the file is not there to damage."""
        old = self.sources_root / source_id / relative
        new = old.with_name(old.name + suffix)
        if not old.is_file():
            raise FileNotFoundError(f"cannot damage: {old} is missing")
        old.rename(new)
        return old, new


# ── Wave-1 persistence steps (journey-spec P3 + P8-S4, lane wt79/W1) ─────────
#
# Each step is the user's own moves over the SAME sandbox the J-blocks
# aged: real fresh terminals, real `niu plugin` verbs, and the seed
# capability writing between sessions. `state` threads the ordered facts
# (rc byte snapshots keyed by milestone, the picked theme line, the
# first themed prompt row) from the wizard block into the assertions.

P3_MARKER_FILE = ".journey-wake.flag"
P3_MARKER_VALUE = "wake-p3s1"


def p3_s1_reopen(exe, home, env, verdict, seed, state):
    """P3-S1: reopen the sandbox session — theme persists, spec persists,
    and the session detects a previous session's state mutation through
    the wake-flag marker file."""
    step = verdict.step(
        "P3-S1", "reopen sandbox session: theme persists, spec persists")
    session = None
    try:
        session = Session([str(exe)], home, env,
                          raw_log=verdict.transcripts / "P3-S1-reopen.raw.ansi",
                          label="P3-S1", delivery_log=verdict.delivery_events)
        row = await_first_prompt_row(session)
        step.check("fresh terminal: prompt renders", row is not None,
                   "no prompt-ish row ever rendered"
                   if row is None else f"first prompt row: {row.strip()[:100]}")
        rc_bytes = seed.rc_bytes()
        rc_text = rc_bytes.decode("utf-8", errors="replace")
        theme_line = state.get("theme_line")
        step.check("theme persists in the rc (the wizard's pick intact)",
                   bool(theme_line) and theme_line in rc_text,
                   f"expected {theme_line!r} in the rc"
                   if theme_line else "the wizard snapshot carried no theme "
                   "line (J2 carried the failure)")
        spec_text = seed.spec_text()
        step.check("spec persists (oh-my-bash declared, theme declared)",
                   "oh-my-bash" in spec_text
                   and "powerline-multiline" in spec_text,
                   f"spec exists: {seed.spec_path.is_file()}")
        # The wake-flag capability: THIS session writes the marker through
        # the shell; the NEXT session (P3-S2's) reads it back — a session
        # detects "a previous session mutated state" across the process
        # boundary, proving the sandbox HOME wiring end-to-end.
        session.send_line(
            f"printf '{P3_MARKER_VALUE}' > ~/{P3_MARKER_FILE}"
            f" && cat ~/{P3_MARKER_FILE}")
        step.check(
            "session wrote the wake-flag marker through the shell",
            await_bare_line(session, P3_MARKER_VALUE, timeout=30))
        verdict.capture("P3-S1-reopen", session)
        state["rc_bytes"]["after-reopen"] = rc_bytes
        state["raw_texts"]["P3-S1"] = session.raw_stripped()
    except Exception as err:  # noqa: BLE001 - a stalled walk is the fail
        step.check("P3-S1 walk completed", False, f"{err}")
        if session is not None:
            verdict.capture("P3-S1-stalled", session)
    finally:
        if session is not None:
            session.close()
    step.finish()


# The themed source whose guard block P3-S2 guards (the wizard's pick
# lives in its managed region; the P4 helpers name it the same way).
THEMED_SOURCE_ID = "oh-my-bash"


def p3_s2_rc_stability(exe, home, env, verdict, seed, state):
    """P3-S2: theme-block byte-stability + theme identity across
    terminals — the niu#168 regression gate. Green expected since
    wt72/themeback landed (release 1.3.4): a red is a regression, not an
    expected-red (the wt73-168-theme-rebound label retired with the
    fix)."""
    step = verdict.step(
        "P3-S2", "theme-block byte-stability + theme identity across "
        "terminals (niu#168)")
    theme_line = state.get("theme_line") or ""
    theme_var = state.get("theme_var") or "OSH_THEME"
    other_var = "BASH_IT_THEME" if theme_var == "OSH_THEME" else "OSH_THEME"
    baseline = state["rc_bytes"].get("after-wizard", b"")
    raws = state["raw_texts"]
    rows = {}
    try:
        # Live session: read back the previous session's marker, then the
        # unchanged-spec source.
        live = Session([str(exe)], home, env,
                       raw_log=verdict.transcripts / "P3-S2-source.raw.ansi",
                       label="P3-S2-live",
                       delivery_log=verdict.delivery_events)
        try:
            live.send_line(f"cat ~/{P3_MARKER_FILE}")
            step.check(
                "live session detects the previous session's wake-flag "
                "marker",
                await_bare_line(live, P3_MARKER_VALUE, timeout=30))
            live.send_line("source ~/.niubashrc", anchor_timeout=60)
            live.send_line("echo P3S2_SRC_DONE")
            try:
                live.wait_for("P3S2_SRC_DONE", timeout=60)
                step.check("source ~/.niubashrc completed", True)
            except TimeoutError as err:
                step.check("source ~/.niubashrc completed", False, str(err))
            drain_notices(live)
            verdict.capture("P3-S2-after-source", live)
            state["rc_bytes"]["after-source"] = seed.rc_bytes()
            raws["P3-S2-source"] = live.raw_stripped()
        finally:
            live.close()

        # Two fresh terminals (the #168 shape: the rebound shows up when
        # a NEW session's startup sync rewrites the rc).
        for i in (1, 2):
            term = Session(
                [str(exe)], home, env,
                raw_log=verdict.transcripts / f"P3-S2-terminal-{i}.raw.ansi",
                label=f"P3-S2-term{i}",
                delivery_log=verdict.delivery_events)
            try:
                rows[i] = await_first_prompt_row(term)
                drain_notices(term)
                verdict.capture(f"P3-S2-terminal-{i}", term)
                state["rc_bytes"][f"after-term{i}"] = seed.rc_bytes()
                raws[f"P3-S2-term{i}"] = term.raw_stripped()
            finally:
                term.close()

        # (a) the THEME block bytes — the oh-my-bash managed guard region,
        # the #168 battleground — identical after every source/terminal,
        # present exactly once, with no activated/deactivated flip of the
        # theme source. The WHOLE-rc equality the pre-wt78 gate asserted
        # is informational only: another managed block may legally
        # materialize between snapshots once its spec declaration goes
        # trusted/declared (release run 37218948432: the bash-completion
        # block J6 trusted landed here and was mislabeled a stability
        # break) — materializing it is sync's job (wt87 recalibration).
        for milestone in ("after-reopen", "after-source", "after-term1",
                          "after-term2"):
            snap = state["rc_bytes"].get(milestone)
            snap_text = (snap or b"").decode("utf-8", errors="replace")
            base_text = baseline.decode("utf-8", errors="replace")
            base_block = rc_managed_block(base_text, THEMED_SOURCE_ID)
            snap_block = rc_managed_block(snap_text, THEMED_SOURCE_ID)
            stable = (bool(snap) and base_block is not None
                      and snap_block == base_block)
            diff = "" if stable else " | ".join(first_diff_lines(
                (base_block or "").encode("utf-8"),
                (snap_block or "").encode("utf-8")))
            check_theme_stability(
                step,
                f"theme block bytes identical after {milestone} (niu#168)",
                stable,
                "the theme block diverged across session opens",
                diff or ("snapshot missing — the earlier step carrying it "
                         "failed" if snap is None else
                         "the theme block is missing after the open"
                         if snap_block is None else
                         "the theme block is missing in the wizard baseline"
                         if base_block is None else
                         "no line-level diff (mode/length change)"))
            # No activated/deactivated flip: the guarded region survives
            # every open exactly once. A flip (an enable/disable life
            # cycle across sessions) adds a second block or drops this
            # one — that IS a theme-state change, in either direction.
            blocks_now = rc_managed_block_count(snap_text, THEMED_SOURCE_ID)
            check_theme_stability(
                step,
                f"theme block present exactly once after {milestone} "
                "(no activated/deactivated flip)",
                blocks_now == 1,
                "the theme source's managed block count changed across "
                "session opens",
                f"count = {blocks_now}")
            # Informational: the whole-rc comparison. Legal spec-driven
            # materialization shows up here without failing the gate.
            if snap == baseline and snap:
                step.note(f"whole rc byte-identical after {milestone} "
                          "(informational)")
            else:
                whole_diff = first_diff_lines(baseline, snap or b"")
                step.note(
                    f"whole rc changed after {milestone} outside the theme "
                    "block (informational — spec-driven materialization is "
                    "sync's job): "
                    + (" | ".join(whole_diff) or
                       "no line-level diff (mode/length change)"))

        # (a2) no activated/deactivated flip of the THEME source in any
        # P3 session stream: the enable/disable life cycle would print
        # `niu plugin sync: activated|deactivated oh-my-bash`.
        for label in sorted(raws):
            if not label.startswith(("P3-S1", "P3-S2")):
                continue
            raw = raws[label]
            flips = (sync_row_keys(raw, "activated")
                     | sync_row_keys(raw, "deactivated"))
            check_theme_stability(
                step,
                f"the theme source is never activated/deactivated during "
                f"{label}",
                THEMED_SOURCE_ID not in flips,
                "the theme source's activation flipped during an "
                "unchanged-spec source/startup",
                f"flip rows name: {sorted(flips)}")

        # (b) the picked theme's variable still present, unchanged, on
        # the SAME framework (the rebound re-points or flips it).
        for milestone in ("after-source", "after-term1", "after-term2"):
            text = (state["rc_bytes"].get(milestone) or b"").decode(
                "utf-8", errors="replace")
            present = bool(theme_line) and theme_line in text
            flipped = re.search(
                rf"^{other_var}='powerline-multiline'",
                text, re.M) is not None
            check_theme_stability(
                step,
                f"{theme_var} still 'powerline-multiline' after "
                f"{milestone} (same framework)",
                present and not flipped,
                "the picked theme's variable was re-pointed or flipped to "
                "the other framework across session opens"
                if flipped or not present else "",
                f"{theme_line!r} present: {present}; "
                f"{other_var}='powerline-multiline' appeared: {flipped}")

        # (c) the #168 rewrite tell never appears during an
        # unchanged-spec source/startup.
        for label in sorted(raws):
            if not label.startswith(("P3-S1", "P3-S2")):
                continue
            raw = raws[label]
            tell = next((line.strip()[:140] for line in raw.splitlines()
                         if "selection materialized" in line), "")
            check_theme_stability(
                step,
                f"'selection materialized' never printed during {label}",
                "selection materialized" not in raw,
                "the #168 rewrite tell appeared during an unchanged-spec "
                "source/startup", tell)

        # (d) terminal 2's first prompt row equals terminal 1's modulo
        # clock digits.
        row1, row2 = rows.get(1), rows.get(2)
        if row1 is not None:
            state["themed_prompt_row"] = normalize_prompt_row(row1)
        same = (row1 is not None and row2 is not None
                and normalize_prompt_row(row1) == normalize_prompt_row(row2))
        check_theme_stability(
            step, "terminal 2's first prompt row equals terminal 1's "
            "(modulo clock digits)",
            same, "the first prompt row differs across fresh terminals",
            f"t1={(row1 or '(none)').strip()[:100]!r} "
            f"t2={(row2 or '(none)').strip()[:100]!r}")
    except Exception as err:  # noqa: BLE001 - a stalled walk is the fail
        step.check("P3-S2 walk completed", False, f"{err}")
    step.finish()


def p3_s3_first_key(exe, home, env, verdict, seed, state):
    """P3-S3: first-key integrity as product behavior — the niu#167
    anti-masking probe. One labeled no-wake probe; on a pass the run's
    global wake is disabled so the gate stops masking."""
    global WAKE_ENABLED
    step = verdict.step(
        "P3-S3", "first-key integrity as product behavior (niu#167 "
        "anti-masking)")
    marker = "NIU167KEY"
    session = None
    try:
        session = Session(
            [str(exe)], home, env,
            raw_log=verdict.transcripts / "P3-S3-first-key.raw.ansi",
            label="P3-S3", delivery_log=verdict.delivery_events)
        step.check("themed prompt renders",
                   prompt_alive(session, "P3S3_ALIVE"))
        # Idle past at least one clock repaint (the themed clock ticks at
        # 1 Hz) — the exact window where the #167 eat consumed the first
        # byte of the next command.
        time.sleep(1.6)
        session.send_line(f"echo {marker}", wake=False)
        executed = await_bare_line(session, marker, timeout=30)
        body = session.transcript() + "\n" + session.raw_stripped()
        eaten_lines = [line.strip()[:120] for line in body.splitlines()
                       if re.search(r"\bcho: command not found\b", line)]
        marker_lines = [line.strip()[:120] for line in body.splitlines()
                        if marker in line][:3]
        step.check("first key survives the idle repaint (full word ran)",
                   executed,
                   f"the marker never printed as output; screen showed: "
                   f"{marker_lines}")
        step.check("no eaten-first-byte evidence in the probe session",
                   not eaten_lines,
                   f"{len(eaten_lines)} line(s), first: "
                   f"{eaten_lines[0] if eaten_lines else ''}")
        verdict.capture("P3-S3-first-key", session)
        if executed and not eaten_lines:
            WAKE_ENABLED = False
            step.note("first-key integrity holds — WAKE_ENABLED flipped "
                      "off: every later send in this run goes without the "
                      "Ctrl-U wake (the gate stops masking #167)")
        else:
            step.note("probe FAILED — WAKE_ENABLED stays True so the rest "
                      "of the run still delivers (the wake stays for "
                      "everything else); this red is a #167 regression")
    except Exception as err:  # noqa: BLE001
        step.check("P3-S3 walk completed", False, f"{err}")
        if session is not None:
            verdict.capture("P3-S3-stalled", session)
    finally:
        if session is not None:
            session.close()
    step.finish()


def p3_s4_aged_state(exe, home, env, verdict, seed, state):
    """P3-S4: aged state — a damaged trusted tree plus a seeded F5
    bootstrap-failures ledger. Bounded startup, documented one-line
    notices, guarded-loader silence, explicit-verb repair, silent after."""
    step = verdict.step(
        "P3-S4", "aged state: damaged tree + bootstrap-failures ledger "
        "(changelog 1.3.1 F5)")
    try:
        # ── seed between sessions (the `seed` capability) ──
        missing = seed.home / ".journey-missing-origin"
        target = missing.as_posix()
        seed.add_spec_source(f'target = "{target}"\n'
                             'id = "journey-missing-origin"')
        seed.seed_bootstrap_failure(
            target=target,
            error="seeded by the journey (the previous session's install "
                  "failed)")
        step.check("bootstrap-failures.toml ledger seeded for the "
                   "declared-but-missing origin",
                   seed.ledger_path.is_file()
                   and target in seed.ledger_path.read_text(encoding="utf-8"),
                   f"ledger: {seed.ledger_path}")
        old, new = seed.damage_source_file("oh-my-bash", "oh-my-bash.sh")
        step.check("trusted tree damaged (the guarded loader's entry file "
                   "renamed)",
                   new.is_file() and not old.exists(),
                   f"{old.name} -> {new.name}")

        # ── fresh terminal: bounded startup, documented notices ──
        started = time.time()
        term = Session(
            [str(exe)], home, env,
            raw_log=verdict.transcripts / "P3-S4-damaged.raw.ansi",
            label="P3-S4-damaged", delivery_log=verdict.delivery_events)
        try:
            secs = time_to_first_prompt(term, timeout=10.0, started=started)
            step.check("prompt within 10s (the failed install is deferred, "
                       "not retried at startup)",
                       secs is not None,
                       "no prompt within 10s"
                       if secs is None else f"{secs:.1f}s to first prompt")
            prompt_alive(term, "P3S4_ALIVE")
            drain_notices(term)
            body = term.transcript() + "\n" + term.raw_stripped()
            deferred = sync_row_keys(body, "deferred")
            failed = sync_row_keys(body, "failed")
            degraded = sync_row_keys(body, "degraded") | {
                line.strip()[:60] for line in body.splitlines()
                if "tree missing" in line}
            nags = sync_row_keys(body, "awaiting-trust")
            first = next(iter(sorted(deferred | failed | degraded)),
                         "(none)")
            step.check("at most the documented one-line notices (the "
                       "deferred memo line; no failed/degraded rows)",
                       len(deferred) == 1 and not failed and not degraded,
                       f"deferred={sorted(deferred)} failed={sorted(failed)} "
                       f"degraded={sorted(degraded)}; first: "
                       f"{first.strip()[:140]}")
            # The documented nag: one line per still-untrusted declared
            # source. The bound derives from the sandbox registry (the
            # J4 derivation): since wt78 expanded `full` to nine entries
            # the literal "<=2" went stale — five untrusted remain after
            # J6 trusted bash-completion, and every one of them must be
            # allowed to name itself.
            untrusted = untrusted_registry_ids(home)
            unexpected = nags - untrusted
            step.check("awaiting-trust notices stay documented (one per "
                       "untrusted source)",
                       not unexpected and len(nags) <= len(untrusted),
                       f"{len(nags)} source(s) vs {len(untrusted)} "
                       f"untrusted in the registry: {sorted(nags)}"
                       + (f"; unexpected: {sorted(unexpected)}"
                          if unexpected else ""))
            storm = {line.strip()[:120] for line in body.splitlines()
                     if "command not found" in line}
            step.check("guarded loader no-ops silently (no error storm)",
                       not storm,
                       f"{len(storm)} line(s), first: "
                       f"{sorted(storm)[0] if storm else ''}")
            verdict_for_syntax_errors(
                step, "zero syntax errors with the damaged tree",
                all_syntax_errors(term))
            verdict.capture("P3-S4-damaged-terminal", term)

            # ── explicit verbs retry and repair ──
            term.send_line("niu plugin sync", anchor_timeout=300)
            try:
                term.wait_for("journey-missing-origin", timeout=120)
                retry_body = term.raw_stripped()
                # The interactive verb's rows have no `niu plugin sync:`
                # prefix (that is the bootstrap form): `  failed <id>  …`.
                readable = any(
                    re.search(r"failed\s+journey-missing-origin\b", line)
                    for line in retry_body.splitlines())
                step.check("explicit sync retries the missing origin "
                           "(readable per-source failure)", readable,
                           "the origin was named but no failed row carried "
                           "it")
            except TimeoutError as err:
                step.check("explicit sync retries the missing origin "
                           "(readable per-source failure)", False, str(err))
            # The restore fetches the pinned commit from the origin; on a
            # degraded network this is the long pole (the five TLS-burned
            # release runs), so it carries the largest bound in the group
            # and fails fast on the transport error instead of timing out.
            term.send_line("niu plugin restore oh-my-bash",
                           anchor_timeout=900)
            try:
                needle = term.wait_for("Restored source 'oh-my-bash'",
                                       "git clone exited", timeout=600)
                step.check("`niu plugin restore oh-my-bash` rebuilt the "
                           "pinned tree",
                           needle == "Restored source 'oh-my-bash'",
                           f"restore returned: {needle}")
            except TimeoutError as err:
                step.check("`niu plugin restore oh-my-bash` rebuilt the "
                           "pinned tree", False, str(err))
            guarded = seed.sources_root / "oh-my-bash" / "oh-my-bash.sh"
            step.check("the damaged file is back after restore (tree "
                       "matches the lockfile pin)", guarded.is_file(),
                       f"{guarded}")
            # Heal the spec the documented way (hand-edit the declaration
            # away); the ledger entry — already cleared by the explicit
            # retry's clear-before-install — is inert either way.
            dropped = seed.drop_spec_source(".journey-missing-origin")
            step.check("healed: the missing origin's declaration dropped "
                       "from the spec", dropped)
            step.note("the spec heal is the documented hand-edit workflow "
                      "(plugins-guide: 'you can equally hand-edit the "
                      "spec and run niu plugin sync')")
        finally:
            term.close()

        # ── the terminal after repair is silent ──
        started = time.time()
        healed = Session(
            [str(exe)], home, env,
            raw_log=verdict.transcripts / "P3-S4-healed.raw.ansi",
            label="P3-S4-healed", delivery_log=verdict.delivery_events)
        try:
            secs = time_to_first_prompt(healed, timeout=10.0, started=started)
            step.check("terminal after repair reaches a prompt within 10s",
                       secs is not None,
                       "no prompt within 10s"
                       if secs is None else f"{secs:.1f}s to first prompt")
            prompt_alive(healed, "P3S4B_ALIVE")
            drain_notices(healed)
            body = healed.transcript() + "\n" + healed.raw_stripped()
            bad = [line for line in body.splitlines()
                   if re.search(r"niu plugin sync: (deferred|failed|"
                                "degraded)\b", line)]
            step.check("terminal after repair is silent (no deferred/"
                       "failed/degraded rows)", not bad,
                       f"{len(bad)} line(s), first: "
                       f"{bad[0].strip()[:140] if bad else ''}")
            nags = sync_row_keys(body, "awaiting-trust")
            untrusted = untrusted_registry_ids(home)
            unexpected = nags - untrusted
            step.check("only the documented awaiting-trust lines remain",
                       not unexpected and len(nags) <= len(untrusted),
                       f"{len(nags)} source(s) vs {len(untrusted)} "
                       f"untrusted in the registry: {sorted(nags)}"
                       + (f"; unexpected: {sorted(unexpected)}"
                          if unexpected else ""))
            verdict_for_syntax_errors(
                step, "zero syntax errors after repair",
                all_syntax_errors(healed))
            verdict.capture("P3-S4-healed-terminal", healed)
        finally:
            healed.close()
    except Exception as err:  # noqa: BLE001
        step.check("P3-S4 walk completed", False, f"{err}")
    step.finish()


def p8_s4_remove_active_theme_source(exe, home, env, verdict, seed, state):
    """P8-S4: `niu plugin source remove oh-my-bash` while its theme is
    applied — the F4 resurrection guard (spec declaration drops, no
    resurrection at the next startup) plus the degradation contract
    (guarded loader no-ops, floor prompt, no syntax-error storm), then
    re-add + re-enable restores the theme with no orphan rc blocks.

    The theme under test is DERIVED from the live journey state at step
    start (rc theme variable == the spec's oh-my-bash entry theme, with
    the assignment inside the oh-my-bash managed block) — whatever theme
    P4 left active IS the state under test, and P4 legitimately re-picks
    after J2 (release runs 37218948432 / 37224044818 red a hardcoded
    `powerline-multiline` precondition here while the product was correct
    at every hop). No active theme is a legal state too: the
    removal-then-restore contract then runs themeless (re-add + trust
    only; nothing may resurrect)."""
    step = verdict.step(
        "P8-S4", "remove the source providing the ACTIVE theme "
        "(changelog 1.3.1 F4)")
    tree = seed.sources_root / "oh-my-bash"
    registry_path = seed.sources_root / "registry.toml"
    try:
        rc_text = seed.rc_bytes().decode("utf-8", errors="replace")
        spec_text = seed.spec_text()
        vars_now = rc_theme_vars(rc_text)
        omb_entry = spec_entry(spec_text, "oh-my-bash")
        declared = entry_field(omb_entry, "theme") if omb_entry else None
        active_var = ("OSH_THEME" if "OSH_THEME" in vars_now
                      else "BASH_IT_THEME" if "BASH_IT_THEME" in vars_now
                      else None)
        active_theme = vars_now.get(active_var) if active_var else None
        block_vars = {var: val.strip().strip("'\"")
                      for var, val in re.findall(
                          THEME_VAR_LINE,
                          rc_managed_block(rc_text, "oh-my-bash") or "")}
        # Themed state: one active variable, the spec's oh-my-bash entry
        # declares the same theme, and the assignment lives in the
        # oh-my-bash managed block (a mismatch is an rc/spec disagreement
        # — a #168-class red, never silently tolerated here).
        themed = bool(active_var and active_theme
                      and active_theme == declared
                      and block_vars.get(active_var) == active_theme)
        theme_line_now = f"{active_var}='{active_theme}'" if themed else ""
        ready = (tree.is_dir() and "oh-my-bash" in spec_text
                 and (themed or (active_var is None and not declared)))
        step.check("precondition: oh-my-bash installed with the active "
                   "theme it declares (live state)", ready,
                   f"rc active: {active_var}={active_theme!r}; spec "
                   f"declares: {declared!r}; assignment inside the "
                   f"oh-my-bash block: {block_vars}; tree: {tree.is_dir()}")
        if not themed:
            step.note("no theme active at step start — the removal-then-"
                      "restore contract runs themeless (re-add + trust "
                      "only; nothing may resurrect)")

        remover = Session(
            [str(exe)], home, env,
            raw_log=verdict.transcripts / "P8-S4-remove.raw.ansi",
            label="P8-S4-remove", delivery_log=verdict.delivery_events)
        try:
            prompt_alive(remover, "P8S4_ALIVE")
            # The structural reference: the live theme's prompt rows as
            # THIS session renders them (same theme, same run — never a
            # cross-theme fingerprint).
            sig_ref = themed_prompt_signature(remover)
            if themed and not sig_ref:
                step.check("the active theme's prompt rendered before the "
                           "removal", False,
                           "the themed prompt's structural signature was "
                           "empty — no themed prompt rows rendered before "
                           "the removal")
            # Self-heal first: the removal contract only deletes a tree it
            # can fingerprint (sources.rs remove_source verifies the
            # adapter layout), so a tree still damaged from P3-S4 (whose
            # restore is network-bound) is repaired to the pin before the
            # removal — the documented repair verb, then the removal.
            entry_file = tree / "oh-my-bash.sh"
            if not entry_file.is_file():
                step.note("tree still damaged from P3-S4 — running the "
                          "documented repair (`niu plugin restore "
                          "oh-my-bash`) before the removal")
                remover.send_line("niu plugin restore oh-my-bash",
                                  anchor_timeout=900)
                try:
                    needle = remover.wait_for("Restored source 'oh-my-bash'",
                                              "git clone exited", timeout=600)
                    step.check("repair before removal (network-bound)",
                               needle == "Restored source 'oh-my-bash'",
                               f"restore returned: {needle}")
                except TimeoutError as err:
                    step.check("repair before removal (network-bound)",
                               False, str(err))
            remover.send_line("niu plugin source remove oh-my-bash",
                              anchor_timeout=120)
            try:
                remover.wait_for("Removed source 'oh-my-bash'", timeout=90)
                step.check("`niu plugin source remove oh-my-bash` reported "
                           "removal", True)
            except TimeoutError as err:
                step.check("`niu plugin source remove oh-my-bash` reported "
                           "removal", False, str(err))
        finally:
            remover.close()

        # F4's resurrection guard, on disk, immediately.
        step.check("F4: the spec declaration dropped",
                   "oh-my-bash" not in seed.spec_text(),
                   "plugins.toml still names oh-my-bash")
        registry_text = (registry_path.read_text(encoding="utf-8")
                         if registry_path.is_file() else "")
        step.check("F4: the registry record dropped",
                   "oh-my-bash" not in registry_text,
                   "registry.toml still records oh-my-bash")
        step.check("F4: the tree is gone", not tree.exists(),
                   f"{tree} still present")

        # Fresh terminal(s): floor prompt, no resurrection, no error storm.
        # The count is a matrix axis (wt87, owner standard: the gate must
        # hold in every cell, not the one we happened to run): resurrection
        # can first appear on a LATER startup (the memoized-defer path
        # differs from the first one), so NIU_JOURNEY_P8S4_FLOOR_TERMINALS
        # (default 1, matrix cell 3) walks that many consecutive fresh
        # terminals and asserts the contract on each.
        floor_terminals = max(1, _env_int("NIU_JOURNEY_P8S4_FLOOR_TERMINALS",
                                          1))
        if floor_terminals > 1:
            step.note(f"matrix cell: {floor_terminals} post-remove "
                      "terminals (NIU_JOURNEY_P8S4_FLOOR_TERMINALS)")
        for floor_idx in range(1, floor_terminals + 1):
            floor_nth = "" if floor_terminals == 1 else f" #{floor_idx}"
            floor = Session(
                [str(exe)], home, env,
                raw_log=verdict.transcripts / (
                    "P8-S4-floor.raw.ansi" if floor_idx == 1 else
                    f"P8-S4-floor-{floor_idx}.raw.ansi"),
                label="P8-S4-floor", delivery_log=verdict.delivery_events)
            try:
                row = await_first_prompt_row(floor)
                step.check(f"fresh terminal{floor_nth} without the theme "
                           "source: prompt renders (floor)", row is not None,
                           "no prompt-ish row ever rendered")
                drain_notices(floor)
                body = floor.transcript() + "\n" + floor.raw_stripped()
                step.check(f"no resurrection (terminal{floor_nth or ' 1'}): "
                           "no 'Cloning into' at the fresh terminal",
                           "Cloning into" not in body,
                           next((line.strip()[:120]
                                 for line in body.splitlines()
                                 if "Cloning into" in line), ""))
                step.check("no resurrection: the spec still lacks oh-my-bash",
                           "oh-my-bash" not in seed.spec_text())
                verdict_for_syntax_errors(
                    step, f"no syntax-error storm after removal"
                    f"{floor_nth}",
                    all_syntax_errors(floor))
                if themed and sig_ref:
                    floor_sig = themed_prompt_signature(floor)
                    step.check(f"prompt fell back to the floor "
                               f"(no longer the themed row){floor_nth}",
                               floor_sig != sig_ref,
                               f"floor signature: {floor_sig}; themed "
                               f"reference: {sig_ref}")
                rc_after = seed.rc_bytes().decode("utf-8", errors="replace")
                orphan = ">>> niu source oh-my-bash" in rc_after
                state["orphan_block_after_fresh_terminal"] = orphan
                if floor_idx == floor_terminals:
                    step.note(
                        "rc block status after the removal + "
                        f"{floor_terminals} fresh terminal(s): "
                        + ("an orphan oh-my-bash block is still in the rc "
                           "(inert: its guarded loader no-ops on the missing "
                           "tree); the steady-state no-orphan gate is "
                           "asserted after the re-add below" if orphan else
                           "the block was dropped — no orphan remained"))
                verdict.capture(
                    "P8-S4-floor-terminal"
                    + ("" if floor_idx == 1 else f"-{floor_idx}"), floor)
            finally:
                floor.close()

        # Re-add + trust + re-enable (the manifest's restore path).
        restorer = Session(
            [str(exe)], home, env,
            raw_log=verdict.transcripts / "P8-S4-restore.raw.ansi",
            label="P8-S4-restore", delivery_log=verdict.delivery_events)
        try:
            prompt_alive(restorer, "P8S4B_ALIVE")
            # The re-clone is the one network-bound leg of the group (the
            # TLS-reset family burned five release runs): one bounded
            # retry when the add fails CLEANLY (a reported clone failure,
            # not a wedge) — a second failure rules. Delivery/transport
            # retry only; the step's assertions still decide the verdict.
            installed = None
            add_detail = ""
            for add_attempt in (1, 2):
                restorer.send_line("niu plugin add oh-my-bash",
                                   anchor_timeout=900)
                try:
                    needle = restorer.wait_for(
                        "Installed source 'oh-my-bash'",
                        "could not install 'oh-my-bash'", timeout=600)
                    installed = needle == "Installed source 'oh-my-bash'"
                    add_detail = f"attempt {add_attempt}: {needle}"
                except TimeoutError as err:
                    installed = None
                    add_detail = f"attempt {add_attempt}: {err}"
                if installed or restorer.dead:
                    break
                if add_attempt == 1 and installed is False:
                    step.note("the re-add clone failed (TLS/transport "
                              "reset family) — retrying the add once; a "
                              "second failure rules")
                else:
                    break
            step.check("`niu plugin add oh-my-bash` re-installed the "
                       "source", installed is True, add_detail)
            restorer.send_line("niu plugin trust oh-my-bash",
                               anchor_timeout=90)
            try:
                restorer.wait_for("is now trusted", timeout=90)
                step.check("`niu plugin trust oh-my-bash` reported trusted",
                           True)
            except TimeoutError as err:
                step.check("`niu plugin trust oh-my-bash` reported trusted",
                           False, str(err))
            if themed:
                # Re-enable the theme that was ACTIVE at step start (the
                # live state's own pick — restoring the state P8-S4 found,
                # not the J2 pick).
                restorer.send_line(f"niu plugin enable oh-my-bash/"
                                   f"{active_theme}", anchor_timeout=120)
                try:
                    restorer.wait_for("Enabled", timeout=90)
                    step.check(f"`niu plugin enable oh-my-bash/"
                               f"{active_theme}` re-applied the theme", True)
                except TimeoutError as err:
                    step.check(f"`niu plugin enable oh-my-bash/"
                               f"{active_theme}` re-applied the theme",
                               False, str(err))
            else:
                step.note("themeless state — the restore leg is re-add + "
                          "trust only; nothing may resurrect")
        finally:
            restorer.close()

        # Steady state: the theme is back and no orphan blocks remain.
        final = Session(
            [str(exe)], home, env,
            raw_log=verdict.transcripts / "P8-S4-restored.raw.ansi",
            label="P8-S4-restored", delivery_log=verdict.delivery_events)
        try:
            row = await_first_prompt_row(final)
            step.check("fresh terminal after re-add: prompt renders",
                       row is not None,
                       "no prompt-ish row ever rendered")
            drain_notices(final)
            verdict.capture("P8-S4-restored-terminal", final)
            rc_text = seed.rc_bytes().decode("utf-8", errors="replace")
            blocks = rc_text.count(">>> niu source oh-my-bash")
            step.check("theme restored: the active theme line is active in "
                       "the rc again",
                       (theme_line_now in rc_text) if themed
                       else not rc_theme_vars(rc_text),
                       f"expected {theme_line_now!r}" if themed else
                       f"theme variables still on file: "
                       f"{sorted(rc_theme_vars(rc_text).items())}")
            step.check("no orphan blocks: exactly one oh-my-bash managed "
                       "block", blocks == 1,
                       f"{blocks} oh-my-bash block(s) in the rc")
            step.check("the spec declares oh-my-bash again",
                       "oh-my-bash" in seed.spec_text())
            verdict_for_syntax_errors(
                step, "zero syntax errors after the restore",
                all_syntax_errors(final))
            # The structural comparison: the restored terminal's prompt rows
            # must equal the pre-removal session's SAME-theme signature —
            # never a cross-theme fingerprint, never mixed with notice rows
            # (the wrapped-fragment artifact of runs 37218948432 /
            # 37224044818).
            sig_now = themed_prompt_signature(final)
            if themed and sig_ref:
                step.check("the themed prompt renders again (structural "
                           "match with the pre-removal prompt)",
                           sig_now == sig_ref,
                           f"signature now: {sig_now}; expected: {sig_ref}")
            elif not themed:
                step.check("nothing resurrects on screen (still the floor "
                           "signature)", sig_now != sig_ref if sig_ref
                           else not sig_now,
                           f"signature now: {sig_now}")
        finally:
            final.close()
    except Exception as err:  # noqa: BLE001
        step.check("P8-S4 walk completed", False, f"{err}")
    step.finish()


# ── The journey steps ───────────────────────────────────────────────────────
# One driver, one phase mechanism (the wt86/jmerge resolution): the base
# J1–J7 gate below always runs; registered phases compose AFTER it via
# PHASE_RUNNERS + --phases (see the phase section). `seed`/`state` thread
# the W1 capabilities + ordered facts (rc byte snapshots keyed by
# milestone, the picked theme line, the first themed prompt row) from the
# wizard block into every phase runner.
def journey(exe: Path, root: Path, verdict: Verdict,
            phases: list = None) -> str:
    home = (root / "home").resolve()
    home.mkdir(parents=True, exist_ok=True)
    env = build_env(home, exe)
    seed = SandboxSeed(home)
    state = {
        "rc_bytes": {},      # milestone -> rc bytes (P3-S2's equality grid)
        "raw_texts": {},     # label -> ANSI-stripped raw stream
        "theme_line": None,  # the exact rc theme line the wizard wrote
        "theme_var": None,   # OSH_THEME / BASH_IT_THEME
        "themed_prompt_row": None,
        "marker_file": P3_MARKER_FILE,
        "marker_value": P3_MARKER_VALUE,
    }
    phases = [p for p in (phases or []) if p in PHASE_RUNNERS]

    # ── J1 + J2 share one session: on a fresh install `niu` IS the wizard.
    step = verdict.step(
        "J1", "fresh-install first contact: v-banner + wizard")
    session = None
    j1_ok = True
    try:
        session = Session([str(exe)], home, env,
                           raw_log=verdict.transcripts / "J1J2.raw.ansi",
                           label="J1J2",
                           delivery_log=verdict.delivery_events)
    except Exception as err:  # noqa: BLE001 - any spawn failure is a J1 fail
        step.check("niu spawned under ConPTY", False, f"{err}")
        j1_ok = False
    if j1_ok:
        try:
            session.wait_for("Welcome to Niubash", timeout=90)
            step.check("welcome v-banner appears", True)
        except TimeoutError as err:
            step.check("welcome v-banner appears", False, str(err))
            j1_ok = False
        body = session.text()
        step.check("banner carries the version (v{CARGO_PKG_VERSION})",
                   re.search(r"v\d+\.\d+\.\d+", body) is not None)
        for needle, name in (
            ("Environment", "environment panel appears"),
            ("No external themes installed yet",
             "empty-ecosystem note (no external themes yet)"),
        ):
            try:
                session.wait_for(needle, timeout=30)
                step.check(name, True)
            except TimeoutError as err:
                step.check(name, False, str(err))
                j1_ok = False
        verdict.capture("J1-fresh-install", session)
    step.finish()

    def block_rest(reason):
        for later in ("J2", "J3", "J4", "J5", "J6", "J7"):
            verdict.step(later, f"{later} (blocked: {reason})").finish(
                status="blocked")
        # The selected phases never run on a broken base gate (nothing to
        # walk on) — record them blocked so the verdict stays honest.
        for phase_id in phases:
            verdict.step(phase_id, f"{phase_id} (blocked: {reason})").finish(
                status="blocked")

    if not j1_ok:
        if session is not None:
            session.close()
        block_rest("J1 failed")
        return verdict.seal()

    # ── J2: the wizard full run, same session.
    step = verdict.step(
        "J2", "wizard full run: full -> trust now -> powerline-multiline")
    try:
        try:
            session.wait_for("Plugin collection?", timeout=60)
            session.answer("4\r")  # 4 = full (both frameworks + hooks)
            step.check("collection menu answered with 'full'", True)
        except TimeoutError as err:
            raise AssertionError(f"collection question never appeared: {err}")

        # Q2.6 (the 2026-10-12 wizard surface; the step niubash#322 added to
        # the d3 smoke journey too): the TAB completion style menu runs on
        # every first run AND re-run. The current effective style (gnu on a
        # fresh install) is pre-highlighted, so Enter is a no-op pick — no
        # rc line is written when the pick equals the effective style.
        try:
            session.wait_for("TAB completion style?", timeout=60)
            session.answer(ENTER)
            step.check("completion-style question answered with the "
                       "pre-highlighted default", True)
        except TimeoutError as err:
            raise AssertionError(
                f"TAB completion style question never appeared: {err}")

        try:
            session.wait_for("niu-git", timeout=60)
            session.answer(ENTER)  # default Skip
            step.check("niu-git question answered with Skip", True)
        except TimeoutError as err:
            raise AssertionError(f"niu-git question never appeared: {err}")

        # Q4: external completion providers (carapace-bin) — Skip is the
        # highlighted default and writes no rc snippet; the journey never
        # opts in.
        try:
            session.wait_for("External completion providers", timeout=60)
            session.answer(ENTER)
            step.check("providers question answered with Skip", True)
        except TimeoutError as err:
            raise AssertionError(
                f"external completion providers question never appeared: "
                f"{err}")

        try:
            session.wait_for("Apply this configuration?", timeout=60)
            session.answer(ENTER)  # Apply is the highlighted default
            step.check("Apply gate confirmed", True)
        except TimeoutError as err:
            raise AssertionError(f"Apply gate never appeared: {err}")

        # The real clones: git's own "Cloning into ..." lands on the
        # terminal (oh-my-bash, bash-it, bash-completion, bash-preexec).
        try:
            session.wait_for("Cloning into", timeout=CLONE_TIMEOUT)
            step.check("clone progress visible on the terminal", True)
        except TimeoutError as err:
            raise AssertionError(f"no clone ever started: {err}")

        # Singular (one theme-bearing source) or plural (several); the
        # 'full' collection installs two theme-bearing frameworks. Abort
        # on the tour/REPL: an install that failed outright never asks
        # this question, and the wait must fail fast with the real reason.
        try:
            session.wait_for("to list its themes?", "to list their themes?",
                             timeout=CLONE_TIMEOUT,
                             abort_on=("press any key to continue",
                                       "Type `about` for a quick tour"))
            clones = session.raw_text().count("Cloning into")
            step.check("multiple sources cloned (>=2 clone progress lines)",
                       clones >= 2, f"counted {clones} 'Cloning into' lines")
            verdict.capture("J2-after-clones", session)
            # Every collection entry must have landed (a failed entry is a
            # real gap — the user sees the failure line in their terminal).
            # Settle first: the question draws while git stderr is still
            # draining, then read the RAW stream only — it is append-only
            # and byte-ordered, immune to menu redraws and screen clears.
            time.sleep(1.0)
            body = session.raw_stripped()
            (verdict.transcripts / "J2-apply-report.txt").write_text(
                body, encoding="utf-8", newline="\n")
            failed_lines = [line.strip() for line in body.splitlines()
                            if re.search(r"\bfailed\b", line)]
            if not failed_lines:
                step.check("every collection entry installed (no 'failed' "
                           "lines)", True)
            else:
                check_with_known_fail(
                    step, "every collection entry installed (no 'failed' "
                    "lines)", "\n".join(failed_lines))
            session.answer("2\r")  # 2 = Trust now (现在信任)
            step.check("trust-now chosen (现在信任)", True)
        except TimeoutError as err:
            raise AssertionError(f"trust question never appeared: {err}")

        # Theme gallery: >60 themes, then navigate to powerline-multiline.
        # The size proof is deterministic, not a screen scrape: after
        # navigating, powerline-multiline's own row number (it sorts deep
        # in the list) proves the gallery size; a mid-redraw scroll marker
        # can read "0 more" and must never gate on its own.
        try:
            session.wait_for("Pick a theme", timeout=120)
            verdict.capture("J2-gallery", session)
        except TimeoutError as err:
            raise AssertionError(f"theme gallery never appeared: {err}")

        def highlight_row(target: str):
            for line in session.text().splitlines():
                if "◆" in line and target in line:
                    return line
            return None

        picked = False
        # Settle before the first arrow: the gallery's own draw drains
        # queued keys (the wt61 lesson), and an arrow lost to that drain
        # is indistinguishable from a clamped one. Inside the walk the
        # existing dynamics stay: per-key resend is deliberately NOT
        # applied there (see answer()'s docstring) — the walk self-heals
        # by polling the highlight and re-verifying after the settle.
        session.wait_quiescent()
        for _ in range(600):
            if highlight_row("powerline-multiline") is not None:
                picked = True
                break
            session.proc.write(DOWN)
            time.sleep(NAV_GAP_SECONDS)
        # Redraw-race guard: rapid Downs can outrun the menu's repaint, so
        # settle and RE-VERIFY the highlight before confirming — an Enter
        # that races the repaint picks whatever row niu actually has.
        if picked:
            time.sleep(SETTLE_SECONDS)
            picked = highlight_row("powerline-multiline") is not None
        step.check("gallery navigation reached powerline-multiline", picked)
        row = highlight_row("powerline-multiline") or ""
        row_number = re.search(r"(\d+)\)", row)
        gallery_position = int(row_number.group(1)) if row_number else 0
        step.check("theme gallery lists >60 themes",
                   gallery_position > 60,
                   f"powerline-multiline sits at gallery position "
                   f"#{gallery_position}; any position past 60 proves the "
                   "gallery size")
        verdict.capture("J2-theme-highlighted", session)
        if not picked:
            raise AssertionError("powerline-multiline never highlighted")
        session.answer(ENTER)  # delivery-verified confirm

        # The wizard prints its finish screen with the undo receipts, then
        # the about-tour takes over (clears the screen, waits for a key).
        try:
            session.wait_for("Undo this run", timeout=120)
            step.check("finish screen listed undo receipts", True)
        except TimeoutError as err:
            step.check("finish screen listed undo receipts", False, str(err))
        try:
            session.wait_for("press any key to continue", timeout=60)
            session.answer(" ")  # delivery-verified tour keypress
        except TimeoutError:
            pass  # tour skipped (non-tty guard); the REPL marker still runs
        step.check("REPL alive after the wizard (first run continues)",
                   prompt_alive(session, "J2_ALIVE"))
        verdict.capture("J2-repl-alive", session)
    except AssertionError as err:
        step.check("wizard full run completed", False, str(err))
        verdict.capture("J2-stalled", session)
    finally:
        session.close()

    # Durable receipts (screen tails can drop at process exit — the files
    # are the truth, the smoke-journey discipline).
    rc_path = home / ".niubashrc"
    spec_path = home / ".niubash" / "plugins.toml"
    journal_path = home / ".niubash" / "setup-journal.toml"
    rc = rc_path.read_text(encoding="utf-8") if rc_path.is_file() else ""
    spec = spec_path.read_text(encoding="utf-8") if spec_path.is_file() else ""
    journal = (journal_path.read_text(encoding="utf-8")
               if journal_path.is_file() else "")
    finish_text = ""
    finish_file = verdict.transcripts / "J2-finish.txt"
    if finish_file.is_file():
        finish_text = finish_file.read_text(encoding="utf-8")
    step.check("~/.niubashrc written with the picked theme active",
               "OSH_THEME='powerline-multiline'" in rc
               or "BASH_IT_THEME='powerline-multiline'" in rc,
               f"rc exists: {rc_path.is_file()}")
    step.check("plugin spec written (oh-my-bash declared)",
               "oh-my-bash" in spec, f"spec exists: {spec_path.is_file()}")
    step.check("plugin spec declares bash-completion",
               "bash-completion" in spec)
    step.check("spec carries the picked theme",
               "powerline-multiline" in spec)
    step.check("setup journal records collection = 'full'",
               "collection = 'full'" in journal
               or 'collection = "full"' in journal)
    step.check("finish screen undo receipts captured",
               "Undo this run" in finish_text
               or "Undo this run" in session.raw_text())
    # The gallery assertion, made durable: the cloned oh-my-bash tree
    # itself holds the themes (>60).
    themes_dir = home / ".niubash" / "sources" / "oh-my-bash" / "themes"
    if themes_dir.is_dir():
        count = sum(1 for child in themes_dir.iterdir() if child.is_dir())
        step.check("cloned oh-my-bash carries >60 themes on disk",
                   count > 60, f"{count} theme directories")
    step.finish()

    # W1 baseline for P3-S2's byte-stability grid: the rc exactly as the
    # wizard left it, plus the picked theme's exact line and variable.
    state["rc_bytes"]["after-wizard"] = rc_path.read_bytes() \
        if rc_path.is_file() else b""
    picked = re.search(r"^((?:OSH|BASH_IT)_THEME)='powerline-multiline'$",
                       rc, re.M)
    if picked:
        state["theme_var"] = picked.group(1)
        state["theme_line"] = picked.group(0)

    if step.record["status"] == "fail":
        block_rest("J2 failed")
        return verdict.seal()

    # ── J3: activation — source the rc in a live session, zero syntax errors.
    step = verdict.step(
        "J3", "activation: source ~/.niubashrc, zero syntax errors")
    s3 = Session([str(exe)], home, env,
                 raw_log=verdict.transcripts / "J3.raw.ansi", label="J3",
                 delivery_log=verdict.delivery_events)
    try:
        step.check("live session prompt renders",
                   prompt_alive(s3, "J3_ALIVE"))
        # source loads the theme loaders; its own completion wait below
        # is 60s, so the anchor bound matches it.
        s3.send_line("source ~/.niubashrc", anchor_timeout=60)
        # The marker only prints after the source finished (theme loaders
        # included); it is the completion signal, not decoration.
        s3.send_line("echo J3_SRC_DONE")
        try:
            s3.wait_for("J3_SRC_DONE", timeout=60)
            step.check("source ~/.niubashrc completed", True)
        except TimeoutError as err:
            step.check("source ~/.niubashrc completed", False, str(err))
        drain_notices(s3)
        verdict.capture("J3-after-source", s3)
        verdict_for_syntax_errors(
            step, "zero syntax errors after source ~/.niubashrc",
            all_syntax_errors(s3))
    finally:
        s3.close()
    step.finish()

    # ── J4: new terminal x3 — no re-downloads, no not-declared nags.
    step = verdict.step(
        "J4", "new-terminal x3: no re-clones, no nags, prompt renders")
    live = None
    for i in (1, 2, 3):
        s = Session([str(exe)], home, env,
                    raw_log=verdict.transcripts / f"J4-terminal-{i}.raw.ansi",
                    label=f"J4-{i}", delivery_log=verdict.delivery_events)
        try:
            marker = f"J4_READY_{i}"
            if not prompt_alive(s, marker):
                step.check(f"terminal #{i}: prompt renders", False,
                           f"echo {marker} never came back")
                continue
            step.check(f"terminal #{i}: prompt renders", True)
            drain_notices(s)
            body = s.transcript() + "\n" + s.raw_stripped()
            step.check(f"terminal #{i}: no 'Cloning into' (no re-download)",
                       "Cloning into" not in body)
            step.check(f"terminal #{i}: no 'not declared' nag",
                       "not declared" not in body)
            nag_lines = [line for line in body.splitlines()
                         if "awaiting-trust" in line]
            # The documented nag: one line per still-untrusted declared
            # source. Since niubash#171 the `full` collection lands EIGHT
            # sources and only oh-my-bash is trusted by the theme pick, so
            # the bound derives from the sandbox registry (the same state
            # sync --bootstrap reads) instead of a literal that every
            # entry-set change would stale: every notice must name a real
            # still-untrusted source, and the unique set may not exceed it
            # (the screen render and the raw stream can each hold a copy).
            untrusted_ids = set()
            registry_path = home / ".niubash" / "sources" / "registry.toml"
            if registry_path.is_file():
                registry = tomllib.loads(
                    registry_path.read_text(encoding="utf-8"))
                for record in registry.get("sources", []):
                    if not record.get("trusted", False):
                        untrusted_ids.add(record.get("id", ""))
                untrusted_ids.discard("")
            noticed = {m.group(1) for line in nag_lines
                       for m in [re.search(r"awaiting-trust\s+(\S+)", line)]
                       if m}
            unexpected = noticed - untrusted_ids
            step.check(f"terminal #{i}: awaiting-trust notices stay "
                       "documented (one per untrusted source)",
                       not unexpected and len(noticed) <= len(untrusted_ids),
                       f"{len(nag_lines)} line(s), {len(noticed)} unique vs "
                       f"{len(untrusted_ids)} untrusted sources"
                       + (f"; unexpected: {sorted(unexpected)}"
                          if unexpected else "")
                       + (f", first: {nag_lines[0].strip()[:100]}"
                          if nag_lines else ""))
            verdict.capture(f"J4-terminal-{i}", s)
            if i == 3:
                live = s
                s = None  # keep the third session alive for J5
        finally:
            if s is not None:
                s.close()
    step.finish()

    # ── J5: the daily battery, in the live session left open by J4.
    step = verdict.step(
        "J5", "daily battery: pipes, cd, test — output, not errors")
    if live is None:
        step.check("live session available from J4", False,
                   "J4 never produced a live session")
        step.finish(status="blocked")
    else:
        try:
            # `ls | wc -l` — a bare number line that was not on screen
            # before the command.
            before = live.text()
            live.send_line("ls | wc -l")
            got_count = False
            deadline = time.time() + 30
            while time.time() < deadline and not got_count:
                for line in live.text().splitlines():
                    if (re.fullmatch(r"\s*\d+\s*", line)
                            and line not in before.splitlines()):
                        got_count = True
                        break
                time.sleep(0.1)
            step.check("`ls | wc -l` printed a count", got_count)

            # `echo hi | cat -n` — the numbered line "1 hi".
            live.send_line("echo hi | cat -n")
            try:
                live.wait_for("1", timeout=30)
                time.sleep(0.5)
                got = any(re.search(r"1\s+hi", line)
                          for line in live.text().splitlines())
                step.check("`echo hi | cat -n` numbered the line", got)
            except TimeoutError as err:
                step.check("`echo hi | cat -n` numbered the line", False,
                           str(err))

            # `cd ~ && pwd` — a NEW line carrying the sandbox path (the
            # prompt already shows the cwd, so compare before/after).
            before = live.text()
            live.send_line("cd ~ && pwd")
            try:
                live.wait_for("journey-", timeout=30)
                time.sleep(0.5)
                got = any("journey-" in line and line not in before.splitlines()
                          and "pwd" not in line
                          for line in live.text().splitlines())
                step.check("`cd ~ && pwd` printed the sandbox home", got)
            except TimeoutError as err:
                step.check("`cd ~ && pwd` printed the sandbox home", False,
                           str(err))

            live.send_line("[[ a != b ]] && echo ok")
            try:
                live.wait_for("ok", timeout=30)
                time.sleep(0.3)
                got = any(re.search(r"^\s*ok\s*$", line)
                          for line in live.text().splitlines())
                step.check("`[[ a != b ]] && echo ok` printed ok", got)
            except TimeoutError as err:
                step.check("`[[ a != b ]] && echo ok` printed ok", False,
                           str(err))

            verdict.capture("J5-battery", live)
            # The picked theme must OWN the prompt: the default fallback
            # prompt is user@host:cwd# — if that shape prefixes the typed
            # battery commands, the theme never rendered (observed on
            # master 2026-10-03: bash-it powerline-multiline sourced
            # without a single syntax error yet PS1 stayed the default).
            battery_echoes = [line for line in live.transcript().splitlines()
                              if re.search(r"(wc -l|cat -n|&& pwd|echo ok)",
                                           line)]
            default_prompt = re.compile(r"^\S+@\S+:[^#]*#\s")
            themed = [line for line in battery_echoes
                      if not default_prompt.match(line)]
            step.check("theme prompt visible after each battery command",
                       bool(battery_echoes) and len(themed) == len(battery_echoes),
                       f"{len(themed)}/{len(battery_echoes)} typed command "
                       "lines carry a non-default prompt; first default-"
                       f"shaped line: {battery_echoes[0].strip()[:100] if battery_echoes else '(none)'}")
            verdict_for_syntax_errors(
                step, "zero syntax errors during the battery",
                all_syntax_errors(live))
            step.check("prompt still visible after the battery",
                       prompt_alive(live, "J5_STILL_HERE"))
            verdict.capture("J5-after", live)
        finally:
            live.close()
        step.finish()

    # ── J6: the owner's exact flow — trust + activate bash-completion.
    step = verdict.step(
        "J6", "trust + activate bash-completion, zero syntax errors")
    s6 = Session([str(exe)], home, env,
                 raw_log=verdict.transcripts / "J6.raw.ansi", label="J6",
                 delivery_log=verdict.delivery_events)
    try:
        # The aliveness probe and the trust verb travel as ONE anchored
        # line: two back-to-back sends after session open were the release
        # glue twice (37153503706 / 37157587488) — prompt_alive's
        # wait_for + 0.3s sleep is not an output anchor, and on slow
        # runners the next line appended to the unsubmitted input. One
        # send cannot glue with itself.
        s6.send_line("echo J6_ALIVE && niu plugin trust bash-completion",
                     anchor_timeout=90)
        try:
            s6.wait_for("J6_ALIVE", timeout=STARTUP_TIMEOUT)
            step.check("live session prompt renders", True)
        except TimeoutError:
            step.check("live session prompt renders", False,
                       "echo J6_ALIVE did not come back")
        # The trust step's own expected-output wait is 90s; the anchor
        # bound matches it so a slow trust run expires the anchor, not
        # the sequencing (run 37153503706's 90s timeout was the glue).
        s6.send_line("niu plugin trust bash-completion", anchor_timeout=90)
        try:
            s6.wait_for("is now trusted", timeout=90)
            step.check("`niu plugin trust bash-completion` reported trusted",
                       True)
        except TimeoutError as err:
            step.check("`niu plugin trust bash-completion` reported trusted",
                       False, str(err))
        s6.send_line("source ~/.niubashrc", anchor_timeout=60)
        s6.send_line("echo J6_SRC_DONE")
        try:
            s6.wait_for("J6_SRC_DONE", timeout=60)
            step.check("source ~/.niubashrc completed after trust", True)
        except TimeoutError as err:
            step.check("source ~/.niubashrc completed after trust", False,
                       str(err))
        drain_notices(s6)
        verdict.capture("J6-after-trust-source", s6)
        verdict_for_syntax_errors(
            step, "zero syntax errors after trust + source",
            all_syntax_errors(s6))
    finally:
        s6.close()

    # Activation completes in the next terminal: the materialized loader
    # block runs when a fresh shell sources the rc. This is where the
    # owner's bash_completion:1376 surfaced.
    s6b = Session([str(exe)], home, env,
                  raw_log=verdict.transcripts / "J6-fresh.raw.ansi",
                  label="J6-fresh", delivery_log=verdict.delivery_events)
    try:
        step.check("fresh terminal after trust: prompt renders",
                   prompt_alive(s6b, "J6B_ALIVE"))
        drain_notices(s6b)
        verdict.capture("J6-fresh-terminal", s6b)
        verdict_for_syntax_errors(
            step, "fresh terminal sources bash_completion cleanly",
            all_syntax_errors(s6b))
        # The activated completion must actually be loaded, not just
        # absent errors: bash_completion defines _init_completion.
        s6b.send_line("type _init_completion >/dev/null 2>&1 && echo BC_LOADED")
        try:
            s6b.wait_for("BC_LOADED", timeout=30)
            step.check("bash_completion functions are loaded", True)
        except TimeoutError as err:
            step.check("bash_completion functions are loaded", False, str(err))
    finally:
        s6b.close()
    step.finish()

    # ── J7: the theme gallery's live prompt preview (niubash#170). When the
    # highlight moves, the pane below the menu renders that theme's ACTUAL
    # prompt — the real PS1 expanded with that theme's config — in place of
    # the old static sentence. Bounded: a hung theme degrades to
    # "(preview unavailable: …)" within the render bound instead of
    # freezing the gallery.
    step = verdict.step(
        "J7", "gallery live preview: the pane renders the highlighted theme's real PS1")
    s7 = Session([str(exe), "setup"], home, env,
                 raw_log=verdict.transcripts / "J7.raw.ansi", label="J7",
                 delivery_log=verdict.delivery_events)
    try:
        try:
            s7.wait_for("Pick a theme", timeout=120)
            step.check("re-run wizard opens the theme gallery", True)
        except TimeoutError as err:
            raise AssertionError(f"gallery never appeared: {err}")
        s7.wait_quiescent()

        def preview_pane():
            """The fixed preview pane: the rows below the menu hint."""
            rows = s7.text().splitlines()
            hint = max(i for i, row in enumerate(rows) if "navigate" in row)
            pane = rows[hint + 2:hint + 7]
            while pane and not pane[-1].strip():
                pane.pop()
            return pane

        def settled_pane(timeout=5.0):
            """The pane after its async render landed: the callback paints
            instantly (placeholder first), the menu picks the render up on
            its idle poll — wait out that pickup, bounded."""
            deadline = time.time() + timeout
            while time.time() < deadline:
                pane = preview_pane()
                body = "\n".join(pane[1:])
                if body.strip() and "rendering preview" not in body:
                    return pane
                time.sleep(0.1)
            return preview_pane()

        # The old preview was a static sentence; the live pane replaces it.
        step.check("static sentence replaced by the live pane",
                   "renders via the bash-compatible PS1 channel"
                   not in s7.text())

        seen_headers = []
        rendered_any = False
        for position, key in enumerate(("2", "5", "9")):
            s7.answer(key)  # digit jump: the highlight moves, no confirm
            pane = settled_pane()
            header = pane[0].strip() if pane else ""
            body = "\n".join(pane[1:])
            seen_headers.append(header)
            # A real oh-my-bash theme renders its face within the render
            # bound; a placeholder still on screen after the settle means
            # the pane is not keeping up. Degraded rows are honest, but at
            # least one of the first themes must show a real prompt.
            if "rendering preview" not in body and body.strip():
                rendered_any = True
            (verdict.transcripts /
             f"J7-preview-{position + 1}.txt").write_text(
                f"header: {header}\npane:\n" + body, encoding="utf-8")
            # A pane that shows a theme header line (`<name> · <source>`)
            # proves the preview follows THIS highlight. The real gallery
            # mixes sources (J2 trusted oh-my-bash AND bash-it), so the
            # source name varies per row — assert the header anatomy, not
            # one source.
            step.check(f"preview follows highlight {position + 1}",
                       "·" in header and header.strip() != "",
                       f"header={header!r}")
        step.check("preview header changes as the highlight moves",
                   len(set(seen_headers)) == len(seen_headers),
                   f"{seen_headers!r}")
        step.check("at least one theme's real PS1 rendered in the pane",
                   rendered_any, f"panes={seen_headers!r}")
        verdict.capture("J7-preview-pane", s7)

        # Esc = use defaults everywhere; the wizard fast-forwards to the
        # Apply gate, Cancel leaves everything untouched — the preview never
        # blocks the flow it decorates.
        s7.answer("\x1b")
        try:
            s7.wait_for("Apply this configuration?", timeout=60)
            step.check("Esc fast-forwards past the gallery", True)
            s7.answer("2\r")  # 2 = Cancel: nothing was written
            step.check("cancel leaves the run side-effect free", True)
        except TimeoutError as err:
            step.check("Esc fast-forwards past the gallery", False, str(err))
    except AssertionError as err:
        step.check("gallery live preview verified", False, str(err))
        verdict.capture("J7-stalled", s7)
    finally:
        s7.close()
    step.finish()

    # ── Spec phases (journey-spec.md §3/§7; wave lanes) ─────────────────────
    # Composed after the base gate on the same sandbox: every phase walks on
    # the installed state J1–J7 leave. Runner functions register themselves
    # under their spec phase id in PHASE_RUNNERS (see the phase section
    # below); --phases selects (default: every registered phase, spec
    # order). A base-gate failure blocks them (nothing to walk on), exactly
    # like the early block_rest returns above.
    for phase_id in phases:
        try:
            PHASE_RUNNERS[phase_id](exe, home, env, verdict, seed, state)
        except Exception as err:  # noqa: BLE001 - a crashed phase must not
            # crash the gate out of the verdict: mark it blocked, seal the
            # rest of the run honestly.
            verdict.step(phase_id, f"{phase_id} (crashed: {err})").finish(
                status="blocked")

    return verdict.seal()


# ═════════════════════════════════════════════════════════════════════════════
# Spec phases (docs/journey-spec.md §3; the lane split of §7)
#
# Wave lanes land their phases HERE, as clearly-separated runner functions
# keyed by the spec's phase ids (docs/journey-steps.json). Lanes share only
# this registry and the --phases flag — never each other's step code — so
# their diffs to this file cannot collide: wt79/jw1-persistence registers
# P3 (+ P8-S4), wt80/jw2-wizardspec registers P4 + P7.
#
# Composition: the DEFAULT run is the base gate (J1–J7) followed by every
# registered phase in spec order (P3, P4, P7, P8) — the gate exercises
# everything. `--phases` selects subsets (`base` = the bare release gate);
# the wt79 single-value vocabulary folds in (full=all, gate=base,
# persist=P3,P8). Phases compose AFTER the base gate on the same sandbox —
# every phase walks on the installed state the base gate leaves — so a
# lane's local run is `--phases base,P4,P7`.
# ═════════════════════════════════════════════════════════════════════════════

PHASE_RUNNERS = {}


def register_phase(phase_id: str):
    """Register a phase runner under its journey-spec phase id."""
    def wrap(fn):
        PHASE_RUNNERS[phase_id] = fn
        return fn
    return wrap


# ── Shared phase helpers (assertions on what the USER sees: screen text and
#    files under the sandbox home; no product internals) ─────────────────────

def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8") if path.is_file() else ""


def read_bytes(path: Path) -> bytes:
    return path.read_bytes() if path.is_file() else b""


RC_BEGIN_MARK = "# >>> niu source {sid} (managed by `niu plugin enable/disable`) >>>"
RC_END_MARK = "# <<< niu source {sid} <<<"


def rc_managed_block(rc_text: str, source_id: str):
    """One managed source block INCLUDING its marker lines — the exact text
    a user sees in ~/.niubashrc — or None. Mirrors the product's marker
    spelling (plugins/assets.rs begin_marker/end_marker)."""
    begin = RC_BEGIN_MARK.format(sid=source_id)
    end = RC_END_MARK.format(sid=source_id)
    lines = rc_text.splitlines()
    for start, line in enumerate(lines):
        if line.strip() != begin:
            continue
        out = [line]
        for later in lines[start + 1:]:
            out.append(later)
            if later.strip() == end:
                return "\n".join(out)
        return None  # begin marker without its end: report as absent
    return None


def rc_managed_block_count(rc_text: str, source_id: str) -> int:
    begin = RC_BEGIN_MARK.format(sid=source_id)
    return sum(1 for line in rc_text.splitlines() if line.strip() == begin)


def rc_without_block(rc_text: str, source_id: str) -> str:
    block = rc_managed_block(rc_text, source_id)
    return rc_text.replace(block, "", 1) if block is not None else rc_text


THEME_VAR_LINE = re.compile(
    r"^(?:export\s+)?(OSH_THEME|BASH_IT_THEME)=(.+)$", re.M)


def rc_theme_vars(rc_text: str) -> dict:
    """The theme variables actually set in the rc (non-empty values) — the
    same two assignments setup_wizard.rs current_theme_pick reads to name
    the active pick."""
    vars_ = {}
    for match in THEME_VAR_LINE.finditer(rc_text):
        value = match.group(2).strip().strip("'").strip('"')
        if value:
            vars_[match.group(1)] = value
    return vars_


def spec_source_blocks(spec_text: str) -> list:
    """The [[sources]] entry bodies of the plugin spec (minimal split for
    user-visible file assertions only)."""
    blocks, current = [], None
    for line in spec_text.splitlines():
        if line.strip() == "[[sources]]":
            current = []
            blocks.append(current)
        elif current is not None:
            current.append(line)
    return ["\n".join(block) for block in blocks]


def entry_field(block: str, field: str):
    match = (re.search(rf"^{field}\s*=\s*'([^']*)'", block, re.M)
             or re.search(rf'^{field}\s*=\s*"([^"]*)"', block, re.M))
    return match.group(1) if match else None


def spec_entry(spec_text: str, source_id: str):
    """The [[sources]] block whose id or target names `source_id`."""
    for block in spec_source_blocks(spec_text):
        if (entry_field(block, "id") == source_id
                or entry_field(block, "target") == source_id):
            return block
    return None


def journal_value(journal_text: str, key: str):
    match = (re.search(rf"^{re.escape(key)}\s*=\s*'(.*)'\s*$", journal_text,
                       re.M)
             or re.search(rf'^{re.escape(key)}\s*=\s*"(.*)"\s*$',
                          journal_text, re.M))
    return match.group(1) if match else None


def fresh_terminal(exe, home, env, verdict, label: str) -> Session:
    """A fresh niu terminal in the sandbox — the user's 'open a new window'."""
    return Session([str(exe)], home, env,
                   raw_log=verdict.transcripts / f"{label}.raw.ansi",
                   label=label, delivery_log=verdict.delivery_events)


DEFAULT_PROMPT_SHAPE = re.compile(r"^\S+@\S+:[^#]*#\s")


def prompt_is_themed(session: Session, marker: str) -> bool:
    """The theme owns the prompt: the typed `echo <marker>` rows do not sit
    behind the default user@host:cwd# fallback shape (the J5 discipline —
    a sourced theme that leaves PS1 alone is not a rendered theme)."""
    typed = [line for line in session.transcript().splitlines()
             if f"echo {marker}" in line]
    return bool(typed) and all(not DEFAULT_PROMPT_SHAPE.match(line.strip())
                               for line in typed)


def gallery_highlighted(session: Session, candidates):
    """The highlighted gallery row naming one of `candidates`, excluding the
    option-0 'Skip - keep my current theme (…)' row — which names the
    CURRENT theme and would otherwise match before the walk moves."""
    for line in session.text().splitlines():
        if "◆" not in line or "keep my current theme" in line:
            continue
        for name in candidates:
            if name in line:
                return name
    return None


def gallery_walk_to(session: Session, candidates):
    """DOWN-walk the wizard theme gallery until the highlighted row names one
    of `candidates`; the J2 walk's dynamics (no per-key resend — a repaint
    can lag a delivered arrow — poll the highlight; settle + re-verify
    before the Enter so a racing Enter picks what niu actually has).
    Returns the matched candidate name, or None."""
    if isinstance(candidates, str):
        candidates = [candidates]
    session.wait_quiescent()
    picked = gallery_highlighted(session, candidates)
    for _ in range(900):
        if picked is not None:
            break
        session.proc.write(DOWN)
        time.sleep(NAV_GAP_SECONDS)
        picked = gallery_highlighted(session, candidates)
    if picked is not None:
        time.sleep(SETTLE_SECONDS)
        picked = gallery_highlighted(session, candidates)
    return picked


def wizard_rerun(session: Session, verdict: Verdict, step, capture: str,
                 pick, expect_current: str = None):
    """One `niu setup` re-run inside a live REPL session (setup_wizard.rs
    rerun_wizard — 'Reconfigure your interactive prompt/plugins. Existing
    rc will be backed up.'). pick=None answers the gallery's
    'Skip - keep my current theme' (the highlighted default); a name or
    candidate list is DOWN-walked in the gallery. Every question after the
    gallery (Q2.6 TAB completion style, Q3 niu-git, Q4 external completion
    providers) takes its highlighted default via Enter. Returns the theme
    name the run picked (None = Skip). Every key is gated on the wizard's
    own screens; delivery hardening comes from send_line/answer."""
    session.send_line("niu setup", anchor=False)
    session.wait_for("Reconfigure your interactive prompt/plugins", timeout=90)
    session.wait_for("Pick a theme", timeout=180)
    verdict.capture(capture, session)
    picked = None
    if pick is None:
        # The gallery draws progressively; let it finish before reading the
        # option rows (the Skip row names the current pick).
        session.wait_quiescent()
        row = next((line for line in session.text().splitlines()
                    if "keep my current theme" in line), None)
        step.check("gallery shows 'Skip - keep my current theme'",
                   row is not None)
        if expect_current is not None:
            step.check(
                f"the Skip option names the current pick ({expect_current})",
                row is not None and expect_current in row,
                f"option row: {row.strip()[:120]}" if row
                else "no Skip row on screen")
        session.answer(ENTER)
    else:
        picked = gallery_walk_to(session, pick)
        step.check(f"gallery walk reached the pick ({picked or pick})",
                   picked is not None)
        if picked is None:
            raise AssertionError(f"gallery walk never reached {pick}")
        verdict.capture(capture + "-highlighted", session)
        session.answer(ENTER)
    session.wait_for("TAB completion style?", timeout=60)
    # Q2.6 runs on every re-run too; the current effective style is
    # pre-highlighted, so Enter is a no-op pick (niubash#322's missing
    # step, same as the J2 and d3 fixes).
    session.answer(ENTER)
    session.wait_for("niu-git", timeout=60)
    session.answer(ENTER)  # Skip — the default; never auto-installs
    session.wait_for("External completion providers", timeout=60)
    session.answer(ENTER)  # Skip — the default; no carapace rc snippet
    session.wait_for("Apply this configuration?", timeout=60)
    session.answer(ENTER)  # Apply — the highlighted default
    session.wait_for("Shell rc written", timeout=120)
    try:
        session.wait_for("Undo this run", timeout=60)
        step.check("finish screen printed the undo receipts", True)
        session.wait_for("restore the previous rc", timeout=30)
        step.check("undo receipt names the rc restore (cp line)", True)
        if picked is not None:
            session.wait_for("niu plugin disable", timeout=30)
            step.check("undo receipt names `niu plugin disable` for the pick",
                       True)
    except TimeoutError as err:
        step.check("finish screen printed the undo receipts", False, str(err))
    verdict.capture(capture + "-finish", session)
    return picked


def hop_terminal_asserts(step, verdict: Verdict, exe, home, env, tag: str,
                         marker: str, picked: str, rc_path: Path,
                         spec_path: Path, journal_path: Path):
    """The user-visible state one wizard theme hop must leave, plus the
    fresh-terminal leg: rc guard block rewritten to the pick, exactly one
    framework's theme variable, journal + backup receipts, and — the niu#168
    gate — the pick STICKING across the terminal (rc block + spec agree).
    Green expected since wt72/themeback landed (1.3.4); a red is a plain
    #168 regression — the wt73 label retired with the fix."""
    rc = read_text(rc_path)
    block = rc_managed_block(rc, "oh-my-bash")
    step.check(f"{tag}: the rc guard block carries the pick",
               block is not None and f"OSH_THEME='{picked}'" in block,
               "block missing" if block is None else "\n".join(
                   line for line in block.splitlines() if "THEME" in line)[:160])
    vars_ = rc_theme_vars(rc)
    step.check(f"{tag}: OSH_THEME/BASH_IT_THEME never both present",
               not ("OSH_THEME" in vars_ and "BASH_IT_THEME" in vars_),
               f"theme variables on file: {sorted(vars_.items())}")
    count = rc_managed_block_count(rc, "oh-my-bash")
    step.check(f"{tag}: exactly one oh-my-bash guard block (no orphans)",
               count == 1, f"count = {count}")
    journal = read_text(journal_path)
    step.check(f"{tag}: journal records the pick",
               journal_value(journal, "theme") == picked,
               f"journal theme = {journal_value(journal, 'theme')!r}")
    backup = journal_value(journal, "rc_backup")
    step.check(f"{tag}: journal names an existing rc backup",
               bool(backup) and Path(backup).is_file(), f"rc_backup = {backup!r}")

    terminal = fresh_terminal(exe, home, env, verdict, f"{tag}-terminal")
    try:
        alive = prompt_alive(terminal, marker)
        step.check(f"{tag}: fresh terminal boots", alive)
        themed = alive and prompt_is_themed(terminal, marker)
        if themed:
            step.check(f"{tag}: fresh terminal renders a themed prompt", True)
        elif f"OSH_THEME='{picked}'" in (rc_managed_block(read_text(rc_path),
                                                          "oh-my-bash") or ""):
            # rc still carries the pick but the prompt fell back: a render
            # bug, not the rebound — plain red.
            step.check(f"{tag}: fresh terminal renders a themed prompt", False,
                       "the rc still carries the pick but the prompt fell "
                       "back to the default shape")
        else:
            check_with_known_fail(
                step, f"{tag}: fresh terminal renders a themed prompt",
                f"niu#168 theme rebound: the fresh "
                f"terminal's prompt fell back to the default shape because "
                f"the pick was already re-materialized away")
        drain_notices(terminal)
        verdict.capture(f"{tag}-terminal", terminal)
        errors = all_syntax_errors(terminal)
        if errors:
            check_with_known_fail(
                step, f"{tag}: zero syntax errors in the fresh terminal",
                "\n".join(errors))
        else:
            step.check(f"{tag}: zero syntax errors in the fresh terminal", True)
    finally:
        terminal.close()

    rc = read_text(rc_path)
    omb_entry = spec_entry(read_text(spec_path), "oh-my-bash")
    declared = entry_field(omb_entry, "theme") if omb_entry else None
    block_now = rc_managed_block(rc, "oh-my-bash") or ""
    if f"OSH_THEME='{picked}'" in block_now and declared == picked:
        step.check(f"{tag}: the pick sticks (rc block + spec agree)", True)
    else:
        check_with_known_fail(
            step, f"{tag}: the pick sticks (rc block + spec agree)",
            f"niu#168 theme ownership disagrees: "
            f"after the fresh terminal the rc carries "
            f"{sorted(rc_theme_vars(rc).items())} while the spec's oh-my-bash "
            f"entry declares theme = {declared!r} (picked {picked!r}) — the "
            "next sync re-materializes the stale claim over the pick")


# ── P4 — re-running setup + switching themes (wave lane W2, wt80) ────────────

@register_phase("P4")
def phase_p4(exe, home, env, verdict, seed=None, state=None):
    """journey-spec P4 — the #168 entry door (owner: 向导重选后) plus the first
    regression walk of the most-burned rc writer (#157/#159), the
    hand-migrated rc coexistence (#143), and the dual-framework same-name
    routing (#168 mechanics / wt61 G2). Runs on the base gate's sandbox:
    theme A ('powerline-multiline') active in rc + spec from J2's pick.
    (seed/state are the shared runner protocol; P4 walks the live files.)"""
    rc_path = home / ".niubashrc"
    spec_path = home / ".niubash" / "plugins.toml"
    journal_path = home / ".niubash" / "setup-journal.toml"
    theme_a = "powerline-multiline"   # the base gate's J2 pick
    hop_b_candidates = ["edsonarios", "agnoster", "brainy", "hawaii50",
                        "iterate"]  # any second gallery theme (spec names one)
    dual_theme = "powerbash10k"       # ships in oh-my-bash AND bash-it

    # ── P4-S1 — re-run `niu setup` on the existing install: the gallery
    # names the current pick; the run ends with exactly one theme state.
    step = verdict.step("P4-S1", "re-run `niu setup` on the existing install "
                                 "(rerun_wizard)")
    block_a_original = None
    s1_failed = False
    try:
        rc_before = read_text(rc_path)
        block_a_original = rc_managed_block(rc_before, "oh-my-bash")
        step.check("pre-state: the base gate left theme A in the rc guard "
                   "block",
                   block_a_original is not None
                   and f"OSH_THEME='{theme_a}'" in block_a_original)
        omb_entry = spec_entry(read_text(spec_path), "oh-my-bash")
        declared_a = entry_field(omb_entry, "theme") if omb_entry else None
        step.check("pre-state: the spec declares theme A",
                   declared_a == theme_a, f"declared = {declared_a!r}")
        wizard = fresh_terminal(exe, home, env, verdict, "P4-S1-wizard")
        try:
            step.check("live session ready",
                       prompt_alive(wizard, "P4_WIZ_READY"))
            wizard_rerun(wizard, verdict, step, "P4-S1", pick=None,
                         expect_current=theme_a)
            verdict_for_syntax_errors(
                step, "zero syntax errors during the re-run",
                all_syntax_errors(wizard))
        finally:
            wizard.close()
        # The settled state after a fresh terminal: rc block and spec agree
        # on one theme — never a third state.
        terminal = fresh_terminal(exe, home, env, verdict, "P4-S1-terminal")
        try:
            step.check("fresh terminal after the re-run: prompt renders",
                       prompt_alive(terminal, "P4S1_ALIVE"))
        finally:
            terminal.close()
        block_now = rc_managed_block(read_text(rc_path), "oh-my-bash")
        omb_entry = spec_entry(read_text(spec_path), "oh-my-bash")
        declared = entry_field(omb_entry, "theme") if omb_entry else None
        if (block_now is not None and f"OSH_THEME='{theme_a}'" in block_now
                and declared == theme_a):
            step.check("one theme state: rc block and spec agree on A", True)
        else:
            check_with_known_fail(
                step, "one theme state: rc block and spec agree on A",
                f"niu#168 theme rebound: rc block "
                f"theme vars {sorted(rc_theme_vars(read_text(rc_path)).items())}, "
                f"spec declares {declared!r} — expected both to agree on "
                f"{theme_a!r}")
    except AssertionError as err:
        s1_failed = True
        step.check("P4-S1 completed", False, str(err))
        step.finish()
    else:
        step.finish()
    if s1_failed:
        for later in ("P4-S2", "P4-S3", "P4-S4"):
            verdict.step(later, f"{later} (blocked: P4-S1 failed)").finish(
                status="blocked")
        return

    # ── P4-S2 — theme switch A→B→A through the wizard gallery; each hop
    # rewrites the rc block, journals the pick + receipts, and must stick.
    step = verdict.step("P4-S2", "theme switch A→B→A through the wizard gallery")
    try:
        wizard = fresh_terminal(exe, home, env, verdict, "P4-S2-wizard-B")
        try:
            step.check("hop A→B: live session ready",
                       prompt_alive(wizard, "P4S2B_READY"))
            picked_b = wizard_rerun(wizard, verdict, step, "P4-S2-hop-B",
                                    pick=hop_b_candidates)
        finally:
            wizard.close()
        if not picked_b:
            raise AssertionError("hop A→B never picked a second theme")
        step.check("hop A→B: a second theme was picked from the gallery",
                   picked_b in hop_b_candidates and picked_b != theme_a,
                   f"picked {picked_b!r}")
        hop_terminal_asserts(step, verdict, exe, home, env,
                             "P4-S2 hop A-B", "P4S2B_ALIVE", picked_b,
                             rc_path, spec_path, journal_path)

        wizard = fresh_terminal(exe, home, env, verdict, "P4-S2-wizard-A")
        try:
            step.check("hop B→A: live session ready",
                       prompt_alive(wizard, "P4S2A_READY"))
            picked_a = wizard_rerun(wizard, verdict, step, "P4-S2-hop-A",
                                    pick=theme_a)
        finally:
            wizard.close()
        if picked_a != theme_a:
            raise AssertionError(f"hop B→A picked {picked_a!r}, wanted A")
        hop_terminal_asserts(step, verdict, exe, home, env,
                             "P4-S2 hop B-A", "P4S2A_ALIVE", theme_a,
                             rc_path, spec_path, journal_path)
        block_restored = rc_managed_block(read_text(rc_path), "oh-my-bash")
        step.check("returning to A restores the managed block byte-identically",
                   block_restored == block_a_original,
                   "the oh-my-bash guard block bytes differ from the "
                   "pre-phase snapshot"
                   if block_restored != block_a_original else "")
    except AssertionError as err:
        step.check("P4-S2 completed", False, str(err))
    step.finish()

    # ── P4-S3 — execute the undo receipts the finish screen printed: the
    # exact `cp` restore line + the `niu plugin disable` line, then verify
    # the state and that nothing resurrects at the next sync (F4).
    step = verdict.step("P4-S3", "execute the undo receipts (cp restore + "
                                 "`niu plugin disable`)")
    disable_error = ""
    try:
        journal = read_text(journal_path)
        backup = journal_value(journal, "rc_backup")
        theme_pick = journal_value(journal, "theme")
        step.check("the journal carries the receipts to execute",
                   bool(backup) and bool(theme_pick),
                   f"rc_backup = {backup!r}, theme = {theme_pick!r}")
        backup_bytes = read_bytes(Path(backup)) if backup else b""
        undo = fresh_terminal(exe, home, env, verdict, "P4-S3-undo")
        try:
            step.check("live session ready", prompt_alive(undo, "P4S3_READY"))
            # Receipt 1: the exact `cp <backup> <rc>` line the journal named.
            undo.send_line(f"cp {backup} {rc_path}", anchor_timeout=30)
            step.check("the cp receipt restored the previous rc byte-for-byte",
                       read_bytes(rc_path) == backup_bytes)
            # Receipt 2: the exact `niu plugin disable <theme>` line.
            undo.send_line(f"niu plugin disable {theme_pick}",
                           anchor_timeout=90)
            try:
                undo.wait_for("removed from the spec", timeout=30)
                step.check("the disable receipt reported the removal", True)
            except TimeoutError:
                time.sleep(1.0)  # let the error line land before reading
                disable_error = "\n".join(
                    line.strip() for line in undo.raw_stripped().splitlines()
                    if "exists in multiple sources" in line)
                if disable_error:
                    check_with_known_fail(
                        step, "the disable receipt reported the removal",
                        f"the wizard-printed undo receipt failed at the "
                        f"REPL: {disable_error}")
                else:
                    step.check("the disable receipt reported the removal",
                               False, "no removal report within 30s")
        finally:
            undo.close()

        def s3_check(name, ok, evidence=""):
            """A downstream S3 assertion: green, or — when the disable
            receipt failed — a labeled known-fail carrying the receipt's
            own error (the cascade is the receipt bug, not a mystery)."""
            if ok:
                step.check(name, True)
            elif disable_error:
                check_with_known_fail(
                    step, name, f"{evidence} — cascade of the failed undo "
                    f"receipt: {disable_error}")
            else:
                step.check(name, False, evidence)

        block_now = rc_managed_block(read_text(rc_path), "oh-my-bash") or ""
        s3_check("after the receipts the rc block no longer activates a "
                 "theme", "OSH_THEME" not in block_now,
                 "\n".join(line for line in block_now.splitlines()
                           if "THEME" in line)[:160])
        omb_entry = spec_entry(read_text(spec_path), "oh-my-bash")
        declared = entry_field(omb_entry, "theme") if omb_entry else None
        s3_check("the spec's theme pick is explicitly cleared (theme = '')",
                 declared == "", f"declared = {declared!r}")
        verify = fresh_terminal(exe, home, env, verdict, "P4-S3-verify")
        try:
            step.check("session after the undo boots",
                       prompt_alive(verify, "P4S3_SYNC_READY"))
            verify.send_line("niu plugin sync", anchor_timeout=90)
            block_after = rc_managed_block(read_text(rc_path),
                                           "oh-my-bash") or ""
            s3_check("nothing resurrects at the next sync (F4)",
                     "OSH_THEME" not in block_after)
            verdict_for_syntax_errors(
                step, "zero syntax errors after the undo",
                all_syntax_errors(verify))
        finally:
            verify.close()
        step.note("the wizard re-runs successfully after the undo — P4-S4's "
                  "run demonstrates it")
    except AssertionError as err:
        step.check("P4-S3 completed", False, str(err))
    step.finish()

    # ── P4-S4 — dual-framework same-name theme routing: with BOTH frameworks
    # trusted, pick the name both ship; the gallery priority must route it
    # through oh-my-bash, exactly one guard block may activate it, and the
    # attribution must stick across a second terminal. Green expected since
    # wt72/themeback landed (1.3.4); a red is a plain #168 regression.
    step = verdict.step("P4-S4", "dual-framework same-name theme routing "
                                 f"({dual_theme} in oh-my-bash AND bash-it)")
    try:
        wizard = fresh_terminal(exe, home, env, verdict, "P4-S4-wizard")
        try:
            step.check("live session ready", prompt_alive(wizard, "P4S4_READY"))
            # The dual-framework precondition: a gallery lists trusted
            # sources only, so bash-it's themes join only after its trust.
            wizard.send_line("niu plugin trust bash-it", anchor_timeout=90)
            try:
                wizard.wait_for("is now trusted", timeout=90)
                step.check("bash-it trusted (both frameworks' themes are in "
                           "the gallery)", True)
            except TimeoutError as err:
                step.check("bash-it trusted (both frameworks' themes are in "
                           "the gallery)", False, str(err))
            picked = wizard_rerun(wizard, verdict, step, "P4-S4",
                                  pick=dual_theme)
        finally:
            wizard.close()
        if picked != dual_theme:
            raise AssertionError(f"gallery walk picked {picked!r}, wanted "
                                 f"{dual_theme!r}")
        # Immediately after the pick: exactly ONE framework's block may
        # activate the same-named theme.
        rc_now = read_text(rc_path)
        vars_now = rc_theme_vars(rc_now)
        omb_block = rc_managed_block(rc_now, "oh-my-bash") or ""
        step.check(f"the same-name pick routed to oh-my-bash (gallery "
                   f"priority): OSH_THEME='{dual_theme}'",
                   f"OSH_THEME='{dual_theme}'" in omb_block,
                   f"theme variables on file: {sorted(vars_now.items())}")
        step.check("BASH_IT_THEME never activated for the same-name pick",
                   "BASH_IT_THEME" not in vars_now,
                   f"theme variables on file: {sorted(vars_now.items())}")
        spec_now = read_text(spec_path)
        omb_entry = spec_entry(spec_now, "oh-my-bash")
        bash_it_entry = spec_entry(spec_now, "bash-it")
        declared_omb = entry_field(omb_entry, "theme") if omb_entry else None
        declared_bashit = (entry_field(bash_it_entry, "theme")
                           if bash_it_entry else None)
        if declared_omb == dual_theme and declared_bashit in (None, ""):
            step.check("spec framework attribution agrees with the rc guard "
                       "block", True)
        else:
            check_with_known_fail(
                step, "spec framework attribution agrees with the rc guard "
                      "block",
                f"niu#168 theme ownership disagrees: "
                f"the rc guard block carries OSH_THEME='{dual_theme}' but the "
                f"spec's oh-my-bash entry declares theme = {declared_omb!r} "
                f"and the bash-it entry {declared_bashit!r} — the wizard's "
                "pick never reached the spec, so the next sync re-materializes "
                "the stale claim over it")
        terminal = fresh_terminal(exe, home, env, verdict, "P4-S4-terminal")
        try:
            alive = prompt_alive(terminal, "P4S4_ALIVE")
            step.check("second terminal renders", alive)
            drain_notices(terminal)
            verdict.capture("P4-S4-terminal", terminal)
            errors = all_syntax_errors(terminal)
            if errors:
                check_with_known_fail(
                    step, "zero syntax errors in the second terminal",
                    "\n".join(errors))
            else:
                step.check("zero syntax errors in the second terminal", True)
        finally:
            terminal.close()
        rc_later = read_text(rc_path)
        vars_later = rc_theme_vars(rc_later)
        block_later = rc_managed_block(rc_later, "oh-my-bash") or ""
        if (f"OSH_THEME='{dual_theme}'" in block_later
                and "BASH_IT_THEME" not in vars_later):
            step.check("second terminal does not flip the framework or lose "
                       "the pick", True)
        else:
            check_with_known_fail(
                step, "second terminal does not flip the framework or lose "
                      "the pick",
                f"niu#168 theme rebound: after the "
                f"second terminal the rc carries "
                f"{sorted(vars_later.items())} — expected "
                f"OSH_THEME='{dual_theme}' in the oh-my-bash guard block only")
    except AssertionError as err:
        step.check("P4-S4 completed", False, str(err))
    step.finish()


# ── P7 — spec hand-editing (wave lane W2, wt80) ──────────────────────────────

@register_phase("P7")
def phase_p7(exe, home, env, verdict, seed=None, state=None):
    """journey-spec P7 — the documented power-user workflow: hand-edit
    `~/.niubash/plugins.toml` + `niu plugin sync` (plugins-guide 'Merge
    semantics: spec vs your hand edits', design §14.6), its corruption
    behavior (a wedged startup bricks every terminal — the hang class),
    and #168's inverse invariant (sync claims only what the spec declares).
    Theme-agnostic by design: it runs after whatever state P4 (or the bare
    base gate) left, snapshotting bytes before each mutation.
    (seed/state are the shared runner protocol; P7 writes the live files.)"""
    rc_path = home / ".niubashrc"
    spec_path = home / ".niubash" / "plugins.toml"
    fixture_root = home / "plugin-fixtures" / "tinysh"
    fixture_root.mkdir(parents=True, exist_ok=True)
    (fixture_root / "tiny.sh").write_text(
        "# journey P7 fixture: one sourceable file\n"
        "tiny_hello() { echo tiny-hello; }\n",
        encoding="utf-8", newline="\n")

    # ── P7-S1 — hand-add a source entry: sync installs it (untrusted) and
    # declares it; the second sync is a byte-identical no-op.
    step = verdict.step("P7-S1", "hand-add a source entry: sync installs it "
                                 "(untrusted) + declares it; second sync is a "
                                 "byte-identical no-op")
    session = None
    rc_stable = None
    try:
        session = fresh_terminal(exe, home, env, verdict, "P7-session")
        step.check("live session ready", prompt_alive(session, "P7_READY"))
        drain_notices(session)
        verdict.capture("P7-session-open", session)
        rc_stable = read_bytes(rc_path)
        spec_before = read_bytes(spec_path)
        hand_entry = (f"\n[[sources]]\n"
                      f"target = '{fixture_root.as_posix()}'\n"
                      f"enable = ['tiny.sh']\n").encode("utf-8")
        spec_path.write_bytes(spec_before + hand_entry)
        session.send_line("niu plugin sync", anchor_timeout=120)
        try:
            session.wait_for("awaiting-trust", timeout=90)
            step.check("sync installed the hand-declared source untrusted "
                       "(awaiting-trust row)", True)
        except TimeoutError as err:
            step.check("sync installed the hand-declared source untrusted "
                       "(awaiting-trust row)", False, str(err))
        try:
            session.wait_for("niu plugin trust tinysh", timeout=30)
            step.check("sync printed the exact trust verb for the new source",
                       True)
        except TimeoutError as err:
            step.check("sync printed the exact trust verb for the new source",
                       False, str(err))
        spec_after = read_bytes(spec_path)
        step.check("sync declared the entry (derived id bound into the spec)",
                   b"id = 'tinysh'" in spec_after)
        step.check("the untrusted install left the rc byte-identical",
                   read_bytes(rc_path) == rc_stable)
        session.send_line("niu plugin sync", anchor_timeout=120)
        step.check("second sync: rc byte-identical (no-op)",
                   read_bytes(rc_path) == rc_stable)
        step.check("second sync: spec byte-identical (no-op)",
                   read_bytes(spec_path) == spec_after)
        step.finish()
    except AssertionError as err:
        step.check("P7-S1 completed", False, str(err))
        step.finish()
        if session is not None:
            session.close()
        verdict.step("P7-S2", "P7-S2 (blocked: P7-S1 failed)").finish(
            status="blocked")
        verdict.step("P7-S3", "P7-S3 (blocked: P7-S1 failed)").finish(
            status="blocked")
        verdict.step("P7-S4", "P7-S4 (blocked: P7-S1 failed)").finish(
            status="blocked")
        return

    # ── P7-S2 — merge semantics: hand entries and wizard entries coexist,
    # and a hand-added selection survives a REAL materialization (the
    # documented hand_added ∪ next(spec) rule, §14.6).
    step = verdict.step("P7-S2", "sync merge semantics: hand + wizard entries "
                                 "coexist; hand-added selection survives")
    try:
        rc_before = read_text(rc_path)
        spec_before = read_bytes(spec_path)
        block = rc_managed_block(rc_before, "oh-my-bash")
        if block is None:
            raise AssertionError("no oh-my-bash managed block to hand-edit")
        marker_line = RC_BEGIN_MARK.format(sid="oh-my-bash")
        rc_hand = rc_before.replace(
            block,
            block.replace(marker_line + "\n",
                          marker_line + "\nplugins=('git')\n", 1), 1)
        rc_path.write_bytes(rc_hand.encode("utf-8"))
        session.send_line("niu plugin sync", anchor_timeout=120)
        try:
            session.wait_for("selection materialized", timeout=90)
        except TimeoutError:
            pass  # the file state below is the assertion, not the wording
        rc_after = read_text(rc_path)
        rc_after_bytes = read_bytes(rc_path)
        block_after = rc_managed_block(rc_after, "oh-my-bash") or ""
        step.check("the hand-added selection survives the materialization",
                   "plugins=('git')" in block_after,
                   "\n".join(line for line in block_after.splitlines()
                             if "plugins" in line)[:160])
        step.check("the merge touched nothing outside the edited block",
                   rc_without_block(rc_after, "oh-my-bash")
                   == rc_without_block(rc_hand, "oh-my-bash"))
        step.check("the merge left the theme variables untouched",
                   rc_theme_vars(rc_after) == rc_theme_vars(rc_before),
                   f"before {sorted(rc_theme_vars(rc_before).items())}, "
                   f"after {sorted(rc_theme_vars(rc_after).items())}")
        spec_now = read_text(spec_path)
        step.check("hand entry and wizard entries coexist (no clobber)",
                   spec_entry(spec_now, "tinysh") is not None
                   and spec_entry(spec_now, "oh-my-bash") is not None
                   and spec_entry(spec_now, "bash-completion") is not None)
        step.check("the sync left the spec file byte-identical",
                   read_bytes(spec_path) == spec_before)
        session.send_line("niu plugin sync", anchor_timeout=120)
        step.check("second sync: rc byte-identical (hand entry stays "
                   "hand-added, not re-merged)",
                   read_bytes(rc_path) == rc_after_bytes)
        step.check("second sync: spec byte-identical",
                   read_bytes(spec_path) == spec_before)
        omb_entry = spec_entry(read_text(spec_path), "oh-my-bash") or ""
        step.check("the hand-added item was not absorbed into the spec's own "
                   "set", "enable = ['git']" not in omb_entry,
                   (["line matches enable = ['git']"] if
                    "enable = ['git']" in omb_entry else []))
    except AssertionError as err:
        step.check("P7-S2 completed", False, str(err))
    step.finish()

    # ── P7-S3 — corrupt the spec (truncated file): sync fails SOFT with a
    # readable error, the default floor session still boots bounded, the rc
    # is never destroyed. A wedged startup here would brick every terminal.
    step = verdict.step("P7-S3", "corrupt spec (truncated TOML): sync fails "
                                 "soft, floor session still boots, no panic, "
                                 "rc not destroyed")
    try:
        rc_stable = read_bytes(rc_path)
        spec_good = read_bytes(spec_path)
        spec_path.write_bytes(spec_good[:40])
        session.send_line("niu plugin sync", anchor_timeout=30)
        raw = session.raw_stripped()
        step.check("the failed sync printed a readable error",
                   "niu:" in raw or "TOML" in raw or "toml" in raw)
        step.check("no panic anywhere in the session",
                   "panicked" not in raw)
        step.check("the failed sync did not touch the rc",
                   read_bytes(rc_path) == rc_stable)
        started = time.time()
        terminal = fresh_terminal(exe, home, env, verdict, "P7-S3-terminal")
        try:
            # The boot bound a human sees, measured from the FIRST BYTE (the
            # banner): the spawn/scheduling lag before it is this machine's,
            # never the product's. The wedge class this step guards (startup
            # bricked by a corrupt spec — the niu#145 hang family via the
            # plugin path) never renders a prompt at all, so any hard bound
            # catches it; the number absorbs documented machine load, because
            # the rc's loaders + the bootstrap child starve on CPU like
            # everything else (observed 2026-10-04: banner→prompt rendered in
            # ~1s on a quiet box, 10-15s under concurrent lane builds — the
            # spec's 10s figure assumed an idle machine). 30s from the banner
            # stays a wedge bound; the rendered-at number stays in the detail
            # so a slow boot is always visible. The echo-alive probe still
            # must pass, outside the bound (delivery retries are driver
            # latency too).
            first_byte = None
            while time.time() - started < 60.0:
                if terminal.last_nonempty_row():
                    first_byte = time.time() - started
                    break
                time.sleep(0.1)
            booted = None
            if first_byte is not None:
                while time.time() - started < first_byte + 30.0:
                    if PROMPTISH_LAST_ROW.match(terminal.last_nonempty_row()):
                        booted = time.time() - started
                        break
                    time.sleep(0.1)
            alive = prompt_alive(terminal, "P7S3_ALIVE")
            if booted is not None:
                bound_name = (f"the corrupt-spec startup still reaches a "
                              f"prompt (bounded; prompt rendered at "
                              f"{booted - first_byte:.1f}s after the banner)")
                bound_detail = (f"first byte at {first_byte:.1f}s, prompt at "
                                f"{booted:.1f}s, echo-alive={alive}")
            else:
                bound_name = ("the corrupt-spec startup still reaches a "
                              "prompt (bounded; no prompt-ish row within 30s "
                              "of the banner)")
                bound_detail = ((f"first byte at {first_byte:.1f}s"
                                 if first_byte is not None
                                 else "no output within 60s")
                                + f", echo-alive={alive}")
            step.check(bound_name, booted is not None and alive,
                       bound_detail)
            step.note("bound deviation, recorded: journey-steps.json says "
                      "'prompt within 10s'; the honest bound is 30s from the "
                      "banner — the corrupt boot rendered in ~1s on a quiet "
                      "box but 10-15s under concurrent lane builds (load, "
                      "not product; the wedge class never renders at all)")
            boot_raw = terminal.raw_stripped()
            step.check("the boot printed the soft error (never a silent "
                       "wrong shell)", "niu:" in boot_raw
                       or "TOML" in boot_raw or "toml" in boot_raw)
            step.check("no panic in the boot", "panicked" not in boot_raw)
            verdict.capture("P7-S3-corrupt-boot", terminal)
        finally:
            terminal.close()
        step.check("the failed startup did not touch the rc either",
                   read_bytes(rc_path) == rc_stable)
        # Restore the good spec; the explicit verb recovers cleanly.
        spec_path.write_bytes(spec_good)
        session.send_line("niu plugin sync && echo P7S3_RECOVERED",
                          anchor_timeout=120)
        try:
            session.wait_for("P7S3_RECOVERED", timeout=60)
            step.check("restoring the spec: explicit sync recovers", True)
        except TimeoutError as err:
            step.check("restoring the spec: explicit sync recovers", False,
                       str(err))
        step.check("rc still byte-stable after recovery",
                   read_bytes(rc_path) == rc_stable)
    except AssertionError as err:
        step.check("P7-S3 completed", False, str(err))
    step.finish()

    # ── P7-S4 — unknown keys in the spec are tolerated (no failure, no
    # destructive rewrite) and the rc stays byte-stable.
    step = verdict.step("P7-S4", "unknown spec keys are tolerated; rc "
                                 "byte-stable")
    try:
        rc_stable = read_bytes(rc_path)
        spec_before = read_bytes(spec_path)
        hand_key = b"\nnote_field = 'hand-added by the journey'\n"
        spec_path.write_bytes(spec_before + hand_key)
        session.send_line("niu plugin sync && echo P7S4_SYNC_OK",
                          anchor_timeout=120)
        try:
            session.wait_for("P7S4_SYNC_OK", timeout=60)
            step.check("sync succeeds with an unknown key in the spec", True)
        except TimeoutError as err:
            step.check("sync succeeds with an unknown key in the spec", False,
                       str(err))
        step.note("observed behavior: unknown keys are tolerated silently "
                  "(the spec parser ignores unrecognized fields; the known "
                  "fields are documented in plugins-guide.md 'The spec')")
        step.check("the unknown key survives the sync (spec not rewritten)",
                   read_bytes(spec_path) == spec_before + hand_key)
        step.check("rc byte-identical across the sync",
                   read_bytes(rc_path) == rc_stable)
    except AssertionError as err:
        step.check("P7-S4 completed", False, str(err))
    finally:
        if session is not None:
            session.close()
    step.finish()


# ── P3 — wave-1 persistence (wt79/jw1-persistence, journey-spec P3) ──────────
# Registered under the wt80 PHASE_RUNNERS mechanism (the wt86/jmerge
# resolution): the step functions above are the lane's own code, verbatim;
# this runner is their composition — S1 reopen, S2 rc byte-stability
# (#168 grid), S3 #167 no-wake first-key probe, S4 aged-state battery —
# in spec order, blocked as a group when the wizard left no theme line.

@register_phase("P3")
def phase_p3(exe, home, env, verdict, seed, state):
    p_steps = (
        ("P3-S1", p3_s1_reopen),
        ("P3-S2", p3_s2_rc_stability),
        ("P3-S3", p3_s3_first_key),
        ("P3-S4", p3_s4_aged_state),
    )
    blocked = None
    if state.get("theme_line") is None:
        blocked = ("no wizard theme line (J2 failed) — the aged-state "
                   "assertions need the picked theme")
    for step_id, fn in p_steps:
        if blocked is not None:
            verdict.step(step_id, f"{step_id} (blocked: {blocked})").finish(
                status="blocked")
            continue
        fn(exe, home, env, verdict, seed, state)


# ── P8 — wave-1 persistence, S4 only (wt79/jw1-persistence, journey-spec
# P8-S4): remove the source providing the ACTIVE theme (F4), then restore.
# Composes after P3 in spec order, so the tree it may need to self-heal is
# exactly what P3-S4 left.

@register_phase("P8")
def phase_p8(exe, home, env, verdict, seed, state):
    if state.get("theme_line") is None:
        verdict.step(
            "P8-S4", "P8-S4 (blocked: no wizard theme line (J2 failed) — "
            "the aged-state assertions need the picked theme)").finish(
                status="blocked")
        return
    p8_s4_remove_active_theme_source(exe, home, env, verdict, seed, state)


def main() -> int:
    # CI runners default to a charmap console; the journey transcript carries
    # CJK/emoji wizard text (the owner's own wording). Force UTF-8 stdio.
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser(
        description="The Golden User Journey gate (owner directive 2026-10-04)")
    parser.add_argument("niu", type=Path, help="path to the built niu.exe")
    parser.add_argument("--artifacts", type=Path, default=None,
                        help="where verdict + transcripts go "
                             "(default: target/journey-results/<timestamp>)")
    parser.add_argument("--keep-sandbox", type=Path, default=None,
                        help="create the sandbox under this directory "
                             "(kept on failure for diagnosis)")
    # Phase composition (journey-spec.md §7 lane split; the wt86/jmerge
    # unified mechanism): comma-separated entries. The base J1–J7 gate
    # ALWAYS runs first; then the registered phases named here. 'all'
    # (the DEFAULT) = every registered phase in spec order — the gate
    # exercises everything; 'base' = the bare release gate. The wt79
    # single-value --phase vocabulary folds in: full=all, gate=base,
    # persist=P3,P8. Lanes register phases in PHASE_RUNNERS.
    parser.add_argument("--phases", type=str, default="all",
                        help="comma-separated phases to compose after the "
                             "base gate (e.g. --phases P4,P7; 'all' = every "
                             "registered phase [default]; 'base' = J1-J7 "
                             "only; legacy --phase words accepted: full, "
                             "gate, persist)")
    # Hidden stress harness (owner-approved validation shape, not a user
    # knob): a randomized 0..N ms pause before every send's settle check,
    # simulating runner slowness — the shape that broke release runs
    # 37149660449 and 37153503706. Validated 3x with N=800.
    parser.add_argument("--stress-delay-ms", type=int, default=0,
                        help=argparse.SUPPRESS)
    args = parser.parse_args()

    global STRESS_DELAY_MS
    STRESS_DELAY_MS = max(0, args.stress_delay_ms)
    selected = [p.strip() for p in args.phases.split(",") if p.strip()]
    if not selected:
        selected = ["all"]
    # The wt79 single-value --phase vocabulary, folded into --phases.
    folded = []
    for entry in selected:
        if entry == "full":
            folded.append("all")
        elif entry == "gate":
            folded.append("base")
        elif entry == "persist":
            folded.extend(["P3", "P8"])
        else:
            folded.append(entry)
    selected = folded
    unknown = [p for p in selected
               if p not in ("base", "all") and p not in PHASE_RUNNERS]
    if unknown:
        print(f"SKIP: unknown phase id(s) {unknown}; registered phases: "
              f"{sorted(PHASE_RUNNERS) or '(none)'}")
        return 2
    phases = sorted(PHASE_RUNNERS) if "all" in selected \
        else list(dict.fromkeys(p for p in selected if p in PHASE_RUNNERS))
    exe = args.niu.resolve()
    if not exe.is_file():
        print(f"SKIP: niu binary not found: {exe}")
        return 2
    if shutil.which("git") is None:
        print("SKIP: git not on PATH (the journey clones real origins)")
        return 2

    repo = Path(__file__).resolve().parent.parent.parent
    if args.artifacts is not None:
        artifacts = args.artifacts
    else:
        artifacts = repo / "target" / "journey-results" / datetime.now(
            ).strftime("%Y%m%d-%H%M%S")
    artifacts.mkdir(parents=True, exist_ok=True)
    verdict = Verdict(artifacts)

    if args.keep_sandbox is not None:
        args.keep_sandbox.mkdir(parents=True, exist_ok=True)
    sandbox_parent = (args.keep_sandbox or Path(tempfile.gettempdir())).resolve()
    # ABSOLUTE on purpose: a relative sandbox home doubles inside niu's
    # path joins (observed 2026-10-03: rc written to <home>/<home>/.niubashrc).
    sandbox = Path(tempfile.mkdtemp(prefix="journey-", dir=sandbox_parent)).resolve()

    (artifacts / "run.json").write_text(
        json.dumps({"niu": str(exe), "sandbox": str(sandbox),
                    "started_utc": now_utc(), "pid": os.getpid(),
                    "stress_delay_ms": STRESS_DELAY_MS,
                    "phases": ["base"] + phases},
                   indent=2) + "\n",
        encoding="utf-8", newline="\n")

    try:
        result = journey(exe, sandbox, verdict, phases=phases)
    finally:
        # Sandbox hygiene: the sources trees are heavy clones; keep them
        # only when a red gate (or a crash) needs diagnosis.
        if gate_is_red(artifacts):
            (artifacts / "sandbox-kept.txt").write_text(
                f"{sandbox}\n", encoding="utf-8", newline="\n")
            print(f"NOTE: sandbox kept for diagnosis: {sandbox}")
        else:
            shutil.rmtree(sandbox, ignore_errors=True)

    print((artifacts / "verdict.txt").read_text(encoding="utf-8"))
    print(f"artifacts: {artifacts}")
    return 0 if result == "pass" else 1


def gate_is_red(artifacts: Path) -> bool:
    """True when the verdict says anything but pass (or could not be
    read — an unread verdict keeps the sandbox for diagnosis)."""
    data = artifacts / "verdict.json"
    if not data.is_file():
        return True
    try:
        return json.loads(data.read_text(encoding="utf-8"))["result"] != "pass"
    except Exception:  # noqa: BLE001
        return True


if __name__ == "__main__":
    sys.exit(main())
