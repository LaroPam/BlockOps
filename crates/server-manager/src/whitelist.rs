//! Build the small set of supported vanilla whitelist commands.
use anyhow::{ensure, Result};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Add,
    Remove,
    List,
}

pub fn command(action: Action, player: Option<&str>) -> Result<String> {
    match action {
        Action::List => {
            ensure!(player.is_none(), "Список не принимает имя игрока");
            Ok("whitelist list".into())
        }
        Action::Add | Action::Remove => {
            let name = player.unwrap_or("");
            ensure!((1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'), "Имя игрока должно содержать от 1 до 16 латинских букв, цифр или знаков подчёркивания");
            let verb = match action {
                Action::Add => "add",
                Action::Remove => "remove",
                Action::List => unreachable!(),
            };
            Ok(format!("whitelist {verb} {name}"))
        }
    }
}
