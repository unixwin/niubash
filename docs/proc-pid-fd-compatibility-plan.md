# Winuxsh Minimal /proc/<pid>/fd Host Plan

> Status: blocked pending WinuxCmd source/API (2026-08-22)

## Boundary

Winuxsh currently selects `winuxcmd.exe`, injects its command-link directory into PATH, and embeds Rubash. Neither this repository nor the Rubash checkout contains the WinuxCmd implementation. Therefore this document defines the smallest host contract and evidence gate; it does not add a fake `/proc` tree or claim Linux procfs support.

Rubash remains the owner of Bash fd syntax and virtual lifetime. Winuxsh is the session/process owner: it must pass a descriptor snapshot to the backend for each external child. WinuxCmd is the only appropriate owner for native process lookup, handle mapping, and external `readlink`, `test`, and directory iteration.

## Minimal Provider Contract

Expose only a read-only virtual provider for `/proc/self/fd` and registered live `/proc/<pid>/fd`:

- list open descriptors 0, 1, 2 and currently open dynamic descriptors;
- support existence/stat and symlink-like `readlink`;
- return logical targets for regular files, `/dev/null`, and pipe endpoints, with an explicit opaque marker for handles without a stable path;
- remove closed descriptors and reject unknown/exited pids;
- snapshot one directory operation so concurrent close/reuse cannot produce stale entries;
- reject everything outside `proc/<pid>/fd` (no `status`, `maps`, `mem`, `sys`, ioctl, or writable procfs).

The provider must be isolated per Winuxsh session. Do not use global environment variables as the fd table, and do not translate the path into a real Windows directory. Native Windows paths remain native when passed to WinuxCmd commands.

## Proposed Integration

1. Add a Rubash child-export record: pid, fd number, direction, logical target, open/closed state, and generation.
2. Before external launch, Winuxsh registers the record with a backend provider keyed by the real child/session pid; after wait/reap it unregisters it.
3. WinuxCmd's path layer recognizes only `/proc/self/fd` and `/proc/<registered-pid>/fd`; its `readlink`, `test`, and directory APIs query the provider.
4. Keep registration lifetime tied to the process/job table. Reused PIDs must not see an older generation.

The exact API names and handle-to-target rules require the WinuxCmd source. Do not implement step 3 in `crates/winuxsh-runtime` as a second command dispatcher or by intercepting command output.

## Real BusyBox Evidence Gate

The primary evidence must be real BusyBox ash, pinned to a revision and run as individual tests first. Archive stdout, stderr, exit code, timeout, and command/backend provenance in `docs/evidence/proc-pid-fd/<run-id>/` or the Rubash raw-artifact directory. Required probes are:

```sh
ash -c 'readlink /proc/self/fd/0; readlink /proc/self/fd/1; readlink /proc/self/fd/2'
ash -c 'test -e /proc/self/fd/0; printf "status:%s\n" "$?"'
ash -c 'exec 3>/tmp/proc-fd-target; readlink /proc/self/fd/3; exec 3>&-; test -e /proc/self/fd/3; printf "status:%s\n" "$?"'
ash -c 'readlink /proc/$$/fd/0; test -d /proc/$$/fd'
```

Run the matching BusyBox `ash_test` cases containing `/proc/self/fd`, `/proc/$pid/fd`, `readlink`, or fd directory enumeration against BusyBox, GNU Bash, and the Winuxsh+Rubash+WinuxCmd stack. A Rubash builtin result does not count: each acceptance case must exercise the external WinuxCmd path at least once.

## Blocker

WinuxCmd source is unavailable in both current repositories, and no provider registration API is documented. Implementation cannot be completed honestly from Winuxsh alone. Acquire or mount the WinuxCmd source/build and document its real process table, child-handle, path, `readlink`, and directory-iteration owners. Then implement the provider, add ignored host-contract tests requiring real command links, and run the BusyBox evidence gate before changing any expected output.

See `D:/repo/rubash/docs/proc-pid-fd-compatibility-plan.md` for the corresponding Rubash contract and test artifact policy.
