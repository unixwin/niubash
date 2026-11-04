//! The gated Tier C bind surface (1.5.0 PLAN row 11, step M-3) —
//! what remains of it after the GAP-3 migration (niubash#863).
//!
//! The registry's whole READ side retired in GAP-3: the entry
//! snapshot, the generation, the `bind -r` tombstones and the
//! readline variable table now arrive through the tier-A
//! `Session::bind_view` (`rubash::api::BindView` — entries with
//! the engine's canonical bytes, removals, editing mode,
//! variables), and the niu-owned mirror types this module used
//! to convert the snapshot into are gone.
//!
//! Two faces remain behind this gate because the Session
//! contract has no tier-A shape for them yet:
//!
//! - registering one macro into the engine registry (the plugin
//!   `rl_macro_bind` route), and
//! - the engine's keymap-name table (the `[keymap]` rule
//!   parser's validity check).
//!
//! This module is still the **only** file in this crate that
//! names the registry's types or the executor's registry
//! accessors, behind the `internals` feature:
//!
//! - With the feature (the default), the two faces work as
//!   before.
//! - Without it (`--no-default-features`), the crate still
//!   builds and runs: macro registration is refused and the
//!   keymap-name check does not fire. That build is the
//!   compile-level proof that no Tier C reference leaks outside
//!   this gate.

/// Register one macro in the engine's readline registry — the same data
/// plane a rc `bind '"\C-t": "text"'` writes.  A plugin's `rl_macro_bind`
/// arrives here so the macro joins the host macro/chord layer and shows up
/// in `bind -s` / `bind -p`.  Returns whether the entry was accepted.
#[cfg(feature = "internals")]
pub fn register_engine_macro(
    executor: &mut rubash::Executor,
    keyseq: String,
    text: String,
    keymap: Option<String>,
) -> bool {
    if keyseq.is_empty() {
        return false;
    }
    executor.register_bind_entry(rubash::shell::bind_registry::BindEntry {
        keyseq,
        kind: rubash::shell::bind_registry::BindKind::Macro { text },
        keymap,
    });
    true
}

#[cfg(not(feature = "internals"))]
pub fn register_engine_macro(
    _executor: &mut rubash::Executor,
    _keyseq: String,
    _text: String,
    _keymap: Option<String>,
) -> bool {
    false
}

/// Whether `name` is one of the engine registry's known keymap
/// names (the `-m` targets a bind entry may name). The `[keymap]`
/// rule parser consults this through the gate so a rule naming an
/// unknown keymap is refused with the engine's own wording
/// (niubash#630, K4). Outside the `internals` feature there is no
/// registry table to consult, so the check does not fire — any
/// name is accepted, and keymap targeting simply never matches an
/// unknown name when the mirror is built.
#[cfg(feature = "internals")]
pub fn is_known_keymap(name: &str) -> bool {
    rubash::shell::bind_registry::KEYMAP_NAMES.contains(&name)
}

#[cfg(not(feature = "internals"))]
pub fn is_known_keymap(_name: &str) -> bool {
    true
}
