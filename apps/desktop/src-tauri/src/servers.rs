use blockops_server_manager::{
    process::{Process, ProcessStatus},
    Manager, Server, Settings,
};
use serde::Serialize;
use std::{collections::HashMap, sync::Arc};
use tauri::{Manager as _, State};
use tokio::sync::Mutex;

pub struct Servers {
    pub manager: Manager,
    pub processes: Mutex<HashMap<String, Process>>,
    pub stopping: Mutex<HashMap<String, tokio::sync::watch::Receiver<ProcessStatus>>>,
    pub installing: Mutex<()>,
    // Owned by the blocking task until disk mutation finishes, even if IPC is cancelled.
    pub disk_operations: Arc<Mutex<()>>,
    pub process_stops: Arc<Mutex<()>>,
    pub closing: std::sync::atomic::AtomicBool,
}
#[derive(Serialize)]
pub struct ServerView {
    server: Server,
    process: Option<ProcessStatus>,
}
#[tauri::command]
pub async fn servers(state: State<'_, Servers>) -> Result<Vec<ServerView>, String> {
    let servers = state.manager.list().map_err(|e| e.to_string())?;
    let processes = state.processes.lock().await;
    let stopping = state.stopping.lock().await;
    Ok(servers
        .into_iter()
        .map(|server| {
            let process = processes
                .get(&server.id)
                .map(|p| p.status.borrow().clone())
                .or_else(|| {
                    stopping.get(&server.id).map(|status| {
                        let mut status = status.borrow().clone();
                        status.phase = "stopping".into();
                        status
                    })
                });
            ServerView { server, process }
        })
        .collect())
}
#[tauri::command]
pub async fn create_server(
    state: State<'_, Servers>,
    settings: Settings,
    accept_eula: bool,
    version: String,
) -> Result<Server, String> {
    let _install = state
        .installing
        .try_lock()
        .map_err(|_| "Дождитесь текущей установки")?;
    if state.closing.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("Приложение завершает работу".into());
    }
    state
        .manager
        .install_version(settings, accept_eula, &version)
        .await
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
pub async fn start_server(
    state: State<'_, Servers>,
    app: tauri::AppHandle,
    id: String,
) -> Result<(), String> {
    let mut processes = state.processes.lock().await;
    if state.closing.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("Приложение завершает работу".into());
    }
    if state.stopping.lock().await.contains_key(&id) {
        return Err("Дождитесь завершения остановки сервера".into());
    }
    if let Some(p) = processes.get(&id) {
        if !matches!(p.status.borrow().phase.as_str(), "stopped" | "failed") {
            return Err("Сервер уже запущен".into());
        }
    }
    if let Some(old) = processes.remove(&id) {
        old.stop().await;
    }
    let process = state
        .manager
        .start(&id)
        .await
        .map_err(|e| format!("{e:#}"))?;
    let owner = format!("{id}:{}", process.generation);
    let mut status = process.status.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let done = matches!(status.borrow().phase.as_str(), "stopped" | "failed");
            if done || status.changed().await.is_err() {
                break;
            }
        }
        let tunnel = app.state::<super::AppState>();
        let session = {
            let mut guard = tunnel.0.lock().await;
            if guard.as_ref().and_then(|(_, _, owner)| owner.as_deref()) == Some(owner.as_str()) {
                tunnel.1.lock().await.clear();
                guard.take()
            } else {
                None
            }
        };
        if let Some((_, session, _)) = session {
            session.stop().await;
        }
    });
    processes.insert(id, process);
    Ok(())
}
#[tauri::command]
pub async fn stop_server(
    state: State<'_, Servers>,
    app: tauri::AppHandle,
    id: String,
) -> Result<(), String> {
    state.manager.get(&id).map_err(|e| e.to_string())?;
    let guard = state
        .process_stops
        .clone()
        .try_lock_owned()
        .map_err(|_| "Дождитесь завершения остановки другого сервера")?;
    if state.closing.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("Приложение завершает работу".into());
    }
    let mut processes = state.processes.lock().await;
    let Some(process) = processes.remove(&id) else {
        return Ok(());
    };
    state
        .stopping
        .lock()
        .await
        .insert(id.clone(), process.status.clone());
    drop(processes);
    // Once removed from the process map, a detached task must retain both the process
    // and shutdown guard even if the window/IPC request disappears during world saving.
    tokio::spawn(async move {
        let _guard = guard;
        let owner = format!("{id}:{}", process.generation);
        let tunnel = app.state::<super::AppState>();
        let session = {
            let mut tunnel_guard = tunnel.0.lock().await;
            if tunnel_guard
                .as_ref()
                .and_then(|(_, _, owner)| owner.as_deref())
                == Some(owner.as_str())
            {
                tunnel.1.lock().await.clear();
                tunnel_guard.take()
            } else {
                None
            }
        };
        if let Some((_, session, _)) = session {
            session.stop().await;
        }
        let status = process.status.clone();
        process.stop().await;
        let failure = if status.borrow().phase == "failed" {
            Some(
                status
                    .borrow()
                    .error
                    .clone()
                    .unwrap_or_else(|| "Ошибка остановки сервера".into()),
            )
        } else {
            None
        };
        app.state::<Servers>().stopping.lock().await.remove(&id);
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("Остановка сервера завершилась неожиданно: {e}"))?
}
#[tauri::command]
pub async fn invite_server(
    state: State<'_, Servers>,
    tunnel: State<'_, super::AppState>,
    id: String,
    relay: Option<String>,
) -> Result<super::View, String> {
    let processes = state.processes.lock().await;
    if state.closing.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("Приложение завершает работу".into());
    }
    let process = processes.get(&id).ok_or("Сначала запустите сервер")?;
    if process.status.borrow().phase != "running" {
        return Err("Сервер ещё не готов; проверьте консоль".into());
    }
    let server = state.manager.get(&id).map_err(|e| e.to_string())?;
    super::make_host(
        tunnel,
        format!("127.0.0.1:{}", server.settings.port),
        relay,
        Some(format!("{id}:{}", process.generation)),
    )
    .await
}

/// The owned lock lives inside the blocking closure, so cancellation of an IPC future
/// cannot make shutdown believe an in-progress filesystem mutation has completed.
async fn disk_operation<T, F>(state: &Servers, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(Manager) -> Result<T, String> + Send + 'static,
{
    let guard = state
        .disk_operations
        .clone()
        .try_lock_owned()
        .map_err(|_| "Дождитесь завершения другой операции с файлами сервера")?;
    if state.closing.load(std::sync::atomic::Ordering::SeqCst) {
        return Err("Приложение завершает работу".into());
    }
    let manager = state.manager.clone();
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        operation(manager)
    })
    .await
    .map_err(|e| format!("Операция с файлами завершилась неожиданно: {e}"))?
}

#[tauri::command]
pub async fn list_backups(
    state: State<'_, Servers>,
    id: String,
) -> Result<Vec<blockops_server_manager::backups::BackupInfo>, String> {
    let manager = state.manager.clone();
    tokio::task::spawn_blocking(move || {
        manager
            .list_backups(&id)
            .map_err(|e| format!("Не удалось получить список копий: {e:#}"))
    })
    .await
    .map_err(|e| format!("Не удалось получить список копий: {e}"))?
}

#[tauri::command]
pub async fn create_backup(
    state: State<'_, Servers>,
    id: String,
) -> Result<blockops_server_manager::backups::BackupInfo, String> {
    disk_operation(&state, move |manager| {
        manager.create_backup(&id).map_err(|e| {
            format!(
                "Не удалось создать копию. Остановите сервер и проверьте доступное место: {e:#}"
            )
        })
    })
    .await
}

#[tauri::command]
pub async fn restore_backup(
    state: State<'_, Servers>,
    id: String,
    backup_id: String,
) -> Result<(), String> {
    disk_operation(&state, move |manager| manager.restore_backup(&id, &backup_id)
        .map_err(|e| format!("Не удалось восстановить копию. Остановите сервер и проверьте состояние файлов: {e:#}"))).await
}

#[tauri::command]
pub async fn recover_restore(state: State<'_, Servers>, id: String) -> Result<(), String> {
    disk_operation(&state, move |manager| {
        manager
            .recover_restore(&id)
            .map_err(|e| format!("Не удалось завершить восстановление: {e:#}"))
    })
    .await
}

#[tauri::command]
pub async fn update_settings(
    state: State<'_, Servers>,
    id: String,
    settings: Settings,
) -> Result<Server, String> {
    disk_operation(&state, move |manager| manager.update_settings(&id, settings)
        .map_err(|e| format!("Не удалось сохранить настройки. Остановите сервер и проверьте введённые значения: {e:#}"))).await
}

#[tauri::command]
pub async fn send_console(
    state: State<'_, Servers>,
    id: String,
    command: String,
) -> Result<(), String> {
    let console = {
        let processes = state.processes.lock().await;
        let process = processes.get(&id).ok_or("Сервер не запущен")?;
        if process.status.borrow().phase != "running" {
            return Err("Дождитесь запуска сервера".into());
        }
        process.console()
    };
    console.send(&command).await.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub async fn whitelist_player(
    state: State<'_, Servers>,
    id: String,
    action: blockops_server_manager::whitelist::Action,
    player: Option<String>,
) -> Result<(), String> {
    let command = blockops_server_manager::whitelist::command(action, player.as_deref())
        .map_err(|e| e.to_string())?;
    send_console(state, id, command).await
}
