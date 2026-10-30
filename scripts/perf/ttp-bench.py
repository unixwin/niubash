#!/usr/bin/env python3
"""Time-to-first-prompt (TTP) bench: niu vs GNU bash, ConPTY, DSR answered.

niubash#373 sub-item 3: measure and attribute interactive first-prompt
latency against GNU bash BEFORE changing any startup/theme behavior. This
script is measurement only — it changes nothing and gates nothing; its job
is to make the next behavior change (if any) honest about what it buys.

Method (same discipline as scripts/perf/asset-timing.py):

  * every run is a fresh ConPTY session over a sandboxed HOME (no real
    profile/history/theme state is touched);
  * the terminal side answers the ESC[6n / ESC[c queries a real terminal
    answers (pywinpty's agent does this itself — the same reply the shared
    interactive test driver models), so this measures NORMAL interactive
    startup, not the DSR-silent degradation path;
  * TTP = spawn -> first prompt sentinel byte in the RAW stream. Absolute
    numbers carry this machine's ConPTY transport floor (1.5-2.5 s here,
    see scripts/perf/budgets.json) on BOTH sides; the comparison and the
    before/after delta are the usable outputs;
  * niu attribution: a second pass re-runs niu with NIU_TRACE_STARTUP=1 and
    parses the per-stage ticks (config load, prompt backend, completion
    state, ...) out of the stream, so the internal cost is attributable
    independently of the transport floor;
  * every session is killed on every exit path (ConPTY terminate does not
    reliably reap the child on Win10 19044 — same belt-and-braces as the
    theme sweep).

Usage:

    python scripts/perf/ttp-bench.py                       # defaults
    python scripts/perf/ttp-bench.py --runs 7 --json
    python scripts/perf/ttp-bench.py --niu target/release/niu.exe

Windows only (ConPTY + pywinpty), like the journey/theme harnesses.
Requires: pywinpty (pyte not needed — sentinels are read from raw bytes).
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

try:
    from winpty import PtyProcess
except ImportError:
    print("ttp-bench: pywinpty is required (pip install pywinpty); "
          "this bench is Windows/ConPTY only", file=sys.stderr)
    sys.exit(2)

PROMPT_SENTINEL = b"P1> "
BANNER_SENTINEL = "Niubash"

# Deterministic rc: ASCII prompt sentinel, no default plugin packs — the
# same shape the interactive test suites pin, so the bench measures the
# shell's floor, not a theme.
NIU_RC = "PS1='P1> '\nNIU_DISABLE_DEFAULT_PLUGINS=1\n"
# bash rc: the same sentinel. --rcfile keeps the user's bashrc out.
BASH_RC = "PS1='P1> '\n"

NIU_CANDIDATES = (
    "target/release/niu.exe",
    "target/debug/niu.exe",
)
BASH_CANDIDATES = (
    r"D:\Git\bin\bash.exe",
    r"C:\Program Files\Git\bin\bash.exe",
)


def first_existing(paths) -> Path | None:
    for p in paths:
        if p and Path(p).exists():
            return Path(p)
    return None


class BenchSession:
    """One ConPTY session with a raw-stream tap and hard kill on close."""

    def __init__(self, argv: list[str], cwd: Path, env: dict, cols=120, rows=30):
        self.proc = PtyProcess.spawn(argv, cwd=str(cwd), env=env,
                                     dimensions=(rows, cols))
        self.spawn_at = time.perf_counter()
        self._raw = bytearray()
        self.alive = True
        import threading
        self._reader = threading.Thread(target=self._pump, daemon=True)
        self._reader.start()

    def _pump(self):
        while self.alive:
            try:
                data = self.proc.read(256)
            except Exception:
                break
            if not data:
                continue
            if isinstance(data, str):
                data = data.encode("utf-8", "ignore")
            self._raw.extend(data)

    def raw(self) -> bytes:
        return bytes(self._raw)

    def write(self, data: str):
        try:
            self.proc.write(data)
        except (OSError, ValueError):
            pass

    def close(self):
        self.alive = False
        try:
            self.proc.terminate()
        except Exception:
            pass
        try:
            if self.proc.isalive():
                subprocess.run(["taskkill", "/PID", str(self.proc.pid), "/T", "/F"],
                               capture_output=True, timeout=15)
        except (OSError, subprocess.SubprocessError):
            pass


def sandbox_home(prefix: str) -> Path:
    root = Path(tempfile.mkdtemp(prefix=f"niu-ttp-{prefix}-"))
    (root / "home" / "tmp").mkdir(parents=True)
    (root / "start").mkdir()
    return root


def session_env(home: Path, start: Path, extra: dict | None = None) -> dict:
    system_root = os.environ.get("SystemRoot", r"C:\Windows")
    system32 = os.path.join(system_root, "System32")
    env = {
        "SystemRoot": system_root,
        "PATH": os.pathsep.join([system32, system_root]),
        "COMSPEC": os.path.join(system32, "cmd.exe"),
        "HOME": str(home),
        "USERPROFILE": str(home),
        "LOCALAPPDATA": str(home / "localappdata"),
        "APPDATA": str(home / "appdata"),
        "TEMP": str(home / "tmp"),
        "TMP": str(home / "tmp"),
        "TERM": "xterm",
        "NIU_LANG": "en",
        "RUST_LOG": "off",
        "RUST_BACKTRACE": "0",
    }
    if extra:
        env.update(extra)
    return env


def measure_ttp(argv: list[str], cwd: Path, env: dict, rc_text: str,
                rc_name: str, timeout_s: float) -> tuple[float, bytes]:
    """Spawn -> first prompt sentinel in the raw stream. Returns (ms, raw)."""
    home = cwd.parent / "home"
    (home / rc_name).write_text(rc_text, encoding="utf-8")
    sess = BenchSession(argv, cwd, env)
    try:
        deadline = time.perf_counter() + timeout_s
        while time.perf_counter() < deadline:
            raw = sess.raw()
            if PROMPT_SENTINEL in raw:
                return (time.perf_counter() - sess.spawn_at) * 1000.0, raw
            time.sleep(0.005)
        raise TimeoutError(f"no prompt sentinel within {timeout_s}s; "
                           f"tail: {sess.raw()[-400:]!r}")
    finally:
        sess.close()


def bench_shell(label: str, argv_for, rc_text: str, rc_name: str, runs: int,
                repo: Path, extra_env: dict | None, warmup: int = 1) -> list[float]:
    samples = []
    for i in range(runs + warmup):
        root = sandbox_home(label)
        try:
            ms, _ = measure_ttp(argv_for(root), root / "start",
                                session_env(root / "home", root / "start", extra_env),
                                rc_text, rc_name, timeout_s=30.0)
            if i >= warmup:
                samples.append(ms)
        finally:
            shutil.rmtree(root, ignore_errors=True)
    return samples


def parse_trace_ticks(raw: bytes) -> dict[str, float]:
    """NIU_TRACE_STARTUP lines -> {stage: cumulative_ms} (last seen wins)."""
    ticks: dict[str, float] = {}
    for line in raw.decode("utf-8", "ignore").splitlines():
        if "NIU_TRACE_STARTUP:" not in line:
            continue
        # "NIU_TRACE_STARTUP:   123.4 ms  (delta     12.3)  stage label"
        try:
            after = line.split("NIU_TRACE_STARTUP:", 1)[1].strip()
            ms_text = after.split("ms", 1)[0].strip()
            label = after.split(")", 1)[1].strip()
            ticks[label] = float(ms_text)
        except (ValueError, IndexError):
            continue
    return ticks


def median(values: list[float]) -> float:
    return statistics.median(values)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--runs", type=int, default=5,
                        help="measured runs per shell (default 5)")
    parser.add_argument("--niu", type=Path, default=None,
                        help="niu binary (default: target/release, then debug)")
    parser.add_argument("--bash", type=Path, default=None,
                        help="GNU bash binary (default: Git for Windows paths)")
    parser.add_argument("--json", action="store_true",
                        help="emit the result table as JSON")
    args = parser.parse_args()

    niu = args.niu or first_existing(NIU_CANDIDATES)
    bash = args.bash or first_existing(BASH_CANDIDATES)
    repo = Path(__file__).resolve().parents[2]
    if niu is None or not niu.exists():
        print("ttp-bench: no niu binary found (build one or pass --niu)",
              file=sys.stderr)
        return 2

    result: dict = {"niu_binary": str(niu), "bash_binary": str(bash) if bash else None,
                    "runs": args.runs, "samples_ms": {}, "median_ms": {},
                    "niu_trace_stages_ms": {}}

    samples = bench_shell("niu", lambda root: [str(niu)], NIU_RC, ".niubashrc",
                          args.runs, repo, None)
    result["samples_ms"]["niu"] = [round(s, 1) for s in samples]
    result["median_ms"]["niu"] = round(median(samples), 1) if samples else None

    # Attribution pass: same flow, NIU_TRACE_STARTUP=1, one representative
    # run parsed for the per-stage cumulative table (the ticks are internal
    # monotonic timing — transport-floor independent).
    root = sandbox_home("niu-trace")
    try:
        _, raw = measure_ttp([str(niu)], root / "start",
                             session_env(root / "home", root / "start",
                                         {"NIU_TRACE_STARTUP": "1"}),
                             NIU_RC, ".niubashrc", timeout_s=30.0)
        result["niu_trace_stages_ms"] = {
            k: round(v, 1) for k, v in parse_trace_ticks(raw).items()}
    finally:
        shutil.rmtree(root, ignore_errors=True)

    if bash and bash.exists():
        # bash --rcfile + --noprofile: interactive, deterministic rc only.
        # POSIX path for the rcfile inside bash's own dialect.
        def bash_argv(root: Path) -> list[str]:
            # C:\Users\...\home -> /c/Users/.../home (MSYS dialect; strip
            # the drive colon or bash reads no rcfile at all).
            posix_home = str(root / "home").replace("\\", "/")
            drive, rest = posix_home[0].lower(), posix_home[2:].lstrip(":")
            rc_posix = f"/{drive}{rest}/.bashrc"
            return [str(bash), "--noprofile", "--rcfile", rc_posix, "-i"]

        samples = bench_shell("bash", bash_argv, BASH_RC, ".bashrc",
                              args.runs, repo, None)
        result["samples_ms"]["bash"] = [round(s, 1) for s in samples]
        result["median_ms"]["bash"] = round(median(samples), 1) if samples else None
    else:
        print("ttp-bench: no GNU bash found; bash reference row skipped "
              "(pass --bash)", file=sys.stderr)

    if args.json:
        print(json.dumps(result, indent=2))
    else:
        print(f"niu binary : {niu}")
        print(f"bash binary: {bash}")
        print(f"runs       : {args.runs} (median of TTP spawn->'P1> ', "
              f"ConPTY transport floor included on both sides)")
        for shell in ("niu", "bash"):
            if result["median_ms"][shell] is not None:
                print(f"TTP {shell:>4}: median {result['median_ms'][shell]:>8.1f} ms   "
                      f"samples {result['samples_ms'][shell]}")
        if result["niu_trace_stages_ms"]:
            print("niu attribution (NIU_TRACE_STARTUP, cumulative ms):")
            for stage, ms in result["niu_trace_stages_ms"].items():
                print(f"  {ms:>8.1f} ms  {stage}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
