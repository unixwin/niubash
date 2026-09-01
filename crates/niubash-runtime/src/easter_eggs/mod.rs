//! Easter eggs for niubash

mod about;
mod matrix;
mod party;
mod snake;

pub(crate) fn dispatch(command: &str) -> anyhow::Result<Option<i32>> {
    match command.to_lowercase().as_str() {
        "matrix" => Ok(Some(matrix::run()?)),
        "party" => Ok(Some(party::run()?)),
        "about" => Ok(Some(about::run()?)),
        "game" => Ok(Some(snake::run()?)),
        _ => Ok(None),
    }
}
