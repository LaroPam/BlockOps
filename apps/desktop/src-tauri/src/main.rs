#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod servers;
use blockops_tunnel::Session;
use serde::Serialize;
use tauri::Manager as _;
use tauri::State;
use tokio::sync::Mutex;
struct AppState(
    Mutex<Option<(String, Session, Option<String>)>>,
    Mutex<Vec<blockops_coordination::client::HostedInvite>>,
);
#[derive(Serialize)]
struct View {
    mode: String,
    invite: Option<String>,
    endpoint: Option<String>,
    active_connections: usize,
    last_error: Option<String>,
    transport: Option<String>,
}
fn view(mode: &str, session: &Session) -> View {
    let status = session.status.borrow();
    View {
        mode: mode.into(),
        invite: None,
        endpoint: session.endpoint.map(|e| e.to_string()),
        active_connections: status.active_connections,
        last_error: status.last_error.clone(),
        transport: status.transport.clone(),
    }
}
#[tauri::command]
async fn host(
    state: State<'_, AppState>,
    target: String,
    relay: Option<String>,
) -> Result<View, String> {
    make_host(state, target, relay, None).await
}
async fn make_host(
    state: State<'_, AppState>,
    target: String,
    relay: Option<String>,
    owner: Option<String>,
) -> Result<View, String> {
    let mut guard = state.0.lock().await;
    if guard.is_some() {
        return Err("Сначала остановите текущую сессию".into());
    }
    let target = target
        .parse()
        .map_err(|_| "Укажите loopback IP и порт, например 127.0.0.1:25565")?;
    let relay = relay
        .map(|r| r.parse())
        .transpose()
        .map_err(|_| "Некорректный multiaddr relay")?;
    let session =
        blockops_tunnel::host_controlled(target, relay, std::time::Duration::from_secs(3600))
            .await
            .map_err(|e| e.to_string())?;
    let result = view("host", &session);
    *guard = Some(("host".into(), session, owner));
    Ok(result)
}
#[tauri::command]
async fn join(state: State<'_, AppState>, invite: String) -> Result<View, String> {
    if invite.len() > 4096 {
        return Err("Приглашение слишком длинное".into());
    }
    let mut guard = state.0.lock().await;
    if guard.is_some() {
        return Err("Сначала остановите текущую сессию".into());
    }
    let invite = serde_json::from_str(&invite).map_err(|_| "Некорректный формат приглашения")?;
    let session = blockops_tunnel::join(invite, "127.0.0.1:0".parse().unwrap())
        .await
        .map_err(|e| e.to_string())?;
    let result = view("join", &session);
    *guard = Some(("join".into(), session, None));
    Ok(result)
}
#[tauri::command]
async fn status(state: State<'_, AppState>) -> Result<Option<View>, String> {
    Ok(state.0.lock().await.as_ref().map(|(m, s, _)| view(m, s)))
}
#[tauri::command]
async fn stop(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.0.lock().await;
    let codes = std::mem::take(&mut *state.1.lock().await);
    drop(codes);
    if let Some((_, session, _)) = guard.take() {
        session.stop().await;
    }
    Ok(())
}
#[tauri::command]
async fn create_code(state: State<'_, AppState>, coordinator: String) -> Result<(), String> {
    let guard = state.0.lock().await;
    let (_, session, _) = guard.as_ref().ok_or("Сначала создайте туннель хоста")?;
    let mut codes = state.1.lock().await;
    codes.retain(|c| matches!(c.status.borrow().phase.as_str(), "waiting" | "active"));
    if codes.len() >= 8 {
        return Err("Максимум 8 приглашений на сессию".into());
    }
    let code = blockops_coordination::client::Client::new(&coordinator)
        .map_err(|e| e.to_string())?
        .publish(session, 600)
        .await
        .map_err(|e| e.to_string())?;
    codes.push(code);
    Ok(())
}
#[tauri::command]
async fn codes(
    state: State<'_, AppState>,
) -> Result<Vec<blockops_coordination::client::CodeStatus>, String> {
    Ok(state
        .1
        .lock()
        .await
        .iter()
        .map(|c| c.status.borrow().clone())
        .collect())
}
#[tauri::command]
async fn revoke_code(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let code = {
        let mut codes = state.1.lock().await;
        let index = codes
            .iter()
            .position(|c| c.status.borrow().id == id)
            .ok_or("Приглашение не найдено")?;
        codes.remove(index)
    };
    code.revoke().await;
    Ok(())
}
#[tauri::command]
async fn join_code(
    state: State<'_, AppState>,
    coordinator: String,
    code: String,
) -> Result<View, String> {
    let mut guard = state.0.lock().await;
    if guard.is_some() {
        return Err("Сначала остановите текущую сессию".into());
    }
    let session = blockops_coordination::client::Client::new(&coordinator)
        .map_err(|e| e.to_string())?
        .join(&code, "127.0.0.1:0".parse().unwrap())
        .await
        .map_err(|e| e.to_string())?;
    let result = view("join", &session);
    *guard = Some(("join".into(), session, None));
    Ok(result)
}
#[tauri::command]
fn open_eula(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(blockops_server_manager::EULA_URL, None::<&str>)
        .map_err(|e| e.to_string())
}
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState(Mutex::new(None), Mutex::new(Vec::new())))
        .setup(|app| {
            let manager = blockops_server_manager::Manager::open(app.path().app_data_dir()?)?;
            app.manage(servers::Servers {
                manager,
                processes: Mutex::new(std::collections::HashMap::new()),
                stopping: Mutex::new(std::collections::HashMap::new()),
                installing: Mutex::new(()),
                disk_operations: std::sync::Arc::new(Mutex::new(())),
                process_stops: std::sync::Arc::new(Mutex::new(())),
                closing: std::sync::atomic::AtomicBool::new(false),
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let app = window.app_handle().clone();
                let state = app.state::<servers::Servers>();
                if state
                    .closing
                    .swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    return;
                }
                tauri::async_runtime::spawn(async move {
                    let _ = stop(app.state::<AppState>()).await;
                    let state = app.state::<servers::Servers>();
                    let _install = state.installing.lock().await;
                    // A copy/restore may still be executing after its IPC request was dropped.
                    let _disk_operation = state.disk_operations.lock().await;
                    let _process_stop = state.process_stops.lock().await;
                    let all = std::mem::take(&mut *state.processes.lock().await);
                    let mut stops = tokio::task::JoinSet::new();
                    for (_, process) in all {
                        stops.spawn(process.stop());
                    }
                    while stops.join_next().await.is_some() {}
                    app.exit(0);
                });
            }
        })
        .invoke_handler(tauri::generate_handler![
            create_code,
            codes,
            revoke_code,
            join_code,
            open_eula,
            host,
            join,
            status,
            stop,
            servers::list_backups,
            servers::create_backup,
            servers::restore_backup,
            servers::recover_restore,
            servers::update_settings,
            servers::servers,
            servers::create_server,
            servers::start_server,
            servers::stop_server,
            servers::send_console,
            servers::invite_server
        ])
        .run(tauri::generate_context!())
        .expect("Tauri runtime failed");
}
