# Performance investigation: pure-sh nested loops

This note tracks issue #18: pure shell scripts with nested loops and heavy
function calls should stay predictable under `niu -c` and script-file
execution.

## Scope

- Measure `niubash` in release mode, not debug mode.
- Compare against the same script under Git Bash or another local bash.
- Keep the benchmark script pure shell: arithmetic expansion, function calls,
  variable assignment, and loop control only.
- Record stdout, stderr, wall time, and exit status.

## Suggested harness

```sh
mkdir -p .tmp
cat > .tmp/perf-nested-loops.sh <<'EOF'
inner() {
  x=$1
  y=$2
  printf ''%s\n'' $((x + y)) > /dev/null
}

i=0
while [ "$i" -lt 200 ]; do
  j=0
  while [ "$j" -lt 200 ]; do
    inner "$i" "$j"
    j=$((j + 1))
  done
  i=$((i + 1))
done
EOF

cargo build --release
time target/release/niu.exe .tmp/perf-nested-loops.sh
time bash .tmp/perf-nested-loops.sh
```

## Current findings

- The hottest expected paths are rubash parse/execution loops, arithmetic
  expansion, and per-command environment synchronization.
- Host-facing fixes in this batch avoid extra work on command startup: stdin
  inheritance and script positional parameters are simple executor state writes.
- `rubash` PR #10 moved repeated loop body/condition AST construction out of
  per-iteration hot paths and was merged as
  `8026e3bfa81f694646f13786242d8d8ebca79ab4`.
- `rubash` PR #11 targeted the larger remaining hot path: per-command
  bookkeeping in `Executor::execute_command`. Windows WPR and `samply` were
  blocked locally, so temporary internal timing was used and removed before the
  PR. The patch avoids unconditional `BASH_COMMAND` text construction,
  avoids process-env sync for function call state, conditionally syncs
  diagnostic line numbers, and reuses function body ASTs across calls. It was
  merged as `8caab03257d9902f2d5215fe9b72f05052b204a4`.
- `rubash` PR #12 moved Bash call-stack arrays out of the function-call hot
  path and materializes them only when read. It also adds a narrow fast path
  for common assignment RHS forms like `$1` and `$((x + y))`. It was merged as
  `e8b5b7a037754579d13b29573e7a4d6cdfba7a98`.

## 2026-07-26 benchmark snapshot

These timings use the 80x80 microbenchmarks in
`tmp/rubash-perf` after pinning `niubash` to
`rubash` `8026e3bfa81f694646f13786242d8d8ebca79ab4`.

| Script | Bash ms | niubash release ms | Median ratio |
| --- | ---: | ---: | ---: |
| `loop.sh` | 201.6, 160.3, 160.1 | 1329.0, 677.1, 653.3 | 4.2x |
| `arith.sh` | 179.1, 172.7, 188.9 | 819.8, 854.3, 823.8 | 4.6x |
| `function-noop.sh` | 206.2, 203.8, 211.0 | 1118.5, 1083.1, 1123.2 | 5.4x |
| `function-args-arith.sh` | 279.4, 303.4, 303.4 | 1656.5, 1603.5, 1632.7 | 5.4x |

Direct `rubash` benchmarking before the `niubash` dependency bump showed the
loop-only case improving from roughly 635-996 ms to 532-536 ms, and the
function-plus-args-plus-arithmetic case improving from roughly 1655-1710 ms to
1498-1550 ms. That confirms the merged `rubash` optimization helps, but the
remaining gap is still large enough to keep issue #18 open for more profiling.

## 2026-07-26 command bookkeeping follow-up

After `rubash` PR #11, direct `rubash` release medians improved materially on
the same 80x80 scripts:

| Script | rubash baseline ms | rubash patched ms | Change |
| --- | ---: | ---: | ---: |
| `loop.sh` | 448.28 | 330.83 | -26.2% |
| `arith.sh` | 519.30 | 428.06 | -17.6% |
| `function-noop.sh` | 732.08 | 545.44 | -25.5% |
| `function-args-arith.sh` | 1126.39 | 827.02 | -26.6% |

After pinning `niubash` to
`rubash` `8caab03257d9902f2d5215fe9b72f05052b204a4`, release medians are:

| Script | Bash ms | niubash release ms | Median ratio |
| --- | ---: | ---: | ---: |
| `loop.sh` | 167.84 | 560.63 | 3.3x |
| `arith.sh` | 177.31 | 771.40 | 4.4x |
| `function-noop.sh` | 237.48 | 879.30 | 3.7x |
| `function-args-arith.sh` | 276.67 | 1260.52 | 4.6x |

Compared with the prior `niubash` snapshot, the new pin reduces median wall
time by about 17% on loop-only, 6% on arithmetic-heavy, 21% on function-noop,
and 23% on function-plus-args-plus-arithmetic. The remaining gap still appears
rubash-executor dominated, so the next profiling targets should be
`expand_embedded_parameters`, command/function dispatch overhead that remains
after PR #11, arithmetic evaluation, and any Windows process/environment calls
still reached from hot shell loops.

## 2026-07-26 call-stack array follow-up

After `rubash` PR #12, direct `rubash` release medians improved further on the
same 80x80 scripts:

| Script | rubash baseline ms | rubash patched ms | Change |
| --- | ---: | ---: | ---: |
| `loop.sh` | 346.09 | 347.59 | +0.4% |
| `arith.sh` | 401.66 | 386.89 | -3.7% |
| `function-noop.sh` | 523.39 | 430.38 | -17.8% |
| `function-args-arith.sh` | 824.77 | 697.68 | -15.4% |

After pinning `niubash` to
`rubash` `e8b5b7a037754579d13b29573e7a4d6cdfba7a98`, release medians are:

| Script | Bash ms | niubash release ms | Median ratio |
| --- | ---: | ---: | ---: |
| `loop.sh` | 119.17 | 402.54 | 3.4x |
| `arith.sh` | 139.50 | 494.38 | 3.5x |
| `function-noop.sh` | 157.77 | 535.09 | 3.4x |
| `function-args-arith.sh` | 209.26 | 804.35 | 3.8x |

Compared with the PR #11 `niubash` snapshot, this reduces median wall time by
about 28% on loop-only, 36% on arithmetic-heavy, 39% on function-noop, and 36%
on function-plus-args-plus-arithmetic. The remaining gap is narrower but still
large enough to keep issue #18 open; the next useful pass should use a real
sampling profiler or narrowly scoped internal timers around command dispatch
and arithmetic evaluation.

## Follow-up

- Add a checked-in ignored benchmark only after the release-mode baseline is
  captured on CI or a dedicated Windows runner.
- Keep the benchmark out of normal `cargo test`; it is timing-sensitive.
