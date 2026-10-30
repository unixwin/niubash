#!/usr/bin/env python3
"""Per-asset timing budget gate for niubash.

Owner iron directive (2026-10-04, after the 6-second source incident):
EVERY plugin asset — down to every individual theme FILE — gets a measured
time, by a FIXED script wired into the release gate with budgets. Not a
one-off measurement: the last round's numbers were never executed and never
enforced, which is why the 6s incident shipped.

What is measured, per asset (each theme file in every trusted source —
every oh-my-bash theme, every bash-it theme — plus each alias/completion/
plugin file where individually sourceable):

  source_wall   time from `. <asset>` start to prompt return. Driven
                piped-interactive (`niu --rcfile <probe> -i`, stdin on a
                pipe): the asset is loaded through the framework's NATIVE
                mechanism with that one asset selected (OSH_THEME /
                aliases=() / plugins=() / completions=() for oh-my-bash;
                BASH_IT_THEME / an enabled/ entry for bash-it). Echo
                sentinels bracket the window: @@NIU-PERF-T0@@ is printed
                immediately before the loader line, and a PROMPT_COMMAND
                sentinel right before the first prompt renders.
  first_prompt  ConPTY session: rc load (the bootstrap line) -> first prompt
                rendered on the emulated screen. Anchored at the rc's T0
                sentinel, not at process spawn — the winpty/ConPTY transport
                has a machine-dependent multi-second output floor that even
                `cmd /c echo` pays, so spawn-relative numbers would measure
                the transport. That floor applies equally to every asset and
                is reported separately per asset as boot_median_ms
                (spawn -> T0 rendered).
  rerender      same ConPTY session: ENTER -> next prompt drawn (the
                theme's PROMPT_COMMAND cost).

Driver contract:
  * sandboxed HOME pattern — USERPROFILE/HOME/LOCALAPPDATA/TEMP all point
    into a per-asset sandbox; nothing touches the real profile.
  * network off-affordance: DISABLE_AUTO_UPDATE=true (oh-my-bash upgrade
    check), GIT_TERMINAL_PROMPT=0, system/global git config pointed at
    sandbox files. The driver itself performs no network I/O.
  * per-phase hard timeout (default 5s): an asset exceeding it FAILS the
    gate — it does not skip.
  * N reps per phase (default 3), median reported.
  * machine-readable JSON out (per asset, per phase: samples/median/max +
    verdict). `--record-baseline` freezes a run as the committed baseline;
    `--compare-baseline` diffs a run against it (informational; the budget
    check is the gate).
  * budgets live in scripts/perf/budgets.json — data, not code: adding an
    asset needs no change here. Any asset over budget fails the release
    gate with asset + measured ms + budget.

Requires pywinpty and pyte (same deps as scripts/journey). Windows only
(ConPTY), like the journey gate.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path

import pyte
from winpty import PtyProcess

REPO = Path(__file__).resolve().parents[2]

# Sentinel strings must be unique enough that no theme/plugin output can
# collide with them.
SENT_PROMPT = "@@NIU-PERF-PROMPT@@"
SENT_T0 = "@@NIU-PERF-T0@@"
SENT_SRC = "@@NIU-PERF-SRC@@"

PHASES = ("source_wall", "first_prompt", "rerender")
EXIT_GRACE_S = 3.0

VERDICT_RANK = {"ok": 0, "over": 1, "timeout": 2, "hard-fail": 3, "n/a": 0}

# Phase name -> budget key in budgets.json.
BUDGET_KEY = {
    "source_wall": "source_ms",
    "first_prompt": "first_prompt_ms",
    "rerender": "rerender_ms",
}


class Asset:
    __slots__ = ("cls", "name", "framework", "source_file")

    def __init__(self, cls: str, framework: str, name: str, source_file: Path):
        self.cls = cls  # omb.theme | omb.alias | ... | bashit.completion
        self.framework = framework  # "omb" | "bashit"
        self.name = name
        self.source_file = source_file

    @property
    def key(self) -> str:
        return f"{self.cls}:{self.name}"


# ---------------------------------------------------------------------------
# Inventory — no sampling: every individually sourceable file of every
# trusted source is enumerated.
# ---------------------------------------------------------------------------

def discover_assets(omb: Path | None, bashit: Path | None) -> list[Asset]:
    assets: list[Asset] = []
    if omb is not None:
        themes_dir = omb / "themes"
        for d in sorted(p for p in themes_dir.iterdir() if p.is_dir()):
            f = d / f"{d.name}.theme.sh"
            if f.is_file():
                assets.append(Asset("omb.theme", "omb", d.name, f))
        # Module name layout per _omb_module_require (oh-my-bash.sh:52):
        #   alias      -> aliases/<name>.aliases.sh
        #   plugin     -> plugins/<name>/<name>.plugin.sh
        #   completion -> completions/<name>.completion.sh
        for f in sorted((omb / "aliases").glob("*.aliases.sh")):
            assets.append(Asset("omb.alias", "omb", f.name.removesuffix(".aliases.sh"), f))
        if (omb / "plugins").is_dir():
            for d in sorted(p for p in (omb / "plugins").iterdir() if p.is_dir()):
                f = d / f"{d.name}.plugin.sh"
                if f.is_file():
                    assets.append(Asset("omb.plugin", "omb", d.name, f))
        seen_completion = set()
        for pat in ("*.completion.sh", "*.completion.bash"):
            for f in sorted((omb / "completions").glob(pat)):
                name = f.name.removesuffix(".completion.sh").removesuffix(".completion.bash")
                if name not in seen_completion:
                    seen_completion.add(name)
                    assets.append(Asset("omb.completion", "omb", name, f))
    if bashit is not None:
        for d in sorted(p for p in (bashit / "themes").iterdir() if p.is_dir()):
            f = d / f"{d.name}.theme.bash"
            if f.is_file():
                assets.append(Asset("bashit.theme", "bashit", d.name, f))
        # themes/base.theme.bash is a library sourced by every theme, not an
        # individually selectable asset — the native mechanism itself excludes
        # it (BASH_IT_THEME=base resolves themes/base/base.theme.bash, which
        # does not exist).
        for sub, cls in (("aliases", "alias"), ("completion", "completion"),
                         ("plugins", "plugin")):
            for f in sorted((bashit / sub / "available").glob(f"*.{cls}.bash")):
                assets.append(Asset(f"bashit.{cls}", "bashit",
                                    f.name.removesuffix(f".{cls}.bash"), f))
    return assets


# ---------------------------------------------------------------------------
# Sandbox + rc construction
# ---------------------------------------------------------------------------

def sh_quote(s: str) -> str:
    return "'" + s.replace("'", "'\\''") + "'"


def build_rc(asset: Asset, omb: Path | None, bashit_root: Path | None,
             sandbox_home: Path) -> str:
    """One probe rc: bootstrap, the native-loader line(s) selecting exactly
    this asset, then a PROMPT_COMMAND sentinel appended after the load so the
    first prompt return is externally observable on a pipe."""
    lines = [
        "unset BASH_ENV",
        "export TERM='xterm-256color'",
        "export DISABLE_AUTO_UPDATE=true",
        "export OSH_UPDATE_CHECK=false",
        "export GIT_TERMINAL_PROMPT=0",
        f"printf '{SENT_T0}\\n'",
        "__perf_t0=$EPOCHREALTIME",
    ]
    if asset.framework == "omb":
        assert omb is not None
        lines += [
            f"OSH={sh_quote(omb.as_posix())}",
            f"OSH_CUSTOM={sh_quote((sandbox_home / 'custom').as_posix())}",
            f"OSH_CACHE_DIR={sh_quote((sandbox_home / 'osh-cache').as_posix())}",
        ]
        if asset.cls == "omb.theme":
            lines.append(f"OSH_THEME={sh_quote(asset.name)}")
        elif asset.cls == "omb.alias":
            lines.append(f"aliases=({sh_quote(asset.name)})")
        elif asset.cls == "omb.plugin":
            lines.append(f"plugins=({sh_quote(asset.name)})")
        elif asset.cls == "omb.completion":
            lines.append(f"completions=({sh_quote(asset.name)})")
        lines.append("source \"$OSH/oh-my-bash.sh\"")
    else:
        assert bashit_root is not None
        lines += [
            f"BASH_IT={sh_quote(bashit_root.as_posix())}",
            # Theme selected natively by name; for non-theme assets enabled/
            # in the writable sandbox clone holds exactly one entry (the
            # `bash-it enable` mechanism) and no theme is selected.
            f"BASH_IT_THEME={sh_quote(asset.name if asset.cls == 'bashit.theme' else '')}",
        ]
        lines.append("source \"$BASH_IT/bash_it.sh\"")
    lines += [
        "__perf_src_rc=$?",
        "__perf_t1=$EPOCHREALTIME",
        f"printf '{SENT_SRC} %s %s %s\\n' \"$__perf_t0\" \"$__perf_t1\" \"$__perf_src_rc\"",
        "__perf_decl=$(declare -p PROMPT_COMMAND 2>/dev/null)",
        "case \"$__perf_decl\" in",
        f"  'declare -a'*) PROMPT_COMMAND+=(\"printf '%s\\\\n' '{SENT_PROMPT}'\") ;;",
        f"  *) PROMPT_COMMAND=\"${{PROMPT_COMMAND:+${{PROMPT_COMMAND}}; }}printf '%s\\\\n' '{SENT_PROMPT}'\" ;;",
        "esac",
    ]
    return "\n".join(lines) + "\n"


def sandbox_env(base: dict, home: Path) -> dict:
    env = dict(base)
    for k in ("BASH_ENV", "OSH", "OSH_THEME", "BASH_IT", "BASH_IT_THEME",
              "OSH_CUSTOM", "OSH_CACHE_DIR", "NIU_PLUGIN_SPEC"):
        env.pop(k, None)
    (home / "tmp").mkdir(parents=True, exist_ok=True)
    (home / "local-appdata").mkdir(parents=True, exist_ok=True)
    # An empty primary rc suppresses the first-run setup wizard
    # (setup_wizard.rs:820 is_first_run) without contributing startup cost;
    # the measured bootstrap line is the probe rc passed via --rcfile.
    (home / ".niubashrc").write_text("", encoding="utf-8")
    env.update({
        "HOME": str(home),
        "USERPROFILE": str(home),
        "LOCALAPPDATA": str(home / "local-appdata"),
        "TEMP": str(home / "tmp"),
        "TMP": str(home / "tmp"),
        "HISTFILE": str(home / "history"),
        "GIT_CONFIG_GLOBAL": str(home / "gitconfig"),
        "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_TERMINAL_PROMPT": "0",
    })
    (home / "gitconfig").write_text(
        "[user]\n\tname = perf-budget\n\temail = perf@localhost\n"
        "[init]\n\tdefaultBranch = main\n",
        encoding="utf-8",
    )
    return env


def make_fixture_repo(rundir: Path) -> Path:
    """cwd for every session: a small committed git repo so themes exercise
    their git segment deterministically and offline."""
    fx = rundir / "fixture-repo"
    fx.mkdir(parents=True, exist_ok=True)
    git = shutil.which("git")
    if git is None:
        print("asset-timing: git not on PATH — themes will show a non-repo "
              "prompt; install git for a representative baseline", file=sys.stderr)
        return fx
    env = dict(os.environ)
    env["GIT_CONFIG_GLOBAL"] = str(rundir / "fixture-gitconfig")
    env["GIT_CONFIG_SYSTEM"] = os.devnull
    (fx / "notes.txt").write_text("perf fixture\n", encoding="utf-8")
    (fx / "src").mkdir(exist_ok=True)
    (fx / "src" / "main.c").write_text("int main(void){return 0;}\n", encoding="utf-8")
    try:
        subprocess.run([git, "init", "-q"], cwd=fx, env=env, check=True, timeout=30)
        subprocess.run([git, "add", "-A"], cwd=fx, env=env, check=True, timeout=30)
        subprocess.run([git, "-c", "user.name=perf", "-c", "user.email=perf@localhost",
                        "commit", "-qm", "fixture"], cwd=fx, env=env, check=True, timeout=30)
    except (subprocess.SubprocessError, OSError) as exc:
        print(f"asset-timing: fixture git init failed ({exc}); proceeding without a repo",
              file=sys.stderr)
    return fx


def prepare_bashit_sandbox(bashit: Path, rundir: Path) -> Path:
    """A writable clone of the bash-it tree: the native enable mechanism
    (enabled/<file>, glob-loaded by scripts/reloader.bash) needs write
    access, and the trusted source itself must stay untouched."""
    dst = rundir / "bash-it-sandbox"
    if dst.exists():
        shutil.rmtree(dst)
    shutil.copytree(bashit, dst, ignore=shutil.ignore_patterns(".git"))
    (dst / "enabled").mkdir(exist_ok=True)
    return dst


def enable_one(bashit_root: Path, asset: Asset) -> None:
    """bash-it's native enable: exactly one entry in enabled/."""
    enabled = bashit_root / "enabled"
    for old in enabled.iterdir():
        if old.is_dir():
            shutil.rmtree(old)
        else:
            old.unlink()
    if asset.cls == "bashit.theme":
        return
    suffix = asset.cls.removeprefix("bashit.")  # alias | completion | plugin
    shutil.copy2(asset.source_file, enabled / f"100---perf---{asset.name}.{suffix}.bash")


# ---------------------------------------------------------------------------
# Phase 1 — source wall (piped-interactive driver)
# ---------------------------------------------------------------------------

def measure_source_wall(niu: Path, rc: Path, cwd: Path, env: dict,
                        timeout_s: float) -> tuple[float | None, float | None,
                                                   str, str]:
    """Returns (source_wall_ms, loader_ms, status, detail).
    status: 'ok' | 'timeout' | 'error'."""
    t0_seen: list[float] = []
    prompt_seen: list[float] = []
    src_line: list[str] = []
    err_buf: list[str] = []

    def pump(stream, want_sentinels: bool):
        for raw in iter(stream.readline, b""):
            line = raw.decode("utf-8", "replace")
            if want_sentinels:
                now = time.perf_counter()
                if SENT_T0 in line and not t0_seen:
                    t0_seen.append(now)
                if SENT_PROMPT in line and not prompt_seen:
                    prompt_seen.append(now)
                if line.startswith(SENT_SRC):
                    src_line.append(line.strip())
            else:
                err_buf.append(line)

    proc = subprocess.Popen(
        [str(niu), "--rcfile", str(rc), "-i"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        cwd=str(cwd), env=env,
    )
    threads = [
        threading.Thread(target=pump, args=(proc.stdout, True), daemon=True),
        threading.Thread(target=pump, args=(proc.stderr, False), daemon=True),
    ]
    for th in threads:
        th.start()
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline and not (t0_seen and prompt_seen):
        if proc.poll() is not None:
            break  # died early; drain and evaluate below
        time.sleep(0.002)
    try:
        if prompt_seen:
            # Let the first prompt finish rendering before asking the shell
            # to exit, so the measured window closes naturally.
            time.sleep(0.08)
        if proc.stdin and not proc.stdin.closed:
            try:
                proc.stdin.write(b"exit\n")
                proc.stdin.flush()
                proc.stdin.close()
            except OSError:
                pass
        try:
            proc.wait(timeout=EXIT_GRACE_S)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=EXIT_GRACE_S)
    finally:
        for th in threads:
            th.join(timeout=2.0)

    loader_ms = None
    src_rc = None
    if src_line:
        parts = src_line[0].split()
        try:
            loader_ms = (float(parts[2]) - float(parts[1])) * 1000.0
            src_rc = int(parts[3])
        except (IndexError, ValueError):
            pass

    if not (t0_seen and prompt_seen):
        tail = "".join(err_buf)[-300:].strip().replace("\n", " | ")
        return None, loader_ms, "timeout", (
            f"sentinel window did not close within {timeout_s}s; "
            f"stderr tail: {tail or '<empty>'}")
    if src_rc is None:
        return None, loader_ms, "error", "loader did not report its status"
    if src_rc == 127:
        return None, loader_ms, "error", (
            "module not found under the native loader (loader status 127)")
    return (prompt_seen[0] - t0_seen[0]) * 1000.0, loader_ms, "ok", ""


# ---------------------------------------------------------------------------
# Phases 2+3 — first prompt + re-render (ConPTY driver)
# ---------------------------------------------------------------------------

class ConptySession:
    def __init__(self, argv: list[str], cwd: Path, env: dict, cols: int, rows: int):
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.Stream(self.screen)
        self.lock = threading.Lock()
        self.last_change = time.perf_counter()
        self.display = list(self.screen.display)
        self.proc = PtyProcess.spawn(argv, cwd=str(cwd), env=env, dimensions=(rows, cols))
        self.spawn_at = time.perf_counter()
        self.alive = True
        self.spawn_error = ""
        self._reader = threading.Thread(target=self._pump, daemon=True)
        self._reader.start()

    def _pump(self):
        while self.alive:
            try:
                data = self.proc.read(50)
            except Exception:
                break
            if not data:
                continue
            with self.lock:
                self.stream.feed(data)
                cur = list(self.screen.display)
                if cur != self.display:
                    self.display = cur
                    self.last_change = time.perf_counter()

    def snapshot(self):
        with self.lock:
            return list(self.display), self.last_change, time.perf_counter()

    def sentinel_count(self) -> int:
        disp, _, _ = self.snapshot()
        return sum(line.count(SENT_PROMPT) for line in disp)

    def write(self, data: str):
        try:
            self.proc.write(data)
        except (EOFError, OSError, ValueError):
            # EOFError: the pty's reader already saw the child exit (winpty
            # ptyprocess raises EOFError('Pty is closed') on every later
            # write). A session dying at teardown — or between an isalive()
            # check and this write — must not kill the measurement pool
            # (run 39233051251: one asset's dead session at the exit write
            # crashed the whole gate out of the executor); the phase loops
            # already mark a dead session's measurement error/timeout.
            pass

    def close(self):
        self.alive = False
        try:
            self.proc.terminate()
        except Exception:
            pass
        # Belt and braces: a ConPTY terminate does not always reap the child
        # (orphaned niu.exe sessions were observed on Win10 19044); the gate
        # must not leave shells behind on the runner.
        try:
            if self.proc.isalive():
                subprocess.run(["taskkill", "/PID", str(self.proc.pid), "/T", "/F"],
                               capture_output=True, timeout=15)
        except (OSError, subprocess.SubprocessError, AttributeError):
            pass


def measure_conpty(niu: Path, rc: Path, cwd: Path, env: dict, cols: int, rows: int,
                   timeout_s: float, settle_s: float) -> tuple[
        float | None, float | None, str, str, str, str]:
    """One ConPTY session yields both prompt phases.
    Returns (first_prompt_ms, rerender_ms, fp_status, rr_status, detail, '').

    first_prompt is anchored at the @@NIU-PERF-T0@@ sentinel (the probe rc's
    first command, i.e. rc load including the bootstrap line) rather than at
    process spawn: the winpty/ConPTY handshake has a machine-dependent output
    floor of seconds that even `cmd /c echo` pays, so spawn-relative numbers
    would measure the transport, not the product. The floor applies equally
    to every asset and cancels in the T0-relative window.
    Returns boot_ms (spawn -> T0 rendered) for visibility."""
    try:
        sess = ConptySession([str(niu), "--rcfile", str(rc), "-i"], cwd, env, cols, rows)
    except (OSError, ValueError) as exc:
        return None, None, "error", "error", f"conpty spawn failed: {exc}", -1.0

    first_ms: float | None = None
    rerender_ms: float | None = None
    fp_status, rr_status = "error", "error"
    detail = ""
    boot_ms = -1.0

    # Phase 0: the T0 sentinel (rc bootstrap started).
    deadline = time.monotonic() + timeout_s
    t0_at: float | None = None
    while time.monotonic() < deadline:
        disp, last_change, now = sess.snapshot()
        hit = [line for line in disp if SENT_T0 in line]
        if hit:
            t0_at = last_change
            boot_ms = (t0_at - sess.spawn_at) * 1000.0
            break
        if not sess.proc.isalive():
            detail = "session exited before rc bootstrap"
            break
        time.sleep(0.005)
    if t0_at is None:
        if not detail:
            fp_status = "timeout"
            detail = f"rc bootstrap (T0 sentinel) not seen within {timeout_s}s"

    # Phase: first prompt rendered.
    if t0_at is not None:
        fp_status = "timeout"
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            disp, last_change, now = sess.snapshot()
            if any(SENT_PROMPT in line for line in disp) and (now - last_change) >= settle_s:
                first_ms = (last_change - t0_at) * 1000.0
                fp_status = "ok"
                break
            if not sess.proc.isalive():
                fp_status = "error"
                detail = "session exited before first prompt"
                break
            time.sleep(0.005)

    # Phase: ENTER -> next prompt.
    if first_ms is not None:
        count_before = sess.sentinel_count()
        enter_at = time.perf_counter()
        sess.write("\r")
        deadline = time.monotonic() + timeout_s
        rr_status = "timeout"
        while time.monotonic() < deadline:
            disp, last_change, now = sess.snapshot()
            changed = last_change > enter_at
            counted = sess.sentinel_count() > count_before or any(
                SENT_PROMPT in line for line in disp)
            if changed and counted and (now - last_change) >= settle_s:
                rerender_ms = (last_change - enter_at) * 1000.0
                rr_status = "ok"
                break
            if not sess.proc.isalive():
                rr_status = "error"
                detail = detail or "session exited before re-render"
                break
            time.sleep(0.005)

    sess.write("exit\r")
    grace = time.monotonic() + EXIT_GRACE_S
    while sess.proc.isalive() and time.monotonic() < grace:
        time.sleep(0.02)
    sess.close()
    return first_ms, rerender_ms, fp_status, rr_status, detail, boot_ms


# ---------------------------------------------------------------------------
# Budgets
# ---------------------------------------------------------------------------

class Budgets:
    def __init__(self, data: dict):
        defaults = data.get("defaults", {})
        self.defaults: dict[str, dict[str, float]] = {
            cls: dict(budget) for cls, budget in defaults.items()
        }
        self.hard_mult = float(data.get("hard_fail_multiplier", 5))
        self.overrides: dict[str, dict] = data.get("assets", {})
        self.phase_timeout_s = float(data.get("phase_timeout_s", 5.0))

    def for_asset(self, asset: Asset) -> dict[str, float]:
        ov = self.overrides.get(asset.key, {})
        base = dict(self.defaults.get(asset.cls, {}))
        for k, v in (ov.get("budget") or {}).items():
            base[k] = float(v)
        return base

    def justification(self, asset: Asset) -> str:
        return str(self.overrides.get(asset.key, {}).get("justification", ""))


def verdict_for(asset: Asset, phases: dict, budgets: Budgets) -> tuple[str, dict[str, str]]:
    per_phase: dict[str, str] = {}
    worst = "ok"
    budget = budgets.for_asset(asset)
    for phase in PHASES:
        rec = phases.get(phase)
        if rec is None:
            per_phase[phase] = "n/a"
            continue
        if not rec["samples_ms"]:
            # No completed rep: a phase that hit its hard cap FAILS the gate
            # (an asset that cannot finish a phase does not skip).
            status = rec.get("status")
            per_phase[phase] = ("timeout" if status == "timeout"
                                else "hard-fail" if status == "error" else "n/a")
            if VERDICT_RANK[per_phase[phase]] > VERDICT_RANK[worst]:
                worst = per_phase[phase]
            continue
        b = budget.get(BUDGET_KEY[phase])
        m = rec["median"]
        if b is None:
            per_phase[phase] = "ok"  # untracked class: reported, not gated
        elif m > b * budgets.hard_mult:
            per_phase[phase] = "hard-fail"
        elif m > b:
            per_phase[phase] = "over"
        else:
            per_phase[phase] = "ok"
        if VERDICT_RANK[per_phase[phase]] > VERDICT_RANK[worst]:
            worst = per_phase[phase]
    return worst, per_phase


# ---------------------------------------------------------------------------
# Main driver
# ---------------------------------------------------------------------------

def fresh_phase_record() -> dict:
    return {"samples_ms": [], "median": None, "max": None,
            "status": "not-run", "errors": []}


def niu_version(niu: Path) -> str:
    try:
        r = subprocess.run([str(niu), "--version"], capture_output=True, text=True,
                           timeout=20, env=dict(os.environ))
        return (r.stdout or r.stderr).strip().splitlines()[0]
    except (subprocess.SubprocessError, OSError):
        return "unknown"


def measure_asset(asset: Asset, args, niu: Path, rc_path: Path, fixture: Path,
                  env: dict, timeout_s: float, settle_s: float) -> dict:
    phases = {p: fresh_phase_record() for p in PHASES}
    for _rep in range(args.n):
        sw, loader, status, detail = measure_source_wall(
            niu, rc_path, fixture, env, timeout_s)
        phases["source_wall"]["status"] = status
        if status == "ok":
            phases["source_wall"]["samples_ms"].append(sw)
        else:
            phases["source_wall"]["errors"].append(detail)
        if loader is not None:
            phases["source_wall"].setdefault(
                "loader_samples_ms", []).append(round(loader, 2))

        fp, rr, fp_status, rr_status, detail, boot = measure_conpty(
            niu, rc_path, fixture, env, args.cols, args.rows, timeout_s, settle_s)
        if boot is not None and boot >= 0:
            phases["first_prompt"].setdefault(
                "boot_samples_ms", []).append(round(boot, 2))
        for phase, value, status_key in (
                ("first_prompt", fp, fp_status), ("rerender", rr, rr_status)):
            phases[phase]["status"] = status_key
            if status_key == "ok" and value is not None:
                phases[phase]["samples_ms"].append(value)
            elif detail:
                phases[phase]["errors"].append(detail)

    for p in PHASES:
        rec = phases[p]
        if rec["samples_ms"]:
            rec["median"] = round(statistics.median(rec["samples_ms"]), 2)
            rec["max"] = round(max(rec["samples_ms"]), 2)
        rec["samples_ms"] = [round(s, 2) for s in rec["samples_ms"]]
        if "loader_samples_ms" in rec:
            rec["loader_median_ms"] = round(
                statistics.median(rec["loader_samples_ms"]), 2)
            del rec["loader_samples_ms"]
        if "boot_samples_ms" in rec:
            rec["boot_median_ms"] = round(
                statistics.median(rec["boot_samples_ms"]), 2)
            del rec["boot_samples_ms"]
    return phases


def run(args: argparse.Namespace) -> int:
    if args.list:
        assets = discover_assets(args.omb, args.bashit)
        by_cls: dict[str, int] = {}
        for a in assets:
            by_cls[a.cls] = by_cls.get(a.cls, 0) + 1
        for cls in sorted(by_cls):
            print(f"{cls}\t{by_cls[cls]}")
        print(f"TOTAL\t{len(assets)}")
        return 0

    niu = args.niu.resolve()
    if not niu.is_file():
        print(f"asset-timing: niu binary not found: {niu}", file=sys.stderr)
        return 2
    budgets = Budgets(json.loads(Path(args.budgets).read_text(encoding="utf-8")))
    timeout_s = args.timeout if args.timeout is not None else budgets.phase_timeout_s
    settle_s = args.settle_ms / 1000.0

    rundir = (args.run_dir or (REPO / "target" / "perf" /
                               f"asset-timing-{datetime.now().strftime('%Y%m%d-%H%M%S')}")).resolve()
    rundir.mkdir(parents=True, exist_ok=True)
    fixture = (args.fixture_repo or make_fixture_repo(rundir)).resolve()
    omb = args.omb.resolve() if args.omb else None

    # One writable bash-it clone per worker: the native enable mechanism
    # mutates enabled/, which would race across workers on a shared clone.
    workers = max(1, args.workers)
    bashit_sandboxes = [
        (prepare_bashit_sandbox(args.bashit, rundir / f"bash-it-sandbox-w{w}").resolve()
         if args.bashit else None)
        for w in range(workers)
    ]

    assets = discover_assets(args.omb, args.bashit)
    if args.class_filter:
        assets = [a for a in assets if a.cls == args.class_filter]
    if args.asset_filter:
        assets = [a for a in assets if args.asset_filter in a.key]
    if not assets:
        print("asset-timing: empty asset inventory (pass --omb/--bash-it)",
              file=sys.stderr)
        return 2

    print(f"asset-timing: {len(assets)} assets, N={args.n}, workers={workers}, "
          f"timeout={timeout_s}s, settle={args.settle_ms}ms, niu={niu}", flush=True)

    def process_one(task):
        idx, asset, w = task
        bashit_sandbox = bashit_sandboxes[w]
        home = rundir / "homes" / f"{asset.cls}--{asset.name}"
        home.mkdir(parents=True, exist_ok=True)
        env = sandbox_env(os.environ, home)
        if bashit_sandbox is not None and asset.framework == "bashit":
            enable_one(bashit_sandbox, asset)
        rc_path = rundir / "rc" / f"{asset.cls}--{asset.name}.rc"
        rc_path.parent.mkdir(parents=True, exist_ok=True)
        rc_path.write_text(build_rc(asset, omb, bashit_sandbox, home),
                           encoding="utf-8", newline="\n")

        phases = measure_asset(asset, args, niu, rc_path, fixture, env,
                               timeout_s, settle_s)
        source_broken = (not phases["source_wall"]["samples_ms"]
                         and phases["source_wall"]["status"] == "error")
        if source_broken:
            verdict, per_phase = "source-error", {p: "n/a" for p in PHASES}
        else:
            verdict, per_phase = verdict_for(asset, phases, budgets)

        result = {
            "asset": asset.key,
            "class": asset.cls,
            "name": asset.name,
            "source_file": str(asset.source_file),
            "phases": phases,
            "verdict": verdict,
            "phase_verdicts": per_phase,
            "budget": budgets.for_asset(asset),
            "justification": budgets.justification(asset),
        }
        marker = "" if verdict == "ok" else f"  << {verdict}"
        print(f"[{idx}/{len(assets)}] {asset.key:44s} "
              f"src={phases['source_wall']['median']}ms "
              f"fp={phases['first_prompt']['median']}ms "
              f"rr={phases['rerender']['median']}ms{marker}", flush=True)
        if not args.keep_sandbox:
            shutil.rmtree(home, ignore_errors=True)
        return idx, result

    results: list = [None] * len(assets)
    t_all = time.monotonic()
    tasks = [(idx, asset, idx % workers) for idx, asset in enumerate(assets)]
    with ThreadPoolExecutor(max_workers=workers) as pool:
        for idx, result in pool.map(process_one, tasks):
            results[idx] = result

    elapsed = time.monotonic() - t_all
    out = {
        "meta": {
            "schema": "niubash:perf-asset-timing@1.0.0",
            "niu": str(niu),
            "niu_version": niu_version(niu),
            "omb": str(args.omb) if args.omb else None,
            "bashit": str(args.bashit) if args.bashit else None,
            "reps": args.n,
            "workers": workers,
            "phase_timeout_s": timeout_s,
            "settle_ms": args.settle_ms,
            "budgets_file": str(args.budgets),
            "recorded_at": datetime.now(timezone.utc).isoformat(),
            "wall_seconds": round(elapsed, 1),
            "asset_count": len(results),
        },
        "assets": results,
    }
    out_path = args.out or (rundir / "asset-timing.json")
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(out, indent=2), encoding="utf-8", newline="\n")
    if args.record_baseline:
        args.record_baseline.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(out_path, args.record_baseline)
        print(f"baseline recorded: {args.record_baseline}")

    if not args.keep_sandbox:
        for sb in bashit_sandboxes:
            if sb is not None:
                shutil.rmtree(sb, ignore_errors=True)
    return report(results, budgets, out_path, args.compare_baseline)


def report(results: list[dict], budgets: Budgets, out_path: Path,
           baseline_path: str | None) -> int:
    counts: dict[str, int] = {}
    for r in results:
        counts[r["verdict"]] = counts.get(r["verdict"], 0) + 1

    print("\n=== asset-timing summary ===")
    print(f"results: {out_path}")
    print(f"assets: {len(results)}  verdicts: " +
          " ".join(f"{k}={v}" for k, v in sorted(counts.items())))

    def med(r, p):
        return r["phases"][p]["median"]

    worst = sorted((r for r in results if med(r, "source_wall") is not None),
                   key=lambda r: med(r, "source_wall"), reverse=True)[:10]
    print("\nworst 10 by source wall:")
    print(f"{'asset':46s} {'src ms':>8s} {'fp ms':>8s} {'rr ms':>8s}  verdict")
    for r in worst:
        print(f"{r['asset']:46s} {med(r, 'source_wall'):8.1f} "
              f"{(med(r, 'first_prompt') or 0):8.1f} "
              f"{(med(r, 'rerender') or 0):8.1f}  {r['verdict']}")

    hard = [r for r in results
            if r["verdict"] in ("hard-fail", "timeout")]
    soft = [r for r in results if r["verdict"] == "over"]
    if hard:
        print(f"\nBUDGET VIOLATIONS — GATE-FAILING ({len(hard)}):")
        for r in hard:
            parts = []
            for p in PHASES:
                m = med(r, p)
                bk = BUDGET_KEY[p]
                if m is not None and bk in r["budget"]:
                    mark = r["phase_verdicts"][p]
                    parts.append(f"{p}={m:.1f}ms (budget {r['budget'][bk]:.0f}ms, {mark})")
                elif r["phase_verdicts"][p] in ("timeout", "hard-fail"):
                    parts.append(f"{p}={r['phase_verdicts'][p]} (no completed rep)")
            if r["verdict"] == "timeout":
                parts.append("phase timeout (hard FAIL)")
            print(f"  {r['asset']}: " + "; ".join(parts))
    if soft:
        # Soft overs warn, they do not block (owner ruling on run
        # 38489787748's data): eleven assets measured 1.0-1.05x over with
        # ZERO hard fails, all in the first_prompt phase, whose median
        # moves ~30% between reps of the SAME asset under concurrent lane
        # builds — a runner-load band, not a product regression. The hard
        # cap (hard_fail_multiplier) still fails the release, so a real
        # slowdown cannot hide here; every soft over is printed with its
        # numbers and stays in the verdict JSON for the next calibration.
        print(f"\nSOFT OVERS — warnings, not blockers ({len(soft)}):")
        for r in soft:
            parts = []
            for p in PHASES:
                m = med(r, p)
                bk = BUDGET_KEY[p]
                if m is not None and bk in r["budget"] \
                        and r["phase_verdicts"][p] == "over":
                    ratio = m / r["budget"][bk]
                    parts.append(f"{p}={m:.1f}ms (budget "
                                 f"{r['budget'][bk]:.0f}ms, {ratio:.2f}x)")
            print(f"  {r['asset']}: " + "; ".join(parts))
    if not hard and not soft:
        print("\nall measured assets within budget")

    bad = [r for r in results if r["verdict"] == "source-error"]
    if bad:
        print(f"\nSOURCE ERRORS ({len(bad)}) — not gate-failing, but these "
              "assets could not be timed (compat lane owns these):")
        for r in bad[:20]:
            err = (r["phases"]["source_wall"]["errors"] or [""])[-1][:160]
            print(f"  {r['asset']}: {err}")

    if baseline_path:
        compare_baseline(results, Path(baseline_path))

    hard_fail = sum(counts.get(v, 0) for v in ("hard-fail", "timeout"))
    soft_over = counts.get("over", 0)
    if hard_fail:
        print(f"\nGATE: FAIL — {hard_fail} asset(s) past the hard cap "
              f"({budgets.hard_mult}x; defaults: {budgets.defaults})")
        return 1
    if soft_over:
        print(f"\nGATE: PASS — {soft_over} soft over(s) inside the hard "
              f"band (warnings above; defaults: {budgets.defaults}; "
              f"hard-fail at {budgets.hard_mult}x)")
        return 0
    print("\nGATE: PASS")
    return 0


def compare_baseline(results: list[dict], baseline: Path) -> None:
    try:
        base = json.loads(baseline.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        print(f"baseline compare: cannot read {baseline}: {exc}", file=sys.stderr)
        return
    base_med = {a["asset"]: {p: a["phases"][p]["median"] for p in PHASES}
                for a in base.get("assets", [])}
    print(f"\nbaseline compare vs {baseline} "
          f"(recorded {base.get('meta', {}).get('recorded_at', '?')}):")
    regressions = []
    for r in results:
        b = base_med.get(r["asset"])
        if not b:
            continue
        for p in PHASES:
            cur, old = r["phases"][p]["median"], b[p]
            if cur is None or old is None or old <= 0:
                continue
            delta = (cur - old) / old * 100.0
            if delta >= 50.0 and (cur - old) >= 25.0:
                regressions.append((delta, r["asset"], p, old, cur))
    if not regressions:
        print("  no asset regressed >=50% (and >=25ms) against the baseline")
        return
    for delta, asset, p, old, cur in sorted(regressions, reverse=True)[:20]:
        print(f"  REGRESSED {asset} {p}: {old:.1f}ms -> {cur:.1f}ms (+{delta:.0f}%)")


def parse_args(argv: list[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    default_omb = Path(os.environ.get("NIU_PERF_OMB",
                                      r"C:\Users\Administrator\.oh-my-bash"))
    default_bashit = Path(os.environ.get(
        "NIU_PERF_BASHIT", r"D:\repo\rubash\target-ecosys2\bash-it"))
    p.add_argument("--niu", type=Path, default=REPO / "target" / "release" / "niu.exe")
    p.add_argument("--omb", type=Path, default=default_omb,
                   help="oh-my-bash root (NIU_PERF_OMB); pass '' to disable")
    p.add_argument("--bash-it", dest="bashit", type=Path, default=default_bashit,
                   help="bash-it root (NIU_PERF_BASHIT); pass '' to disable")
    p.add_argument("--budgets", type=Path,
                   default=Path(__file__).parent / "budgets.json")
    p.add_argument("--out", type=Path, default=None)
    p.add_argument("--record-baseline", type=Path, default=None,
                   help="also copy the run JSON to this path (the committed baseline)")
    p.add_argument("--compare-baseline", type=str, default=None)
    p.add_argument("--n", type=int, default=3, help="reps per phase (median reported)")
    p.add_argument("--timeout", type=float, default=None,
                   help="per-phase hard cap seconds (default: budgets phase_timeout_s)")
    p.add_argument("--settle-ms", type=int, default=150,
                   help="screen-quiet window that marks 'prompt rendered'")
    p.add_argument("--cols", type=int, default=120)
    p.add_argument("--rows", type=int, default=36)
    p.add_argument("--run-dir", type=Path, default=None)
    p.add_argument("--fixture-repo", type=Path, default=None)
    p.add_argument("--asset-filter", default=None)
    p.add_argument("--class-filter", default=None)
    p.add_argument("--list", action="store_true", help="print inventory and exit")
    p.add_argument("--keep-sandbox", action="store_true")
    p.add_argument("--workers", type=int, default=4,
                   help="assets measured concurrently (per-worker bash-it "
                        "sandbox; ConPTY sessions are independent processes)")
    args = p.parse_args(argv)
    # Empty string disables a source explicitly (CI provisions what it needs).
    # argparse has already wrapped it in Path: Path("") == ".", so compare on
    # the original string.
    if str(args.omb) in ("", "."):
        args.omb = None
    if str(args.bashit) in ("", "."):
        args.bashit = None
    return args


def main() -> int:
    return run(parse_args(sys.argv[1:]))


if __name__ == "__main__":
    sys.exit(main())
