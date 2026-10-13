#!/usr/bin/env python3
"""Verdict-to-exit regression tests for golden-journey.py (run
38452840701): a labeled known-fail must NOT hold the release gate
(exit 0) while an unlabeled fail must (exit 1). Labeling, never
waiving — the BAD lines stay in the verdict with the owning ticket.

Runs against the real module (importlib, no niu.exe, no ConPTY
sessions — the verdict machinery is pure file+dict work). Needs
pywinpty + pyte importable because the module itself imports them at
top level; the release journey job installs both before this runs.
Exit 0 = contract holds, 1 = broken.
"""

import importlib.util
import json
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
MODULE_PATH = HERE / "golden-journey.py"


def load_module():
    spec = importlib.util.spec_from_file_location(
        "golden_journey_under_test", MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def seal_with(gj, steps_builder):
    with tempfile.TemporaryDirectory() as tmp:
        verdict = gj.Verdict(Path(tmp))
        steps_builder(verdict)
        worst = verdict.seal()
        recorded = json.loads(
            (verdict.artifacts / "verdict.json").read_text(encoding="utf-8"))
        text = (verdict.artifacts / "verdict.txt").read_text(encoding="utf-8")
        return worst, recorded, text


def main() -> int:
    gj = load_module()
    failures = []

    def expect(name, got, want):
        if got != want:
            failures.append(f"{name}: got {got!r}, want {want!r}")

    # 1. The run-38452840701 shape: a disable-receipt BAD whose evidence
    # carries the wt80 signature routes through check_with_known_fail to a
    # LABELED known-fail — and the gate exits 0.
    def labeled_wt80(verdict):
        step = verdict.step("P4-S3", "execute the undo receipts")
        step.check("the cp receipt restored the previous rc", True)
        gj.check_with_known_fail(
            step, "the disable receipt reported the removal",
            "the wizard-printed undo receipt failed at the REPL: "
            "'powerline-multiline' exists in multiple sources — pick one: "
            "oh-my-bash/powerline-multiline, bash-it/powerline-multiline")
        step.finish()

    worst, recorded, text = seal_with(gj, labeled_wt80)
    expect("labeled wt80 verdict", worst, "known-fail")
    expect("labeled wt80 exit", gj.gate_exit_code(worst), 0)
    step = recorded["steps"][0]
    bad = [a for a in step["assertions"] if not a["ok"]]
    if len(bad) != 1 or not bad[0]["known"]:
        failures.append(f"labeled wt80 BAD line not recorded known: {bad}")
    if "wt80-undo-receipt-ambiguous-theme" not in bad[0]["detail"]:
        failures.append(
            f"labeled BAD does not name the owning ticket: {bad[0]['detail']}")
    # The BAD stays a BAD in the text verdict — labeling, never waiving.
    if "[KNOWN-FAIL] P4-S3" not in text or "BAD  the disable receipt" not in text:
        failures.append("verdict.txt no longer shows the labeled BAD line")

    # 2. An UNLABELED failure exits 1.
    def unlabeled(verdict):
        step = verdict.step("J3", "activation")
        step.check("zero syntax errors after source", False,
                   "2 syntax error line(s): ['mystery: syntax error near `??`']")
        step.finish()

    worst, _, _ = seal_with(gj, unlabeled)
    expect("unlabeled verdict", worst, "fail")
    expect("unlabeled exit", gj.gate_exit_code(worst), 1)

    # 3. A clean run exits 0.
    def clean(verdict):
        step = verdict.step("J1", "fresh-install first contact")
        step.check("welcome v-banner appears", True)
        step.finish()

    worst, _, _ = seal_with(gj, clean)
    expect("clean verdict", worst, "pass")
    expect("clean exit", gj.gate_exit_code(worst), 0)

    # 4. Mixed: a labeled known-fail PLUS an unlabeled fail is still a
    # fail — the label never launders an unlabeled red.
    def mixed(verdict):
        step = verdict.step("P4-S3", "undo receipts")
        gj.check_with_known_fail(
            step, "the disable receipt reported the removal",
            "'powerline-multiline' exists in multiple sources")
        step.check("zero syntax errors after the undo", False,
                   "1 syntax error line(s): ['x: syntax error near `end`']")
        step.finish()

    worst, _, _ = seal_with(gj, mixed)
    expect("mixed verdict", worst, "fail")
    expect("mixed exit", gj.gate_exit_code(worst), 1)

    # 5. An unrecognized verdict is never green.
    for odd in ("blocked", "skip", "", "PASS"):
        expect(f"unrecognized verdict {odd!r}", gj.gate_exit_code(odd), 1)

    if failures:
        print("FAIL:")
        for line in failures:
            print(f"  - {line}")
        return 1
    print("ok: verdict-exit contract holds (known-fail 0, unlabeled fail 1)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
