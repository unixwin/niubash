#!/usr/bin/env python
"""ConPTY benchmark for niubash#202 (slow multiline paste) — the perf
companion of `PasteChunkEditMode` (niubash#252, PSReadLine-style 50 ms
paste-chunk detection on the Windows console path).

Calibration contract (CHANGELOG 1.4.0, niubash#252 entry): the issue's
257-line author payload (scripts/perf/fixtures/issue202_repro_payload.txt,
a `set -euo pipefail` build script) drops from >300 s (timeout — the old
path executed every pasted line as it arrived) to ~4.6 s end to end. This
script measures exactly that workload end to end and gates the defect
metric; `elapsed` is comparable with the CHANGELOG number because the
harness cadence (settle drain, 1.0 s Enter resend) is part of the contract
and must not change silently.

What is timed (end to end, the calibrated scope):
  from the first paste byte written into the ConPTY until the marker line
  `PASTE_DONE` appears as an executed output line. This spans the real
  user-visible cost: burst absorption + the synthetic user's Enter +
  the submitted script's execution. Components are reported separately
  (`settle_wait`, `enters_sent`) so path cost and harness wait split.

What is deliberately NOT timed (old paths, unrelated to
PasteChunkEditMode):
  - the `bracket` flavor this bench used to time by default. ConPTY
    swallows the ?2004/?201~ markers (crossterm#737), so a "bracketed"
    paste through ConPTY exercised the SAME raw per-line feed as `raw` —
    a duplicate datapoint under a misleading name. Windows has no
    bracketed-paste code path at all: niubash#252 IS its replacement
    (cfg(windows)); Unix keeps niubash#234's real `Event::Paste` and never
    runs this heuristic. The bench therefore feeds exactly one shape: the
    raw burst a Windows terminal delivers on Ctrl+V.
  - shell startup: the clock starts after the prompt has settled, not at
    process spawn.

GNU bash / PSReadLine paste semantics: the paste lands in the edit buffer
as ONE unit and its trailing newline does NOT execute (PSReadLine waits
for the user's own Enter). The harness therefore sends the user's Enter
after the paste settles, resending every 1.0 s until the marker executes:
an Enter that lands while the feed is still hot is absorbed by
PasteChunkEditMode as paste data (a blank line in the chunk — by design,
see `paste_chunk_detector_absorbs_burst_newlines_and_flushes_on_gap`), and
the first Enter after the feed ends submits the buffered script once.

Metrics:
  elapsed               end-to-end wall time, paste write -> executed
                        `PASTE_DONE` line. THE calibration number: ~4.6 s
                        expected on the issue202 payload (CHANGELOG 1.4.0);
                        the pre-#252 path runs into `--timeout` (300 s).
  settle_wait           part of `elapsed` that is pure harness wait before
                        the first Enter (lets the feed drain so the Enter
                        submits instead of being absorbed; on the healthy
                        path the paste is silent, so this is a small known
                        constant — path-vs-harness split).
  bad_executions_paste  bare `BAD<n>` OUTPUT lines seen while the paste
                        was still arriving (before the first Enter). This
                        is the niubash#202 defect metric — it must be 0.
                        The echoed edit-buffer text (`echo BAD<n>`) in
                        repaints does not count.
  bad_executions_total  the same count over the whole session. Under
                        correct paste semantics the user's Enter submits
                        the whole buffered script, so it approaches the
                        payload size by design and is informational only.
  bytes_out             total output volume (the #252 repaint-collapse
                        datapoint: 176 KB pre-#252 -> ~3.3 KB after).

Payloads:
  issue202 (default): the exact 257-line build script the issue author
              posted (heredocs, backslash continuations, `$(find ...)`
              substitution) — the CHANGELOG-calibrated workload.
  synthetic : N `echo BAD<n>` lines + a final `echo PASTE_DONE`. The
              BAD lines must NOT execute mid-paste; their per-line output
              is what the defect metric counts.

Usage:
  python scripts/perf/paste_multiline_bench.py [--payload issue202]
          [--lines 200] [--timeout 300] [--exe PATH] [--json]

  `--exe` defaults to target/debug/niu.exe (or $NIU_BENCH_EXE), so a
  before/after run benches two builds with this same harness version.
  Windows only (ConPTY/pywinpty driver, and the code under test is
  cfg(windows)); on other platforms the script explains and exits 2.
  CI shape: a windows-latest job — the same runner and pywinpty driver
  the perfbudget-trend job in .github/workflows/ci.yml already uses:

      cargo build --bin niu
      python scripts/perf/paste_multiline_bench.py --payload issue202 --json
"""
import argparse
import json
import os
import platform
import queue
import re
import subprocess
import sys
import tempfile
import threading
import time

from winpty import PtyProcess

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, '..', '..'))
ISSUE202_PAYLOAD = os.path.join(HERE, 'fixtures', 'issue202_repro_payload.txt')

NL = '\r\n'

# Harness cadence contract — changing these changes what `elapsed` means
# and breaks comparability with the CHANGELOG 1.4.0 ~4.6 s reference.
SETTLE_QUIET = 0.3     # s of output silence that ends the paste-phase drain
ENTER_RESEND = 1.0     # s between synthetic user Enter attempts
POLL_TICK = 0.1        # s output poll granularity
MARKER = 'PASTE_DONE'


def build_synthetic_paste(lines):
    parts = []
    for i in range(max(0, lines - 1)):
        parts.append('echo BAD%d' % i)
    parts.append('echo %s' % MARKER)
    return NL.join(parts) + NL


def build_issue202_paste():
    with open(ISSUE202_PAYLOAD, encoding='utf-8') as f:
        body = f.read().replace('\r\n', '\n').rstrip('\n')
    # The script's own last line is a plain echo. NOTE: the marker is NOT
    # appended to the pasted text — the payload is a `set -euo pipefail`
    # build script, and once the paste is buffered as one unit and
    # submitted (correct paste semantics), a failing command aborts the
    # script before any in-paste marker would run. The harness types the
    # marker as its own command after the submit instead.
    return body + NL


def payload_stats(payload_name, text):
    st = {
        'payload': payload_name,
        'lines': text.count('\n'),
        'bytes': len(text.encode('utf-8')),
    }
    if payload_name == 'issue202':
        # The author's post is 257 lines (the fixture file as committed);
        # the actual paste is the normalized body — trailing blank lines
        # of the post are not paste data.
        with open(ISSUE202_PAYLOAD, 'rb') as f:
            st['fixture_lines'] = f.read().count(b'\n')
    return st


ANSI_RE = re.compile(r'\x1b\[[0-9;?]*[A-Za-z]|\x1b\][^\x07]*\x07')


def count_executed_bad(output):
    """Count executed `BAD<n>` lines (bare output), ignoring the echoed
    `echo BAD<n>` edit-buffer text that repaints render."""
    plain = ANSI_RE.sub('', output)
    return sum(1 for line in re.split(r'[\r\n]+', plain)
               if re.fullmatch(r'BAD\d+', line))


class PtyReader:
    """winpty reads block, so pump them from a thread."""

    def __init__(self, pty):
        self.pty = pty
        self.q = queue.Queue()
        self.t = threading.Thread(target=self._pump, daemon=True)
        self.t.start()

    def _pump(self):
        while True:
            try:
                c = self.pty.read(4096)
            except Exception:
                self.q.put(None)
                return
            self.q.put(c)

    def read_until_settled(self, quiet=SETTLE_QUIET, timeout=90.0):
        """Accumulate output until it has been quiet for `quiet` seconds."""
        acc = ''
        last = time.time()
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                c = self.q.get(timeout=POLL_TICK)
            except queue.Empty:
                if time.time() - last > quiet:
                    break
                continue
            if c is None:
                break
            acc += c
            last = time.time()
        return acc

    def read_until(self, needle, timeout, on_tick=None):
        """Accumulate output until `needle` appears as a whole output LINE
        (after ANSI stripping). Returns (found, acc).

        The echoed edit-buffer text of a pending paste (`echo PASTE_DONE`)
        must not count as execution, hence the bare-line match. Matching
        is incremental — only new chunks plus a carried tail are scanned —
        so the harness's own CPU cannot distort the timing on a regressed
        (high-output) path. `on_tick` fires after each poll with the raw
        new chunk; the caller uses it to resend Enter on cadence."""
        acc = ''
        tail = ''
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                c = self.q.get(timeout=POLL_TICK)
            except queue.Empty:
                if on_tick is not None:
                    on_tick()
                continue
            if c is None:
                break
            acc += c
            hay = tail + c
            plain = ANSI_RE.sub('', hay)
            if any(line == needle for line in re.split(r'[\r\n]+', plain)):
                return True, acc
            lines = re.split(r'([\r\n]+)', plain)
            tail = ''.join(lines[-2:]) if len(lines) > 1 else hay
            if on_tick is not None:
                on_tick()
        return False, acc


def niu_version(exe):
    """The binary's own version banner — it embeds the rubash rev, so the
    bench output is attributable to a build without any repo context."""
    try:
        out = subprocess.run([exe, '--version'], capture_output=True,
                             text=True, timeout=30)
        return ' '.join(out.stdout.split()) or 'unknown'
    except Exception as exc:
        return 'unknown (%s)' % exc


def repo_rev():
    try:
        out = subprocess.run(
            ['git', '-C', REPO, 'rev-parse', '--short', 'HEAD'],
            capture_output=True, text=True, timeout=15)
        return out.stdout.strip() or 'unknown'
    except Exception:
        return 'unknown'


def print_env(exe, payload_name, text, lines, timeout):
    argv = 'python scripts/perf/paste_multiline_bench.py --payload %s' \
           ' --lines %d --timeout %s --exe %s' % (
               payload_name, lines, timeout, exe)
    try:
        import winpty as _w
        pywinpty = getattr(_w, '__version__', 'unknown')
    except Exception:
        pywinpty = 'unknown'
    st = payload_stats(payload_name, text)
    print('paste-bench-env niu="%s" niubash_rev=%s payload=%s(%d pasted '
          'lines%s, %d bytes) os=%s python=%s pywinpty=%s exe=%s'
          % (niu_version(exe), repo_rev(), st['payload'], st['lines'],
             ' / %d in fixture' % st['fixture_lines']
             if 'fixture_lines' in st else '',
             st['bytes'], platform.platform(), platform.python_version(),
             pywinpty, exe), flush=True)
    print('paste-bench-repro %s' % argv, flush=True)


def run_case(exe, payload_name, payload_text, timeout):
    home = tempfile.mkdtemp(prefix='niubash-paste-bench-')
    with open(os.path.join(home, '.niubashrc'), 'w', encoding='utf-8') as f:
        f.write('')
    env = dict(os.environ)
    env['HOME'] = home
    env['USERPROFILE'] = home
    env['RUST_LOG'] = 'off'
    pty = PtyProcess.spawn(exe + ' --norc', dimensions=(40, 120),
                           cwd=home, env=env)
    reader = PtyReader(pty)
    try:
        reader.read_until_settled(quiet=1.2, timeout=90.0)

        # Raw feed only — see module docstring for why there is no
        # bracketed flavor on the Windows console path.
        payload = payload_text
        t_paste_start = time.time()
        pty.write(payload)
        # Bound the paste-phase defect window: output produced while the
        # paste is still arriving must contain no executions. On the
        # healthy path the paste is silent here (the chunk stays pending),
        # so this drain costs its small known constant — reported below,
        # not hidden inside the path time.
        pre_enter = reader.read_until_settled(quiet=SETTLE_QUIET,
                                              timeout=30.0)
        settle_wait = time.time() - t_paste_start
        t_first_enter = time.time()

        # PSReadLine / GNU bash paste semantics: the paste's own trailing
        # newline never executes — the user presses Enter to submit the
        # buffered chunk. An Enter typed while the feed is still hot is
        # absorbed as paste data by design, so resend Enter + the marker
        # command on the contract cadence until the marker executes.
        state = {'next_send': t_first_enter + ENTER_RESEND, 'enters': 0}

        def send_enter():
            pty.write('\r')
            pty.write('echo %s\r\n' % MARKER)
            state['enters'] += 1

        send_enter()

        def on_tick():
            if time.time() >= state['next_send']:
                send_enter()
                state['next_send'] = time.time() + ENTER_RESEND

        found, acc = reader.read_until(MARKER, timeout, on_tick=on_tick)
        elapsed = time.time() - t_paste_start
        return {
            'payload': payload_name,
            'ok': found,
            'elapsed': round(elapsed, 2),
            'settle_wait': round(settle_wait, 2),
            'enters_sent': state['enters'],
            'bad_executions_paste': count_executed_bad(pre_enter),
            'bad_executions_total': count_executed_bad(pre_enter + acc),
            'bytes_out': len(pre_enter + acc),
        }
    finally:
        try:
            pty.terminate(force=True)
        except Exception:
            pass


def main():
    if not sys.platform.startswith('win'):
        print('SKIP: paste_multiline_bench drives the Windows console path '
              'only — PasteChunkEditMode (niubash#252) is cfg(windows) and '
              'the driver is pywinpty/ConPTY. Run it on Windows (CI: '
              'windows-latest, see .github/workflows/ci.yml perfbudget-trend '
              'for the driver setup).', flush=True)
        return 2
    ap = argparse.ArgumentParser(
        description='End-to-end ConPTY bench for the multiline paste path '
                    '(niubash#202 / PasteChunkEditMode, niubash#252).')
    ap.add_argument('lines_pos', nargs='?', type=int, default=None,
                    help='synthetic payload line count (default 200)')
    ap.add_argument('payload_pos', nargs='?', default=None,
                    choices=['synthetic', 'issue202'],
                    help='payload (default issue202 — the CHANGELOG-'
                         'calibrated author workload)')
    ap.add_argument('--lines', type=int, default=None,
                    help='synthetic payload line count (default 200)')
    ap.add_argument('--payload', default=None,
                    choices=['synthetic', 'issue202'],
                    help='payload (default issue202)')
    ap.add_argument('--timeout', type=float, default=300.0,
                    help='per-case timeout in seconds (default 300 — the '
                         '">300 s" pre-#252 calibration bound)')
    ap.add_argument('--exe', default=None,
                    help='niu binary to bench (default target/debug/niu.exe'
                         ' or $NIU_BENCH_EXE)')
    ap.add_argument('--json', action='store_true',
                    help='emit one JSON object per case (CI form)')
    args = ap.parse_args()

    exe = (args.exe or os.environ.get('NIU_BENCH_EXE')
           or os.path.join(REPO, 'target', 'debug', 'niu.exe'))
    if not os.path.exists(exe):
        print('FAIL: %s not found (cargo build first or pass --exe)'
              % exe, flush=True)
        return 2

    lines = (args.lines or args.lines_pos or 200)
    payload_name = args.payload or args.payload_pos or 'issue202'
    if payload_name == 'issue202':
        text = build_issue202_paste()
    else:
        text = build_synthetic_paste(lines)

    print_env(exe, payload_name, text, lines, args.timeout)

    r = run_case(exe, payload_name, text, args.timeout)
    if args.json:
        print(json.dumps(r), flush=True)
    else:
        print('paste-bench payload=%s ok=%s elapsed=%.2fs settle_wait=%.2fs '
              'enters_sent=%d bad_executions_paste=%d bad_executions_total=%d '
              'bytes_out=%d'
              % (r['payload'], r['ok'], r['elapsed'], r['settle_wait'],
                 r['enters_sent'], r['bad_executions_paste'],
                 r['bad_executions_total'], r['bytes_out']), flush=True)
    rc = 0 if (r['ok'] and not r['bad_executions_paste']) else 1
    return rc


if __name__ == '__main__':
    sys.exit(main())
