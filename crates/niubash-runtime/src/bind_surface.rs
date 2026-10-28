//! The gated Tier C bind surface (1.5.0 PLAN row 11, step M-3).
//!
//! The engine's bind registry (`rubash::shell::bind_registry`)
//! is Tier C in the API ledger: its target home is "the
//! interactive layer's own gated surface". This module is that
//! gate on the niubash side. It is the **only** file in this
//! crate that names the registry's types or the executor's
//! registry accessors, and the conversion to the niu-owned
//! mirror types below happens here, behind the `internals`
//! feature:
//!
//! - With the feature (the default), the mirror is populated
//!   from the engine registry exactly as before this row.
//! - Without it (`--no-default-features`), the crate still
//!   builds and runs: the mirror is simply empty, so the editor
//!   keeps its built-in keybindings and runtime `bind` entries
//!   are not mirrored. That build is the compile-level proof
//!   that no Tier C reference leaks outside this gate.
//!
//! Everything outside this module — the REPL's keymap
//! translation, the shell's `bind -x` dispatch — works on the
//! mirror types only.

/// One engine-registered `bind` entry, mirrored into niu-owned
/// data (the registry entry's three facts, nothing else).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineBindEntry {
    /// The key sequence, in readline spelling.
    pub keyseq: String,
    /// The entry's action.
    pub kind: EngineBindKind,
    /// The `-m keymap` target, when one was named.
    pub keymap: Option<String>,
}

/// The action of an engine bind entry (mirrors the registry's
/// `BindKind` variant for variant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineBindKind {
    /// `bind -x`: run this command on the keypress.
    Execute {
        /// The command text.
        command: String,
    },
    /// A readline function name.
    Function {
        /// The function's name.
        name: String,
    },
    /// A macro: insert this text.
    Macro {
        /// The macro text.
        text: String,
    },
}

/// Take the engine registry's snapshot and generation, converted
/// to the mirror types. Outside the `internals` feature the
/// snapshot is empty and the generation is 0, consistently, so
/// staleness checks simply never fire.
#[cfg(feature = "internals")]
pub fn load_engine_bindings(executor: &rubash::Executor) -> (Vec<EngineBindEntry>, u64) {
    let generation = executor.bind_registry_generation();
    let entries = executor
        .bind_registry_snapshot()
        .into_iter()
        .map(convert)
        .collect();
    (entries, generation)
}

/// The registry's current generation (the staleness probe the
/// REPL runs between prompts).
#[cfg(feature = "internals")]
pub fn current_generation(executor: &rubash::Executor) -> u64 {
    executor.bind_registry_generation()
}

#[cfg(not(feature = "internals"))]
pub fn load_engine_bindings(_executor: &rubash::Executor) -> (Vec<EngineBindEntry>, u64) {
    (Vec::new(), 0)
}

#[cfg(not(feature = "internals"))]
pub fn current_generation(_executor: &rubash::Executor) -> u64 {
    0
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

#[cfg(feature = "internals")]
fn convert(entry: rubash::shell::bind_registry::BindEntry) -> EngineBindEntry {
    use rubash::shell::bind_registry::BindKind;
    let kind = match entry.kind {
        BindKind::Execute { command } => EngineBindKind::Execute { command },
        BindKind::Function { name } => EngineBindKind::Function { name },
        BindKind::Macro { text } => EngineBindKind::Macro { text },
    };
    EngineBindEntry {
        keyseq: entry.keyseq,
        kind,
        keymap: entry.keymap,
    }
}

#[cfg(all(test, feature = "internals"))]
mod tests {
    use super::{convert, EngineBindKind};
    use rubash::shell::bind_registry::{BindEntry, BindKind};

    #[test]
    fn conversion_mirrors_every_variant() {
        let execute = convert(BindEntry {
            keyseq: "\\C-r".to_string(),
            kind: BindKind::Execute {
                command: "echo x".to_string(),
            },
            keymap: None,
        });
        assert_eq!(
            execute.kind,
            EngineBindKind::Execute {
                command: "echo x".to_string()
            }
        );
        let function = convert(BindEntry {
            keyseq: "\\C-a".to_string(),
            kind: BindKind::Function {
                name: "beginning-of-line".to_string(),
            },
            keymap: Some("emacs".to_string()),
        });
        assert_eq!(function.keymap.as_deref(), Some("emacs"));
        assert_eq!(
            function.kind,
            EngineBindKind::Function {
                name: "beginning-of-line".to_string()
            }
        );
        let mac = convert(BindEntry {
            keyseq: "\\C-t".to_string(),
            kind: BindKind::Macro {
                text: "text".to_string(),
            },
            keymap: None,
        });
        assert_eq!(
            mac.kind,
            EngineBindKind::Macro {
                text: "text".to_string()
            }
        );
    }
}
