#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod servers;
use blockops_tunnel::Session;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::Manager as _;
use tauri::State;
use tokio::sync::Mutex;
static NEXT_WORLD: AtomicU64 = AtomicU64::new(1);
struct WorldSlot {
    id: u64,
    watch: Option<blockops_coordination::local_bridge::WorldWatch>,
}
#[derive(Serialize)]
struct WorldDetection {
    id: u64,
    port: u16,
}
struct AppState(
    Mutex<Option<(String, Session, Option<String>)>>,
    Mutex<Vec<blockops_coordination::client::HostedInvite>>,
    Mutex<Option<WorldSlot>>,
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
    world_watch_id: Option<u64>,
) -> Result<View, String> {
    make_host(state, target, relay, None, world_watch_id).await
}
async fn make_host(
    state: State<'_, AppState>,
    target: String,
    relay: Option<String>,
    owner: Option<String>,
    world_watch_id: Option<u64>,
) -> Result<View, String> {
    let mut guard = state.0.lock().await;
    if guard.is_some() {
        return Err("Сначала остановите текущую сессию".into());
    }
    let target: std::net::SocketAddr = target
        .parse()
        .map_err(|_| "Укажите loopback IP и порт, например 127.0.0.1:25565")?;
    let relay = relay
        .map(|r| r.parse())
        .transpose()
        .map_err(|_| "Некорректный multiaddr relay")?;
    let mut worlds = state.2.lock().await;
    if let Some(id) = world_watch_id {
        let slot = worlds
            .as_ref()
            .filter(|slot| slot.id == id)
            .ok_or("Мир больше не отслеживается; определите порт заново")?;
        let watch = slot
            .watch
            .as_ref()
            .ok_or("Мир закрыт или связь с модом потеряна")?;
        if !*watch.alive.borrow()
            || target != std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, watch.port))
        {
            return Err("Мир закрыт или его порт изменился; определите порт заново".into());
        }
    } else {
        worlds.take();
    }
    let session =
        blockops_tunnel::host_controlled(target, relay, std::time::Duration::from_secs(3600))
            .await
            .map_err(|e| e.to_string())?;
    if let Some(id) = world_watch_id {
        if !worlds
            .as_ref()
            .filter(|slot| slot.id == id)
            .and_then(|slot| slot.watch.as_ref())
            .is_some_and(|watch| *watch.alive.borrow())
        {
            session.stop().await;
            return Err("Мир закрылся до создания соединения".into());
        }
    }
    if let Some(id) = world_watch_id {
        if let Some(watch) = worlds
            .as_ref()
            .filter(|slot| slot.id == id)
            .and_then(|slot| slot.watch.as_ref())
        {
            // This independent task cancels the tunnel even if the UI/session lock is busy.
            drop(blockops_coordination::local_bridge::cancel_on_world_loss(
                watch.alive.clone(),
                session.cancellation_handle(),
            ));
        }
    }
    let result = view("host", &session);
    let owner = world_watch_id
        .map(|id| format!("world-watch:{id}"))
        .or(owner);
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
    state.2.lock().await.take();
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
    state.2.lock().await.take();
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
    state.2.lock().await.take();
    let session = blockops_coordination::client::Client::new(&coordinator)
        .map_err(|e| e.to_string())?
        .join(&code, "127.0.0.1:0".parse().unwrap())
        .await
        .map_err(|e| e.to_string())?;
    let result = view("join", &session);
    *guard = Some(("join".into(), session, None));
    Ok(result)
}
const NO_COMPANION: &str = "Minecraft с модом BlockOps не найден на этом компьютере. Запустите игру с модом или введите код мода вручную.";

/// Running Minecraft instances that advertise the mod through private discovery.
async fn companions() -> Vec<blockops_local_discovery::Descriptor> {
    let Some(directory) = blockops_local_discovery::default_directory() else {
        return Vec::new();
    };
    tokio::task::spawn_blocking(move || blockops_local_discovery::discover(&directory))
        .await
        .unwrap_or_default()
}

/// Companions whose state hint matches, or all of them when none does: the hint can lag
/// the game by a second, and the bridge checks the real state on each request anyway.
fn prefer(
    found: &[blockops_local_discovery::Descriptor],
    state: blockops_local_discovery::GameState,
) -> Vec<&blockops_local_discovery::Descriptor> {
    let matching: Vec<_> = found
        .iter()
        .filter(|companion| companion.state == state)
        .collect();
    if matching.is_empty() {
        found.iter().collect()
    } else {
        matching
    }
}

#[derive(Serialize)]
struct LocalMinecraft {
    running: usize,
    open_worlds: usize,
    in_menu: usize,
}

/// What Minecraft with the mod reports doing on this computer. Reads only the private
/// descriptors and spends no bridge, so the interface can poll it.
#[tauri::command]
async fn local_minecraft() -> Result<LocalMinecraft, String> {
    use blockops_local_discovery::GameState;
    let found = companions().await;
    let count = |state| found.iter().filter(|c| c.state == state).count();
    Ok(LocalMinecraft {
        running: found.len(),
        open_worlds: count(GameState::Lan),
        in_menu: count(GameState::Menu),
    })
}

/// Ask each discovered companion for an open world; exactly one must have it.
async fn find_open_world() -> Result<blockops_coordination::local_bridge::WorldWatch, String> {
    let found = companions().await;
    if found.is_empty() {
        return Err(NO_COMPANION.into());
    }
    let mut worlds = Vec::new();
    for companion in prefer(&found, blockops_local_discovery::GameState::Lan) {
        // Each probe uses up that bridge; the mod advertises a fresh one within a second.
        if let Ok(watch) =
            blockops_coordination::local_bridge::watch_world_at(companion.port, &companion.token)
                .await
        {
            worlds.push(watch);
        }
    }
    let world = worlds.pop().ok_or(
        "Minecraft с модом BlockOps запущен, но открытый для сети мир не найден. Откройте мир для сети и попробуйте снова.",
    )?;
    if !worlds.is_empty() {
        return Err(
            "Открыты миры в нескольких копиях Minecraft. Оставьте один и попробуйте снова.".into(),
        );
    }
    Ok(world)
}

/// Without a pairing code, the only discovered Minecraft is asked to join.
#[tauri::command]
async fn join_in_minecraft(
    state: State<'_, AppState>,
    pairing_code: Option<String>,
) -> Result<(), String> {
    if pairing_code.as_ref().is_some_and(|code| code.len() > 80) {
        return Err("Некорректный код сопряжения".into());
    }
    // Keep the session alive until the local request completes; never accept a destination from the UI.
    let guard = state.0.lock().await;
    let (mode, session, _) = guard
        .as_ref()
        .ok_or("Сначала подключитесь к миру по приглашению")?;
    if mode != "join" {
        return Err("Для входа нужен активный гостевой туннель".into());
    }
    let endpoint = session
        .endpoint
        .ok_or("Локальный адрес гостевого туннеля недоступен")?;
    let Some(pairing_code) = pairing_code else {
        let found = companions().await;
        let candidates = prefer(&found, blockops_local_discovery::GameState::Menu);
        let companion = match candidates.as_slice() {
            [] => return Err(NO_COMPANION.into()),
            [companion] => companion,
            _ => return Err("Запущено несколько копий Minecraft с модом BlockOps. Оставьте одну или введите код мода вручную.".into()),
        };
        return blockops_coordination::local_bridge::request_join(
            companion.port,
            &companion.token,
            endpoint,
        )
        .await
        .map_err(|_| {
            "Minecraft не начал подключение. Откройте главное меню игры и попробуйте снова."
                .to_owned()
        });
    };
    blockops_coordination::local_bridge::request_paired_join(&pairing_code, endpoint).await
        .map_err(|_| "Не удалось начать подключение в Minecraft. Откройте главное меню игры и получите новый код мода.".to_owned())
}

/// Without a pairing code, the open world is found through private discovery.
#[tauri::command]
async fn detect_open_world(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    pairing_code: Option<String>,
) -> Result<WorldDetection, String> {
    if pairing_code.as_ref().is_some_and(|code| code.len() > 80) {
        return Err("Некорректный код сопряжения".into());
    }
    let guard = state.0.lock().await;
    if guard.is_some() {
        return Err("Сначала отключите текущее соединение".into());
    }
    let watch = match pairing_code {
        Some(code) => blockops_coordination::local_bridge::watch_world(&code)
            .await
            .map_err(|_| {
                "Не удалось определить порт. Откройте мир для сети и получите новый код мода."
                    .to_owned()
            })?,
        None => find_open_world().await?,
    };
    let id = NEXT_WORLD.fetch_add(1, Ordering::Relaxed);
    let port = watch.port;
    let mut alive = watch.alive.clone();
    *state.2.lock().await = Some(WorldSlot {
        id,
        watch: Some(watch),
    });
    tauri::async_runtime::spawn(async move {
        loop {
            if !*alive.borrow() || alive.changed().await.is_err() {
                break;
            }
        }
        let state = app.state::<AppState>();
        {
            let mut worlds = state.2.lock().await;
            let Some(slot) = worlds.as_mut().filter(|slot| slot.id == id) else {
                return;
            };
            slot.watch.take();
        }
        let mut active = state.0.lock().await;
        let worlds = state.2.lock().await;
        if !worlds.as_ref().is_some_and(|slot| slot.id == id) {
            return;
        }
        let owner = format!("world-watch:{id}");
        let is_owner =
            active.as_ref().and_then(|(_, _, owner)| owner.as_deref()) == Some(owner.as_str());
        let session = if is_owner {
            state.1.lock().await.clear();
            active.take().map(|(_, session, _)| session)
        } else {
            None
        };
        drop(worlds);
        drop(active);
        if let Some(session) = session {
            session.stop().await;
        }
    });
    Ok(WorldDetection { id, port })
}

#[tauri::command]
async fn world_watch_alive(state: State<'_, AppState>, id: u64) -> Result<bool, String> {
    Ok(state
        .2
        .lock()
        .await
        .as_ref()
        .filter(|slot| slot.id == id)
        .and_then(|slot| slot.watch.as_ref())
        .is_some_and(|watch| *watch.alive.borrow()))
}

/// The Minecraft Launcher folder: the one the user typed, or the standard one.
fn launcher_root(folder: Option<String>) -> Result<std::path::PathBuf, String> {
    match folder.filter(|value| !value.trim().is_empty()) {
        Some(folder) => Ok(std::path::PathBuf::from(folder)),
        None => std::env::var_os("APPDATA")
            .map(|base| std::path::PathBuf::from(base).join(".minecraft"))
            .ok_or_else(|| "Укажите путь к папке Minecraft вручную".to_owned()),
    }
}

#[tauri::command]
async fn discover_minecraft_profiles(
    folder: Option<String>,
) -> Result<Vec<blockops_server_manager::minecraft_profiles::DiscoveredProfile>, String> {
    let root = launcher_root(folder)?;
    tokio::task::spawn_blocking(move || {
        blockops_server_manager::minecraft_profiles::discover(&root)
    })
    .await
    .map_err(|_| "Не удалось завершить поиск сборок".to_owned())?
    .map_err(|_| {
        "Не удалось прочитать профили Minecraft Launcher. Проверьте выбранную папку.".to_owned()
    })
}
/// The mod jar the installer placed beside the application, if this build carries one.
fn bundled_mod(
    app: &tauri::AppHandle,
) -> Option<blockops_server_manager::blockops_mod::BundledMod> {
    let directory = app.path().resource_dir().ok()?.join("mod");
    let jar = std::fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|extension| extension == "jar"))?;
    blockops_server_manager::blockops_mod::BundledMod::load(&jar).ok()
}

fn explain_mod_error(error: blockops_server_manager::blockops_mod::InstallError) -> String {
    use blockops_server_manager::blockops_mod::InstallError;
    match error {
        InstallError::NotFabric => "Эта папка не принадлежит сборке Fabric 1.21.1 из Minecraft Launcher. Мод BlockOps работает только с Fabric 1.21.1.".into(),
        InstallError::SharedWith(name) => format!("Из этой папки также запускается профиль «{name}», и мод BlockOps может помешать ему. Укажите для сборки Fabric 1.21.1 отдельную папку игры в настройках профиля Minecraft Launcher."),
        InstallError::InUse => "Файл мода занят. Закройте Minecraft и повторите.".into(),
        InstallError::NameTaken(name) => format!("В папке mods уже есть файл {name}, но это не мод BlockOps. Переименуйте или удалите его и повторите."),
        InstallError::Other(_) => "Не удалось изменить папку модов. Проверьте папку сборки и права доступа.".into(),
    }
}

#[tauri::command]
async fn blockops_mod_status(
    app: tauri::AppHandle,
    game_dir: String,
) -> Result<blockops_server_manager::blockops_mod::ModStatus, String> {
    tokio::task::spawn_blocking(move || {
        blockops_server_manager::blockops_mod::status(
            std::path::Path::new(&game_dir),
            bundled_mod(&app).as_ref(),
        )
    })
    .await
    .map_err(|_| "Не удалось проверить мод BlockOps".to_owned())?
    .map_err(|_| "Не удалось прочитать папку модов этой сборки.".to_owned())
}

/// Re-inspects the launcher profiles itself: the interface's view is never trusted.
#[tauri::command]
async fn install_blockops_mod(
    app: tauri::AppHandle,
    folder: Option<String>,
    game_dir: String,
) -> Result<blockops_server_manager::blockops_mod::ModStatus, String> {
    let root = launcher_root(folder)?;
    let bundled = bundled_mod(&app).ok_or(
        "В этой сборке BlockOps нет мода. Установите BlockOps из сборки, в которую он входит.",
    )?;
    tokio::task::spawn_blocking(move || {
        blockops_server_manager::blockops_mod::install(
            &root,
            std::path::Path::new(&game_dir),
            &bundled,
        )
    })
    .await
    .map_err(|_| "Не удалось установить мод BlockOps".to_owned())?
    .map_err(explain_mod_error)
}

#[tauri::command]
async fn remove_blockops_mod(
    app: tauri::AppHandle,
    game_dir: String,
) -> Result<blockops_server_manager::blockops_mod::ModStatus, String> {
    tokio::task::spawn_blocking(move || {
        blockops_server_manager::blockops_mod::remove(
            std::path::Path::new(&game_dir),
            bundled_mod(&app).as_ref(),
        )
    })
    .await
    .map_err(|_| "Не удалось удалить мод BlockOps".to_owned())?
    .map_err(explain_mod_error)
}

#[tauri::command]
async fn get_minecraft_selection(
    state: tauri::State<'_, servers::Servers>,
) -> Result<Option<blockops_server_manager::minecraft_selection::MinecraftSelection>, String> {
    let manager = state.manager.clone();
    tokio::task::spawn_blocking(move || manager.minecraft_selection())
        .await
        .map_err(|_| "Не удалось прочитать выбранную сборку".to_owned())?
        .map_err(|_| "Не удалось прочитать выбранную сборку".to_owned())
}

#[tauri::command]
async fn save_minecraft_selection(
    state: tauri::State<'_, servers::Servers>,
    selection: blockops_server_manager::minecraft_selection::MinecraftSelection,
) -> Result<blockops_server_manager::minecraft_selection::MinecraftSelection, String> {
    let manager = state.manager.clone();
    tokio::task::spawn_blocking(move || manager.save_minecraft_selection(selection))
        .await
        .map_err(|_| "Не удалось сохранить выбор".to_owned())?
        .map_err(|_| {
            "Не удалось сохранить выбор. Укажите полный путь к существующей папке сборки."
                .to_owned()
        })
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
        .manage(AppState(
            Mutex::new(None),
            Mutex::new(Vec::new()),
            Mutex::new(None),
        ))
        .setup(|app| {
            let manager = blockops_server_manager::Manager::open(app.path().app_data_dir()?)?;
            app.manage(servers::Servers {
                manager,
                processes: Mutex::new(std::collections::HashMap::new()),
                stopping: Mutex::new(blockops_server_manager::stop_states::StopStates::default()),
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
            join_in_minecraft,
            detect_open_world,
            world_watch_alive,
            local_minecraft,
            open_eula,
            discover_minecraft_profiles,
            blockops_mod_status,
            install_blockops_mod,
            remove_blockops_mod,
            get_minecraft_selection,
            save_minecraft_selection,
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
            servers::whitelist_player,
            servers::invite_server
        ])
        .run(tauri::generate_context!())
        .expect("Tauri runtime failed");
}
