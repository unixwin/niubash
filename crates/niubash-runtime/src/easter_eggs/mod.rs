//! Easter eggs for niubash: small terminal toys and one-shot fun.
//!
//! Dispatch is deliberately conservative. Eggs are only routed from an
//! interactive REPL, so `niu -c`, script files, and piped stdin stay quiet,
//! deterministic, and free of escape sequences (see the non-interactive
//! contract in AGENTS.md). Anything that reaches an animation is guaranteed
//! an interactive host; each game also checks stdout is a real terminal
//! before touching the screen.

mod about;
mod cow;
mod matrix;
mod party;
mod snake;
mod tic;
mod typing;

/// Every registered egg name. Keep sorted; used by docs and tests.
const EGG_NAMES: &[&str] = &[
    "about",
    "cow",
    "game",
    "games",
    "matrix",
    "party",
    "tic",
    "typing",
];

const DIM: &str = "\x1b[90m";
const RESET: &str = "\x1b[0m";

/// Games advertised by the `game` launcher: (name, invocation, blurb).
pub(crate) const GAMES: &[(&str, &str, &str)] = &[
    ("snake", "game snake", "eat the apples, do not eat yourself"),
    ("cow", "game cow", "the bull, animated"),
    ("typing", "game typing", "measure your words per minute"),
    ("tic", "game tic", "tic-tac-toe against the machine"),
];

/// Every registered egg name.
#[allow(dead_code)]
pub(crate) fn names() -> &'static [&'static str] {
    EGG_NAMES
}

/// True when `command` is a registered egg name. Used to decide routing.
pub(crate) fn is_registered(command: &str) -> bool {
    EGG_NAMES.contains(&command.to_ascii_lowercase().as_str())
}

fn is_flag(word: &str) -> bool {
    word.starts_with("-")
}

fn stdout() -> std::io::Stdout {
    std::io::stdout()
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
        "matrix" => Ok(Some(matrix::run()?)),
        "party" => Ok(Some(party::run()?)),
        "about" => Ok(Some(about::run()?)),
        "cow" => Ok(Some(cow::run()?)),
        "typing" => Ok(Some(typing::run()?)),
        "tic" => Ok(Some(tic::run()?)),
        "game" | "games" => match argv.get(1) {
            Some(selection) if !is_flag(selection) => {
                let wanted = selection.to_ascii_lowercase();
                match wanted.as_str() {
                    "snake" => snake::run().map(Some),
                    "cow" => cow::run().map(Some),
                    "typing" | "t" | "wpm" => typing::run().map(Some),
                    "tic" | "ttt" | "tictactoe" => tic::run().map(Some),
                    other => print_usage_for_unknown(&name, other).map(Some),
                }
            }
            _ => list_games().map(Some),
        },
        _ => unreachable!("registered egg name is unhandled: {name}"),
    }
}

fn list_games() -> anyhow::Result<i32> {
    use std::io::Write;
    let mut out = stdout();
    write!(out, "\x1b[1;96m  niubash games\x1b[0m\n")?;
    write!(out, "  {DIM}type `game` for this list, `game <name>` to play{RESET}\n\n")?;
    for (_name, invocation, blurb) in GAMES {
        write!(out, "  \x1b[1;96m{invocation:<14}\x1b[0m {DIM}{blurb}{RESET}\n")?;
    }
    write!(out, "\n  {DIM}each one also works as a top-level command: `snake`, `cow`, `typing`, `tic`{RESET}\n")?;
    write!(out, "  {DIM}also hidden: `matrix`, `party`, `about`{RESET}\n")?;
    out.flush()?;
    Ok(0)
}

fn print_usage_for_unknown(_launcher: &str, wanted: &str) -> anyhow::Result<i32> {
    use std::io::Write;
    let mut out = stdout();
    write!(out, "{DIM}no such game: {wanted}\n{RESET}")?;
    out.flush()?;
    list_games()
}

fn print_usage(name: &str) -> anyhow::Result<i32> {
    use std::io::Write;
    let mut out = stdout();
    match name {
        "game" | "games" => {
            write!(out, "\x1b[1;96m  niubash games\x1b[0m\n")?;
            write!(out, "  {DIM}usage: game [<name>] | game --help{RESET}\n")?;
            write!(out, "  {DIM}names: {}", GAMES.iter().map(|g| g.0).collect::<Vec<_>>().join(", "))?;
            write!(out, "{RESET}\n")?;
            out.flush()?;
            Ok(0)
        }
        "cow" => {
            write!(out, "  {DIM}cow -- a short ASCII bull animation; q or ctrl+c leaves early{RESET}\n")?;
            out.flush()?;
            Ok(0)
        }
        "typing" => {
            write!(out, "  {DIM}typing -- type the shown words; ctrl+r restarts, q quits{RESET}\n")?;
            out.flush()?;
            Ok(0)
        }
        "tic" => {
            write!(out, "  {DIM}tic -- tic-tac-toe; press 1-9 to place, n for another round, q quits{RESET}\n")?;
            out.flush()?;
            Ok(0)
        }
        _ => {
            write!(out, "  {DIM}{name}: an interactive niubash easter egg{RESET}\n")?;
            out.flush()?;
            Ok(0)
        }
    }
}
