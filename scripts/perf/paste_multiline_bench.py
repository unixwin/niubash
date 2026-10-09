#!/usr/bin/env python
"""ConPTY benchmark for ISSUE unixwin/niubash#202 (slow multiline paste).

Drives target/debug/niu.exe under a Windows ConPTY (pywinpty — the same
driver style as the repo's eco-test harness), writes a paste burst as one
chunk (the way Windows Terminal feeds a Ctrl+V paste) and measures wall
time until the shell prints the marker command appended after the paste.

Paste flavors:
  raw    : plain text with CRLF endings — what a terminal sends when the
           app did NOT enable bracketed paste (mode ?2004).
  bracket: the same text wrapped in ESC[200~ ... ESC[201~ — what a
           terminal sends once the app enables bracketed paste.

Payloads:
  synthetic : N synthetic `echo BAD<n>` lines + a final `echo PASTE_DONE`.
              Intermediate BAD lines must NOT execute (GNU bash paste
              semantics: a multiline paste lands in the edit buffer as one
              unit, no per-line execution).
  issue202  : the exact 250+ line build script the issue author posted
              (heredocs, backslash continuations, $(find ...) substitution),
              with a trailing `echo PASTE_DONE` appended. On 1.1.4 the
              author reports the paste renders "one character at a time".

Usage:
  python scripts/perf/paste_multiline_bench.py [lines] [flavor] [payload]
    lines   default 200   flavor: raw|bracket|both (default both)
    payload synthetic|issue202 (default synthetic)
"""
import os
import queue
import sys
import tempfile
import threading
import time

from winpty import PtyProcess

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, '..', '..'))
ISSUE202_PAYLOAD = os.path.join(HERE, 'fixtures', 'issue202_repro_payload.txt')

NL = '\r\n'


def build_synthetic_paste(lines):
    parts = []
    for i in range(max(0, lines - 1)):
        parts.append('echo BAD%d' % i)
    parts.append('echo PASTE_DONE')
    return NL.join(parts) + NL


def build_issue202_paste():
    with open(ISSUE202_PAYLOAD, encoding='utf-8') as f:
        body = f.read().replace('\r\n', '\n').rstrip('\n')
    # The script's own last line is a plain echo; append our marker command
    # so the harness knows when the shell finished consuming the paste.
    return body + NL + 'echo PASTE_DONE' + NL


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

    def read_until_settled(self, quiet=1.2, timeout=90.0):
        """Accumulate output until it has been quiet for `quiet` seconds."""
        acc = ''
        last = time.time()
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                c = self.q.get(timeout=0.1)
            except queue.Empty:
                if time.time() - last > quiet:
                    break
                continue
            if c is None:
                break
            acc += c
            last = time.time()
        return acc

    def read_until(self, needle, timeout):
        """Accumulate output until `needle` appears. Returns (found, acc)."""
        acc = ''
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                c = self.q.get(timeout=0.1)
            except queue.Empty:
                continue
            if c is None:
                break
            acc += c
            if needle in acc:
                return True, acc
        return False, acc


def run_case(exe, flavor, payload_name, payload_text, timeout=300.0):
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
        startup = time.time()

        if flavor == 'bracket':
            payload = '\x1b[200~' + payload_text + '\x1b[201~'
        else:
            payload = payload_text

        pty.write(payload)
        found, acc = reader.read_until('PASTE_DONE', timeout)
        elapsed = time.time() - startup
        return {
            'flavor': flavor,
            'payload': payload_name,
            'ok': found,
            'elapsed': elapsed,
            'bad_executions': acc.count('BAD'),
            'bytes_out': len(acc),
        }
    finally:
        try:
            pty.terminate(force=True)
        except Exception:
            pass


def main():
    exe = os.path.join(REPO, 'target', 'debug', 'niu.exe')
    if not os.path.exists(exe):
        print('FAIL: %s not found (cargo build first)' % exe)
        return 2
    lines = int(sys.argv[1]) if len(sys.argv) > 1 else 200
    flavor = sys.argv[2] if len(sys.argv) > 2 else 'both'
    payload_name = sys.argv[3] if len(sys.argv) > 3 else 'synthetic'
    flavors = ['raw', 'bracket'] if flavor == 'both' else [flavor]
    rc = 0
    for f in flavors:
        if payload_name == 'issue202':
            text = build_issue202_paste()
        else:
            text = build_synthetic_paste(lines)
        r = run_case(exe, f, payload_name, text)
        print('paste-bench flavor=%s payload=%s ok=%s elapsed=%.2fs '
              'bad_executions=%d bytes_out=%d'
              % (r['flavor'], r['payload'], r['ok'], r['elapsed'],
                 r['bad_executions'], r['bytes_out']), flush=True)
        if not r['ok']:
            rc = 1
    return rc


if __name__ == '__main__':
    sys.exit(main())
