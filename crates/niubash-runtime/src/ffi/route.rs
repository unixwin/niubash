//! The route classifier: for a command name (and its argument
//! shapes), which execution route owns it?
//!
//! The decision order is the #2036 registry's, and #567 gate note
//! ⑤ pins the outcomes this classifier must reproduce:
//!
//! 1. **Builtins and keywords dispatch before any external route
//!    exists.** The FFI route is structurally unreachable for them;
//!    they appear in the classifier only so the census can assert
//!    that unreachability over the full name union.
//! 2. **Identity rows** (`uname`/`arch`/`hostname`): registry
//!    `HiddenFastpath` identity faces with ScriptYield/FailClosed
//!    stand-down. Not FFI names — the ruling is explicit, and this
//!    classifier declines them even if a tier table mislabels one.
//! 3. **The host terminal row** (`stty`): a `HiddenFastpath` host
//!    face (Replace/FailClosed). Not an FFI name; it also sits on
//!    the §6.3 hard floor below.
//! 4. Everything else is an external candidate. FFI claims it only
//!    when **all** of these hold: the master switch is on, the name
//!    is on no exclusion list (user's, or the §6.3 hard floor), the
//!    shipped tier table classifies it `tier2`, and no fallback
//!    shape matches this call's arguments. Tier-1 names stay on
//!    the emulation route while [`TIER1_FFI_CLAIMABLE`] is false.

use super::config::FfiConfig;
use super::tiers::{matching_fallback_shape, Tier, TierTable};

/// Whether FFI may claim tier-1 names. Pinned `false` by #567:
/// tier-1 takes the baton from the emulation layer only after its
/// own differential gate (and the FFI-vs-emulation arm) passes.
/// The census asserts that with the switch on, tier-1 names do NOT
/// route to FFI while this is false.
pub const TIER1_FFI_CLAIMABLE: bool = false;

/// Registry identity rows (HiddenFastpath, stand-down
/// ScriptYield/FailClosed): engine identity faces, never FFI.
pub const IDENTITY_FASTPATH_NAMES: &[&str] = &["uname", "arch", "hostname"];

/// Registry host-terminal rows (HiddenFastpath, stand-down
/// Replace/FailClosed): host-owned console faces, never FFI.
pub const HOST_TERMINAL_NAMES: &[&str] = &["stty"];

/// DESIGN §6.3's never-FFI floor, enforced here as a hard check
/// independent of the tier table's contents: a mislabeled table
/// cannot pull any of these into the process. (`stty` is also a
/// host-terminal row above; `wpm` manages the tree the DLL lives
/// in; the rest are interactive or long-running shapes.)
pub const NEVER_FFI_NAMES: &[&str] = &[
    "wpm", "less", "more", "top", "watch", "sleep", "yes", "stty",
];

/// Bash reserved words: like builtins, they dispatch in the
/// grammar before external resolution exists.
pub const KEYWORDS: &[&str] = &[
    "!", "[[", "]]", "{", "}", "case", "coproc", "do", "done", "elif", "else", "esac", "fi", "for",
    "function", "if", "in", "select", "then", "time", "until", "while",
];

/// The `type` / `command -V` annotation for an FFI-claimed name
/// (#567 discoverability ruling: checkable, never noisy — the
/// annotation lives only on introspection faces, never in a
/// command's own output bytes).
pub const FFI_ANNOTATION: &str = "winuxcmd FFI (in-process via winuxcore.dll)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Shell builtin or keyword: dispatched before external
    /// resolution; no external route (FFI included) can claim it.
    Builtin,
    /// Registry identity fastpath (uname/arch/hostname).
    Identity,
    /// Registry host-terminal fastpath (stty).
    HostTerminal,
    /// The engine's in-process emulation layer (tier-1 today).
    Emulation,
    /// The FFI route claims this call.
    Ffi,
    /// The external form: spawn the resolved identity.
    Spawn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteReason {
    BuiltinOrKeyword,
    IdentityFastpath,
    HostTerminalFastpath,
    SwitchOff,
    /// The name contains a path separator: it was never a bare
    /// registry name to begin with.
    PathLikeName,
    /// User exclusion (`NIU_WINUXCMD_FFI_EXCLUDE`).
    UserExcluded,
    /// The §6.3 hard floor.
    NeverFfi,
    /// No DLL candidate / no tier table: nothing to claim with.
    NoTierTable,
    /// The name is not in the tier table at all: not a winuxcmd
    /// registry name (a PATH external, a script, an unknown name).
    NotWinuxCmd,
    TierUnclassified,
    TierConditional,
    TierNever,
    /// Tier-1 while its gate is closed: emulation if the layer is
    /// available, else the reason records the gate itself.
    Tier1GateClosed,
    Tier1Emulation,
    /// A per-call fallback shape matched (e.g. `rm -i`).
    FallbackShape(String),
    Claimed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision {
    pub route: Route,
    pub reason: RouteReason,
}

impl RouteDecision {
    fn new(route: Route, reason: RouteReason) -> Self {
        RouteDecision { route, reason }
    }
}

/// Inputs the decision reads besides the name and arguments.
pub struct RouteContext<'a> {
    pub config: &'a FfiConfig,
    /// The tier table of the resolved DLL version, if discovery
    /// produced one. `None` declines every FFI claim.
    pub tiers: Option<&'a TierTable>,
    /// The engine's canonical builtin names
    /// (`Executor::builtin_command_names`, already a consumed
    /// ledger face via completion).
    pub builtins: &'a [&'a str],
    /// Whether the engine's emulation layer is enabled for this
    /// session (tier-1's current home).
    pub emulation_available: bool,
}

/// Classify one call. Pure: no I/O, no process state — the census
/// and the unit tests drive it directly.
pub fn route_for(name: &str, args: &[String], ctx: &RouteContext<'_>) -> RouteDecision {
    if ctx.builtins.iter().any(|b| *b == name) || KEYWORDS.contains(&name) {
        return RouteDecision::new(Route::Builtin, RouteReason::BuiltinOrKeyword);
    }
    if IDENTITY_FASTPATH_NAMES
        .iter()
        .any(|n| n.eq_ignore_ascii_case(name))
    {
        return RouteDecision::new(Route::Identity, RouteReason::IdentityFastpath);
    }
    if HOST_TERMINAL_NAMES
        .iter()
        .any(|n| n.eq_ignore_ascii_case(name))
    {
        return RouteDecision::new(Route::HostTerminal, RouteReason::HostTerminalFastpath);
    }
    if name.contains(['/', '\\']) {
        return RouteDecision::new(Route::Spawn, RouteReason::PathLikeName);
    }
    if !ctx.config.enabled {
        return RouteDecision::new(Route::Spawn, RouteReason::SwitchOff);
    }
    if NEVER_FFI_NAMES.iter().any(|n| n.eq_ignore_ascii_case(name)) {
        return RouteDecision::new(Route::Spawn, RouteReason::NeverFfi);
    }
    if ctx.config.is_excluded(name) {
        return RouteDecision::new(Route::Spawn, RouteReason::UserExcluded);
    }
    let Some(tiers) = ctx.tiers else {
        return RouteDecision::new(Route::Spawn, RouteReason::NoTierTable);
    };
    let Some(row) = tiers.lookup(name) else {
        return RouteDecision::new(Route::Spawn, RouteReason::NotWinuxCmd);
    };
    match row.tier {
        Tier::Tier2 => match matching_fallback_shape(&row.fallback_shapes, args) {
            Some(shape) => {
                RouteDecision::new(Route::Spawn, RouteReason::FallbackShape(shape.to_string()))
            }
            None => RouteDecision::new(Route::Ffi, RouteReason::Claimed),
        },
        Tier::Tier1 => {
            if TIER1_FFI_CLAIMABLE {
                match matching_fallback_shape(&row.fallback_shapes, args) {
                    Some(shape) => RouteDecision::new(
                        Route::Spawn,
                        RouteReason::FallbackShape(shape.to_string()),
                    ),
                    None => RouteDecision::new(Route::Ffi, RouteReason::Claimed),
                }
            } else if ctx.emulation_available {
                RouteDecision::new(Route::Emulation, RouteReason::Tier1Emulation)
            } else {
                RouteDecision::new(Route::Spawn, RouteReason::Tier1GateClosed)
            }
        }
        Tier::Conditional => RouteDecision::new(Route::Spawn, RouteReason::TierConditional),
        Tier::Never => RouteDecision::new(Route::Spawn, RouteReason::TierNever),
        Tier::Spawn => RouteDecision::new(Route::Spawn, RouteReason::TierUnclassified),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffi::tiers::TierTable;

    const TABLE: &str = "command\ttier\tfallback_shapes\tnote\n\
cp\ttier1\t-\tTier-1\n\
rm\ttier1\t-i,-I\tTier-1 interactive\n\
grep\ttier2\t-\tTier-2\n\
head\ttier2\t-f\tTier-2 with shape\n\
tee\tconditional\t-\tBlocked\n\
wpm\tnever\t-\tTree manager\n\
arch\tspawn\t-\tunclassified\n\
base64\tspawn\t-\tunclassified\n";

    fn ctx<'a>(
        config: &'a FfiConfig,
        tiers: Option<&'a TierTable>,
        emulation: bool,
    ) -> RouteContext<'a> {
        RouteContext {
            config,
            tiers,
            builtins: &["echo", "cd", "type"],
            emulation_available: emulation,
        }
    }

    fn on() -> FfiConfig {
        FfiConfig::from_pairs([("NIU_WINUXCMD_FFI", "1")])
    }
    fn off() -> FfiConfig {
        FfiConfig::from_pairs(Vec::<(String, String)>::new())
    }
    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn builtins_and_keywords_are_unreachable_in_both_states() {
        let table = TierTable::parse(TABLE).unwrap();
        for cfg in [on(), off()] {
            let c = ctx(&cfg, Some(&table), true);
            assert_eq!(route_for("echo", &[], &c).route, Route::Builtin);
            assert_eq!(route_for("for", &[], &c).route, Route::Builtin);
            assert_eq!(route_for("[[", &[], &c).route, Route::Builtin);
        }
    }

    #[test]
    fn identity_and_host_rows_are_never_ffi() {
        let table = TierTable::parse(TABLE).unwrap();
        let cfg = on();
        let c = ctx(&cfg, Some(&table), true);
        // `arch` even carries a (spawn) tier row; class wins.
        assert_eq!(route_for("arch", &[], &c).route, Route::Identity);
        assert_eq!(route_for("uname", &[], &c).route, Route::Identity);
        assert_eq!(route_for("stty", &[], &c).route, Route::HostTerminal);
    }

    #[test]
    fn switch_off_spawns_everything_external() {
        let table = TierTable::parse(TABLE).unwrap();
        let cfg = off();
        let c = ctx(&cfg, Some(&table), true);
        let d = route_for("grep", &args(&["x"]), &c);
        assert_eq!(d.route, Route::Spawn);
        assert_eq!(d.reason, RouteReason::SwitchOff);
    }

    #[test]
    fn tier2_claims_and_shapes_fall_back() {
        let table = TierTable::parse(TABLE).unwrap();
        let cfg = on();
        let c = ctx(&cfg, Some(&table), true);
        assert_eq!(route_for("grep", &args(&["x", "f"]), &c).route, Route::Ffi);
        let shaped = route_for("head", &args(&["-f", "f"]), &c);
        assert_eq!(shaped.route, Route::Spawn);
        assert_eq!(shaped.reason, RouteReason::FallbackShape("-f".to_string()));
        assert_eq!(route_for("head", &args(&["-n", "1"]), &c).route, Route::Ffi);
    }

    #[test]
    fn tier1_stays_on_emulation_while_its_gate_is_closed() {
        let table = TierTable::parse(TABLE).unwrap();
        assert!(!TIER1_FFI_CLAIMABLE);
        let cfg = on();
        let c = ctx(&cfg, Some(&table), true);
        assert_eq!(
            route_for("cp", &args(&["a", "b"]), &c).route,
            Route::Emulation
        );
        // Even the shape that will matter after the gate (rm -i)
        // is emulation's today, not FFI's and not spawn's.
        assert_eq!(
            route_for("rm", &args(&["-i", "x"]), &c).route,
            Route::Emulation
        );
        let no_emu = ctx(&cfg, Some(&table), false);
        let d = route_for("cp", &args(&["a", "b"]), &no_emu);
        assert_eq!(d.route, Route::Spawn);
        assert_eq!(d.reason, RouteReason::Tier1GateClosed);
    }

    #[test]
    fn never_conditional_unclassified_and_unknown_all_spawn() {
        let table = TierTable::parse(TABLE).unwrap();
        let cfg = on();
        let c = ctx(&cfg, Some(&table), true);
        // `wpm` is stopped by the §6.3 hard floor before the
        // table's own `never` row is even consulted.
        assert_eq!(route_for("wpm", &[], &c).reason, RouteReason::NeverFfi);
        assert_eq!(
            route_for("tee", &[], &c).reason,
            RouteReason::TierConditional
        );
        assert_eq!(
            route_for("base64", &[], &c).reason,
            RouteReason::TierUnclassified
        );
        assert_eq!(route_for("git", &[], &c).reason, RouteReason::NotWinuxCmd);
        assert_eq!(
            route_for("some/path/grep", &[], &c).reason,
            RouteReason::PathLikeName
        );
    }

    #[test]
    fn user_exclusion_beats_the_table() {
        let table = TierTable::parse(TABLE).unwrap();
        let cfg = FfiConfig::from_pairs([
            ("NIU_WINUXCMD_FFI", "1"),
            ("NIU_WINUXCMD_FFI_EXCLUDE", "grep"),
        ]);
        let c = ctx(&cfg, Some(&table), true);
        assert_eq!(route_for("grep", &[], &c).reason, RouteReason::UserExcluded);
    }
}
