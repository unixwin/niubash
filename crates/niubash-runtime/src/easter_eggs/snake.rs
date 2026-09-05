use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal,
};

const TICK_MS: u64 = 150;
const WIDTH: usize = 20;
const HEIGHT: usize = 15;

#[derive(Clone, Copy, PartialEq)]
struct Pos {
    x: usize,
    y: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

struct Game {
    snake: Vec<Pos>,
    dir: Dir,
    food: Pos,
    score: u32,
    over: bool,
}

impl Game {
    fn new() -> Self {
        let mid_x = WIDTH / 2;
        let mid_y = HEIGHT / 2;
        let snake = vec![
            Pos { x: mid_x, y: mid_y },
            Pos {
                x: mid_x - 1,
                y: mid_y,
            },
            Pos {
                x: mid_x - 2,
                y: mid_y,
            },
        ];
        let food = Self::random_pos(&snake);
        Game {
            snake,
            dir: Dir::Right,
            food,
            score: 0,
            over: false,
        }
    }

    fn random_pos(occupied: &[Pos]) -> Pos {
        let mut candidates = Vec::new();
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let p = Pos { x, y };
                if !occupied.contains(&p) {
                    candidates.push(p);
                }
            }
        }
        if candidates.is_empty() {
            return Pos { x: 0, y: 0 };
        }
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as usize;
        candidates[nanos % candidates.len()]
    }

    fn tick(&mut self) {
        if self.over {
            return;
        }

        let head = self.snake[0];
        // All four edges wrap, so the playfield is a torus in every direction.
        // (Down and Right already wrapped; Up and Left used `wrapping_sub`,
        // which sent the head to `usize::MAX` and made the snake vanish.)
        let new_head = match self.dir {
            Dir::Up => Pos {
                x: head.x,
                y: (head.y + HEIGHT - 1) % HEIGHT,
            },
            Dir::Down => Pos {
                x: head.x,
                y: (head.y + 1) % HEIGHT,
            },
            Dir::Left => Pos {
                x: (head.x + WIDTH - 1) % WIDTH,
                y: head.y,
            },
            Dir::Right => Pos {
                x: (head.x + 1) % WIDTH,
                y: head.y,
            },
        };

        if self.snake.contains(&new_head) {
            self.over = true;
            return;
        }

        self.snake.insert(0, new_head);

        if new_head == self.food {
            self.score += 1;
            self.food = Self::random_pos(&self.snake);
        } else {
            self.snake.pop();
        }
    }

    fn draw(&self, stdout: &mut io::Stdout) -> anyhow::Result<()> {
        execute!(stdout, cursor::MoveTo(0, 0))?;

        // Title
        write!(
            stdout,
            "\x1b[1;96m~ niubash snake ~  \x1b[1;93mscore: {:<5}\x1b[0m\x1b[K\n\x1b[K\n",
            self.score
        )?;

        for y in 0..HEIGHT {
            execute!(stdout, cursor::MoveTo(2, (y + 2) as u16))?;
            for x in 0..WIDTH {
                let pos = Pos { x, y };
                if pos == self.snake[0] {
                    stdout.write_all(b"\x1b[1;92m@\x1b[0m")?;
                } else if self.snake.contains(&pos) {
                    stdout.write_all(b"\x1b[92mo\x1b[0m")?;
                } else if pos == self.food {
                    stdout.write_all(b"\x1b[1;91m*\x1b[0m")?;
                } else {
                    stdout.write_all(b" ")?;
                }
            }
            stdout.write_all(b"\x1b[K")?;
        }

        let status_y = (HEIGHT + 2) as u16;
        execute!(stdout, cursor::MoveTo(0, status_y))?;
        if self.over {
            stdout.write_all(b"\x1b[K\n\x1b[1;91m  GAME OVER! Press q to quit.\x1b[0m\x1b[K\n\x1b[K")?;
        } else {
            stdout.write_all(b"\x1b[K\n  \x1b[90mwasd/arrows to move, q to quit\x1b[0m\x1b[K\n\x1b[K")?;
        }

        stdout.flush()?;
        Ok(())
    }
}

pub(crate) fn run() -> anyhow::Result<i32> {
    if !crate::terminal::stdout_is_terminal() {
        return Ok(0);
    }
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, terminal::EnterAlternateScreen)?;
    execute!(stdout, cursor::Hide)?;

    let mut game = Game::new();
    let mut last_tick = Instant::now();

    let code = loop {
        game.draw(&mut stdout)?;

        // Poll input
        if event::poll(Duration::from_millis(10))? {
            if let Event::Key(KeyEvent {
                code, modifiers, ..
            }) = event::read()?
            {
                match code {
                    KeyCode::Char('q') | KeyCode::Char('Q') => break 0,
                    KeyCode::Char('w') | KeyCode::Char('W') | KeyCode::Up => {
                        if game.dir != Dir::Down {
                            game.dir = Dir::Up;
                        }
                    }
                    KeyCode::Char('s') | KeyCode::Char('S') | KeyCode::Down => {
                        if game.dir != Dir::Up {
                            game.dir = Dir::Down;
                        }
                    }
                    KeyCode::Char('a') | KeyCode::Char('A') | KeyCode::Left => {
                        if game.dir != Dir::Right {
                            game.dir = Dir::Left;
                        }
                    }
                    KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Right => {
                        if game.dir != Dir::Left {
                            game.dir = Dir::Right;
                        }
                    }
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => break 0,
                    _ => {}
                }
            }
        }

        // Tick
        if last_tick.elapsed() >= Duration::from_millis(TICK_MS) {
            game.tick();
            last_tick = Instant::now();
            if game.over {
                game.draw(&mut stdout)?;
                // Wait for q
                loop {
                    if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                        if code == KeyCode::Char('q') || code == KeyCode::Char('Q') {
                            break;
                        }
                    }
                }
                break 0;
            }
        }
    };

    execute!(stdout, cursor::Show)?;
    execute!(stdout, terminal::LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    Ok(code)
}
