# GNU Bash upstream local gate

Niubash keeps the GNU Bash upstream compatibility run as a local development
gate. Do not add the upstream Bash test tree to this repository, and do not run
the full gate in CI by default; it is intentionally too slow for the normal
Windows CI loop.

## Expected layout

Keep a sibling rubash checkout with its existing Bash upstream fixture:

```text
<workspace>/niubash
<workspace>/rubash/third_party/bash/tests
```

The runner can be pointed at another Bash upstream checkout with
`BASH_UPSTREAM_DIR`, but it must stay external to the niubash repository.

## Run command

Run through the installed Niubash command runner:

```sh
niu -c 'BASH_RUNNER="${BASH_RUNNER:-bash}"; "$BASH_RUNNER" scripts/run-bash-upstream-with-niubash.sh'
```

If the Bash runner is not named `bash` on your machine, set `BASH_RUNNER` in
your local environment or replace it in your local command. Keep that path out
of committed files.

On a typical Windows machine with Git for Windows installed but `bash` missing
from Niubash's `PATH`, call Git Bash explicitly:

```sh
niu -c 'cd C:/path/to/niubash && C:/Progra~1/Git/bin/bash.exe scripts/run-bash-upstream-with-niubash.sh'
```

Replace `C:/path/to/niubash` and the Git Bash path for your machine. The short
`C:/Progra~1/...` form avoids quoting the `Program Files` space through nested
shells.

To test a release build instead of the default debug build:

```sh
niu -c 'BASH_RUNNER="${BASH_RUNNER:-bash}"; NIU_BASH_UPSTREAM_PROFILE=release "$BASH_RUNNER" scripts/run-bash-upstream-with-niubash.sh'
```

To test an already-built binary, set `NIU_BASH_UPSTREAM_SHELL_BIN` to that
`niu.exe`; the runner will skip its own `cargo build` step.

For focused debugging, pass one upstream runner name after the script:

```sh
niu -c 'cd C:/path/to/niubash && C:/Progra~1/Git/bin/bash.exe scripts/run-bash-upstream-with-niubash.sh run-alias'
```

The gate passes only when it reports:

```text
Total: 86
Passed: 86
Failed: 0
```

Results are written under:

```text
target/bash-upstream-tests
```

Each run rewrites `target/bash-upstream-tests/results.tsv`, `summary.md`, and
the per-runner logs. If a long run is interrupted, kill any remaining
`bash.exe`/`niu.exe` children before starting another full run, otherwise an
old process can keep appending stale failures to the same result directory.

## Troubleshooting

If every upstream runner fails with exit `126`, open one log under
`target/bash-upstream-tests/logs/`. A message like this means the harness
safety guard rejected a path before Niubash actually ran the Bash test:

```text
Refusing rm outside Bash upstream work dir: /c/.../work/run-alias/tests
Allowed: C:/.../work/run-alias
```

That is a path-format mismatch between Git Bash `/c/...` paths and Windows
`C:/...` paths, not a shell compatibility failure. The runner intentionally
keeps destructive-command guards for `rm`, `touch`, `mkdir`, `cp`, `mv`, and
`ln`; do not remove or loosen those guards. Fix or preserve normalization so
both `/c/...` and `C:/...` compare as the same path.

If the command fails with `bash: command not found`, invoke Git Bash explicitly
as shown above or set `BASH_RUNNER` to a real Bash executable.

## Performance guardrails

The runner is test infrastructure and must not move into normal startup, `-c`,
script-file, or REPL hot paths. When touching Niubash host execution, follow the
Rubash performance process from the local rubash checkout:

```text
../rubash/docs/performance-debugging-process.md
```

In practice:

- keep shell semantics in rubash and avoid host-side interpreter rewrites;
- avoid eager process environment synchronization in hot command loops;
- avoid rebuilding static command shape or materializing Bash arrays unless a
  script observes them;
- compare behavioral fixes against focused tests first, broad tests second, and
  the local upstream gate when Bash compatibility is affected.
