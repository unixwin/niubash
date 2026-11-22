# Niubash Test Tiers & CI Coverage

This document records which test targets run in CI, which run on a nightly/manual sweep, and which are intentionally local-only or `#[ignore]`d (see [unixwin/niubash#955](https://github.com/unixwin/niubash/issues/955)).

## Cross-Repo Scope (`rubash` vs `niubash`)

The ~10k-test integration corpus (`crates/rubash/tests/`, 1,176 files / 10,352 `#[test]` functions) referenced in `niubash#955` lives in the upstream [`unixwin/rubash`](https://github.com/unixwin/rubash) repository rather than this repository:

- **Upstream `rubash` coverage (`rubash#3600`):** `rubash/.github/workflows/ci.yml` now runs `pin-stem-census` (`scripts/ci-run-pin-stems.sh` + `scripts/ci-pin-exclusions.txt`) on `ubuntu-latest` with `nextest` `terminate-after = 3` (90s hard kill).
- **`coproc_redirect_order_matters_both_ways`:** Lives in `rubash/tests/issue1644_1655_coproc_dup_stderr_regressions.rs` (not in `niubash`). It still hangs on Windows when invoked directly against `rubash` and is tracked upstream.

Within `niubash`, the workspace (`niubash` + `niubash-runtime`) has 153 integration test files across `tests/` and `crates/niubash-runtime/tests/`, organized into three explicit tiers below.

---

## Tier 1 — PR / Push Hard Gates (`.github/workflows/ci.yml`)

| Job | Platform | Timeout | What runs | Notes |
| --- | --- | --- | --- | --- |
| `build-and-test` | `windows-latest` | 60m | `cargo fmt --check --all`, `cargo clippy -p niubash-runtime --all-targets --locked -- -D warnings`, `cargo build --locked`, `cargo test --workspace --locked --no-fail-fast`, `scripts/harvest` unit tests, `scripts/test-skill-bundle.py` | Runs all non-ignored, non-unix-only unit and integration tests. Hosted-CI ConPTY interactive tests skip via `require_pty_or_skip()` unless `NIU_INTERACTIVE_TESTS=1`. |
| `cross-check` | `ubuntu-latest`, `macos-latest` | 45m | `cargo check --workspace --locked`, `cargo test --test issue827_pty_gate --locked`, `cargo test --test issue731_k3_pty --locked` (with `NIU_INTERACTIVE_TESTS=1`) | Also runs `cargo test --workspace --lib --locked` and `cargo test -p niubash-runtime --tests --locked --no-fail-fast` with `continue-on-error: true` (informational until wave-2 Unix runtime lands). |
| `cross-target-check` | `ubuntu-latest` | 30m | `cargo check --workspace --locked --target <triple>` | Covers `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-ohos`, `aarch64-linux-android`, `armv7-linux-androideabi`. |
| `cross-target-smoke` | `ubuntu-22.04` | 30m | Release build + 4 static-binary smoke checks | `x86_64-unknown-linux-musl`. |
| `windows-gnu-link` | `ubuntu-latest` | 60m | `cargo build --locked --target x86_64-pc-windows-gnu --bin niu` + PE export table audit | Verifies MinGW PE export count `< 65535` and readline ABI symbols. |

---

## Tier 2 — Nightly / Manual Integration Sweep (`.github/workflows/ci.yml`)

Triggered on `schedule` (`cron: "23 3 * * *"`) or `workflow_dispatch`:

| Job | Platform | Timeout | What runs | Notes |
| --- | --- | --- | --- | --- |
| `integration-sweep-windows` | `windows-latest` | 60m | `cargo nextest run --workspace --locked --no-fail-fast --profile ci` | Uses `.config/nextest.toml` (`slow-timeout = 30s`, `terminate-after = 3`) so any hung test is killed at 90s and reported by name instead of stalling the runner. |
| `integration-sweep-unix` | `ubuntu-latest` | 45m | `cargo test -p niubash-runtime --tests --locked --no-fail-fast` + `#![cfg(unix)]` integration targets (`issue351_tput_oneshot_regressions`, `issue3605_exec_a_argv0`, `issue727_ffi_removed`, `issue769_suspend_cycle`, `issue974_unix_hangup`, `issue985_noediting_hangup`, `issue988_editor_hangup`) with `NIU_INTERACTIVE_TESTS=1` | `continue-on-error: true` while wave-2 Unix runtime work is in progress; provides a complete nightly census of Unix-only targets that `cross-check` does not run. |
| `perfbudget-trend` | `windows-2025` | 75m | `scripts/perf/asset-timing.py` over pinned `oh-my-bash` and `bash-it` | Scheduled nightly trend gate. |

---

## Tier 3 — Local-Only / Environment-Gated Tests

These tests compile in CI (`cargo test --no-run`) but do not execute in hosted PR CI by design:

1. **Windows ConPTY Interactive Suites (`NIU_INTERACTIVE_TESTS=1`)**
   - Targets: `tests/interactive.rs`, `tests/repl_pty_suite.rs`, `tests/issue373.rs`, `tests/issue535_stty_conpty.rs`, `tests/issue631_completion_interaction.rs`, `tests/issue731_k3_pty.rs`, `tests/issue742_completion_zsh.rs`, `tests/issue821_menu_enter_accepts.rs`, `tests/issue827_pty_gate.rs`.
   - Reason: Hosted GitHub Windows runners expose a ConPTY session model whose timing and console behavior are not representative of desktop terminals (`tests/interactive/driver.rs` `require_pty_or_skip()`).
   - Local run: `NIU_INTERACTIVE_TESTS=1 cargo test --test interactive --locked -- --nocapture`

2. **`winuxcmd` Command-Link Suites (`#[ignore]`)**
   - Targets: `tests/compat.rs`, `tests/host_contract.rs` (`requires real winuxcmd.exe command links`), `tests/issue156_xargs_option_boundary.rs`.
   - Reason: Require an installed `winuxcmd` command-link directory on `PATH`.
   - Local run: `cargo test --test compat --locked -- --ignored`

3. **Known-Red Pins & Forensic Probes (`#[ignore]`)**
   - `tests/host_contract.rs` (`export a` / `export A` Windows env-block case collision — known `rubash` gap)
   - `tests/issue149_export_boundary.rs` (`HOME` seeding — `niubash#149`)
   - `tests/interactive.rs::ctrl_z_at_prompt_is_ignored_but_shell_survives` (`rubash#287` residual)
   - `tests/keyevent_record_probe.rs` (3 forensic probes for `#341`; spawns `rustc` + ConPTY)
