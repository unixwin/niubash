use std::io::{self, Write};
use std::time::Duration;

use crossterm::{
    cursor,
    event::{self, Event, KeyCode},
    execute,
    terminal::{self, ClearType},
};

const YOU: &str = "\x1b[1;96m";
const CPU: &str = "\x1b[1;95m";
const DIM: &str = "\x1b[90m";
const RESET: &str = "\x1b[0m";

#[derive(Clone, Copy, PartialEq)]
enum Mark {
    X,
    O,
}

type Cell = Option<Mark>;

const LINES: [[usize; 3]; 8] = [
    [0, 1, 2],
    [3, 4, 5],
    [6, 7, 8],
    [0, 3, 6],
    [1, 4, 7],
    [2, 5, 8],
    [0, 4, 8],
    [2, 4, 6],
];

/// `tic`: tic-tac-toe against the machine. Press 1-9 to place a mark,
/// `n` or enter for another round, `q` to quit. Refuses to draw when stdout
/// is not a terminal so piped output stays clean.
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

fn play(stdout: &mut io::Stdout) -> anyhow::Result<i32> {
    let mut round = 1u32;
    loop {
        // Alternate who opens so neither side owns the first move.
        let mut game = Game::new();
        if round % 2 == 0 {
            game.cpu_turn();
        }
        match round_play(stdout, &mut game)? {
            RoundEnd::Again => {}
            RoundEnd::Quit => break,
        }
        round += 1;
    }
    Ok(0)
}

enum RoundEnd {
    Again,
    Quit,
}

struct Game {
    board: [Cell; 9],
    outcome: Option<Option<Mark>>,
}

impl Game {
    fn new() -> Self {
        Game {
            board: [None; 9],
            outcome: None,
        }
    }

    fn cpu_turn(&mut self) {
        if self.outcome.is_some() {
            return;
        }
        if let Some(index) = choose_cpu_move(&self.board) {
            self.board[index] = Some(Mark::O);
        }
        self.outcome = find_outcome(&self.board);
    }
}

fn find_outcome(board: &[Cell; 9]) -> Option<Option<Mark>> {
    for line in LINES {
        if let Some(mark) = board[line[0]] {
            if board[line[1]] == Some(mark) && board[line[2]] == Some(mark) {
                return Some(Some(mark));
            }
        }
    }
    if board.iter().all(|cell| cell.is_some()) {
        return Some(None);
    }
    None
}

fn winning_move(board: &[Cell; 9], mark: Mark) -> Option<usize> {
    for index in 0..9usize {
        if board[index].is_some() {
            continue;
        }
        let mut probe = *board;
        probe[index] = Some(mark);
        if find_outcome(&probe) == Some(Some(mark)) {
            return Some(index);
        }
    }
    None
}

fn choose_cpu_move(board: &[Cell; 9]) -> Option<usize> {
    // Take the win, refuse the loss, grab the centre, then a random corner.
    if let Some(index) = winning_move(board, Mark::O) {
        return Some(index);
    }
    if let Some(index) = winning_move(board, Mark::X) {
        return Some(index);
    }
    if board[4].is_none() {
        return Some(4);
    }
    let corners = [0usize, 2, 6, 8]
        .into_iter()
        .filter(|index| board[*index].is_none())
        .collect::<Vec<_>>();
    if !corners.is_empty() {
        return Some(corners[rand_index() % corners.len()]);
    }
    let open: Vec<usize> = (0..9).filter(|index| board[*index].is_none()).collect();
    if open.is_empty() {
        return None;
    }
    Some(open[rand_index() % open.len()])
}

fn rand_index() -> usize {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    nanos as usize
}

fn round_play(stdout: &mut io::Stdout, game: &mut Game) -> anyhow::Result<RoundEnd> {
    loop {
        draw(stdout, game)?;
        if !event::poll(Duration::from_millis(50))? {
            continue;
        }
        let event = event::read()?;
        if let Event::Key(key) = event {
            match key.code {
                KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(RoundEnd::Quit),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Enter => {
                    return Ok(RoundEnd::Again);
                }
                KeyCode::Char(digit)
                    if digit.is_ascii_digit() && game.outcome.is_none() =>
                {
                    let index = (digit as usize) - ('1' as usize);
                    if index >= 9 || game.board[index].is_some() {
                        continue;
                    }
                    game.board[index] = Some(Mark::X);
                    game.outcome = find_outcome(&game.board);
                    if game.outcome.is_none() {
                        game.cpu_turn();
                    }
                }
                _ => {}
            }
        }
    }
}

fn draw(stdout: &mut io::Stdout, game: &Game) -> anyhow::Result<()> {
    execute!(stdout, terminal::Clear(ClearType::All), cursor::MoveTo(0, 0))?;
    write!(stdout, "  \x1b[1;96m~ niubash tic ~\x1b[0m   {DIM}tic-tac-toe{RESET}\n\n")?;
    write!(stdout, "    {DIM}   1     2     3{RESET}\n")?;
    write!(stdout, "    \u{250c}\u{2500}\u{2500}\u{2500}\u{252c}\u{2500}\u{2500}\u{2500}\u{252c}\u{2500}\u{2500}\u{2500}\u{2510}\n")?;
    for row in 0..3usize {
        for col in 0..3usize {
            let cell = game.board[row * 3 + col];
            let glyph = match cell {
                Some(Mark::X) => format!("{YOU} X {RESET}"),
                Some(Mark::O) => format!("{CPU} O {RESET}"),
                None => format!("{DIM}. {RESET}"),
            };
            if col > 0 {
                write!(stdout, "\u{2534}\u{2500}\u{2500}\u{2500}\u{252c}")?;
            }
            write!(stdout, "\u{2502} {glyph} \u{2502}")?;
        }
        write!(stdout, "\n")?;
        if row < 2 {
            write!(stdout, "    \u{251c}\u{2500}\u{2500}\u{2500}\u{253c}\u{2500}\u{2500}\u{2500}\u{253c}\u{2500}\u{2500}\u{2500}\u{2524}\n")?;
        }
    }
    write!(stdout, "    \u{2514}\u{2500}\u{2500}\u{2500}\u{2534}\u{2500}\u{2500}\u{2500}\u{2534}\u{2500}\u{2500}\u{2500}\u{2518}\n\n")?;
    write!(stdout, "  {DIM}you{RESET} {YOU}X{RESET}   {DIM}machine{RESET} {CPU}O{RESET}\n\n")?;
    match game.outcome {
        Some(Some(Mark::X)) => {
            write!(stdout, "  \x1b[1;92myou win!{RESET} {DIM}enter or n for another round, q quits{RESET}\n")?;
        }
        Some(Some(Mark::O)) => {
            write!(stdout, "  \x1b[1;91mmachine wins{RESET} {DIM}enter or n for another round, q quits{RESET}\n")?;
        }
        Some(None) => {
            write!(stdout, "  \x1b[1;93mcat game, nobody wins{RESET} {DIM}enter or n for another round, q quits{RESET}\n")?;
        }
        None => {
            write!(stdout, "  {DIM}your move: press 1-9{RESET}\n")?;
        }
    }
    stdout.flush()?;
    Ok(())
}
