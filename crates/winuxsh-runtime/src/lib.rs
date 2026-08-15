//! winuxsh-runtime: Windows bash-compatible shell runtime
//!
//! Built on top of rubash (shell language engine) and winuxcmd (coreutils).
//! This crate provides the interactive shell experience: reedline REPL,
//! completion system, theming, configuration, and Windows integration.

pub mod autosuggest;
pub mod completion;
pub mod config;
pub mod ctrl_c;
pub mod git_status;
pub(crate) mod history;
pub(crate) mod path_utils;
pub mod plugins;
pub mod prompt;
pub mod prompt_segments;
pub mod repl;
pub mod setup_wizard;
pub mod shell;
pub mod syntax_highlighting;
pub mod terminal;
pub mod theme;
pub mod windows_terminal;
pub mod winuxcmd;

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex;

    pub(crate) static PROCESS_STATE_LOCK: Mutex<()> = Mutex::new(());
}

pub use completion::{CompletionBehavior, CompletionMatchMode, CompletionState, WinuxshCompleter};
pub use config::{
    AutosuggestConfig, EditorConfig, EditorMode, HistoryConfig, MenuConfig, PluginConfig,
    PluginPackConfig, ShellConfig, SyntaxHighlightConfig,
};
pub use prompt::PromptBackend;
pub use prompt::PromptIndicators;
pub use prompt_segments::{
    SegmentId, SegmentPreset, SegmentPrompt, SegmentPromptAdapter, SegmentPromptConfig,
};
pub use shell::Shell;
pub use theme::Theme;
