//! niubash-runtime: Windows bash-compatible shell runtime
//!
//! Built on top of rubash (shell language engine) and winuxcmd (coreutils).
//! This crate provides the interactive shell experience: reedline REPL,
//! completion system, theming, configuration, and Windows integration.

/// #125/#140: std `println!`/`print!` panic on any stdout write error, and
/// the niu binary builds with `panic = "abort"`, so a reader closing the
/// pipe (Windows reports os error 232 = ERROR_NO_DATA rather than EPIPE)
/// aborts the whole process mid-output. Shadow both macros crate-wide with
/// writers that follow the engine's closed-pipe rule (`BrokenPipe` or raw
/// os error 232): write what fits, exit 0 quietly — the SIGPIPE death GNU
/// exhibits when its stdout reader goes away. stderr writes get the same
/// treatment minus the exit; a dead stderr must not abort either.
pub(crate) fn write_stdout_lossy(text: &str) {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => {}
        Err(error)
            if error.kind() == std::io::ErrorKind::BrokenPipe
                || error.raw_os_error() == Some(232) =>
        {
            std::process::exit(0)
        }
        Err(_) => {}
    }
}

pub(crate) fn write_stderr_lossy(text: &str) {
    use std::io::Write;
    let mut stderr = std::io::stderr().lock();
    let _ = stderr
        .write_all(text.as_bytes())
        .and_then(|()| stderr.flush());
}

macro_rules! print {
    ($($arg:tt)*) => {
        crate::write_stdout_lossy(&format!($($arg)*))
    };
}
macro_rules! println {
    () => {
        crate::write_stdout_lossy("\n")
    };
    ($($arg:tt)*) => {
        crate::write_stdout_lossy(&format!("{}\n", format_args!($($arg)*)))
    };
}
macro_rules! eprintln {
    () => {
        crate::write_stderr_lossy("\n")
    };
    ($($arg:tt)*) => {
        crate::write_stderr_lossy(&format!("{}\n", format_args!($($arg)*)))
    };
}

pub mod autosuggest;
pub mod completion;
pub mod config;
pub mod console_guard;
pub mod ctrl_c;
pub mod doctor;
pub(crate) mod easter_eggs;
pub mod fonts;
pub(crate) mod history;
pub mod interactive_menu;
pub mod logo;
pub mod panic_restore;
pub(crate) mod path_utils;
pub mod plugins;
pub mod prompt;
pub mod prompt_right_align;
pub mod prompt_segments;
pub mod repl;
pub mod setup_wizard;
pub mod shell;
pub mod skill;
pub mod startup_trace;
pub mod syntax_highlighting;
pub mod terminal;
pub mod text_style;
pub mod typeahead_guard;
#[cfg(windows)]
pub mod windows_terminal;
pub mod winuxcmd;

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex;

    pub(crate) static PROCESS_STATE_LOCK: Mutex<()> = Mutex::new(());
}

pub use completion::{CompletionBehavior, CompletionMatchMode, CompletionState, NiubashCompleter};
pub use config::{
    AutosuggestConfig, CommandNotFoundHint, EditorConfig, EditorMode, HistoryConfig, MenuConfig,
    ShellConfig, SyntaxHighlightConfig,
};
pub use prompt::PromptBackend;
pub use prompt::PromptIndicators;
pub use prompt_segments::{
    SegmentId, SegmentPreset, SegmentPrompt, SegmentPromptAdapter, SegmentPromptConfig,
};
pub use shell::Shell;
