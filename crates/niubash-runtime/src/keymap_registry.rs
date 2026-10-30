//! The unified keymap registry (niubash#630): the single source of
//! truth for the *effective* key bindings on this host.
//!
//! Four sources feed one table — reedline defaults, the host baseline
//! (menus, clipboard, native-widget presets), bundle widget bindings,
//! engine `bind` registrations (scripts), and user bindings — built in
//! that order by `crate::repl` every time the line editor is built.
//! The registry is what the editor actually runs with, so every
//! consumer reads from here instead of keeping a private copy of the
//! key table: the startup reminder pass (this module), the
//! `niu help keys` topic, and the doctor key-binding face.
//!
//! Bindings the bridge cannot mirror are recorded as
//! [`DroppedBind`]s, classified by *shape* (niubash#612's rule: the
//! mechanism class is the criterion, never the emitting tool). Each
//! dropped record carries an actionable fix rendered from the reason
//! class (DESIGN §8.1); structural classes (chained sequences,
//! editing-mode toggles) are recorded and listed but never warned
//! about per-item — an unactionable per-startup warning only trains
//! blindness.
//!
//! The registry is published in-process at every editor build and
//! persisted to `~/.niubash/state.toml` (machine state, not user
//! configuration: the program writes it, a corrupt file is simply
//! rebuilt). The CLI faces (`niu help keys`, doctor) read the
//! persisted snapshot and say which build it came from.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

// reedline 0.52 speaks crossterm 0.29 (the crate's `crossterm29`
// alias); the key types below are the ones reedline's combinations
// actually carry, not the crate's direct 0.27 dependency.
use crossterm29::event::{KeyCode, KeyModifiers};
use reedline::{EditCommand, ReedlineEvent};
use serde::{Deserialize, Serialize};

/// The one help topic this layer registers (DESIGN §8.4). The topic
/// name is defined exactly once, here; the reminder templates, the
/// `niu --help` topics section and the help listing all build their
/// command strings from this constant so the three faces can never
/// drift apart.
pub const KEYMAP_HELP_TOPIC: &str = "keys";

/// The full command that opens the keymap help topic.
pub fn keymap_help_command() -> String {
    format!("niu help {KEYMAP_HELP_TOPIC}")
}

/// The `niu --help` "Help topics" section plus the `niu help` topic
/// listing. One definition, referenced by both faces.
pub fn help_topics_section() -> String {
    format!(
        "Help topics:\n  {KEYMAP_HELP_TOPIC}    Key bindings — the effective table, \
         dropped bindings and their fixes (`{}`)",
        keymap_help_command()
    )
}

/// Which of the host's three editor keymaps a registry row belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeymapMode {
    Emacs,
    ViInsert,
    ViNormal,
}

impl KeymapMode {
    pub fn label(self) -> &'static str {
        match self {
            KeymapMode::Emacs => "Emacs mode",
            KeymapMode::ViInsert => "Vi insert",
            KeymapMode::ViNormal => "Vi normal",
        }
    }
}

/// Where a user-layer binding came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserBindingVia {
    /// The `NIU_BINDKEYS` rc variable.
    NiuBindkeys,
}

/// The layer that last wrote a key slot (DESIGN §5.3 build order:
/// defaults < host baseline < bundle < script < user).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeymapSource {
    Default,
    HostBaseline,
    Bundle {
        file: Option<String>,
        line: Option<usize>,
    },
    Script,
    User {
        via: UserBindingVia,
    },
}

impl KeymapSource {
    /// The bracket label used in listings: `[script]`, `[user: NIU_BINDKEYS]`, …
    pub fn label(&self) -> String {
        match self {
            KeymapSource::Default => "default".to_string(),
            KeymapSource::HostBaseline => "host default".to_string(),
            KeymapSource::Bundle { .. } => "bundle".to_string(),
            KeymapSource::Script => "script".to_string(),
            KeymapSource::User {
                via: UserBindingVia::NiuBindkeys,
            } => "user: NIU_BINDKEYS".to_string(),
        }
    }
}

/// One effective binding: a (mode, key) slot, what it does, and which
/// layer won it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeymapEntry {
    pub mode: KeymapMode,
    /// Display form of the key, e.g. `"\C-t"`, `"<Tab>"`.
    pub key_display: String,
    /// Human-readable action: `host command: …`, `widget: …`, …
    pub action: String,
    pub source: KeymapSource,
    /// The highest lower layer that had bound this key to something
    /// else, when the winner shadowed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadowed: Option<KeymapSource>,
}

/// The `bind` payload class of a dropped entry (mirrors the engine's
/// `BindKind` discriminant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DroppedKind {
    Execute,
    Function,
    Macro,
}

/// Why a bind could not be mirrored (DESIGN §4/§6.1). The classes are
/// shape classes: any tool's bind lands in a class by its mechanism,
/// never by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DroppedReason {
    /// Multi-key / chained sequence: reedline bindings are single-key.
    ChainedSequence,
    /// A readline editing-mode toggle: a session setting, not a key.
    ModeToggleFunction,
    /// A key-script macro (control escapes / substitution constructs):
    /// the host has no key-script executor.
    ScriptMacro,
    /// A key sequence shape the parser does not recognize.
    UnparseableKeyseq,
    /// A readline function name outside the host's deliverable set.
    UnknownFunction,
    /// A terminal response sequence (e.g. the DSR answer `\e[0n`):
    /// the input layer does not deliver it as a key event.
    ResponseSequence,
}

impl DroppedReason {
    /// Structural classes are design limitations, dropped silently
    /// whoever emitted them (niubash#612); they appear in listings and
    /// the startup summary count, never as per-item reminders.
    pub fn is_structural(self) -> bool {
        matches!(
            self,
            DroppedReason::ChainedSequence | DroppedReason::ModeToggleFunction
        )
    }

    /// The one-line "why" used by reminders and listings (DESIGN §8.1).
    pub fn why(self) -> &'static str {
        match self {
            DroppedReason::ChainedSequence => {
                "chained key sequence — reedline bindings are single-key"
            }
            DroppedReason::ModeToggleFunction => "mode switches are session settings, not keys",
            DroppedReason::ScriptMacro => {
                "key-script macro — the host has no key-script executor; \
                 the macro text is inserted literally today"
            }
            DroppedReason::UnparseableKeyseq => {
                "key sequence shape the input layer does not deliver"
            }
            DroppedReason::UnknownFunction => "function is not in the host's deliverable set",
            DroppedReason::ResponseSequence => {
                "terminal response sequence — the input layer does not \
                 deliver it as a key"
            }
        }
    }
}

/// A bind entry the editor could not mirror faithfully, with
/// everything a fix needs: the original keyseq, its payload class and
/// target, and the shape classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroppedBind {
    /// The keyseq exactly as the script/user wrote it.
    pub keyseq: String,
    pub kind: DroppedKind,
    /// Function name / command / macro text, by kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_name: Option<String>,
    pub reason: DroppedReason,
    /// Whether this exact problem has already been surfaced by a
    /// startup reminder (the §8.2 dedup state; persisted).
    #[serde(default)]
    pub reminded: bool,
}

impl DroppedBind {
    /// The dedup identity (DESIGN §8.2): the same problem reminded
    /// once; the same keyseq reappearing with a different target is a
    /// new problem.
    pub fn triple(&self) -> (String, DroppedReason, Option<String>) {
        (self.keyseq.clone(), self.reason, self.target_name.clone())
    }

    /// The session-form fix for this record (DESIGN §8.1 templates).
    pub fn session_fix(&self) -> String {
        match self.reason {
            DroppedReason::ModeToggleFunction => match self.target_name.as_deref() {
                Some("vi-editing-mode") => "set -o vi".to_string(),
                Some("emacs-editing-mode") => "set -o emacs".to_string(),
                _ => "set -o vi / set -o emacs".to_string(),
            },
            DroppedReason::ScriptMacro => format!(
                "bind -x '\"{}\": \"eval \\\"$({})\\\"\"'",
                self.keyseq, "<command>"
            ),
            DroppedReason::UnknownFunction => {
                format!("bind -x '\"{}\": <your command>'", self.keyseq)
            }
            DroppedReason::ChainedSequence
            | DroppedReason::UnparseableKeyseq
            | DroppedReason::ResponseSequence => match self.kind {
                DroppedKind::Function => format!(
                    "bind '\"<single-key>\": {}'",
                    self.target_name.as_deref().unwrap_or("<function>")
                ),
                DroppedKind::Execute => format!(
                    "bind -x '\"<single-key>\": {}'",
                    self.target_name.as_deref().unwrap_or("<command>")
                ),
                DroppedKind::Macro => format!(
                    "bind '\"<single-key>\": \"{}\"'",
                    self.target_name.as_deref().unwrap_or("")
                ),
            },
        }
    }

    /// The persistent-form fix for this record (DESIGN §8.1/§5.2):
    /// GNU-shaped rules belong in the rc (`bind`) or `~/.inputrc`.
    pub fn persist_fix(&self) -> String {
        format!(
            "add `{}` to ~/.niubashrc (or ~/.inputrc)",
            self.session_fix()
        )
    }
}

/// The registry as materialized by one editor build.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeymapRegistry {
    /// The engine bind-registry generation this snapshot was built
    /// from (the freshness signal the viewing faces annotate).
    pub generation: u64,
    /// The editor mode the snapshot was built for: `emacs` or `vi`.
    pub editor_mode: String,
    pub entries: Vec<KeymapEntry>,
    pub dropped: Vec<DroppedBind>,
}

/// The persisted state file: the last build's registry (with the
/// reminded flags as written by the reminder pass).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeymapState {
    pub registry_generation: u64,
    #[serde(default)]
    pub editor_mode: String,
    #[serde(default)]
    pub entries: Vec<KeymapEntry>,
    #[serde(default)]
    pub dropped: Vec<DroppedBind>,
}

impl From<&KeymapRegistry> for KeymapState {
    fn from(registry: &KeymapRegistry) -> Self {
        Self {
            registry_generation: registry.generation,
            editor_mode: registry.editor_mode.clone(),
            entries: registry.entries.clone(),
            dropped: registry.dropped.clone(),
        }
    }
}

/// Whether a keyseq is a terminal *response* sequence: a single CSI
/// unit whose final byte answers a query (`n` = device status report,
/// `R` = cursor position report), e.g. zoxide's `\e[0n`. Shape only:
/// parameterized key CSI (final byte a letter like `A`) is not a
/// response.
pub fn is_response_sequence(keyseq: &str) -> bool {
    let chars: Vec<char> = keyseq.chars().collect();
    // CSI introducer, in either spelling: `\e[` or the caret form `^[[`.
    let is_csi = chars.starts_with(&['\\', 'e', '[']) || chars.starts_with(&['^', '[', '[']);
    if !is_csi {
        return false;
    }
    let rest: &[char] = &chars[3..];
    !rest.is_empty()
        && rest[..rest.len() - 1]
            .iter()
            .all(|c| c.is_ascii_digit() || *c == ';' || *c == '?')
        && matches!(rest[rest.len() - 1], 'n' | 'R')
}

/// Whether a macro's text is a *key script* rather than literal text
/// (DESIGN §4, classes T3 vs R1): its meaning is a sequence of key
/// actions and evaluation, not text to insert. Evidence, by shape:
/// substitution constructs (backquotes, `$(`); control-function
/// escapes in source notation (`\C-`, `\M-`, `\e`); or — the form the
/// engine actually stores, escapes already translated — raw control
/// characters other than tab/newline (a stored Ctrl-B byte is a key
/// action, never text). Plain backslash escapes (`\\`, `\"`) and
/// tab/newline stay literal.
pub fn is_script_macro_text(text: &str) -> bool {
    if text.contains('`') || text.contains("$(") {
        return true;
    }
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
    {
        return true;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            match chars[i + 1] {
                'C' | 'M' if chars.get(i + 2) == Some(&'-') => return true,
                'e' => return true,
                _ => {}
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    false
}

/// Map the bridge's coarse classification onto the registry reason
/// (DESIGN §6.1): `structural` carries the two #612 classes; the
/// `Other` bucket refines by payload shape.
pub fn dropped_reason(
    structural: Option<DroppedReason>,
    keyseq: &str,
    kind: &crate::bind_surface::EngineBindKind,
) -> DroppedReason {
    if let Some(reason) = structural {
        return reason;
    }
    // The keyseq shape is the root cause when it is a response
    // sequence (the entry never reached its payload); otherwise the
    // payload class decides.
    if is_response_sequence(keyseq) {
        return DroppedReason::ResponseSequence;
    }
    match kind {
        crate::bind_surface::EngineBindKind::Function { .. } => DroppedReason::UnknownFunction,
        _ => DroppedReason::UnparseableKeyseq,
    }
}

/// Build a dropped record from an engine bind entry and its reason.
pub fn dropped_bind_for(
    entry: &crate::bind_surface::EngineBindEntry,
    reason: DroppedReason,
) -> DroppedBind {
    let (kind, target_name) = match &entry.kind {
        crate::bind_surface::EngineBindKind::Execute { command } => {
            (DroppedKind::Execute, Some(command.clone()))
        }
        crate::bind_surface::EngineBindKind::Function { name } => {
            (DroppedKind::Function, Some(name.clone()))
        }
        crate::bind_surface::EngineBindKind::Macro { text } => {
            (DroppedKind::Macro, Some(text.clone()))
        }
    };
    DroppedBind {
        keyseq: entry.keyseq.clone(),
        kind,
        target_name,
        reason,
        reminded: false,
    }
}

/// Display form of a key combination for listings: GNU spellings for
/// the chord forms (`"\C-t"`, `"\M-c"`), angle form for named keys
/// (`"<Tab>"`, `"<Ctrl-Up>"`).
pub fn key_display(modifiers: KeyModifiers, code: KeyCode) -> String {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    if let KeyCode::Char(c) = code {
        match (ctrl, alt, shift) {
            (false, false, false) => {
                return if c == ' ' {
                    "\"<Space>\"".to_string()
                } else {
                    format!("\"{c}\"")
                };
            }
            (true, false, false) => {
                return format!("\"\\C-{}\"", c.to_ascii_lowercase());
            }
            (false, true, false) => return format!("\"\\M-{c}\""),
            (true, true, false) => {
                return format!("\"\\M-\\C-{}\"", c.to_ascii_lowercase());
            }
            (false, false, true) if c.is_alphabetic() => return format!("\"{c}\""),
            _ => {}
        }
    }
    let name = match code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::BackTab => "Tab".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Backspace => "Backspace".to_string(),
        KeyCode::Delete => "Delete".to_string(),
        KeyCode::Insert => "Insert".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PageUp".to_string(),
        KeyCode::PageDown => "PageDown".to_string(),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::F(n) => format!("F{n}"),
        KeyCode::Null => "Null".to_string(),
        _ => "Unknown".to_string(),
    };
    let shift = shift || matches!(code, KeyCode::BackTab);
    format!(
        "<{}{}{}{name}>",
        if ctrl { "Ctrl-" } else { "" },
        if alt { "Alt-" } else { "" },
        if shift { "Shift-" } else { "" },
    )
}

/// A human-readable summary of what a reedline event does, for the
/// listing's action column. `bind -x` host commands resolve through
/// the engine snapshot to the command they run.
pub fn action_summary(
    event: &ReedlineEvent,
    engine_bindings: &[crate::bind_surface::EngineBindEntry],
) -> String {
    match event {
        ReedlineEvent::ExecuteHostCommand(payload) => {
            if let Some(index) = payload
                .strip_prefix(crate::repl::BINDX_HOST_COMMAND_PREFIX)
                .and_then(|rest| rest.parse::<usize>().ok())
            {
                match engine_bindings.get(index) {
                    Some(entry) => match &entry.kind {
                        crate::bind_surface::EngineBindKind::Execute { command } => {
                            format!("host command: {command}")
                        }
                        _ => format!("host command (bind #{index})"),
                    },
                    None => format!("host command (bind #{index})"),
                }
            } else if let Some(name) = payload.strip_prefix(crate::repl::WIDGET_HOST_COMMAND_PREFIX)
            {
                format!("widget: {name}")
            } else {
                format!("host command: {payload}")
            }
        }
        ReedlineEvent::Edit(commands) => edit_summary(commands),
        ReedlineEvent::Menu(name) => format!("menu: {name}"),
        ReedlineEvent::MenuAccept => "accept menu item".to_string(),
        ReedlineEvent::MenuNext => "menu next".to_string(),
        ReedlineEvent::MenuPrevious => "menu previous".to_string(),
        ReedlineEvent::MenuUp => "menu up".to_string(),
        ReedlineEvent::MenuDown => "menu down".to_string(),
        ReedlineEvent::MenuLeft => "menu left".to_string(),
        ReedlineEvent::MenuRight => "menu right".to_string(),
        ReedlineEvent::MenuPageNext => "menu page next".to_string(),
        ReedlineEvent::MenuPagePrevious => "menu page previous".to_string(),
        ReedlineEvent::UntilFound(events) => events
            .iter()
            .take(2)
            .map(|event| action_summary(event, engine_bindings))
            .collect::<Vec<_>>()
            .join(" / "),
        ReedlineEvent::Multiple(events) => events
            .iter()
            .take(2)
            .map(|event| action_summary(event, engine_bindings))
            .collect::<Vec<_>>()
            .join(" then "),
        ReedlineEvent::Enter => "accept line".to_string(),
        ReedlineEvent::Submit => "submit".to_string(),
        ReedlineEvent::SubmitOrNewline => "submit or newline".to_string(),
        ReedlineEvent::Esc => "escape".to_string(),
        ReedlineEvent::CtrlC => "interrupt".to_string(),
        ReedlineEvent::CtrlD => "end of input".to_string(),
        ReedlineEvent::Up => "up".to_string(),
        ReedlineEvent::Down => "down".to_string(),
        ReedlineEvent::Left => "left".to_string(),
        ReedlineEvent::Right => "right".to_string(),
        ReedlineEvent::ClearScreen => "clear screen".to_string(),
        ReedlineEvent::ClearScrollback => "clear scrollback".to_string(),
        ReedlineEvent::Repaint => "redraw".to_string(),
        ReedlineEvent::SearchHistory => "search history".to_string(),
        ReedlineEvent::HistoryHintComplete => "accept history hint".to_string(),
        ReedlineEvent::HistoryHintWordComplete => "accept history hint word".to_string(),
        ReedlineEvent::OpenEditor => "open editor".to_string(),
        ReedlineEvent::None => "no-op".to_string(),
        other => {
            let debug = format!("{other:?}");
            debug
                .split([' ', '{', '('])
                .next()
                .unwrap_or("event")
                .to_string()
        }
    }
}

fn edit_summary(commands: &[EditCommand]) -> String {
    match commands {
        [] => "edit".to_string(),
        [only] => match only {
            EditCommand::InsertString(text) => {
                let shown: String = text.chars().take(24).collect();
                if text.chars().count() > 24 {
                    format!("insert text \"{shown}…\"")
                } else {
                    format!("insert text \"{shown}\"")
                }
            }
            EditCommand::InsertChar(c) => format!("insert '{c}'"),
            other => format!("edit: {}", edit_command_name(other)),
        },
        [first, rest @ ..] => {
            format!("edit: {} +{} more", edit_command_name(first), rest.len())
        }
    }
}

fn edit_command_name(command: &EditCommand) -> String {
    let debug = format!("{command:?}");
    debug
        .split([' ', '{', '('])
        .next()
        .unwrap_or("edit")
        .to_string()
}

/// One keymap's provenance stacks: for each `(modifiers, key code)`
/// slot, the layers that wrote it, in build order (the last write is
/// the effective one). Keyed by the pair because reedline 0.52 does
/// not export its combination type name.
pub type KeymapStacks = Vec<((KeyModifiers, KeyCode), Vec<(KeymapSource, ReedlineEvent)>)>;

/// Turn one keymap's provenance stacks into registry entries: the
/// winner of each slot is the last layer that wrote it; `shadowed`
/// names the highest lower layer that had it bound differently.
pub fn entries_from_stacks(
    mode: KeymapMode,
    stacks: KeymapStacks,
    engine_bindings: &[crate::bind_surface::EngineBindEntry],
) -> Vec<KeymapEntry> {
    let mut entries: Vec<KeymapEntry> = stacks
        .into_iter()
        .filter_map(|((modifiers, code), stack)| {
            let (source, event) = stack.last()?.clone();
            let shadowed = stack[..stack.len() - 1]
                .iter()
                .rev()
                .find(|(_, lower)| *lower != event)
                .map(|(lower_source, _)| lower_source.clone());
            Some(KeymapEntry {
                mode,
                key_display: key_display(modifiers, code),
                action: action_summary(&event, engine_bindings),
                source,
                shadowed,
            })
        })
        .collect();
    entries.sort_by(|a, b| a.key_display.cmp(&b.key_display));
    entries
}

/// The startup reminder lines for the *new* dropped records of one
/// build (DESIGN §8.3): a one-line summary when new true drops exist,
/// then a per-item block (which + why + exact fixes + topic pointer)
/// for each new non-structural record. Structural classes ride the
/// summary count only — they are listed in full, with fixes, in the
/// help topic and doctor.
pub fn startup_notices(new_dropped: &[DroppedBind]) -> Vec<String> {
    let mut lines = Vec::new();
    let true_drops = new_dropped
        .iter()
        .filter(|bind| bind.reason != DroppedReason::ScriptMacro)
        .count();
    if true_drops > 0 {
        lines.push(format!(
            "niubash: {true_drops} key binding{} from startup files were not applied \
             — fixes: {}",
            if true_drops == 1 { "" } else { "s" },
            keymap_help_command()
        ));
    }
    for bind in new_dropped
        .iter()
        .filter(|bind| !bind.reason.is_structural())
    {
        let target = match (&bind.kind, &bind.target_name) {
            (DroppedKind::Function, Some(name)) => format!(" ({name})"),
            (DroppedKind::Execute, Some(command)) => format!(" ({command})"),
            (DroppedKind::Macro, _) => " (macro)".to_string(),
            _ => String::new(),
        };
        // A key-script macro is *mirrored with loss*, not unapplied —
        // the wording must not claim otherwise.
        let headline = if bind.reason == DroppedReason::ScriptMacro {
            "mirrored with loss"
        } else {
            "not applied"
        };
        lines.push(format!(
            "niubash: bind: '{}'{target} {headline} — {}",
            bind.keyseq,
            bind.reason.why()
        ));
        lines.push(format!("  fix (session): {}", bind.session_fix()));
        lines.push(format!("  fix (persist): {}", bind.persist_fix()));
        lines.push(format!(
            "  full dropped list and fixes: {}",
            keymap_help_command()
        ));
    }
    lines
}

/// Where the runtime state file lives: `~/.niubash/state.toml`.
pub fn state_path() -> PathBuf {
    crate::path_utils::shell_home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".niubash")
        .join("state.toml")
}

/// Load the persisted state; a missing or corrupt file is simply
/// "no state" (it is rebuilt at the next editor build).
pub fn load_state_from(path: &std::path::Path) -> Option<KeymapState> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

/// Persist the state (best effort: state is a cache, never a reason
/// to fail a build).
pub fn save_state_to(path: &std::path::Path, state: &KeymapState) {
    let Ok(text) = toml::to_string(state) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("toml.tmp");
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

static CURRENT: Mutex<Option<KeymapRegistry>> = Mutex::new(None);

/// The registry of the most recent editor build in this process.
pub fn current_registry() -> Option<KeymapRegistry> {
    CURRENT.lock().ok().and_then(|slot| slot.clone())
}

/// Publish a freshly built registry: keep it in-process, fold the
/// persisted reminded set in, persist the new snapshot (with every
/// current record marked reminded — its problem has now been
/// surfaced), and return the reminder lines for the problems that are
/// new in this build. Records that vanish from the build vanish from
/// the state with them; if the same triple reappears later it is new
/// again and reminds again.
pub fn publish(registry: KeymapRegistry) -> Vec<String> {
    publish_at(&state_path(), registry)
}

/// [`publish`] against an explicit state path (the test seam).
pub fn publish_at(path: &std::path::Path, registry: KeymapRegistry) -> Vec<String> {
    if let Ok(mut slot) = CURRENT.lock() {
        *slot = Some(registry.clone());
    }
    let previous = load_state_from(path);
    let reminded: HashSet<(String, DroppedReason, Option<String>)> = previous
        .as_ref()
        .map(|state| {
            state
                .dropped
                .iter()
                .filter(|bind| bind.reminded)
                .map(|bind| bind.triple())
                .collect()
        })
        .unwrap_or_default();
    let mut dropped = registry.dropped.clone();
    for bind in &mut dropped {
        bind.reminded = reminded.contains(&bind.triple());
    }
    let new_dropped: Vec<DroppedBind> = dropped
        .iter()
        .filter(|bind| !bind.reminded)
        .cloned()
        .collect();
    let notices = startup_notices(&new_dropped);
    for bind in &mut dropped {
        bind.reminded = true;
    }
    let mut state = KeymapState::from(&registry);
    state.dropped = dropped;
    save_state_to(path, &state);
    notices
}

/// Render the `niu help keys` topic from a persisted snapshot
/// (DESIGN §6.2). `None` renders the "no snapshot yet" form, which
/// still carries the how-to-change guide.
pub fn render_keymap_help(state: Option<&KeymapState>) -> String {
    let mut out = String::new();
    match state {
        Some(state) => {
            out.push_str(&format!(
                "Key bindings — effective table (editor mode {}, registry generation {}, \
                 from the last interactive session)\n\n",
                state.editor_mode, state.registry_generation
            ));
            for mode in [
                KeymapMode::Emacs,
                KeymapMode::ViInsert,
                KeymapMode::ViNormal,
            ] {
                let rows: Vec<&KeymapEntry> =
                    state.entries.iter().filter(|e| e.mode == mode).collect();
                if rows.is_empty() {
                    continue;
                }
                out.push_str(mode.label());
                out.push('\n');
                let width = rows
                    .iter()
                    .map(|e| e.key_display.chars().count())
                    .max()
                    .unwrap_or(0);
                for entry in rows {
                    let shadow = match &entry.shadowed {
                        Some(lower) => format!(" (shadows {})", lower.label()),
                        None => String::new(),
                    };
                    out.push_str(&format!(
                        "  {:<width$}  {}  [{}]{}\n",
                        entry.key_display,
                        entry.action,
                        entry.source.label(),
                        shadow,
                        width = width + 2
                    ));
                }
                out.push('\n');
            }
            let true_drops: Vec<&DroppedBind> = state
                .dropped
                .iter()
                .filter(|b| b.reason != DroppedReason::ScriptMacro)
                .collect();
            if !true_drops.is_empty() {
                out.push_str(&format!("Not applied ({})\n", true_drops.len()));
                for bind in &true_drops {
                    out.push_str(&dropped_listing_line(bind));
                }
                out.push('\n');
            }
            let lossy: Vec<&DroppedBind> = state
                .dropped
                .iter()
                .filter(|b| b.reason == DroppedReason::ScriptMacro)
                .collect();
            if !lossy.is_empty() {
                out.push_str(&format!("Mirrored with loss ({})\n", lossy.len()));
                for bind in &lossy {
                    out.push_str(&dropped_listing_line(bind));
                }
                out.push('\n');
            }
        }
        None => {
            out.push_str(
                "Key bindings — no registry snapshot yet\n\n\
                 No interactive session has built a keymap on this machine yet, \
                 so there is no effective table to show. Start niu interactively \
                 once, then re-run this topic.\n\n",
            );
        }
    }
    out.push_str(&how_to_change_block());
    out
}

/// One dropped record as a listing block (keyseq + target + why +
/// both fix forms), shared by the help topic.
fn dropped_listing_line(bind: &DroppedBind) -> String {
    let target = bind.target_name.as_deref().unwrap_or("");
    format!(
        "  \"{}\"  {}   {}\n            fix (session):  {}\n            fix (persist):  {}\n",
        bind.keyseq,
        target,
        bind.reason.why(),
        bind.session_fix(),
        bind.persist_fix()
    )
}

/// The "How to change bindings" guide block. One definition: the
/// help topic prints it and the doctor's key face points at the
/// topic that carries it.
pub fn how_to_change_block() -> String {
    let (style, source) = crate::config::resolve_completion_style();
    let style_origin = match source {
        crate::config::SettingSource::Env => "NIU_COMPLETION_STYLE",
        crate::config::SettingSource::Inputrc => "inputrc",
        crate::config::SettingSource::Default => "default",
    };
    // The interaction axis rides beside the style axis (niubash#631,
    // design §7.5): the guide names both, each with its own origin and
    // its own change command.
    let (interaction, interaction_source) = crate::config::resolve_completion_interaction();
    let interaction_origin = match interaction_source {
        crate::config::SettingSource::Env => "NIU_COMPLETION_INTERACTION",
        crate::config::SettingSource::Inputrc => "inputrc",
        crate::config::SettingSource::Default => "default",
    };
    format!(
        "How to change bindings\n  \
         session only : bind '\"\\C-x\": <function>'  ·  bind -x '\"\\C-x\": <command>'\n  \
         persistent   : put the same bind line in ~/.niubashrc (or ~/.inputrc)\n                 \
         remove a key  : bind -r '\"\\C-t\"' in ~/.niubashrc (or ~/.inputrc)\n                 \
         shell-function widgets can also ride NIU_BINDKEYS in ~/.niubashrc\n  \
         completion   : completion.style = {} ({}) — set NIU_COMPLETION_STYLE in the environment or ~/.niubashrc\n                 \
         interaction  : completion.interaction = {} ({}) — set NIU_COMPLETION_INTERACTION in the environment or ~/.niubashrc\n",
        style.as_str(),
        style_origin,
        interaction.as_str(),
        interaction_origin
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bind_surface::{EngineBindEntry as BindEntry, EngineBindKind as BindKind};

    fn engine_entry(keyseq: &str, kind: BindKind) -> BindEntry {
        BindEntry {
            keyseq: keyseq.to_string(),
            kind,
            keymap: None,
        }
    }

    #[test]
    fn issue630_response_sequence_shape() {
        assert!(is_response_sequence(r"\e[0n"));
        assert!(is_response_sequence(r"\e[24;1R"));
        assert!(!is_response_sequence(r"\e[1;5A"));
        assert!(!is_response_sequence(r"\e[A"));
        assert!(!is_response_sequence(r"\C-t"));
        assert!(!is_response_sequence("x"));
    }

    #[test]
    fn issue630_script_macro_shape() {
        // fzf's Alt-C macro (DESIGN §3 #7): control escapes + backquotes.
        assert!(is_script_macro_text(r" \C-b\C-k \C-u`__fzf_cd__`\e\C-e"));
        assert!(is_script_macro_text("echo $(date)"));
        // The stored form: the engine translates `\C-` escapes to raw
        // control bytes before the host ever sees the macro.
        assert!(is_script_macro_text(" \u{2}\u{b} __fzf_cd__"));
        assert!(!is_script_macro_text("ins-macro"));
        assert!(!is_script_macro_text("tab\tand newline\nstay literal"));
        assert!(!is_script_macro_text(r#"literal \\ and \" text"#));
    }

    #[test]
    fn issue630_key_display_forms() {
        assert_eq!(
            key_display(KeyModifiers::CONTROL, KeyCode::Char('t')),
            "\"\\C-t\""
        );
        assert_eq!(
            key_display(KeyModifiers::ALT, KeyCode::Char('c')),
            "\"\\M-c\""
        );
        assert_eq!(key_display(KeyModifiers::NONE, KeyCode::Char('x')), "\"x\"");
        assert_eq!(key_display(KeyModifiers::NONE, KeyCode::Tab), "<Tab>");
        assert_eq!(
            key_display(KeyModifiers::SHIFT, KeyCode::BackTab),
            "<Shift-Tab>"
        );
        assert_eq!(key_display(KeyModifiers::CONTROL, KeyCode::Up), "<Ctrl-Up>");
    }

    #[test]
    fn issue630_action_summary_resolves_bindx() {
        let engine = vec![engine_entry(
            r"\C-t",
            BindKind::Execute {
                command: "fzf-file-widget".to_string(),
            },
        )];
        let event = ReedlineEvent::ExecuteHostCommand("__niu_bindx 0".to_string());
        assert_eq!(
            action_summary(&event, &engine),
            "host command: fzf-file-widget"
        );
        let widget = ReedlineEvent::ExecuteHostCommand("__niu_widget my_fn".to_string());
        assert_eq!(action_summary(&widget, &engine), "widget: my_fn");
        let insert = ReedlineEvent::Edit(vec![EditCommand::InsertString("abc".to_string())]);
        assert_eq!(action_summary(&insert, &engine), "insert text \"abc\"");
    }

    #[test]
    fn issue630_entries_winner_and_shadowed() {
        let combo = (KeyModifiers::CONTROL, KeyCode::Char('g'));
        let stacks = vec![(
            combo,
            vec![
                (
                    KeymapSource::Script,
                    ReedlineEvent::ExecuteHostCommand("__niu_bindx 0".to_string()),
                ),
                (
                    KeymapSource::User {
                        via: UserBindingVia::NiuBindkeys,
                    },
                    ReedlineEvent::Repaint,
                ),
            ],
        )];
        let entries = entries_from_stacks(KeymapMode::Emacs, stacks, &[]);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].source,
            KeymapSource::User {
                via: UserBindingVia::NiuBindkeys
            }
        );
        assert_eq!(entries[0].shadowed, Some(KeymapSource::Script));
        assert_eq!(entries[0].key_display, "\"\\C-g\"");
    }

    fn dropped(reason: DroppedReason, keyseq: &str, target: &str) -> DroppedBind {
        DroppedBind {
            keyseq: keyseq.to_string(),
            kind: DroppedKind::Function,
            target_name: Some(target.to_string()),
            reason,
            reminded: false,
        }
    }

    #[test]
    fn issue630_notices_structural_only_summary() {
        let new = vec![
            dropped(
                DroppedReason::ChainedSequence,
                r"\C-\e(",
                "redraw-current-line",
            ),
            dropped(
                DroppedReason::ModeToggleFunction,
                r"\C-z",
                "vi-editing-mode",
            ),
        ];
        let lines = startup_notices(&new);
        assert_eq!(lines.len(), 1, "summary only: {lines:?}");
        assert!(lines[0].contains("2 key bindings"));
        assert!(lines[0].contains(&keymap_help_command()));
    }

    #[test]
    fn issue630_notices_actionable_get_fixes() {
        let new = vec![dropped(
            DroppedReason::UnknownFunction,
            r"\C-g",
            "some-unknown-fn",
        )];
        let lines = startup_notices(&new);
        assert!(lines.len() >= 4, "{lines:?}");
        assert!(lines[1].contains("not applied"));
        assert!(lines[2].contains("fix (session): bind -x"));
        assert!(lines[3].contains("~/.niubashrc"));
        // The topic pointer is built from the single constant.
        assert!(lines[4].contains(&format!("niu help {KEYMAP_HELP_TOPIC}")));
    }

    #[test]
    fn issue630_notices_script_macro_has_no_summary() {
        let mut bind = dropped(DroppedReason::ScriptMacro, r"\ec", "macro text");
        bind.kind = DroppedKind::Macro;
        let lines = startup_notices(&[bind]);
        assert!(
            lines.iter().all(|line| !line.contains("were not applied")),
            "script macros are mirrored (lossily), not unapplied: {lines:?}"
        );
        assert!(lines.iter().any(|line| line.contains("key-script macro")));
        assert!(
            lines.iter().any(|line| line.contains("mirrored with loss")),
            "the headline says what actually happened: {lines:?}"
        );
    }

    #[test]
    fn issue630_publish_dedups_and_persists() {
        let dir =
            std::env::temp_dir().join(format!("niu-keymap-state-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.toml");
        let registry = KeymapRegistry {
            generation: 3,
            editor_mode: "emacs".to_string(),
            entries: vec![],
            dropped: vec![dropped(
                DroppedReason::UnknownFunction,
                r"\C-g",
                "some-unknown-fn",
            )],
        };
        let first = publish_at(&path, registry.clone());
        assert!(!first.is_empty(), "first build surfaces the problem");
        let second = publish_at(&path, registry.clone());
        assert!(
            second.is_empty(),
            "same problem never nags twice: {second:?}"
        );
        let state = load_state_from(&path).expect("state persisted");
        assert_eq!(state.registry_generation, 3);
        assert!(state.dropped[0].reminded);
        // The problem disappears, then reappears with a different
        // target: a new problem, reminded again.
        let mut changed = registry.clone();
        changed.dropped = vec![dropped(
            DroppedReason::UnknownFunction,
            r"\C-g",
            "another-fn",
        )];
        let third = publish_at(&path, changed);
        assert!(!third.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn issue630_state_toml_roundtrip() {
        // The persisted snapshot must survive TOML with every source
        // shape present — including a bundle row whose file/line were
        // never recorded (None fields inside an enum variant).
        let state = KeymapState {
            registry_generation: 7,
            editor_mode: "vi".to_string(),
            entries: vec![
                KeymapEntry {
                    mode: KeymapMode::ViInsert,
                    key_display: "\"x\"".to_string(),
                    action: "edit: InsertChar".to_string(),
                    source: KeymapSource::Bundle {
                        file: None,
                        line: None,
                    },
                    shadowed: None,
                },
                KeymapEntry {
                    mode: KeymapMode::Emacs,
                    key_display: "\"\\C-g\"".to_string(),
                    action: "accept line".to_string(),
                    source: KeymapSource::User {
                        via: UserBindingVia::NiuBindkeys,
                    },
                    shadowed: Some(KeymapSource::Script),
                },
            ],
            dropped: vec![dropped(
                DroppedReason::ChainedSequence,
                r"\C-x\C-f",
                "find-file",
            )],
        };
        let text = toml::to_string(&state).expect("state serializes to TOML");
        let back: KeymapState = toml::from_str(&text).expect("state parses back");
        assert_eq!(back, state);
    }

    #[test]
    fn issue630_topic_constant_single_source() {
        // The three faces — topics section, help rendering, reminder
        // lines — all carry the command built from the one constant.
        let command = keymap_help_command();
        assert!(help_topics_section().contains(&command));
        assert!(render_keymap_help(None).contains("How to change bindings"));
        let state = KeymapState {
            registry_generation: 1,
            editor_mode: "emacs".to_string(),
            entries: vec![],
            dropped: vec![dropped(
                DroppedReason::ChainedSequence,
                r"\C-x\C-f",
                "find-file",
            )],
        };
        let rendered = render_keymap_help(Some(&state));
        assert!(rendered.contains("Not applied (1)"));
        assert!(rendered.contains("chained key sequence"));
    }

    #[test]
    fn issue631_how_to_change_block_shows_both_completion_axes() {
        let block = how_to_change_block();
        assert!(block.contains("completion.style"), "{block}");
        assert!(block.contains("completion.interaction"), "{block}");
        assert!(block.contains("NIU_COMPLETION_INTERACTION"), "{block}");
        assert!(block.contains("~/.inputrc"), "{block}");
    }
}
