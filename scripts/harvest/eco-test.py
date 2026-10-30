#!/usr/bin/env python3
"""eco-test.py — ConPTY-test every harvested ecosystem asset; stamp verdicts
into the manifest and emit the engine-divergence gold list.

Consumes the manifest produced by scripts/harvest/eco-harvest.py. Per asset:

  1. fetch content on demand (raw.githubusercontent at the pinned commit;
     gz cache under --cache-dir (dev-box E:/eco-harvest/cache when that drive
     exists, else <repo>/eco/cache — #356); T4 assets get sha256 backfilled),
  2. rubash -n pre-filter,
  3. if rubash accepts: ConPTY-test with a temporary HOME in `niu -i` (the wt91/journey
     harness shape: sandbox HOME with a minimal rc, marker-verified input
     delivery, hard timeouts on every phase) — source wall, first prompt
     (marker), ENTER, second ENTER,
  4. failures get GNU parity classification under a bash oracle. The oracle
     is Git for Windows' usr/bin/bash.exe — msys bash 5.2.x, shipped with
     every MinGit/GfW distribution and preinstalled on windows-latest
     runners, no install step (#409; ECO_GNU_BASH pins it explicitly) —
     with WSL GNU Bash 5.3 kept as the dev-box fallback. Both are driven
     with a file argument only; the oracle kind and version are stamped
     into every result via the build identity, because 5.2 and 5.3
     disagree on enough corners that an unversioned oracle result is
     ambiguous.

Verdicts:
  OK                        sourced; prompt + both ENTERs healthy
  SLOW                      OK but a phase exceeded --slow-threshold (2s)
  HANG                      a phase never completed within its hard cap AND
                            the oracle stayed healthy (engine suspect)
  ERROR-STORM               >= storm-lines error lines or > storm-bytes of
                            output during the phases AND the oracle stayed
                            healthy (engine suspect)
  EXITED                    the sourced asset killed the interactive shell
                            AND the oracle stayed healthy
  SYNTAX-REJECT-RUBASH-ONLY rubash -n rejects, oracle bash -n accepts
                            (ENGINE DIVERGENCE CANDIDATE — the gold)
  ASSET-NATURE              the oracle reproduces the failure (hang or
                            non-clean exit under the oracle too): upstream
                            content, not an engine bug (#409; absorbs the
                            old GNU-ALSO-FAILS)
  PARITY                    both engines agree the file is fine — e.g.
                            both `bash -n` reject the same pre-exec
                            strictness (extglob-before-shopt) while both
                            runtimes source it clean (#409)
  HARNESS-ERROR             the test harness itself raised
  DUPLICATE                 T4 late-dedup: sha256 already in the manifest
  FETCH-GONE                pinned raw URL is permanently gone
                            (404/410/451 — not retried, not gate-relevant)
  FETCH-FAILED              raw fetch failed after retries (resumable)

Results stream to results/test-results.jsonl (one line per asset, flushed).
Verdicts are keyed by binary SHA256 + checkout SHAs + harness SHA256 + GNU
oracle availability/version in the
manifest DB. Legacy verdicts are stale, not current. All tiers are selected
by default: unseen/transient first, then at most 80 stale assets, at most
400 total; dispatch stops after 2400 seconds and drains in-flight work.
Temporary HOME is NOT security isolation: untrusted assets must only run
on disposable runners, never on the developer host.
Every line carries a normalized `signature`; `--report` groups the gold
verdicts (SYNTAX-REJECT-RUBASH-ONLY + HANG) by minimal failing construct —
each group is one engine bug candidate with its asset list. Raw URLs are
percent-encoded per path segment at fetch time (#408: literal spaces/'&'
in manifest URLs made raw.githubusercontent reject the request).

Usage:
  python scripts/harvest/eco-test.py --tiers t1,t3 --workers 8
  python scripts/harvest/eco-test.py --report            # group the gold
  python scripts/harvest/eco-test.py --tiers all --workers 8

Exit codes: 0 clean · 2 setup failure (missing binary / unusable path)
            3 fetch gate tripped (#356/#408: FETCH-FAILED share above
            --max-fetch-failed-rate, default 15%)
            4 unexplained-failure gate tripped (#409: HANG+ERROR-STORM —
            and, without an oracle, unconfirmed SYNTAX-REJECT-RUBASH-ONLY —
            with NO oracle available to explain them)
"""

from __future__ import annotations

import argparse
import collections
import concurrent.futures
import datetime as dt
import gzip
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

PtyProcess = None


def load_pty():
    if os.name != "nt":
        raise RuntimeError("eco-test drives ConPTY (Windows only)")
    from winpty import PtyProcess
    return PtyProcess

NOW = lambda: dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

# ── Defaults ────────────────────────────────────────────────────────────────
REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_NIU = REPO_ROOT / "target" / "debug" / "niu.exe"
DEFAULT_RUBASH = Path("D:/repo/rubash/target/debug/rubash.exe")


def _data_dir(env: str, preferred: Path, fallback: Path) -> Path:
    """Env override > dev-box path (only when that drive exists) > fallback.

    The E:\\ defaults were this machine's layout (#356): on a runner with no
    E: drive the first `CACHE_ROOT.mkdir` raised `[WinError 3] 'E:\\'`, every
    nightly asset landed in FETCH-FAILED, and the job still exited 0."""
    override = os.environ.get(env)
    if override:
        return Path(override)
    try:
        if preferred.anchor and Path(preferred.anchor).exists():
            return preferred
    except OSError:
        pass
    return fallback


ECO_ROOT = _data_dir("ECO_ROOT", Path("D:/eco-harvest"), REPO_ROOT / "eco")
CACHE_ROOT = _data_dir("ECO_CACHE_DIR", Path("E:/eco-harvest/cache"),
                        ECO_ROOT / "cache")
TMP_ROOT = _data_dir("ECO_TMP_DIR", Path("E:/eco-harvest/tmp"),
                     ECO_ROOT / "tmp")
RESULTS_DIR = ECO_ROOT / "results"

COLS, ROWS = 100, 30
ENTER = "\r"
WAKE = "\x15"  # Ctrl-U — the journey harness's sacrificial kill-line

# Phase hard caps: the sum bounds one asset's ConPTY wall time.
STARTUP_TIMEOUT = 20.0
SOURCE_TIMEOUT = 12.0
ENTER_TIMEOUT = 6.0
GNU_SYNTAX_TIMEOUT = 15.0
GNU_RUNTIME_TIMEOUT = 10.0
FETCH_RETRIES = 6  # CDN throttle bursts outlast short ladders (wt97b)

STORM_LINES = 100       # >= this many error-pattern lines -> ERROR-STORM
STORM_BYTES = 512_000   # > this much stripped output -> ERROR-STORM
ERROR_PATTERNS = [
    "command not found", "syntax error", "no such file", "unbound variable",
    "not a valid identifier", "bad array subscript", "permission denied",
    "ambiguous redirect", "bad substitution", "event not found",
    "cannot execute", "is not a function", "invalid option",
]

ANSI_RX = re.compile(r"\x1b(?:\[[0-9;?]*[a-zA-Z]|\][^\x07\x1b]*(?:\x07|\x1b\\)|[@-Z\\-_])")


def strip_ansi(raw: str) -> str:
    return ANSI_RX.sub("", raw).replace("\r\n", "\n").replace("\r", "\n")


def lines_of(stripped: str):
    return [ln.strip() for ln in stripped.split("\n") if ln.strip()]


def norm_signature(s: str) -> str:
    """Normalize a diagnostic line into a groupable signature: paths and
    numbers collapse so the same construct from different files dedupes."""
    s = strip_ansi(s)
    s = re.sub(r"[A-Za-z]:[\\/][^\s:'\"]+", "<path>", s)
    s = re.sub(r"/mnt/[a-z]/\S+", "<path>", s)
    s = re.sub(r"(?<!\w)/[a-z]/\S+", "<path>", s)   # MSYS-style /e/...
    s = re.sub(r"[\x00-\x1f]", "", s)               # wake bytes etc.
    s = re.sub(r"\b\d+\b", "N", s)
    s = re.sub(r"\s+", " ", s).strip()
    return s[:200]


def error_line_count(stripped: str):
    """(count, head) over the error-pattern lines of a stripped stream."""
    count = 0
    head = []
    for ln in lines_of(stripped):
        low = ln.lower()
        if any(p in low for p in ERROR_PATTERNS):
            count += 1
            if len(head) < 10:
                head.append(ln[:160])
    return count, head


# ── Content fetch (on demand, gz-cached under --cache-dir) ───────────────────
_fetch_lock = threading.Lock()
_fetch_failures = 0


_BARE_PERCENT = re.compile(r"%(?![0-9A-Fa-f]{2})")


def percent_encode_raw_url(url: str) -> str:
    """#408: manifest rows carry f-string-interpolated raw URLs whose path
    segments hold literal spaces and '&' — raw.githubusercontent rejects
    both, which is 74 space-bearing + 4 ampersand-bearing of the 78
    FETCH-FAILED rows. Encode each path segment (keeps '/' separators);
    scheme://host and anything from '?' on (query) pass through untouched.
    safe='%' keeps already-encoded segments idempotent: rows written by the
    fixed eco-harvest raw_url() are never double-encoded. A bare '%' that is
    NOT a valid escape (the `files/%HOME%/.bashrc` class) still gets %25 —
    a literal percent sign on the wire is always a protocol error."""
    scheme, sep, rest = url.partition("://")
    if not sep:
        return url
    host, sep2, tail = rest.partition("/")
    if not sep2:
        return url
    path, _, query = tail.partition("?")
    encoded = "/".join(
        urllib.parse.quote(_BARE_PERCENT.sub("%25", seg), safe="%")
        for seg in path.split("/"))
    return f"{scheme}{sep}{host}/{encoded}" + (f"?{query}" if query else "")


class FetchGone(Exception):
    """The pinned raw URL is permanently gone (404/410/451).

    A raw URL is content-addressed at a fixed commit, so a 404 means the repo
    or commit is unreachable, not that we asked badly. Retry such rows forever
    and they burn the 6-rung ladder on every run AND inflate the fetch gate
    (#356) until the alarm means nothing — so they get a permanent badge."""


def fetch_asset(asset: dict, token: str | None, cache_root: Path,
                tmp_root: Path) -> Path | None:
    """Fetch raw bytes at the pinned commit into the gz cache; returns the
    extracted work-file path. T4 (no git blob sha) backfills a sha256.
    Raises FetchGone for a permanently dead URL."""
    h = asset["content_hash"]
    sub = cache_root / h[:2]
    gz = sub / f"{h}.gz"
    with _fetch_lock:
        cache_root.mkdir(parents=True, exist_ok=True)
    url = percent_encode_raw_url(asset["source_url"])
    req = urllib.request.Request(url, headers={"User-Agent":
                                               "niubash-eco-test/wt92"})
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    data = None
    if gz.exists():
        data = gzip.decompress(gz.read_bytes())
    else:
        for attempt in range(1, FETCH_RETRIES + 1):
            try:
                with urllib.request.urlopen(req, timeout=45) as resp:
                    data = resp.read()
                break
            except urllib.error.HTTPError as err:
                if err.code in (404, 410, 451):
                    raise FetchGone(
                        f"HTTP {err.code} — pinned raw URL gone") from None
                if err.code in (403, 429, 502, 503, 504) and \
                        attempt < FETCH_RETRIES:
                    retry_after = err.headers.get("Retry-After")
                    time.sleep(min(float(retry_after) if
                                   (retry_after or "").isdigit() else
                                   5 * attempt, 60))
                    continue
                if attempt == FETCH_RETRIES:
                    return None
                time.sleep(3 * attempt)
            except (urllib.error.URLError, TimeoutError, ConnectionError):
                if attempt == FETCH_RETRIES:
                    return None
                time.sleep(3 * attempt)
        if data is None:
            return None
        with _fetch_lock:
            sub.mkdir(parents=True, exist_ok=True)
            tmp = gz.with_suffix(".tmp")
            tmp.write_bytes(gzip.compress(data, 6))
            tmp.replace(gz)
    # T4 late dedup: real content hash now known.
    if asset.get("hash_kind") == "code-search-sha":
        import hashlib
        sha256 = hashlib.sha256(data).hexdigest()
        asset["sha256"] = sha256
    # Work file (kept through GNU parity, cleaned after the asset finishes).
    # --tmp-root governs it: a stray dev-box TMP_ROOT must not decide where
    # CI writes its scratch (#356 was exactly that class of leak).
    workdir = tmp_root / f"work-{h[:12]}"
    workdir.mkdir(parents=True, exist_ok=True)
    suffix = Path(asset["path"]).suffix or ".sh"
    work = workdir / f"asset{suffix}"
    work.write_bytes(data)
    return work


def wsl_path(p: Path) -> str:
    """E:/x/y -> /mnt/e/x/y (file-argument invocation only; we never build a
    `wsl bash -c` string, so the doubled-backslash corruption class in the
    repo rules cannot apply)."""
    drive = p.drive.rstrip(":").lower()
    return f"/mnt/{drive}" + str(p).replace("\\", "/")[2:]


_SELF_FORWARDER_MARKER = "winuxcmd"


def _msys_bash_candidates():
    """Git for Windows ships usr/bin/bash.exe (msys bash 5.2.x) inside every
    MinGit/GfW distribution — including the copy preinstalled on
    windows-latest runners — so the oracle needs no install step, just a
    locate (#409). ECO_GNU_BASH overrides; then the standard GfW homes;
    then wherever `where git` points; finally PATH's bash (never
    System32\\bash.exe — that one is the WSL launcher, not a bash).
    Candidates inside niubash's own winuxcmd shim layout are dropped
    unconditionally (#410 CR F1): on a dev box `where git` resolves to our
    own install, and grading the engine under test against a forwarder of
    itself is exactly the oracle we must not elect."""
    out = []
    override = os.environ.get("ECO_GNU_BASH", "").strip()
    if override:
        out.append(Path(override))
    roots = []
    for env_name in ("ProgramFiles", "ProgramFiles(x86)"):
        root = os.environ.get(env_name)
        if root:
            roots.append(Path(root))
    local = os.environ.get("LOCALAPPDATA")
    if local:
        roots.append(Path(local) / "Programs")
    for root in roots:
        out.append(root / "Git" / "usr" / "bin" / "bash.exe")
    try:
        where = subprocess.run(["where.exe", "git"], capture_output=True,
                               text=True, timeout=15)
        for git_exe in (where.stdout or "").splitlines():
            cmd_dir = Path(git_exe.strip()).parent
            out.append(cmd_dir.parent / "usr" / "bin" / "bash.exe")
    except (OSError, subprocess.TimeoutExpired):
        pass
    found = shutil.which("bash")
    if found and "system32" not in found.lower():
        out.append(Path(found))
    # The self-layout filter is the authoritative defense (CR #410 F1): a
    # FUNCTIONAL niu forwarder passes the script smoke too — the product is
    # deliberately bash-compatible — so no output check can separate them.
    # It applies to the ECO_GNU_BASH pin as well: an operator pinning into
    # our own shim layout is either a mistake or a self-test, and both must
    # be refused loudly instead of grading niu against niu.
    kept = [c for c in out
            if _SELF_FORWARDER_MARKER not in str(c).lower()]
    dropped = [c for c in out if c not in kept]
    for cand in dropped:
        print(f"WARNING: oracle candidate dropped — inside the niubash shim "
              f"layout (would grade the engine under test against itself): "
              f"{cand}")
    return kept


def _kill_tree(pid: int):
    """taskkill /T takes the whole child tree: a hung asset's grandchildren
    (sleep loops, subshells) must not outlive the oracle call on a shared
    runner."""
    try:
        subprocess.run(["taskkill", "/PID", str(pid), "/T", "/F"],
                       capture_output=True, timeout=15)
    except (OSError, subprocess.TimeoutExpired):
        pass


class GnuBash:
    """The parity oracle (#409): Git for Windows' usr/bin/bash.exe — msys
    bash 5.2.x, a native exe present on every windows-latest runner — with
    WSL GNU Bash 5.3 (the old dev-box oracle) as fallback. Both are invoked
    with a file argument only (never a `-c` string); msys bash is a native
    exe so a Windows path works directly. Kind and version travel with every
    verdict: 5.2 and 5.3 disagree on enough corners that an unversioned
    oracle result is ambiguous."""

    def __init__(self):
        self.bin = None
        self.kind = None      # "wsl" | "msys"
        self.version = None
        if os.name != "nt":
            return
        # Explicit ECO_GNU_BASH (the workflow probe's pick) wins; then the
        # GfW msys bash every windows runner already has; WSL — the old
        # dev-box oracle — is now the fallback, still preferred to nothing.
        self._probe_msys()
        if self.bin is None:
            self._probe_wsl()

    def _probe_wsl(self):
        for cand in ("/usr/local/bin/bash", "/usr/bin/bash", "bash"):
            try:
                r = subprocess.run(["wsl.exe", cand, "--version"],
                                   capture_output=True, text=True, timeout=60)
            except (OSError, subprocess.TimeoutExpired):
                continue  # no wsl.exe / no distro: degrade, never crash
            if r.returncode == 0 and "GNU bash" in r.stdout:
                self.bin = cand
                self.kind = "wsl"
                self.version = r.stdout.splitlines()[0].strip()
                break

    def _probe_msys(self):
        for cand in _msys_bash_candidates():
            try:
                if not cand.is_file():
                    continue
                r = subprocess.run([str(cand), "--version"],
                                   capture_output=True, text=True, timeout=30)
            except (OSError, subprocess.TimeoutExpired):
                continue
            first = (r.stdout or "").splitlines()
            if r.returncode != 0 or not first or "GNU bash" not in first[0]:
                continue
            if not self._script_smoke(str(cand)):
                continue
            self.bin = str(cand)
            self.kind = "msys"
            self.version = first[0].strip()
            return

    def _script_smoke(self, bash_exe: str) -> bool:
        """--version only proves the binary talks (#410 CR F1): niu's own
        winuxcmd forwarder passes it and then fails — or worse, *is* — the
        engine under test. A real oracle must actually execute a script:
        rc=0 AND the marker on stdout, or the candidate is not bash and the
        probe moves on (WSL stays next in line, exactly like the pre-#409
        lane). Note this smoke alone cannot catch a FUNCTIONAL forwarder —
        niu runs marker scripts too — that class is cut by the winuxcmd
        layout filter in _msys_bash_candidates; the smoke catches the
        broken-shim class (version banner talks, script exec fails). An
        explicit ECO_GNU_BASH pin OUTSIDE the self layout is honored as
        operator authority even if it turns out to be a shim — the layout
        filter and the smoke cannot second-guess a deliberate pin, and no
        output check can: the product is deliberately bash-compatible."""
        marker = "ECO-ORACLE-SMOKE-OK"
        try:
            with tempfile.TemporaryDirectory(
                    prefix="eco-oracle-smoke-") as smoke_dir:
                script = Path(smoke_dir) / "smoke.sh"
                script.write_text(f"printf '%s\\n' '{marker}'\n", newline="\n")
                r = subprocess.run(
                    [bash_exe, "--noprofile", "--norc", script.as_posix()],
                    capture_output=True, text=True, timeout=30,
                    stdin=subprocess.DEVNULL, cwd=smoke_dir)
        except (OSError, subprocess.TimeoutExpired):
            return False
        return r.returncode == 0 and marker in (r.stdout or "")

    def syntax(self, work: Path):
        """bash -n under the oracle; returns (ok, first_error_line)."""
        if self.kind == "msys":
            r = subprocess.run(
                [self.bin, "-n", work.as_posix()],
                capture_output=True, text=True, timeout=GNU_SYNTAX_TIMEOUT,
                stdin=subprocess.DEVNULL)
        else:
            r = subprocess.run(
                ["wsl.exe", self.bin, "-n", wsl_path(work)],
                capture_output=True, text=True, timeout=GNU_SYNTAX_TIMEOUT,
                stdin=subprocess.DEVNULL)
        err = strip_ansi(r.stderr or "").strip().splitlines()
        return (r.returncode == 0, err[0] if err else None)

    def runtime(self, work: Path):
        """Source the asset non-interactively under the oracle with a hard
        timeout. Returns (verdict_note, detail): 'ok' | 'hang' | 'error'.
        Any non-zero rc is a failure (#409): assets that end the shell with
        `exit N` must classify as oracle-failed even without error-pattern
        lines, or niu's EXITED verdicts can never reach parity."""
        if self.kind == "msys":
            try:
                proc = subprocess.Popen(
                    [self.bin, "--noprofile", "--norc", work.as_posix()],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    stdin=subprocess.DEVNULL, text=True, errors="replace",
                    cwd=str(work.parent))
            except OSError as err:
                return None, f"msys oracle spawn failed: {err}"
            try:
                out, errout = proc.communicate(timeout=GNU_RUNTIME_TIMEOUT)
            except subprocess.TimeoutExpired:
                _kill_tree(proc.pid)
                try:
                    proc.communicate(timeout=10)
                except subprocess.TimeoutExpired:
                    pass  # tree-kill already fired; the runner is ephemeral
                return ("hang",
                        f"msys oracle wall > {GNU_RUNTIME_TIMEOUT:.0f}s "
                        f"(tree-killed)")
            rc = proc.returncode
            n, _ = error_line_count(strip_ansi((errout or "") + (out or "")))
            if rc != 0 and n >= 1:
                return "error", f"msys oracle rc={rc} with {n} error lines"
            if rc != 0:
                return "error", f"msys oracle rc={rc}"
            return "ok", "msys oracle rc=0"
        try:
            r = subprocess.run(
                ["wsl.exe", "timeout", str(int(GNU_RUNTIME_TIMEOUT)),
                 self.bin, "--noprofile", "--norc", wsl_path(work)],
                capture_output=True, text=True,
                timeout=GNU_RUNTIME_TIMEOUT + 15, stdin=subprocess.DEVNULL)
        except subprocess.TimeoutExpired:
            return "hang", f"GNU wall > {GNU_RUNTIME_TIMEOUT + 10}s"
        if r.returncode == 124:
            return "hang", "GNU timeout(1) killed it (rc=124)"
        n, _ = error_line_count(strip_ansi((r.stderr or "") +
                                           (r.stdout or "")))
        # wt97b triage fix: ANY error-pattern line with rc != 0 is a GNU
        # failure. The old gate (>= STORM_LINES) stamped `ok` for rc=2 with
        # 1-2 syntax-error lines, which mis-classified 5 assets
        # (bash-completion 7z/cvs/ps, ohmyzsh changelog/check_for_upgrade)
        # as SYNTAX-REJECT-RUBASH-ONLY instead of GNU-ALSO-FAILS.
        # #409: so is a bare non-zero rc (the `exit N`-in-sourced-script
        # class behind most EXITED verdicts) — with 5.2 and 5.3 now both in
        # play, the two runtimes must share one classification.
        if r.returncode not in (0,) and n >= 1:
            return "error", f"GNU rc={r.returncode} with {n} error lines"
        if r.returncode not in (0,):
            return "error", f"GNU rc={r.returncode}"
        return "ok", f"GNU rc={r.returncode}"


# ── The ConPTY session (journey-harness shape, marker-verified) ─────────────

class NiuSession:
    """One `niu -i` under ConPTY, with a pyte-parsed screen (the journey
    harness's pattern). Marker detection runs on the RENDERED screen rows —
    niu's prompt repaints with ESC7/ESC8 save/restore pairs, and stripping
    escapes from the raw stream can shred or join text so a typed echo can
    masquerade as an output line; the rendered viewport can never lie about
    what a user would see. The raw stream is kept for diagnostics."""

    def __init__(self, niu: Path, home: Path):
        import pyte
        from winpty import PtyProcess as _PtyProcess
        env = dict(os.environ)
        env.update({
            "HOME": str(home), "USERPROFILE": str(home),
            "TEMP": str(home / "tmp"), "TMP": str(home / "tmp"),
            "TERM": "xterm", "NIU_LANG": "en", "NIU_NO_UPDATE_CHECK": "1",
        })
        # Ambient overrides must not leak into the sandbox (journey rule).
        for key in ("NIU_PLUGIN_SOURCES_ROOT", "NIU_PLUGIN_SPEC",
                    "NIU_MIRRORS", "NIU_PLUGIN_BOOTSTRAP",
                    "NIU_REPL_STARTUP", "BASH_ENV",
                    "NIU_THEME", "OSH_THEME", "BASH_IT_THEME"):
            env.pop(key, None)
        (home / "tmp").mkdir(parents=True, exist_ok=True)
        (home / ".niubashrc").write_text("PS1='ECO> '\n",
                                         encoding="utf-8", newline="\n")
        self.proc = _PtyProcess.spawn([str(niu), "-i"], cwd=str(home),
                                      env=env, dimensions=(ROWS, COLS))
        self.dead = False
        self.chunks: list[str] = []
        self.lock = threading.Lock()
        try:
            self.screen = pyte.HistoryScreen(COLS, ROWS, history=200)
        except TypeError:  # older pyte: top/bottom deques
            self.screen = pyte.HistoryScreen(COLS, ROWS, top=200)
        self.stream = pyte.Stream(self.screen)
        self._reader = threading.Thread(target=self._pump, daemon=True)
        self._reader.start()

    def _pump(self):
        while self.proc.isalive():
            try:
                data = self.proc.read()
            except Exception:
                break
            if data:
                with self.lock:
                    self.chunks.append(data)
                    self.stream.feed(data)
        self.dead = True

    def write(self, data: str) -> bool:
        if self.dead:
            return False
        try:
            self.proc.write(data)
            return True
        except (EOFError, OSError):
            self.dead = True
            return False

    def rendered_lines(self) -> list[str]:
        """Scrollback + viewport as the terminal shows it. History rows are
        char-maps (`Char.data` is the glyph) — same render as the journey
        gate's render_history_line."""
        with self.lock:
            rows = []
            for line in self.screen.history.top:
                if isinstance(line, str):
                    rows.append(line)
                else:
                    rows.append("".join(
                        (getattr(line.get(col), "data", None) or " ")
                        for col in range(COLS)))
            rows.extend(self.screen.display)
        return [ln.rstrip() for ln in rows]

    def has_marker(self, marker: str) -> bool:
        """Output-anchored: the marker must be a WHOLE rendered row. The
        typed echo line carries the `ECO> ` prompt prefix on the same row,
        so it can never equal the marker alone."""
        return any(ln.strip() == marker for ln in self.rendered_lines())

    def tail(self, n=12):
        return [ln.strip() for ln in self.rendered_lines() if ln.strip()][-n:]

    def send_probe(self, marker: str):
        self.write(WAKE + f"command echo {marker}" + ENTER)

    def settle(self, quiet: float = 0.25, budget: float = 2.0):
        """Journey settle-before-send: the shell drops input queued while it
        redraws, so wait for a quiet screen (no new bytes, no viewport
        change) before writing. Bounded."""
        deadline = time.time() + budget
        last = (len(self.chunks), tuple(self.rendered_lines()[-1:]))
        while time.time() < deadline:
            time.sleep(quiet)
            cur = (len(self.chunks), tuple(self.rendered_lines()[-1:]))
            if cur == last:
                return
            last = cur

    def wait_marker(self, marker: str, timeout: float) -> bool:
        """Wait for the output marker; wake+retry at 50%/80% of the budget
        (input delivery is verified, never assumed — journey rule).
        Returns True when seen; False on timeout/death."""
        deadline = time.time() + timeout
        retries = [0.5, 0.8]
        done: list[float] = []
        while time.time() < deadline:
            if self.dead:
                return False
            if self.has_marker(marker):
                return True
            elapsed = timeout - (deadline - time.time())
            while retries and elapsed >= timeout * retries[0]:
                done.append(retries.pop(0))
                self.send_probe(marker)
            time.sleep(0.08)
        return self.has_marker(marker)

    def terminate(self):
        try:
            self.proc.terminate(force=True)
        except Exception:
            pass
        time.sleep(0.2)
        try:
            if self.proc.isalive():
                subprocess.run(["taskkill", "/PID", str(self.proc.pid),
                                "/T", "/F"], capture_output=True, timeout=15)
        except Exception:
            pass


def test_asset_conpty(asset: dict, work: Path, niu: Path, tmp_root: Path,
                      slow_threshold: float) -> dict:
    """The wt91-shape interactive test. Returns verdict fields."""
    home = tmp_root / f"sand-{asset['content_hash'][:12]}"
    if home.exists():
        shutil.rmtree(home, ignore_errors=True)
    home.mkdir(parents=True)
    sess = NiuSession(niu, home)
    phases_ms = {}
    out = {"phases_ms": phases_ms, "phase": None, "tail": [],
           "storm_head": [], "storm_lines": 0}
    try:
        # Startup: settle past the banner, then a marker-verified liveness
        # probe (input delivery verified, never assumed — journey rule).
        time.sleep(1.0)
        sess.send_probe("ECOSTART")
        t0 = time.time()
        alive = sess.wait_marker("ECOSTART", STARTUP_TIMEOUT)
        phases_ms["startup"] = int((time.time() - t0) * 1000)
        if not alive:
            if sess.dead:
                out["verdict"], out["phase"] = "EXITED", "startup"
            else:
                out["verdict"], out["phase"] = "HANG", "startup"
            out["tail"] = sess.tail()
            return out

        # Source wall: the probe goes AFTER the source line — the marker can
        # only appear once sourcing returned to the prompt.
        sess.settle()
        t0 = time.time()
        sess.write(f"source '{work}'" + ENTER)
        sess.send_probe("ECOSRC")
        ok = sess.wait_marker("ECOSRC", SOURCE_TIMEOUT)
        src_ms = int((time.time() - t0) * 1000)
        phases_ms["source_wall"] = src_ms
        if not ok:
            if sess.dead:
                out["verdict"], out["phase"] = "EXITED", "source"
            else:
                out["verdict"], out["phase"] = "HANG", "source"
            out["tail"] = sess.tail()
            return out
        if sess.dead:
            out["verdict"], out["phase"] = "EXITED", "after-source"
            out["tail"] = sess.tail()
            return out

        for i, name in ((1, "enter1"), (2, "enter2")):
            sess.settle()
            t0 = time.time()
            sess.write(ENTER)
            sess.send_probe(f"ECOE{i}")
            ok = sess.wait_marker(f"ECOE{i}", ENTER_TIMEOUT)
            phases_ms[name] = int((time.time() - t0) * 1000)
            if not ok:
                if sess.dead:
                    out["verdict"], out["phase"] = "EXITED", name
                else:
                    out["verdict"], out["phase"] = "HANG", name
                out["tail"] = sess.tail()
                return out
            if sess.dead:
                out["verdict"], out["phase"] = "EXITED", name
                out["tail"] = sess.tail()
                return out

        transcript = "\n".join(sess.rendered_lines())
        n, head = error_line_count(transcript)
        out["storm_lines"] = n
        out["storm_head"] = head
        if n >= STORM_LINES or len(transcript) > STORM_BYTES:
            out["verdict"] = "ERROR-STORM"
            return out
        # SLOW judges the ASSET's cost only: startup (~2.3s of banner+init)
        # is identical environment overhead for every asset, not a property
        # of the file under test.
        worst = max(phases_ms.get(k, 0)
                    for k in ("source_wall", "enter1", "enter2"))
        out["verdict"] = "SLOW" if worst > slow_threshold * 1000 else "OK"
        return out
    finally:
        sess.terminate()
        shutil.rmtree(home, ignore_errors=True)


def syntax_signature(rubash_err: str, work: Path) -> str:
    """Minimal failing construct: normalized rubash error + the offending
    source line (parsed from `line N`), whitespace-collapsed."""
    m = re.search(r"line (\d+)", rubash_err or "")
    line_txt = ""
    if m and work.exists():
        try:
            lines = work.read_text(encoding="utf-8", errors="replace")\
                .splitlines()
            idx = int(m.group(1)) - 1
            if 0 <= idx < len(lines):
                line_txt = re.sub(r"\s+", " ",
                                  lines[idx].strip())[:120]
        except OSError:
            pass
    core = re.sub(r"[A-Za-z]:[\\/][^\s:]+", "<path>", rubash_err or "unknown")
    core = re.sub(r"(?<!\w)/[a-z]/\S+", "<path>", core)
    core = re.sub(r"[\x00-\x1f]", "", core)
    core = re.sub(r"\b\d+\b", "N", core)
    core = re.sub(r"\s+", " ", core).strip()
    return f"{core} @ `{line_txt}`" if line_txt else core


def hang_signature(phase: str, tail: list[str]) -> str:
    # Probe echoes (`command echo ECO...`) are harness input, not output —
    # never a hang signature.
    sig_tail = [ln for ln in tail if "command echo ECO" not in ln]
    last = sig_tail[-1] if sig_tail else (tail[-1] if tail else "")
    err = next((ln for ln in sig_tail
                if any(p in ln.lower() for p in ERROR_PATTERNS)), "")
    return f"hang:{phase}:" + norm_signature(err or last)[:140]


def refine_class(content_first_line: str, path: str, harvest_cat: str) -> str:
    first = (content_first_line or "").strip()
    if first.startswith("#!"):
        harvest_cat = harvest_cat or "shebang"
    low = path.lower()
    if harvest_cat in (None, "unknown", ""):
        if "complete" in low:
            return "completion"
        if "theme" in low:
            return "theme"
    return harvest_cat or "unknown"


# ── One asset, end to end ───────────────────────────────────────────────────

def test_one(asset: dict, cfg) -> dict:
    h = asset["content_hash"]
    t0 = time.time()
    rec = {
        "ts": NOW(), "content_hash": h, "repo": asset["repo"],
        "path": asset["path"], "tier": asset["tier"],
        "category": asset.get("category"), "bytes": asset.get("bytes"),
        "source_url": asset["source_url"],
        "niu": str(cfg.niu), "rubash": str(cfg.rubash),
    }
    work = None
    # Permanent pre-fetch guard (wt97b): a raw URL carrying control
    # characters can NEVER be fetched ("URL can't contain control
    # characters" — 18 such rows burned retries every resume). Badge it
    # once, permanently; UNFETCHABLE-URL is not in the transient set, so
    # resume skips it. (eco-harvest now filters these at t4 harvest time.)
    if not all(ch.isprintable() for ch in (asset.get("source_url") or "")):
        rec.update(verdict="UNFETCHABLE-URL",
                   detail="control characters in raw URL (unfetchable)")
        return rec
    try:
        work = fetch_asset(asset, cfg.token, cfg.cache_root, cfg.tmp_root)
    except FetchGone as err:
        # Permanent (not in `transient`): resume skips it, the fetch gate
        # ignores it — a dead upstream URL is not our fetch tier breaking.
        rec.update(verdict="FETCH-GONE", detail=str(err)[:160])
        return rec
    except Exception as err:
        rec.update(verdict="FETCH-FAILED", detail=str(err)[:160])
        return rec
    if work is None:
        rec.update(verdict="FETCH-FAILED", detail="fetch failed after retries")
        return rec
    rec["local_work"] = str(work)

    # T4 late dedup on real content. The result line keeps THIS row's hash
    # as its key (so resume skips it) and only points at the duplicate.
    if asset.get("hash_kind") == "code-search-sha" and asset.get("sha256"):
        with cfg.man.lock:
            row = cfg.man.db.execute(
                "SELECT 1 FROM assets WHERE content_hash=? AND content_hash!=?",
                (asset["sha256"], h)).fetchone()
        if row:
            rec.update(verdict="DUPLICATE", dup_of=asset["sha256"],
                       detail=f"same bytes as {asset['sha256']}",
                       ms=int((time.time() - t0) * 1000))
            cleanup_work(work)
            return rec

    # ── rubash -n pre-filter ──
    try:
        r = subprocess.run([str(cfg.rubash), "-n", str(work)],
                           capture_output=True, text=True, timeout=20,
                           stdin=subprocess.DEVNULL)
        rub_ok = r.returncode == 0
        rub_err = (r.stderr or r.stdout or "").strip().splitlines()
        rub_err = rub_err[0] if rub_err else ""
    except subprocess.TimeoutExpired:
        rub_ok, rub_err = False, "rubash -n timed out (20s)"
    rec["rubash_ok"] = rub_ok
    if not rub_ok:
        # GNU parity classification (file argument, stdin pinned). GNU -n
        # alone is NOT a sufficient oracle: bash parses incrementally, so a
        # `shopt -s extglob` makes `case x in @(a|b))` legal at runtime while
        # `bash -n` (which never executes the shopt) rejects it — observed on
        # oh-my-bash lib/cli.bash (sources clean under GNU, rc=0). When -n
        # rejects, fall back to an oracle RUNTIME source before calling the
        # file upstream-broken.
        oracle = cfg.gnu
        gnu_ok, gnu_err = \
            (False, f"{getattr(oracle, 'kind', None) or 'bash'} oracle unavailable") \
            if oracle.bin is None else oracle.syntax(work)
        rec["gnu_syntax_ok"] = gnu_ok
        rec["gnu_err"] = gnu_err
        gnu_runtime_note = None
        if not gnu_ok and oracle.bin is not None:
            gnu_runtime_note, gnu_runtime_detail = oracle.runtime(work)
            rec["gnu_parity"] = gnu_runtime_note
            rec["gnu_runtime_detail"] = gnu_runtime_detail
        if oracle.bin is None:
            # No oracle: an unconfirmed reject must not masquerade as gold.
            # Keep the historical verdict but say so in the record — the
            # run-level alarm is the unexplained-failure gate's job (#409).
            try:
                out = test_asset_conpty(asset, work, cfg.niu, cfg.tmp_root,
                                        cfg.slow_threshold)
                rec["niu_sources"] = out["verdict"]
            except Exception as err:
                rec["niu_sources"] = f"HARNESS-ERROR: {err!r}"[:120]
            rec.update(verdict="SYNTAX-REJECT-RUBASH-ONLY",
                       detail="UNCONFIRMED: no oracle available",
                       signature=syntax_signature(rub_err, work),
                       rubash_err=rub_err, ms=int((time.time() - t0) * 1000))
            cleanup_work(work)
            return rec
        if gnu_ok or gnu_runtime_note == "ok":
            # Engine divergence candidate — the oracle parses (or at least
            # executes) what rubash rejects. Confirm what niu's EXECUTOR
            # does with the same file (rubash -n may be stricter than
            # rubash's own runtime — still a parser divergence worth its
            # own group).
            try:
                out = test_asset_conpty(asset, work, cfg.niu, cfg.tmp_root,
                                        cfg.slow_threshold)
                rec["niu_sources"] = out["verdict"]
            except Exception as err:
                rec["niu_sources"] = f"HARNESS-ERROR: {err!r}"[:120]
            # wt97b triage fix, verdict renamed by #409: when the oracle's
            # -n ALSO rejects the file but its runtime sources it clean,
            # both engines' -n share the same strictness while both
            # runtimes accept it (the extglob-after-shopt class:
            # `shopt -s extglob` makes a later `case x in @(a|b))` legal at
            # runtime under bash, which `bash -n` never executes —
            # oh-my-bash lib/cli.bash). That is NOT a divergence: PARITY.
            # But if niu's runtime chokes where the oracle's is clean, the
            # SYNTAX-REJECT gold stands.
            if not gnu_ok and rec.get("niu_sources") in ("OK", "SLOW"):
                rec.update(verdict="PARITY",
                           detail="bash -n rejects under BOTH engines "
                                  "(pre-execution strictness, e.g. "
                                  "extglob-before-shopt); runtimes OK",
                           signature="parity:nn-reject-both-runtimes-clean",
                           ms=int((time.time() - t0) * 1000))
                cleanup_work(work)
                return rec
            rec.update(verdict="SYNTAX-REJECT-RUBASH-ONLY",
                       signature=syntax_signature(rub_err, work),
                       rubash_err=rub_err, ms=int((time.time() - t0) * 1000))
            cleanup_work(work)
            return rec
        # The oracle rejects the syntax AND its runtime fails or hangs: the
        # asset is broken upstream — ASSET-NATURE, not an engine bug (#409;
        # this was the old GNU-ALSO-FAILS).
        rec.update(verdict="ASSET-NATURE",
                   detail=f"oracle {getattr(oracle, 'kind', 'bash')} also "
                          f"rejects the syntax and its runtime fails "
                          f"({gnu_runtime_note})",
                   signature="asset-nature:reject:" + norm_signature(
                       gnu_runtime_detail or gnu_err or "?"),
                   rubash_err=rub_err, ms=int((time.time() - t0) * 1000))
        cleanup_work(work)
        return rec

    # ── ConPTY interactive test ──
    try:
        out = test_asset_conpty(asset, work, cfg.niu, cfg.tmp_root,
                                cfg.slow_threshold)
    except Exception as err:
        out = {"verdict": "HARNESS-ERROR", "phase": None, "tail": [],
               "detail": repr(err)[:200], "phases_ms": {}}
    verdict = out["verdict"]
    rec["phases_ms"] = out.get("phases_ms", {})
    rec["phase"] = out.get("phase")
    rec["tail"] = out.get("tail", [])[:8]
    rec["storm_lines"] = out.get("storm_lines", 0)
    rec["storm_head"] = out.get("storm_head", [])

    # Oracle runtime parity for interactive failures (upstream vs engine,
    # #409): when the oracle reproduces the failure, the verdict is
    # ASSET-NATURE — upstream content misbehaving everywhere — regardless of
    # the niu flavor (a HANG whose oracle run errors is still the asset's
    # nature; `same form` is recorded for triage). Only an oracle-healthy
    # run leaves the niu verdict standing as an engine suspect.
    if verdict in ("HANG", "ERROR-STORM", "EXITED") and cfg.gnu.bin:
        note, detail = cfg.gnu.runtime(work)
        rec["gnu_parity"] = note
        rec["gnu_detail"] = detail
        if note in ("hang", "error"):
            same_form = (verdict == "HANG") == (note == "hang")
            rec.update(verdict="ASSET-NATURE",
                       detail=f"oracle {getattr(cfg.gnu, 'kind', 'bash')} "
                              f"reproduces the failure (same form: "
                              f"{same_form}): {detail}",
                       signature=f"asset-nature:{note}:" +
                                 norm_signature(detail),
                       ms=int((time.time() - t0) * 1000))
            cleanup_work(work)
            return rec
    if "signature" not in rec:
        if verdict == "HANG":
            rec["signature"] = hang_signature(out.get("phase") or "?",
                                              out.get("tail", []))
        elif verdict == "ERROR-STORM":
            rec["signature"] = "storm:" + norm_signature(
                (out.get("storm_head") or ["?"])[0])
        elif verdict == "EXITED":
            rec["signature"] = "exit:" + norm_signature(
                (out.get("tail") or ["?"])[-1])
        else:
            rec["signature"] = verdict.lower()
    rec["verdict"] = verdict
    rec["ms"] = int((time.time() - t0) * 1000)
    cleanup_work(work)
    return rec


def cleanup_work(work: Path):
    try:
        shutil.rmtree(work.parent, ignore_errors=True)
    except Exception:
        pass


# ── Report: group the gold verdicts by minimal failing construct ────────────

GOLD = ("SYNTAX-REJECT-RUBASH-ONLY", "HANG")


def report(results_path: Path, top: int, include_storms: bool,
           include_gnu: bool, build_id: str | None = None):
    groups: dict[str, list[dict]] = collections.defaultdict(list)
    verdicts = collections.Counter()
    latest: dict[str, dict] = {}
    builds: collections.Counter = collections.Counter()
    build_oracle: dict[str, str] = {}
    with open(results_path, encoding="utf-8") as fh:
        for ln in fh:
            ln = ln.strip()
            if not ln:
                continue
            try:
                r = json.loads(ln)
            except json.JSONDecodeError:
                continue
            # One asset, one verdict: the LAST result line wins (re-tests
            # and resume retries override their stale predecessors).
            latest[r.get("content_hash", "?")] = r
    rows = list(latest.values())
    for r in rows:
        bid = r.get("build_id", "legacy-unknown") or "legacy-unknown"
        builds[bid] += 1
        ident = r.get("build_identity") or {}
        oracle = ident.get("gnu_oracle") or {}
        if bid not in build_oracle:
            build_oracle[bid] = (
                f"{oracle.get('kind', '?')} :: "
                f"{oracle.get('version', '?')}")
    if build_id is not None:
        rows = [r for r in rows
                if (r.get("build_id") or "legacy-unknown") == build_id]
    for r in rows:
        verdicts[r.get("verdict", "?")] += 1
    for r in rows:
        v = r.get("verdict")
        gold = v in GOLD or (include_storms and v == "ERROR-STORM") or \
            (include_gnu and v in ("GNU-ALSO-FAILS", "ASSET-NATURE"))
        if gold:
            groups[r.get("signature") or v].append(r)
    tested = len(rows)
    if build_id is None and len(builds) > 1:
        print(f"== eco-test report: {tested} assets total across "
              f"{len(builds)} builds ({results_path}) ==")
        print("per-build asset counts (issue titles MUST use ONE build):")
        for bid, n in builds.most_common():
            print(f"  {n:5d}  build {bid[:12]}  oracle {build_oracle[bid]}")
        print("gold groups below are CROSS-BUILD cumulative watchlists — "
              "do NOT copy their totals into issue titles; re-run with "
              "--build-id <id> for the current-build counts.")
    else:
        label = (f"build {build_id[:12]}" if build_id
                 else f"({results_path})")
        print(f"== eco-test report: {tested} assets tested {label} ==")
        if rows:
            ident0 = rows[0].get("build_identity") or {}
            oracle0 = ident0.get("gnu_oracle") or {}
            print(f"oracle: {oracle0.get('kind', '?')} :: "
                  f"{oracle0.get('version', '?')}  "
                  f"(titles must stamp kind+version)")
    print("verdict distribution:")
    for v, n in verdicts.most_common():
        print(f"  {n:5d}  {v}")
    if tested:
        ms = [r["ms"] for r in rows if isinstance(r.get("ms"), int)]
        workers_env = os.environ.get("ECO_WORKERS")
        if ms and workers_env:
            w = int(workers_env)
            mean_serial = sum(ms) / len(ms) / 1000
            per_hour = 3600 / mean_serial * w if mean_serial else 0
            print(f"throughput: mean {mean_serial:.1f} s/asset latency "
                  f"~ {per_hour:.0f} assets/hour at {w} workers "
                  f"(observed wall rate for this run: see test-mode summary)")
    print(f"\n== gold groups ({len(groups)} signatures; "
          f"{sum(len(v) for v in groups.values())} assets) ==")
    ranked = sorted(groups.items(), key=lambda kv: -len(kv[1]))
    report_json = []
    for sig, rs in ranked[:top]:
        ex = [{"repo": r["repo"], "path": r["path"], "verdict": r["verdict"],
               "url": r["source_url"]} for r in rs[:5]]
        print(f"\n[{len(rs)} assets] {sig}")
        for e, r in zip(ex, rs[:5]):
            bid = (r.get("build_id") or "legacy-unknown")[:12]
            print(f"    {e['repo']} :: {e['path']} ({e['verdict']}, "
                  f"build {bid})")
        report_json.append({"signature": sig, "count": len(rs),
                            "examples": ex,
                            "assets": [{"repo": r["repo"], "path": r["path"],
                                        "hash": r["content_hash"]}
                                       for r in rs]})
    out = results_path.parent / "divergence-groups.json"
    out.write_text(json.dumps(report_json, indent=2), encoding="utf-8")
    print(f"\ngroup json: {out}")
    return 0


TIERS = ("t1", "t2", "t3", "t4", "t5")
TRANSIENT = {"FETCH-FAILED", "HARNESS-ERROR", "EXITED"}


def build_identity(niu, rubash, niu_revision, rubash_revision, slow_threshold, gnu):
    def digest(path):
        with open(path, "rb") as source:
            return hashlib.file_digest(source, "sha256").hexdigest()

    identity = {
        "niu_sha256": digest(niu), "rubash_sha256": digest(rubash),
        "niu_revision": niu_revision, "rubash_revision": rubash_revision,
        "harness_sha256": digest(Path(__file__)),
        "slow_threshold": slow_threshold,
        "gnu_oracle": {
            "available": gnu.bin is not None,
            "bin": gnu.bin,
            # kind: "wsl" (GNU 5.3, dev box) vs "msys" (5.2.x, GfW on the
            # runner) — an unversioned oracle result is ambiguous (#409).
            "kind": getattr(gnu, "kind", None),
            "version": gnu.version if gnu.bin is not None else None,
        },
    }
    build_id = hashlib.sha256(json.dumps(
        identity, sort_keys=True).encode()).hexdigest()
    return build_id, identity


def migrate_verdicts(db, results_path):
    db.executescript("""
        CREATE TABLE IF NOT EXISTS eco_test_builds (
            build_id TEXT PRIMARY KEY, identity_json TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS eco_test_verdicts (
            content_hash TEXT NOT NULL, build_id TEXT NOT NULL,
            verdict TEXT NOT NULL, tested_at TEXT NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 1,
            PRIMARY KEY (content_hash, build_id)
        );
        CREATE TABLE IF NOT EXISTS eco_test_meta (
            key TEXT PRIMARY KEY, value TEXT NOT NULL
        );
    """)
    if db.execute("SELECT 1 FROM eco_test_meta WHERE key='legacy_imported'").fetchone():
        return
    latest = {}
    if results_path.exists():
        with results_path.open(encoding="utf-8") as source:
            for line in source:
                try:
                    record = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if isinstance(record, dict) and record.get("content_hash") and record.get("verdict"):
                    latest[record["content_hash"]] = record
    with db:
        db.execute("INSERT OR IGNORE INTO eco_test_builds VALUES (?, ?)",
                   ("legacy", json.dumps({"version": "unknown"})))
        for content_hash, verdict in db.execute(
                "SELECT content_hash, verdict FROM assets WHERE verdict IS NOT NULL"):
            latest.setdefault(content_hash, {"verdict": verdict})
        db.executemany(
            "INSERT OR IGNORE INTO eco_test_verdicts "
            "(content_hash, build_id, verdict, tested_at) VALUES (?, 'legacy', ?, ?)",
            [(content_hash, record["verdict"], record.get("ts") or "")
             for content_hash, record in latest.items()])
        db.execute("INSERT INTO eco_test_meta VALUES ('legacy_imported', '1')")


def select_assets(db, tiers, build_id, limit, stale_limit, redo=False):
    qmarks = ",".join("?" for tier in tiers)
    rows = db.execute(
        "SELECT a.content_hash, a.hash_kind, a.source_url, a.repo, a.path, "
        "a.commit_sha, a.bytes, a.category, a.tier, "
        "v.verdict, h.last_tested, h.last_verdict "
        "FROM assets a LEFT JOIN eco_test_verdicts v "
        "ON a.content_hash=v.content_hash AND v.build_id=? "
        "LEFT JOIN (SELECT content_hash, MAX(tested_at) AS last_tested, "
        "verdict AS last_verdict FROM eco_test_verdicts GROUP BY content_hash) h "
        "ON a.content_hash=h.content_hash "
        f"WHERE a.tier IN ({qmarks}) ORDER BY a.path, a.content_hash",
        [build_id, *tiers]).fetchall()
    columns = ("content_hash", "hash_kind", "source_url", "repo", "path",
               "commit", "bytes", "category", "tier")
    stats = collections.Counter(unseen=0, transient=0, stale=0, current=0)
    queues = {kind: collections.defaultdict(list) for kind in ("priority", "stale", "redo")}
    tier_last = collections.defaultdict(str)
    for row in rows:
        asset = dict(zip(columns, row[:9]))
        current, last_tested, last_verdict = row[9:]
        if last_tested is None:
            kind = "unseen"
        elif (current or last_verdict) in TRANSIENT:
            kind = "transient"
        elif current is not None:
            kind = "current"
        else:
            kind = "stale"
        stats[kind] += 1
        tier = asset["tier"]
        tier_last[tier] = max(tier_last[tier], last_tested or "")
        queue = "redo" if redo else "priority" if kind in ("unseen", "transient") else kind
        if queue in queues:
            queues[queue][tier].append((last_tested or "", asset))

    def fair_order(groups):
        active = sorted(groups, key=lambda tier: (tier_last[tier], tier))
        ordered = {tier: collections.deque(asset for stamp, asset in sorted(
            groups[tier], key=lambda item: (item[0], item[1]["path"], item[1]["content_hash"])))
            for tier in active}
        while active:
            for tier in active[:]:
                yield ordered[tier].popleft()
                if not ordered[tier]:
                    active.remove(tier)

    capacity = limit or len(rows)
    selected = []
    for asset in fair_order(queues["redo" if redo else "priority"]):
        if len(selected) >= capacity:
            break
        selected.append(asset)
    if not redo:
        for index, asset in enumerate(fair_order(queues["stale"])):
            if index >= stale_limit or len(selected) >= capacity:
                break
            selected.append(asset)
    stats["selected"] = len(selected)
    stats["total"] = len(rows)
    return selected, dict(stats)


def save_verdict(db, record):
    with db:
        db.execute(
            "INSERT INTO eco_test_verdicts "
            "(content_hash, build_id, verdict, tested_at) VALUES (?, ?, ?, ?) "
            "ON CONFLICT(content_hash, build_id) DO UPDATE SET "
            "verdict=excluded.verdict, tested_at=excluded.tested_at, "
            "attempts=eco_test_verdicts.attempts+1",
            (record["content_hash"], record["build_id"], record["verdict"], record["ts"]))
        db.execute("UPDATE assets SET fetched=1, verdict=? WHERE content_hash=?",
                   (record["verdict"], record["content_hash"]))


def run_bounded(assets, workers, max_seconds, run_one):
    deadline = time.monotonic() + max_seconds if max_seconds else float("inf")
    remaining = iter(assets)
    submitted = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        active = set()

        def submit_one():
            nonlocal submitted
            if time.monotonic() >= deadline:
                return False
            asset = next(remaining, None)
            if asset is None:
                return False
            active.add(pool.submit(run_one, asset))
            submitted += 1
            return True

        for worker in range(workers):
            if not submit_one():
                break
        while active:
            finished, active = concurrent.futures.wait(
                active, return_when=concurrent.futures.FIRST_COMPLETED)
            for future in finished:
                future.result()
                submit_one()
    return submitted


# ── Step summary + fetch gate (the #356 "green job, dead tier" class) ───────

def write_step_summary(counts, tested, results_path, gate_msgs=None,
                       idle_note=None, selection=None, build_id=None):
    """Append the verdict distribution to $GITHUB_STEP_SUMMARY (no-op when
    unset, so local runs stay quiet). Six nightly runs reported success with
    547/547 FETCH-FAILED: the distribution has to be visible, not buried in
    the log. gate_msgs is the list of tripped gates (#356/#408 fetch gate,
    #409 unexplained-failure gate)."""
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if not path:
        return
    lines = ["## eco-test verdicts", ""]
    if build_id:
        lines += [f"Build identity: `{build_id}`", ""]
    if selection is not None:
        lines += ["| queue (before run) | count |", "| --- | ---: |"]
        for key in ("total", "unseen", "transient", "stale", "current", "selected"):
            lines.append(f"| {key} | {selection[key]} |")
        lines += [f"| completed | {tested} |",
                  f"| deferred by time budget | {selection['selected'] - tested} |",
                  f"| idle | {'yes' if tested == 0 else 'no'} |", ""]
    for gate_msg in gate_msgs or []:
        lines += [f"> 🚨 {gate_msg}", ""]
    lines += ["| verdict | count |", "| --- | ---: |"]
    for v, n in collections.Counter(counts).most_common():
        lines.append(f"| {v} | {n} |")
    lines += ["", f"{tested} assets tested this run · `{results_path}`", ""]
    if idle_note:
        # An idle lane must not read as a broken one (#356 was the opposite
        # failure: a broken lane reading as healthy). Say which it is.
        lines += [idle_note, ""]
    try:
        with open(path, "a", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")
    except OSError:
        pass


# ── Main ────────────────────────────────────────────────────────────────────

def main(argv=None):
    ap = argparse.ArgumentParser(
        description="ConPTY-test harvested ecosystem assets (wt92 harvest "
                    "lane); stamp verdict badges into the manifest")
    ap.add_argument("--tiers", default="all",
                    help="all (default) or comma list of t1,t2,t3,t4,t5")
    ap.add_argument("--workers", type=int, default=8)
    ap.add_argument("--limit", type=int, default=400,
                    help="cap assets tested this run (0 = all pending)")
    ap.add_argument("--stale-limit", type=int, default=80,
                    help="old-build retest cap after unseen/transient work")
    ap.add_argument("--max-seconds", type=float, default=2400,
                    help="stop dispatch after this budget; drain in-flight work (0 = unlimited)")
    ap.add_argument("--slow-threshold", type=float, default=2.0)
    ap.add_argument("--niu", default=str(DEFAULT_NIU))
    ap.add_argument("--rubash", default=str(DEFAULT_RUBASH))
    ap.add_argument("--niu-revision", default="unknown",
                    help="build checkout SHA; binary SHA256 is always included")
    ap.add_argument("--rubash-revision", default="unknown",
                    help="actual sibling checkout SHA; not the patched Cargo.lock revision")
    ap.add_argument("--manifest-dir", default=str(ECO_ROOT / "manifest"))
    ap.add_argument("--results-dir", default=str(RESULTS_DIR))
    ap.add_argument("--tmp-root", default=str(TMP_ROOT))
    ap.add_argument("--cache-dir", default=str(CACHE_ROOT),
                    help="gz content cache (default falls back to "
                         "<repo>/eco/cache when the dev-box drive is gone)")
    ap.add_argument("--max-fetch-failed-rate", type=float, default=0.15,
                    help="exit 3 when FETCH-FAILED exceeds this share of the "
                         "assets tested this run (#356/#408; 0 disables the "
                         "gate)")
    ap.add_argument("--redo", action="store_true",
                    help="force all verdicts eligible, ignoring stale cap (overall budgets still apply)")
    ap.add_argument("--report", action="store_true",
                    help="group gold verdicts from existing results; no test")
    ap.add_argument("--top", type=int, default=10)
    ap.add_argument("--include-storms", action="store_true",
                    help="report mode: also group ERROR-STORM")
    ap.add_argument("--include-gnu", action="store_true",
                    help="report mode: also group ASSET-NATURE (and legacy "
                         "GNU-ALSO-FAILS lines)")
    ap.add_argument("--build-id", default=None,
                    help="report mode: restrict to one build id; without it "
                         "the report is a cross-build cumulative watchlist "
                         "and must not feed issue titles")
    args = ap.parse_args(argv)

    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")

    # EVERY path becomes absolute before it is used. The workflow passes
    # workspace-relative paths (eco/...), and `tmp_root` is handed to the
    # child as HOME/USERPROFILE: a relative HOME lets niu resolve
    # `eco/tmp/sand-x/.niubashrc` against the child's OWN cwd, the rc check
    # misses, `is_first_run()` fires and the setup wizard blocks the asset
    # until the startup timeout (544/544 HANG, run 38690079988). wsl_path()
    # is drive-absolute-only too, so a relative --tmp-root also broke the
    # GNU parity oracle.
    results_dir = Path(args.results_dir).resolve()
    results_dir.mkdir(parents=True, exist_ok=True)
    results_path = results_dir / "test-results.jsonl"

    if args.report:
        return report(results_path, args.top, args.include_storms,
                      args.include_gnu, args.build_id)

    import sqlite3
    manifest_dir = Path(args.manifest_dir).resolve()
    db = sqlite3.connect(manifest_dir / "eco-manifest.sqlite",
                         check_same_thread=False,
                         timeout=30)  # harvest+test can run concurrently
    tier_list = list(TIERS) if args.tiers == "all" else list(dict.fromkeys(
        tier.strip() for tier in args.tiers.split(",") if tier.strip()))
    if not tier_list or set(tier_list) - set(TIERS):
        ap.error("--tiers must be all or a comma list of t1,t2,t3,t4,t5")
    if args.workers < 1 or min(args.limit, args.stale_limit, args.max_seconds) < 0:
        ap.error("workers must be positive; limits and budget must be non-negative")

    niu = Path(args.niu).resolve()
    rubash = Path(args.rubash).resolve()
    if not niu.is_file() or not rubash.is_file():
        print(f"FATAL: build binaries first: niu={niu}, rubash={rubash}")
        return 2
    gnu = GnuBash()
    if gnu.bin is None:
        msg = ("GNU oracle unavailable (no WSL distro, no Git for Windows "
               "usr/bin/bash.exe) — parity classification degrades: failures "
               "keep niu verdicts, and any HANG/ERROR-STORM will trip the "
               "unexplained-failure gate (#409)")
        print(f"::warning::{msg}" if os.environ.get("GITHUB_ACTIONS")
              else f"WARNING: {msg}")
    else:
        print(f"GNU parity oracle: {gnu.kind} :: {gnu.version}")
    build_id, identity = build_identity(
        niu, rubash, args.niu_revision, args.rubash_revision, args.slow_threshold, gnu)
    migrate_verdicts(db, results_path)
    with db:
        db.execute("INSERT OR IGNORE INTO eco_test_builds VALUES (?, ?)",
                   (build_id, json.dumps(identity, sort_keys=True)))
    pending, selection = select_assets(
        db, tier_list, build_id, args.limit, args.stale_limit, args.redo)
    print(f"build: {build_id} {json.dumps(identity, sort_keys=True)}")
    print(f"manifest tiers={tier_list}: " + " ".join(
        f"{key}={value}" for key, value in selection.items()))
    if not pending:
        eligible = selection["unseen"] + selection["transient"] + selection["stale"]
        idle = ("idle: no eligible assets for this build" if not eligible else
                "idle: eligible backlog remains, disabled by selection limits")
        print(idle)
        write_step_summary({}, 0, results_path, idle_note=idle,
                           selection=selection, build_id=build_id)
        db.close()
        return 0

    global PtyProcess
    try:
        PtyProcess = load_pty()
    except (ImportError, RuntimeError) as err:
        print(f"SKIP: {err}; execution needs Windows + pywinpty")
        return 2
    tmp_root = Path(args.tmp_root).resolve()
    cache_root = Path(args.cache_dir).resolve()
    # Fail fast and loud on an unusable path: the #356 failure mode was an
    # E:\ mkdir exception swallowed per asset as FETCH-FAILED × 547.
    for label, p in (("tmp root", tmp_root), ("cache dir", cache_root)):
        try:
            p.mkdir(parents=True, exist_ok=True)
        except OSError as err:
            print(f"FATAL: {label} {p} not creatable: {err}")
            return 2

    # Auth (#222 was the CI token layer; #356 the path layer): prefer the
    # injected token, fall back to gh, and never crash the lane on a missing
    # gh binary — an unauthenticated fetch would otherwise surface as a
    # 403-shaped FETCH-FAILED storm.
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if not token:
        try:
            token = subprocess.run(["gh", "auth", "token"],
                                   capture_output=True,
                                   text=True).stdout.strip() or None
        except OSError:
            token = None
    class Cfg:
        pass
    cfg = Cfg()
    cfg.niu = niu
    cfg.rubash = rubash
    cfg.tmp_root = tmp_root
    cfg.cache_root = cache_root
    cfg.gnu = gnu
    cfg.slow_threshold = args.slow_threshold
    cfg.token = token
    cfg.man = type("M", (), {"lock": threading.RLock(), "db": db})()

    t0 = time.time()
    counts = collections.Counter()
    lock = cfg.man.lock
    fh = open(results_path, "a", encoding="utf-8")
    os.environ["ECO_WORKERS"] = str(args.workers)  # report-mode throughput

    def run_one(asset):
        try:
            rec = test_one(asset, cfg)
        except Exception as err:  # one asset must never kill the pool
            rec = {"ts": NOW(), "content_hash": asset["content_hash"],
                   "repo": asset["repo"], "path": asset["path"],
                   "tier": asset["tier"], "verdict": "HARNESS-ERROR",
                   "detail": repr(err)[:200]}
        rec.update(build_id=build_id, build_identity=identity,
                   ts=dt.datetime.now(dt.timezone.utc).isoformat(timespec="microseconds"))
        with lock:
            fh.write(json.dumps(rec, ensure_ascii=False) + "\n")
            fh.flush()
            counts[rec["verdict"]] += 1
            save_verdict(db, rec)
        n = sum(counts.values())
        if n % 25 == 0:
            rate = n / max(time.time() - t0, 1)
            eta = (len(pending) - n) / rate if rate else 0
            print(f"[{n}/{len(pending)}] {dict(counts)} "
                  f"{rate:.2f}/s ETA {eta/60:.0f}m", flush=True)
        return rec

    try:
        run_bounded(pending, args.workers, args.max_seconds, run_one)
    finally:
        fh.close()
        db.close()

    wall = time.time() - t0
    tested = sum(counts.values())
    rate = tested / wall if wall else 0
    print(f"\n== smoke verdict distribution == {dict(counts)}")
    print(f"tested {tested} in {wall:.0f}s -> {rate:.2f} assets/s "
          f"({rate*3600:.0f}/hour) at workers={args.workers}")
    print(f"selected={len(pending)} completed={tested} "
          f"deferred_by_time_budget={len(pending) - tested}")

    # Gates (the #356 "green job, dead tier" class, continued by #408/#409).
    # Fetch gate: raw-fetch death is infrastructure (bad path, bad token,
    # dead CDN, #408's unencoded URLs), never a property of the assets — and
    # it used to be completely invisible: no non-zero exit, no ::warning, no
    # summary, so six nights of a 100%-dead tier shipped green.
    # Threshold (F6): 15% is the owner's directive for the #408 fix round,
    # and the observed nightly distribution backs it — healthy runs sit at
    # ~1% (nightly 38982913268: 4/400), the worst real bug class (#408's
    # unencoded URLs) landed at 19.5% (nightly 39078222286: 78/400), a dead
    # tier at ~100%. 15% keeps >15x headroom over healthy noise while still
    # tripping on every observed failure mode.
    # Unexplained-failure gate (#409, aligned with the UNCONFIRMED stamping
    # per #410 CR F7): HANG/ERROR-STORM are only engine-bug candidates when
    # an oracle stayed healthy, and a SYNTAX-REJECT-RUBASH-ONLY stamped with
    # no oracle is UNCONFIRMED — when NO oracle exists none of these can be
    # explained, and a lane that cannot explain its failures must not ship
    # green either.
    gates = []
    fetch_failed = counts.get("FETCH-FAILED", 0)
    if tested and args.max_fetch_failed_rate > 0 and \
            fetch_failed / tested > args.max_fetch_failed_rate:
        gates.append(
            "**fetch gate failed**: FETCH-FAILED "
            f"{fetch_failed}/{tested} ({fetch_failed / tested:.1%}) exceeds "
            f"{args.max_fetch_failed_rate:.0%} — the fetch tier is dead; "
            f"see {results_path}")
    unexplained = counts.get("HANG", 0) + counts.get("ERROR-STORM", 0)
    if gnu.bin is None:
        unexplained += counts.get("SYNTAX-REJECT-RUBASH-ONLY", 0)
    if tested and gnu.bin is None and unexplained:
        gates.append(
            "**unexplained-failure gate failed**: HANG+ERROR-STORM"
            "+unconfirmed-SYNTAX-REJECT "
            f"{unexplained}/{tested} with NO oracle available to explain "
            "them (#409) — wire Git for Windows usr/bin/bash.exe (or a WSL "
            "distro) before trusting this lane")
    for gate_msg in gates:
        print(f"::error::{gate_msg}" if os.environ.get("GITHUB_ACTIONS")
              else f"ERROR: {gate_msg}")
    write_step_summary(
        counts, tested, results_path, gates,
        selection=selection, build_id=build_id)
    if gates:
        return 3 if any("fetch gate" in g for g in gates) else 4
    return 0


if __name__ == "__main__":
    sys.exit(main())
