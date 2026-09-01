import ctypes
import os
import sys
import time
import tempfile

"""End-to-end check for the trapint hook.

Drives target/debug/niu.exe through a Windows ConPTY and asserts that the
niubash_run_trapint_hooks function defined in .niubashrc runs when the REPL
receives Ctrl+C.
"""

kernel32 = ctypes.WinDLL('kernel32', use_last_error=True)

NL = chr(13) + chr(10)
BS = chr(92)
Q = chr(34)

CREATE_UNICODE_ENVIRONMENT = 0x00000400
STARTF_USESTDHANDLES = 0x00000100
KEY_EVENT = 1
LEFT_CTRL_PRESSED = 0x80000
TRAP_MARKER = b'TRAPINT_HOOK_FIRED'


class COORD(ctypes.Structure):
    _fields_ = [('X', ctypes.c_short), ('Y', ctypes.c_short)]


class SECURITY_ATTRIBUTES(ctypes.Structure):
    _fields_ = [('nLength', ctypes.c_ulong),
                ('lpSecurityDescriptor', ctypes.c_void_p),
                ('bInheritHandle', ctypes.c_int)]


class STARTUPINFOWEX(ctypes.Structure):
    _fields_ = [
        ('cb', ctypes.c_ulong), ('lpReserved', ctypes.c_wchar_p),
        ('lpDesktop', ctypes.c_wchar_p), ('lpTitle', ctypes.c_wchar_p),
        ('dwX', ctypes.c_ulong), ('dwY', ctypes.c_ulong),
        ('dwXSize', ctypes.c_ulong), ('dwYSize', ctypes.c_ulong),
        ('dwXCountChars', ctypes.c_ulong), ('dwYCountChars', ctypes.c_ulong),
        ('dwFillAttribute', ctypes.c_ulong), ('dwFlags', ctypes.c_ulong),
        ('wShowWindow', ctypes.c_ushort), ('cbReserved2', ctypes.c_ulong),
        ('lpReserved2', ctypes.c_void_p),
        ('hStdInput', ctypes.c_void_p), ('hStdOutput', ctypes.c_void_p),
        ('hStdError', ctypes.c_void_p),
    ]


class PROCESS_INFORMATION(ctypes.Structure):
    _fields_ = [('hProcess', ctypes.c_void_p), ('hThread', ctypes.c_void_p),
                ('dwProcessId', ctypes.c_ulong), ('dwThreadId', ctypes.c_ulong)]


class KEY_EVENT_RECORD(ctypes.Structure):
    _fields_ = [('bKeyDown', ctypes.c_int), ('wRepeatCount', ctypes.c_ushort),
                ('wVirtualKeyCode', ctypes.c_ushort),
                ('wVirtualScanCode', ctypes.c_ushort),
                ('uChar', ctypes.c_ushort),
                ('dwControlKeyState', ctypes.c_ulong)]


class INPUT_RECORD(ctypes.Structure):
    _fields_ = [('EventType', ctypes.c_ushort), ('pad', ctypes.c_ushort),
                ('KeyEvent', KEY_EVENT_RECORD)]


kernel32.CreatePseudoConsole.argtypes = [
    ctypes.POINTER(COORD), ctypes.c_void_p, ctypes.c_void_p, ctypes.c_ulong,
    ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p)]
kernel32.CreatePseudoConsole.restype = ctypes.c_long
kernel32.CreateEventW.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_int,
                                  ctypes.c_wchar_p]
kernel32.CreateEventW.restype = ctypes.c_void_p
kernel32.CreateProcessW.argtypes = [
    ctypes.c_void_p, ctypes.c_wchar_p, ctypes.c_void_p, ctypes.c_void_p,
    ctypes.c_int, ctypes.c_ulong, ctypes.c_void_p, ctypes.c_wchar_p,
    ctypes.POINTER(STARTUPINFOWEX), ctypes.POINTER(PROCESS_INFORMATION)]
kernel32.CreateProcessW.restype = ctypes.c_int
kernel32.WriteConsoleInputW.argtypes = [ctypes.c_void_p, ctypes.POINTER(INPUT_RECORD),
                                        ctypes.c_ulong, ctypes.POINTER(ctypes.c_ulong)]
kernel32.WriteConsoleInputW.restype = ctypes.c_int
kernel32.ReadFile.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_ulong,
                              ctypes.POINTER(ctypes.c_ulong), ctypes.c_void_p]
kernel32.ReadFile.restype = ctypes.c_int


def send_key(handle, u_char, vk, ctrl):
    rec = INPUT_RECORD(EventType=KEY_EVENT)
    rec.KeyEvent.bKeyDown = 1
    rec.KeyEvent.wRepeatCount = 1
    rec.KeyEvent.wVirtualKeyCode = vk
    rec.KeyEvent.uChar = u_char
    rec.KeyEvent.dwControlKeyState = LEFT_CTRL_PRESSED if ctrl else 0
    n = ctypes.c_ulong(0)
    return kernel32.WriteConsoleInputW(handle, ctypes.byref(rec), 1, ctypes.byref(n)) != 0


def drain_output(out_handle, timeout=6):
    chunks = []
    deadline = time.time() + timeout
    while time.time() < deadline:
        buf = ctypes.create_string_buffer(8192)
        got = ctypes.c_ulong(0)
        ok = kernel32.ReadFile(out_handle, buf, 8192, ctypes.byref(got), None)
        if ok and got.value > 0:
            chunks.append(buf.raw[:got.value])
        else:
            time.sleep(0.05)
    return b''.join(chunks)


def main():
    niu = os.path.join(os.getcwd(), 'target', 'debug', 'niu.exe').replace('/', BS)
    if not os.path.exists(niu):
        print('FAIL: niu binary not found at ' + niu)
        return 2

    home = tempfile.mkdtemp(prefix='niubash-ctrlc-e2e-')
    rc = NL.join([
        'niubash_run_trapint_hooks() {',
        '  printf ' + chr(39) + 'TRAPINT_HOOK_FIRED' + BS + 'n' + chr(39),
        '}',
    ]) + NL
    with open(os.path.join(home, '.niubashrc'), 'w', encoding='utf-8') as f:
        f.write(rc)

    GENERIC_READ = 0x80000000
    GENERIC_WRITE = 0x40000000

    def sa():
        return SECURITY_ATTRIBUTES(ctypes.sizeof(SECURITY_ATTRIBUTES), None, 1)

    in_handle = kernel32.CreateEventW(ctypes.byref(sa()), 1, 1, None)
    out_handle = kernel32.CreateEventW(ctypes.byref(sa()), 1, 1, None)
    print('handles: in=%s out=%s' % (in_handle, out_handle))

    pty = ctypes.c_void_p(None)
    hr = kernel32.CreatePseudoConsole(ctypes.byref(COORD(160, 40)), in_handle,
                                      out_handle, 0, ctypes.byref(pty), None)
    print('CreatePseudoConsole hr=%d err=%d pty=%s' % (hr, ctypes.get_last_error(), pty.value))
    if hr != 0:
        print('FAIL: CreatePseudoConsole failed')
        return 2

    si = STARTUPINFOWEX()
    si.cb = ctypes.sizeof(STARTUPINFOWEX)
    si.dwFlags = STARTF_USESTDHANDLES
    pty_val = ctypes.cast(pty, ctypes.c_void_p).value
    si.hStdInput = pty_val
    si.hStdOutput = pty_val
    si.hStdError = pty_val

    pi = PROCESS_INFORMATION()
    cwd = home.replace('/', BS)
    env_items = os.environ.copy()
    env_items['HOME'] = cwd
    env_items['USERPROFILE'] = cwd
    env_items['RUST_LOG'] = 'off'
    env_block = NL.join(k + '=' + v for k, v in env_items.items()) + NL + chr(0)
    env_w = ctypes.create_unicode_buffer(env_block, len(env_block) + 1)
    kernel32.SetCurrentDirectoryW(cwd)
    created = kernel32.CreateProcessW(None, Q + niu + Q, None, None, 1,
                                      CREATE_UNICODE_ENVIRONMENT, env_w, cwd,
                                      ctypes.byref(si), ctypes.byref(pi))
    if not created:
        print('FAIL: CreateProcessW failed err=%d' % ctypes.get_last_error())
        return 2

    time.sleep(12)

    ok_ctrl = send_key(in_handle, 0x43, 0x43, True)
    time.sleep(8)

    kernel32.FreeConsole()
    out_data = drain_output(out_handle)

    print('=== captured PTY output (%d bytes) ===' % len(out_data))
    print(out_data.decode('utf-8', 'replace'))
    print('=== end of output ===')
    print('write-ctrl-c-ok:', ok_ctrl)
    if TRAP_MARKER in out_data:
        print('PASS: trapint framework hook fired on Ctrl+C')
        return 0
    print('FAIL: trapint framework hook did not fire')
    return 1


if __name__ == '__main__':
    sys.exit(main())
