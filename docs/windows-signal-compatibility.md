# Windows Signal Compatibility

Niubash implements a ZSH/FISH-compatible hook system with 20 hooks. On Windows,
some signals have no direct equivalent. This document records the compatibility
status of each signal.

## Signal Compatibility Matrix

| Signal | Windows Equivalent | Status | Alternative |
|--------|-------------------|--------|-------------|
| trapint | Ctrl+C (Win32 Console Handler) | ✅ Native | - |
| trapwinch | - | ❌ Not available | Poll GetConsoleScreenBufferInfo |
| trapusr1 | - | ❌ Not available | Windows message queue |
| trapusr2 | - | ❌ Not available | Windows message queue |
| trappipe | Broken pipe detection | ⚠️ Partial | Check os error 232 |
| trapterm | Exit path | ⚠️ Partial | finish_with_exit_trap |
| trapchld | Job Object API | ⚠️ Partial | Job Object + GetExitCodeProcess |
| trapdebug | Programmatic only | ⚠️ Manual | RUST_LOG=debug |
| traperr | Programmatic only | ⚠️ Manual | Call run_trap_hooks on failure |
| trappzerr | Programmatic only | ⚠️ Manual | Call run_trap_hooks on pipeline error |

## Detailed Notes

### trapint (✅ Native)

Fully supported via Win32 Console Control Handler. When Ctrl+C is pressed:
1. ctrl_handler() sets CTRL_C_RECEIVED flag
2. REPL loop checks consume_ctrl_c()
3. run_trap_hooks("trapint") dispatches to registered trapint hooks

### trapwinch (❌ Not available)

Windows has no SIGWINCH equivalent. Terminal resize is handled by:
- Windows Terminal's resize events
- Console buffer size changes

To implement: poll GetConsoleScreenBufferInfo() in precmd or period hooks.

### trapusr1/2 (❌ Not available)

Windows has no SIGUSR1/SIGUSR2. These are only available on Unix systems.
On Windows, these hooks are registered but never triggered automatically.

### trappipe (⚠️ Partial)

Windows detects broken pipes via error code 232 ("管道正在被关闭").
The run_exit_trap() function can check for this and dispatch trappipe hooks.

### trapterm (⚠️ Partial)

No SIGTERM equivalent. Exit hooks fire via finish_with_exit_trap() on REPL exit.

### trapchld (⚠️ Partial)

Windows Job Objects can monitor child process exit. Implementation requires:
- CreateJobObject() with JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
- AssignProcessToJobObject() for spawned children
- GetExitCodeProcess() to detect exit

### trapdebug (⚠️ Manual)

No automatic trigger. Can be called programmatically:
- shell.run_trapdebug_hooks()
- Useful for debugging hooks with RUST_LOG=debug

### traperr (⚠️ Manual)

No automatic trigger. Should be called after command failures:
- Detect non-zero exit code
- Call shell.run_traperr_hooks()

### trappzerr (⚠️ Manual)

Alias for traperr in bash/ZSH. Same behavior.

## Hook Registration (All 20 Hooks)

All hooks are available for registration via hooks.niu:

```bash
# Lifecycle hooks
niubash_add_startup_hook my_func
niubash_add_precmd_hook my_func
niubash_add_preexec_hook my_func
niubash_add_postcmd_hook my_func
niubash_add_chpwd_hook my_func
niubash_add_period_hook my_func
niubash_add_zshaddhistory_hook my_func
niubash_add_zshexit_hook my_func
niubash_add_greeting_hook my_func
niubash_add_title_hook my_func

# Trap hooks (only trapint auto-triggers on Windows)
niubash_add_trapdebug_hook my_func
niubash_add_traperr_hook my_func
niubash_add_trapint_hook my_func      # Ctrl+C
niubash_add_trapwinch_hook my_func    # Not triggered on Windows
niubash_add_trapusr1_hook my_func     # Not triggered on Windows
niubash_add_trapusr2_hook my_func     # Not triggered on Windows
niubash_add_trappipe_hook my_func
niubash_add_trapterm_hook my_func
niubash_add_trapchld_hook my_func
niubash_add_trappzerr_hook my_func
```

## Future Improvements

1. **trapwinch**: Implement console buffer size polling
2. **trappipe**: Auto-detect broken pipe errors in run_exit_trap()
3. **trapchld**: Job Object integration for child process monitoring
4. **trapusr1/2**: Windows message queue implementation (complex)
