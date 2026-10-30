//! niubash#567 gate item ⑤ — the route census pin.
//!
//! For every name in the union of {engine builtins, bash
//! keywords, the #2036 registry identity rows, the host-terminal
//! row, every name in the generated tier table, external PATH
//! samples} × {FFI switch off, FFI switch on}, pin the route the
//! host provider assigns. The FFI route may only ever claim a
//! name the tier table classifies for it; builtins, keywords,
//! identity rows, and the host-terminal row are structurally
//! unreachable by FFI, in both switch states.
//!
//! The tier table fixture (`tests/fixtures/ffi/tiers.tsv`,
//! provenance in its README) is the real generated table from the
//! WinuxCmd audited manifest. That manifest currently classifies
//! ZERO tier-2 names (the R1–R7 audit has not assigned the tier-2
//! class), so against the fixture the FFI claim surface is empty
//! by data. The claim mechanism itself is pinned non-vacuously by
//! the synthetic-table test at the bottom.

use niubash_runtime::ffi::config::FfiConfig;
use niubash_runtime::ffi::route::{
    route_for, Route, RouteContext, RouteReason, HOST_TERMINAL_NAMES, IDENTITY_FASTPATH_NAMES,
    KEYWORDS, NEVER_FFI_NAMES,
};
use niubash_runtime::ffi::tiers::{Tier, TierTable};

const FIXTURE: &str = include_str!("fixtures/ffi/tiers.tsv");

fn fixture_table() -> TierTable {
    TierTable::parse(FIXTURE).expect("fixture table parses")
}

fn switch(on: bool) -> FfiConfig {
    if on {
        FfiConfig::from_pairs([("NIU_WINUXCMD_FFI", "1")])
    } else {
        FfiConfig::from_pairs(Vec::<(String, String)>::new())
    }
}

fn decide(
    config: &FfiConfig,
    tiers: Option<&TierTable>,
    name: &str,
    args: &[String],
    emulation_available: bool,
) -> (Route, RouteReason) {
    let decision = route_for(
        name,
        args,
        &RouteContext {
            config,
            tiers,
            builtins: rubash::executor::Executor::builtin_command_names(),
            emulation_available,
        },
    );
    (decision.route, decision.reason)
}

#[test]
fn builtins_and_keywords_are_never_external_in_either_state() {
    let table = fixture_table();
    let builtins = rubash::executor::Executor::builtin_command_names();
    assert!(!builtins.is_empty());
    for on in [false, true] {
        let config = switch(on);
        for name in builtins.iter().chain(KEYWORDS.iter()) {
            let (route, reason) = decide(&config, Some(&table), name, &[], true);
            assert_eq!(
                (route, reason),
                (Route::Builtin, RouteReason::BuiltinOrKeyword),
                "name {name} with switch on={on}"
            );
        }
    }
}

#[test]
fn identity_and_host_terminal_rows_are_not_ffi_names() {
    let table = fixture_table();
    for on in [false, true] {
        let config = switch(on);
        for name in IDENTITY_FASTPATH_NAMES {
            let (route, reason) = decide(&config, Some(&table), name, &[], true);
            assert_eq!(
                (route, reason),
                (Route::Identity, RouteReason::IdentityFastpath),
                "identity name {name} with switch on={on}"
            );
        }
        for name in HOST_TERMINAL_NAMES {
            let (route, reason) = decide(&config, Some(&table), name, &[], true);
            assert_eq!(
                (route, reason),
                (Route::HostTerminal, RouteReason::HostTerminalFastpath),
                "host-terminal name {name} with switch on={on}"
            );
        }
    }
}

#[test]
fn tier1_names_stay_on_emulation_until_their_own_gate() {
    // #567 gate note ⑤: with the switch ON and the baton not yet
    // passed (TIER1_FFI_CLAIMABLE == false), tier-1 names must NOT
    // be claimed by FFI — they ride emulation, exactly as today;
    // without emulation they fall to spawn, still never to FFI.
    let table = fixture_table();
    let tier1 = table.names_with_tier(Tier::Tier1);
    assert_eq!(tier1.len(), 10, "the audited ten: {tier1:?}");
    let config_on = switch(true);
    for name in &tier1 {
        assert_eq!(
            decide(&config_on, Some(&table), name, &[], true),
            (Route::Emulation, RouteReason::Tier1Emulation),
            "tier-1 {name} with emulation available"
        );
        assert_eq!(
            decide(&config_on, Some(&table), name, &[], false),
            (Route::Spawn, RouteReason::Tier1GateClosed),
            "tier-1 {name} without emulation"
        );
    }
    // The standing fallback shape (`rm -i` / `rm -I`, DESIGN
    // §6.1) binds the FFI route once tier-1 takes the baton; while
    // the gate is closed those calls are emulation's, exactly as
    // today — the census pins only that FFI does not claim them,
    // in either emulation state.
    for args in [vec!["-i".to_string()], vec!["-I".to_string()]] {
        assert_eq!(
            decide(&config_on, Some(&table), "rm", &args, true).0,
            Route::Emulation,
            "rm {args:?} stays on emulation while the tier-1 gate is closed"
        );
        assert_eq!(
            decide(&config_on, Some(&table), "rm", &args, false),
            (Route::Spawn, RouteReason::Tier1GateClosed),
            "rm {args:?} without emulation falls to spawn, never to FFI"
        );
    }
}

#[test]
fn every_fixture_row_routes_by_its_tier_in_both_states() {
    let table = fixture_table();
    assert_eq!(table.len(), 176, "the generated table's full census");
    let config_off = switch(false);
    let config_on = switch(true);
    let builtins = rubash::executor::Executor::builtin_command_names();
    for &tier in &[
        Tier::Tier1,
        Tier::Tier2,
        Tier::Conditional,
        Tier::Never,
        Tier::Spawn,
    ] {
        for name in table.names_with_tier(tier) {
            // Switch off: nothing external is ever claimed, and
            // the pre-switch classes still answer first. (Seven
            // fixture names — echo/false/kill/printf/pwd/test/
            // true — are engine builtins too, and `time` is a
            // bash keyword: the builtin/keyword route wins in both
            // states, which is itself part of the census: a
            // same-named applet is unreachable.)
            let (route, reason) = decide(&config_off, Some(&table), &name, &[], true);
            let expected_off = if builtins.contains(&name) || KEYWORDS.contains(&name) {
                (Route::Builtin, RouteReason::BuiltinOrKeyword)
            } else if IDENTITY_FASTPATH_NAMES.contains(&name) {
                (Route::Identity, RouteReason::IdentityFastpath)
            } else if HOST_TERMINAL_NAMES.contains(&name) {
                (Route::HostTerminal, RouteReason::HostTerminalFastpath)
            } else {
                (Route::Spawn, RouteReason::SwitchOff)
            };
            assert_eq!(
                (route, reason),
                expected_off,
                "{name} (tier {tier:?}) with switch off"
            );
            // Switch on: per-tier verdicts. (Tier-1 and the
            // identity/host rows are pinned in their own tests.)
            if matches!(tier, Tier::Tier1) {
                continue;
            }
            let (route, reason) = decide(&config_on, Some(&table), &name, &[], true);
            let expected_on = if builtins.contains(&name) || KEYWORDS.contains(&name) {
                (Route::Builtin, RouteReason::BuiltinOrKeyword)
            } else if IDENTITY_FASTPATH_NAMES.contains(&name) {
                (Route::Identity, RouteReason::IdentityFastpath)
            } else if HOST_TERMINAL_NAMES.contains(&name) {
                (Route::HostTerminal, RouteReason::HostTerminalFastpath)
            } else if NEVER_FFI_NAMES.contains(&name) {
                (Route::Spawn, RouteReason::NeverFfi)
            } else {
                match tier {
                    Tier::Conditional => (Route::Spawn, RouteReason::TierConditional),
                    Tier::Never => (Route::Spawn, RouteReason::TierNever),
                    Tier::Spawn => (Route::Spawn, RouteReason::TierUnclassified),
                    Tier::Tier2 => (Route::Ffi, RouteReason::Claimed),
                    Tier::Tier1 => unreachable!(),
                }
            };
            assert_eq!(
                (route, reason),
                expected_on,
                "{name} (tier {tier:?}) with switch on"
            );
        }
    }
}

#[test]
fn external_and_unknown_names_are_never_claimed() {
    let table = fixture_table();
    for on in [false, true] {
        let config = switch(on);
        for name in [
            "git",
            "cargo",
            "node",
            "python",
            "ssh",
            "definitely-not-a-command-xyz",
        ] {
            let (route, reason) = decide(&config, Some(&table), name, &[], true);
            if on {
                assert_eq!(
                    (route, reason),
                    (Route::Spawn, RouteReason::NotWinuxCmd),
                    "external {name} with switch on"
                );
            } else {
                assert_eq!(route, Route::Spawn, "external {name} with switch off");
            }
        }
    }
    // Without a tier table at all (nothing installed), even a
    // would-be FFI name declines: no table, no claim.
    let config = switch(true);
    assert_eq!(
        decide(&config, None, "grep", &[], true),
        (Route::Spawn, RouteReason::NoTierTable)
    );
}

/// The audited fixture has zero tier-2 rows, so the claim path
/// itself is pinned against this small synthetic table (same
/// generated shape): tier-2 claims when on, shape-fallback,
/// user exclusion, and the kill switch all answer as designed.
#[test]
fn synthetic_tier2_claims_and_its_exits() {
    const SYNTHETIC: &str = "# winuxcmd-version: synthetic\n\
command\ttier\tfallback_shapes\tnote\n\
grep\ttier2\t--interactive\tsynthetic claim row\n\
head\ttier2\t-\tsynthetic claim row\n";
    let table = TierTable::parse(SYNTHETIC).expect("synthetic table parses");
    let on = switch(true);

    assert_eq!(
        decide(&on, Some(&table), "grep", &[], true),
        (Route::Ffi, RouteReason::Claimed)
    );
    assert_eq!(
        decide(
            &on,
            Some(&table),
            "head",
            &["-n".to_string(), "1".to_string()],
            true
        ),
        (Route::Ffi, RouteReason::Claimed)
    );
    // Shape fallback: the table's interactive shape declines.
    assert_eq!(
        decide(
            &on,
            Some(&table),
            "grep",
            &["--interactive".to_string()],
            true
        ),
        (
            Route::Spawn,
            RouteReason::FallbackShape("--interactive".to_string())
        )
    );
    // User exclusion beats the table.
    let excluded = FfiConfig::from_pairs([
        ("NIU_WINUXCMD_FFI", "1"),
        ("NIU_WINUXCMD_FFI_EXCLUDE", "grep"),
    ]);
    assert_eq!(
        decide(&excluded, Some(&table), "grep", &[], true),
        (Route::Spawn, RouteReason::UserExcluded)
    );
    // The kill switch beats everything external.
    let killed = FfiConfig::from_pairs([("NIU_WINUXCMD_FFI", "0")]);
    assert_eq!(
        decide(&killed, Some(&table), "grep", &[], true),
        (Route::Spawn, RouteReason::SwitchOff)
    );
    // And with the switch simply off (default), the same.
    let off = switch(false);
    assert_eq!(
        decide(&off, Some(&table), "grep", &[], true),
        (Route::Spawn, RouteReason::SwitchOff)
    );
}
