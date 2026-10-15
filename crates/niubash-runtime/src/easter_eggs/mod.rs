//! Easter eggs for niubash: one terminal game (`snake`) and the `about`
//! information screen.
//!
//! Dispatch is deliberately conservative. Eggs are only routed from an
//! interactive REPL, so `niu -c`, script files, and piped stdin stay quiet,
//! deterministic, and free of escape sequences (see the non-interactive
//! contract in AGENTS.md). Anything that reaches the game is guaranteed an
//! interactive host; snake also checks stdout is a real terminal before
//! touching the screen.

mod about;
mod snake;

/// Every registered egg name. Keep sorted.
const EGG_NAMES: &[&str] = &["about", "snake"];

const DIM: &str = "\x1b[90m";
const RESET: &str = "\x1b[0m";

/// True when `command` is a registered egg name. Used to decide routing.
pub(crate) fn is_registered(command: &str) -> bool {
    EGG_NAMES.contains(&command.to_ascii_lowercase().as_str())
}

fn is_flag(word: &str) -> bool {
    word.starts_with("-")
}

/// Run one egg.
///
/// `argv` is the full word list including the egg name at `argv[0]`.
/// Returns `Ok(None)` when the host is not an interactive session; the
/// caller must then fall through to ordinary command resolution, which
/// reports the name as not found.
pub(crate) fn dispatch(interactive: bool, argv: &[String]) -> anyhow::Result<Option<i32>> {
    let Some(first) = argv.first() else {
        return Ok(None);
    };
    let name = first.to_ascii_lowercase();
    if !EGG_NAMES.contains(&name.as_str()) {
        return Ok(None);
    }
    if !interactive {
        return Ok(None);
    }
    if argv.get(1).is_some_and(|word| is_flag(word)) {
        return Ok(Some(print_usage(&name)?));
    }

    match name.as_str() {
        "about" => Ok(Some(about::run()?)),
        "snake" => Ok(Some(snake::run()?)),
        _ => unreachable!("registered egg name is unhandled: {name}"),
    }
}

fn print_usage(name: &str) -> anyhow::Result<i32> {
    use std::io::Write;
    let mut out = std::io::stdout();
    write!(
        out,
        "  {DIM}{name}: an interactive niubash easter egg{RESET}\n"
    )?;
    out.flush()?;
    Ok(0)
}
