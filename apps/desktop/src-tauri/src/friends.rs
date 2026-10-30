//! Friends who come back without a new code: this computer's key, the friends a host lets
//! in, and the worlds a guest may enter again. The book lives beside the app's other data.
use crate::{AppState, Invite};
use blockops_tunnel::friends::Book;
use serde::Serialize;
use std::path::PathBuf;
use tauri::State;
use tokio::sync::Mutex;

/// The book's file, under one lock for every read and change.
pub struct Friends(pub Mutex<PathBuf>);

impl Friends {
    pub async fn read(&self) -> Result<Book, String> {
        let path = self.0.lock().await;
        Book::load_or_create(&path)
            .map_err(|e| format!("Не удалось прочитать список друзей: {e:#}"))
    }

    pub async fn change<T>(
        &self,
        change: impl FnOnce(&mut Book) -> Result<T, String>,
    ) -> Result<T, String> {
        let path = self.0.lock().await;
        let mut book = Book::load_or_create(&path)
            .map_err(|e| format!("Не удалось прочитать список друзей: {e:#}"))?;
        let result = change(&mut book)?;
        book.save(&path)
            .map_err(|e| format!("Не удалось сохранить список друзей: {e:#}"))?;
        Ok(result)
    }
}

/// What the window may see: names and ids, never keys or capabilities.
#[derive(Serialize)]
pub struct FriendsView {
    friends: Vec<FriendView>,
    worlds: Vec<WorldView>,
}
#[derive(Serialize)]
struct FriendView {
    peer: String,
    name: String,
    added: u64,
}
#[derive(Serialize)]
struct WorldView {
    host: String,
    label: String,
    added: u64,
}

fn view(book: &Book) -> FriendsView {
    FriendsView {
        friends: book
            .friends
            .iter()
            .map(|f| FriendView {
                peer: f.peer.clone(),
                name: f.name.clone(),
                added: f.added,
            })
            .collect(),
        worlds: book
            .worlds
            .iter()
            .map(|w| WorldView {
                host: w.host.clone(),
                label: w.label.clone(),
                added: w.added,
            })
            .collect(),
    }
}

#[tauri::command]
pub async fn friends(friends: State<'_, Friends>) -> Result<FriendsView, String> {
    Ok(view(&friends.read().await?))
}

/// Keep letting in, without a code, the friend who used the code `id` in this session.
#[tauri::command]
pub async fn remember_friend(
    state: State<'_, AppState>,
    friends: State<'_, Friends>,
    id: String,
) -> Result<FriendsView, String> {
    let (peer, token) = {
        let codes = state.1.lock().await;
        match codes.iter().find(|c| c.status().id == id) {
            Some(Invite::Direct(code)) => code.friend().ok_or("Друг ещё не вошёл по этому коду")?,
            Some(Invite::Coordinated(_)) => {
                return Err(
                    "Без кода могут заходить только друзья, вошедшие без своего сервера".into(),
                )
            }
            None => return Err("Приглашение не найдено".into()),
        }
    };
    // Named after the player this friend logged in as, when the game is already running.
    let name = state
        .0
        .lock()
        .await
        .as_ref()
        .and_then(|(_, session, _)| {
            let status = session.status.borrow();
            status
                .players
                .iter()
                .find(|player| player.peer == peer.to_string())
                .and_then(|player| player.name.clone())
        })
        .unwrap_or_default();
    friends
        .change(|book| {
            book.remember_friend(peer, token, &name)
                .map_err(|e| e.to_string())?;
            Ok(view(book))
        })
        .await
}

/// Stop letting a friend in without a code, ending their game now if they are in.
#[tauri::command]
pub async fn forget_friend(
    state: State<'_, AppState>,
    friends: State<'_, Friends>,
    peer: String,
) -> Result<FriendsView, String> {
    let result = friends
        .change(|book| {
            book.forget_friend(&peer);
            Ok(view(book))
        })
        .await?;
    if let (Ok(peer), Some((_, session, _))) = (peer.parse(), state.0.lock().await.as_ref()) {
        session.forget(peer);
    }
    Ok(result)
}

#[tauri::command]
pub async fn forget_world(
    friends: State<'_, Friends>,
    host: String,
) -> Result<FriendsView, String> {
    friends
        .change(|book| {
            book.forget_world(&host);
            Ok(view(book))
        })
        .await
}
