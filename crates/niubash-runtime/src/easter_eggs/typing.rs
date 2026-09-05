use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{self, ClearType},
};

const TARGET_WORDS: usize = 12;

// Small, common vocabulary so a round finishes in about twenty seconds
// of typing instead of hunting for obscure words.
const WORDS: &[&str] = &[
    "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog",
    "shell", "prompt", "pipe", "alias", "script", "command", "output", "input",
    "window", "native", "path", "drive", "folder", "line", "token", "parser",
    "engine", "builtin", "function", "expand", "quote", "escape", "history", "theme",
    "plugin", "bundle", "manifest", "cache", "search", "match", "status", "branch",
    "commit", "merge", "build", "cargo", "crate", "error", "debug", "release",
];

// Deterministic 64-bit generator so a round is reproducible from its seed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407))
    }

    fn next_index(&mut self) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as usize % WORDS.len()
    }
}

struct Game {
    target: Vec<char>,
    typed: Vec<char>,
    started: Option<Instant>,
    done: bool,
}

impl Game {
    fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let mut target = String::new();
        for index in 0..TARGET_WORDS {
            if index > 0 {
                target.push(' ');
            }
            target.push_str(WORDS[rng.next_index()]);
        }
        Game {
            target: target.chars().collect(),
            typed: Vec::new(),
            started: None,
            done: false,
        }
    }

    fn elapsed(&self) -> Duration {
        self.started
            .map(|start| Instant::now().saturating_duration_since(start))
            .unwrap_or(Duration::ZERO)
    }

    fn correct_chars(&self) -> usize {
        self.typed
            .iter()
            .zip(self.target.iter())
            .filter(|(typed, target)| typed == target)
            .count()
    }

    fn wpm(&self) -> u32 {
        let minutes = self.elapsed().as_secs_f64() / 60.0;
        if minutes < 0.001 {
            return 0;
        }
        (self.correct_chars() as f64 / 5.0 / minutes).round() as u32
    }

    fn accuracy(&self) -> u32 {
        if self.typed.is_empty() {
            return 100;
        }
        let pct = self.correct_chars() as f64 * 100.0 / self.typed.len() as f64;
        pct.round() as u32
    }
}

/// `typing`: a short words-per-minute test. `ctrl+r` starts a fresh round,
/// `q` quits. Refuses to draw when stdout is not a terminal so piped output
/// stays clean.
pub(crate) fn run() -> anyhow::Result<i32> {
    if !crate::terminal::stdout_is_terminal() {
        return Ok(0);
    }
    let mut stdout = io::stdout();
    terminal::enable_raw_mode()?;
    execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;
    let code = play(&mut stdout);
    execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    code
}

fn seed() -> u64 {
    let nanos: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    nanos.wrapping_mul(2654435761)
}

fn play(stdout: &mut io::Stdout) -> anyhow::Result<i32> {
    let mut rounds = 0u64;
    loop {
        let mut game = Game::new(seed().wrapping_add(rounds.wrapping_mul(7919)));
        match round(stdout, &mut game)? {
            RoundResult::Again => rounds += 1,
            RoundResult::Quit => break,
        }
    }
    Ok(0)
}

enum RoundResult {
    Again,
    Quit,
}

fn round(stdout: &mut io::Stdout, game: &mut Game) -> anyhow::Result<RoundResult> {
    loop {
        draw(stdout, game)?;
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let event = event::read()?;
        if let Event::Key(KeyEvent { code, modifiers, .. }) = event {
            match (code, modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Char('Q'), _) => return Ok(RoundResult::Quit),
            (KeyCode::Char('r'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                return Ok(RoundResult::Again);
            }
            (KeyCode::Enter, _) | (KeyCode::Char('n'), _) if game.done => {
                return Ok(RoundResult::Again);
            }
            (KeyCode::Char(c), _) if !game.done => {
                game.started.get_or_insert(Instant::now());
                game.typed.push(c);
                if game.typed.len() >= game.target.len() {
                    game.done = true;
                }
            }
            (KeyCode::Backspace, _) if !game.done && !game.typed.is_empty() => {
                game.typed.pop();
            }
            _ => {}
            }
        }
    }
}

fn draw(stdout: &mut io::Stdout, game: &Game) -> anyhow::Result<()> {
    execute!(stdout, terminal::Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    let secs = game.elapsed().as_secs_f64();
    write!(stdout, "  \x1b[1;96m~ niubash typing ~\x1b[0m   \x1b[90m{secs:.1}s elapsed\x1b[0m\n\n")?;

    // Target line: typed-correct bright, typed-wrong red, untyped dim,
    // and a block over the character being typed.
    write!(stdout, "  ")?;
    let cursor_at = game.typed.len();
    for (index, ch) in game.target.iter().enumerate() {
        if index < game.typed.len() {
            if game.typed[index] == *ch {
                write!(stdout, "\x1b[1;97m{ch}")?;
            } else {
                write!(stdout, "\x1b[41;97m{ch}\x1b[0m")?;
            }
        } else if index == cursor_at && !game.done {
            write!(stdout, "\x1b[48;5;238;38;5;255m{ch}")?;
        } else {
            write!(stdout, "\x1b[90m{ch}")?;
        }
    }
    write!(stdout, "\x1b[0m\n\n")?;

    write!(stdout, "  \x1b[90mwords per minute  \x1b[0m\x1b[1;96m{:>4}\x1b[0m\n", game.wpm())?;
    write!(stdout, "  \x1b[90mcharacters correct\x1b[0m \x1b[1;96m{:>4}%\x1b[0m\n", game.accuracy())?;
    write!(stdout, "  \x1b[90mcharacters typed  \x1b[0m\x1b[1;96m{}/{}\x1b[0m\n", game.typed.len(), game.target.len())?;
    write!(stdout, "\n")?;
    if game.done {
        write!(stdout, "  \x1b[1;92mdone! enter or n for another round\x1b[0m\n")?;
    } else {
        write!(stdout, "  \x1b[90mbackspace undoes, ctrl+r restarts, q quits\x1b[0m\n")?;
    }
    stdout.flush()?;
    Ok(())
}
