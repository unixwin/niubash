#!/usr/bin/env python3
"""Perfbudget verdict regression tests for asset-timing.py
(niubash#335): five release runs went red on soft overs alone
(1.0-1.05x on first_prompt, zero hard fails) — runner-load noise
blocking releases with no product signal. The calibrated semantics
pinned here: a phase median over budget but within the hard
multiplier is a soft `over` (a printed warning, data kept for
budget recalibration); only beyond the hard multiplier
(hard_fail_multiplier, default 5x) is a phase a `hard-fail`; a
phase with no completed sample fails closed (timeout / hard-fail),
it does not skip.

Runs against the real module (importlib; synthetic phase records
only — no measurement, no niu.exe). Needs pywinpty + pyte importable
because the module imports them at top level; the perf lane installs
both (same contract as scripts/journey/test_verdict_exit.py).
Exit 0 = contract holds, 1 = broken.
"""

import importlib.util
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
MODULE_PATH = HERE / "asset-timing.py"


def load_module():
    spec = importlib.util.spec_from_file_location(
        "asset_timing_under_test", MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main():
    at = load_module()
    failures = []

    def expect(label, actual, wanted):
        if actual != wanted:
            failures.append(f"{label}: got {actual!r}, want {wanted!r}")

    asset = at.Asset("theme", "omb", "dummy", Path("dummy.toml"))
    budgets = at.Budgets({
        "defaults": {"theme": {"source_ms": 100.0,
                               "first_prompt_ms": 100.0,
                               "rerender_ms": 100.0}},
        "hard_fail_multiplier": 5,
    })

    def phases(median, status="ok", samples=(1,)):
        return {phase: {"samples_ms": list(samples), "median": median,
                        "status": status}
                for phase in at.PHASES}

    # 1. Under budget everywhere: ok.
    worst, per = at.verdict_for(asset, phases(50.0), budgets)
    expect("under-budget verdict", worst, "ok")

    # 2. The filed shape: a 1.05x first_prompt over is SOFT — the
    # worst verdict is `over`, never a failure, and the phase that
    # crossed is the one reported.
    soft = phases(50.0)
    soft["first_prompt"] = {"samples_ms": [105.0], "median": 105.0,
                             "status": "ok"}
    worst, per = at.verdict_for(asset, soft, budgets)
    expect("soft-over verdict", worst, "over")
    expect("soft-over phase", per["first_prompt"], "over")
    expect("soft-over other phases", per["source_wall"], "ok")

    # 3. Beyond the hard multiplier on ANY phase: hard-fail.
    hard = phases(50.0)
    hard["rerender"] = {"samples_ms": [501.0], "median": 501.0,
                        "status": "ok"}
    worst, per = at.verdict_for(asset, hard, budgets)
    expect("hard verdict", worst, "hard-fail")
    expect("hard phase", per["rerender"], "hard-fail")

    # 4. Exactly at the hard cap is still soft (the cap is a strict
    # greater-than), exactly at budget is still ok.
    edge = phases(100.0)
    worst, _ = at.verdict_for(asset, edge, budgets)
    expect("at-budget verdict", worst, "ok")
    edge = phases(500.0)
    worst, _ = at.verdict_for(asset, edge, budgets)
    expect("at-hard-cap verdict", worst, "over")

    # 5. No completed sample fails closed: a timed-out phase reports
    # `timeout`, an errored phase `hard-fail` — neither skips.
    dead = phases(0.0, status="timeout", samples=())
    worst, per = at.verdict_for(asset, dead, budgets)
    expect("timeout verdict", worst, "timeout")
    dead = phases(0.0, status="error", samples=())
    worst, per = at.verdict_for(asset, dead, budgets)
    expect("error verdict", worst, "hard-fail")

    # 6. The default hard multiplier is 5 when budgets.toml omits it.
    expect("default hard multiplier",
           at.Budgets({"defaults": {}}).hard_mult, 5.0)

    if failures:
        print("FAIL:")
        for line in failures:
            print(f"  - {line}")
        return 1
    print("ok: perfbudget verdicts calibrated "
          "(soft over warns, 5x hard cap blocks, no-sample fails closed)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
