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
    /// From the world's LAN announcement, when there is one.
    name: Option<String>,
    version: Option<String>,
}
struct AppState(
    Mutex<Option<(String, Session, Option<String>)>>,
    Mutex<Vec<Invite>>,
    Mutex<Option<WorldSlot>>,
    /// Started on first use; `None` until the announcement port could be opened.
    Mutex<Option<blockops_coordination::lan::LanWorlds>>,
);

/// A friend's code, issued by a coordinator or announced on the public network.
enum Invite {
    Coordinated(blockops_coordination::client::HostedInvite),
    Direct(blockops_tunnel::direct::DirectCode),
}
impl Invite {
    fn status(&self) -> blockops_coordination::client::CodeStatus {
        match self {
            Self::Coordinated(invite) => invite.status.borrow().clone(),
            Self::Direct(code) => {
                let status = code.status.borrow();
                blockops_coordination::client::CodeStatus {
                    id: status.id.clone(),
                    code: status.code.clone(),
                    expires_at: status.expires_at,
                    phase: status.phase.clone(),
                    peer: status.peer.clone(),
                    error: status.error.clone(),
                }
            }
        }
    }
    async fn revoke(self) {
        match self {
            Self::Coordinated(invite) => invite.revoke().await,
            Self::Direct(code) => code.revoke().await,
        }
    }
}
#[derive(Serialize)]
struct View {
    mode: String,
    invite: Option<String>,
    endpoint: Option<String>,
    active_connections: usize,
    last_error: Option<String>,
    transport: Option<String>,
    players: Vec<blockops_tunnel::Player>,
    rtt_ms: Option<u32>,
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
        players: status.players.clone(),
        rtt_ms: status.rtt_ms,
    }
}

/// "Steve теперь в вашем мире" for each arrival and "… больше не в мире" for each departure.
fn player_changes(
    before: &[blockops_tunnel::Player],
    after: &[blockops_tunnel::Player],
) -> Vec<String> {
    let name = |player: &blockops_tunnel::Player| player.name.clone().unwrap_or("Друг".into());
    let arrived = after
        .iter()
        .filter(|p| !before.iter().any(|b| b.id == p.id))
        .map(|p| format!("{} теперь в вашем мире", name(p)));
    let left = before
        .iter()
        .filter(|p| !after.iter().any(|a| a.id == p.id))
        .map(|p| format!("{} больше не в мире", name(p)));
    arrived.chain(left).collect()
}

/// Tell the host in a Windows notification when friends come and go while BlockOps is in
/// the background; the window shows its own notice. Ends with the session.
fn notify_players(
    app: tauri::AppHandle,
    mut status: tokio::sync::watch::Receiver<blockops_tunnel::Status>,
) {
    tauri::async_runtime::spawn(async move {
        use tauri_plugin_notification::NotificationExt;
        let mut known = status.borrow_and_update().players.clone();
        while status.changed().await.is_ok() {
            let players = status.borrow_and_update().players.clone();
            let changes = player_changes(&known, &players);
            known = players;
            let focused = app
                .get_webview_window("main")
                .and_then(|window| window.is_focused().ok())
                .unwrap_or(false);
            if changes.is_empty() || focused {
                continue;
            }
            let _ = app
                .notification()
                .builder()
                .title("BlockOps")
                .body(changes.join("\n"))
                .show();
        }
    });
}
/// `direct` meets friends through the public network instead of a coordinator and relay.
#[tauri::command]
async fn host(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    target: String,
    relay: Option<String>,
    world_watch_id: Option<u64>,
    direct: Option<bool>,
) -> Result<View, String> {
    make_host(
        app,
        state,
        target,
        relay,
        None,
        world_watch_id,
        direct.unwrap_or(false),
    )
    .await
}
async fn make_host(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    target: String,
    relay: Option<String>,
    owner: Option<String>,
    world_watch_id: Option<u64>,
    direct: bool,
) -> Result<View, String> {
    if state.0.lock().await.is_some() {
        return Err("Сначала остановите текущую сессию".into());
    }
    let target: std::net::SocketAddr = target
        .parse()
        .map_err(|_| "Укажите loopback IP и порт, например 127.0.0.1:25565")?;
    let relay = relay
        .map(|r| r.parse())
        .transpose()
        .map_err(|_| "Некорректный multiaddr relay")?;
    {
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
    }
    // Joining the public network takes up to a minute; the status lock is not held meanwhile.
    let lifetime = std::time::Duration::from_secs(3600);
    let session = if direct {
        blockops_tunnel::direct::host_direct(
            target,
            blockops_tunnel::direct::Network::public(),
            lifetime,
        )
        .await
    } else {
        blockops_tunnel::host_controlled(target, relay, lifetime).await
    }
    .map_err(|e| e.to_string())?;
    let mut guard = state.0.lock().await;
    if guard.is_some() {
        session.stop().await;
        return Err("Сначала остановите текущую сессию".into());
    }
    let worlds = state.2.lock().await;
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
    notify_players(app, session.status.clone());
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
async fn create_code(
    state: State<'_, AppState>,
    coordinator: String,
) -> Result<blockops_coordination::client::CodeStatus, String> {
    let guard = state.0.lock().await;
    let (_, session, _) = guard.as_ref().ok_or("Сначала создайте туннель хоста")?;
    let mut codes = state.1.lock().await;
    codes.retain(|c| {
        matches!(
            c.status().phase.as_str(),
            "publishing" | "waiting" | "active"
        )
    });
    if codes.len() >= 8 {
        return Err("Максимум 8 приглашений на сессию".into());
    }
    let code = if session.is_direct() {
        Invite::Direct(
            session
                .issue_direct_code(std::time::Duration::from_secs(600))
                .await
                .map_err(|e| e.to_string())?,
        )
    } else {
        Invite::Coordinated(
            blockops_coordination::client::Client::new(&coordinator)
                .map_err(|e| e.to_string())?
                .publish(session, 600)
                .await
                .map_err(|e| e.to_string())?,
        )
    };
    let status = code.status();
    codes.push(code);
    Ok(status)
}
#[tauri::command]
async fn codes(
    state: State<'_, AppState>,
) -> Result<Vec<blockops_coordination::client::CodeStatus>, String> {
    Ok(state.1.lock().await.iter().map(Invite::status).collect())
}
#[tauri::command]
async fn revoke_code(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let code = {
        let mut codes = state.1.lock().await;
        let index = codes
            .iter()
            .position(|c| c.status().id == id)
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
    if state.0.lock().await.is_some() {
        return Err("Сначала остановите текущую сессию".into());
    }
    state.2.lock().await.take();
    let listener = guest_listener().await?;
    // A four-group code was announced on the public network; others come from a coordinator.
    let session = if blockops_tunnel::pake::is_direct_code(&code) {
        blockops_tunnel::direct::join_direct(
            &code,
            listener,
            blockops_tunnel::direct::Network::public(),
        )
        .await
    } else {
        blockops_coordination::client::Client::new(&coordinator)
            .map_err(|e| e.to_string())?
            .join_on(&code, listener)
            .await
    }
    .map_err(|e| e.to_string())?;
    let mut guard = state.0.lock().await;
    if guard.is_some() {
        session.stop().await;
        return Err("Сначала остановите текущую сессию".into());
    }
    let result = view("join", &session);
    *guard = Some(("join".into(), session, None));
    Ok(result)
}
/// The guest's fixed address, so a server saved in Minecraft works every time.
const GUEST_ADDRESS: &str = "127.0.0.1:25595";
const GUEST_SERVER_NAME: &str = "BlockOps — мир друга";

/// The fixed address when it is free, otherwise any loopback port.
async fn guest_listener() -> Result<tokio::net::TcpListener, String> {
    match tokio::net::TcpListener::bind(GUEST_ADDRESS).await {
        Ok(listener) => Ok(listener),
        Err(_) => tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| e.to_string()),
    }
}

#[derive(Serialize)]
struct ServerEntries {
    address: &'static str,
    added: usize,
    present: usize,
    failed: usize,
}

/// Every Minecraft game folder known from the launcher, the saved choice and the standard
/// folder, each once.
async fn game_folders(servers: &servers::Servers) -> Vec<std::path::PathBuf> {
    let mut folders = Vec::new();
    if let Ok(root) = launcher_root(None) {
        folders.push(root.clone());
        let root_for_scan = root.clone();
        if let Ok(Ok(profiles)) = tokio::task::spawn_blocking(move || {
            blockops_server_manager::minecraft_profiles::discover(&root_for_scan)
        })
        .await
        {
            folders.extend(
                profiles
                    .into_iter()
                    .map(|p| std::path::PathBuf::from(p.game_dir)),
            );
        }
    }
    let manager = servers.manager.clone();
    if let Ok(Ok(Some(selection))) =
        tokio::task::spawn_blocking(move || manager.minecraft_selection()).await
    {
        folders.push(selection.game_dir.into());
    }
    tokio::task::spawn_blocking(move || {
        let mut seen = std::collections::HashSet::new();
        folders
            .into_iter()
            .filter_map(|folder| folder.canonicalize().ok())
            .filter(|folder| folder.is_dir() && seen.insert(folder.clone()))
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Add the guest's fixed address to the server list of every known game folder. Works in
/// any version.
#[tauri::command]
async fn add_server_entry(servers: State<'_, servers::Servers>) -> Result<ServerEntries, String> {
    let folders = game_folders(&servers).await;
    tokio::task::spawn_blocking(move || {
        let mut result = ServerEntries {
            address: GUEST_ADDRESS,
            added: 0,
            present: 0,
            failed: 0,
        };
        for folder in folders {
            match blockops_server_manager::server_list::ensure_entry(
                &folder,
                GUEST_SERVER_NAME,
                GUEST_ADDRESS,
            ) {
                Ok(true) => result.added += 1,
                Ok(false) => result.present += 1,
                Err(_) => result.failed += 1,
            }
        }
        result
    })
    .await
    .map_err(|_| "Не удалось обновить список серверов Minecraft".to_owned())
}

/// Whether some known game folder already lists the guest's fixed address. Reads only.
#[tauri::command]
async fn server_entry_present(servers: State<'_, servers::Servers>) -> Result<bool, String> {
    let folders = game_folders(&servers).await;
    tokio::task::spawn_blocking(move || {
        folders.iter().any(|folder| {
            blockops_server_manager::server_list::has_entry(folder, GUEST_ADDRESS).unwrap_or(false)
        })
    })
    .await
    .map_err(|_| "Не удалось прочитать список серверов Minecraft".to_owned())
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

const NO_WORLD: &str = "Открытый для сети мир не найден. В Minecraft откройте мир и нажмите «Открыть для сети»: BlockOps найдёт его сам.";

/// Worlds announced to LAN from this computer by any Minecraft version. The listener starts
/// on first use and is retried while its port cannot be opened.
async fn lan_worlds(state: &AppState) -> Option<blockops_coordination::lan::LanWorlds> {
    let mut lan = state.3.lock().await;
    if lan.is_none() {
        *lan = blockops_coordination::lan::LanWorlds::listen().ok();
    }
    lan.clone()
}

#[derive(Serialize)]
struct LocalMinecraft {
    running: usize,
    open_worlds: usize,
    in_menu: usize,
    lan_worlds: Vec<blockops_coordination::lan::LanWorld>,
}

/// What Minecraft reports doing on this computer: the mod through its private descriptors,
/// any version through LAN announcements. Spends no bridge, so the interface can poll it.
#[tauri::command]
async fn local_minecraft(state: State<'_, AppState>) -> Result<LocalMinecraft, String> {
    use blockops_local_discovery::GameState;
    let found = companions().await;
    let count = |state| found.iter().filter(|c| c.state == state).count();
    let lan_worlds = match lan_worlds(&state).await {
        Some(lan) => lan.current(),
        None => Vec::new(),
    };
    Ok(LocalMinecraft {
        running: found.len(),
        // A world opened with the mod is announced too; count each world once.
        open_worlds: count(GameState::Lan).max(lan_worlds.len()),
        in_menu: count(GameState::Menu),
        lan_worlds,
    })
}

/// The open world: through the mod when it is installed, which also authenticates the
/// answer, otherwise from the LAN announcement every Minecraft version makes.
async fn find_open_world(
    lan: Option<blockops_coordination::lan::LanWorlds>,
) -> Result<blockops_coordination::local_bridge::WorldWatch, String> {
    if let Some(world) = find_world_through_mod().await? {
        return Ok(world);
    }
    let lan = lan.ok_or(NO_WORLD)?;
    match lan.current().as_slice() {
        [] => Err(NO_WORLD.into()),
        [world] => Ok(lan.watch(world.port)),
        _ => Err(
            "Открыто несколько миров. Оставьте открытым для сети один и попробуйте снова.".into(),
        ),
    }
}

/// Ask each discovered companion for an open world. `None` when no mod has one; an error
/// when several do, since only the player can tell which world to share.
async fn find_world_through_mod(
) -> Result<Option<blockops_coordination::local_bridge::WorldWatch>, String> {
    let found = companions().await;
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
    if worlds.len() > 1 {
        return Err(
            "Открыты миры в нескольких копиях Minecraft. Оставьте один и попробуйте снова.".into(),
        );
    }
    Ok(worlds.pop())
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
        None => find_open_world(lan_worlds(&state).await).await?,
    };
    let announced = match lan_worlds(&state).await {
        Some(lan) => lan
            .current()
            .into_iter()
            .find(|world| world.port == watch.port),
        None => None,
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
    Ok(WorldDetection {
        id,
        port,
        name: announced.as_ref().map(|world| world.name.clone()),
        version: announced.and_then(|world| world.version),
    })
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
/// The mod jars the installer placed beside the application, one per loader and version.
fn mod_catalog(app: &tauri::AppHandle) -> blockops_server_manager::blockops_mod::Catalog {
    match app.path().resource_dir() {
        Ok(directory) => {
            blockops_server_manager::blockops_mod::Catalog::load(&directory.join("mod"))
        }
        Err(_) => Default::default(),
    }
}

/// "Fabric 1.21.1, Forge 1.20.1" for messages.
fn describe_targets(targets: &[blockops_server_manager::blockops_mod::Target]) -> String {
    targets
        .iter()
        .map(|target| {
            let loader = match target.loader.as_str() {
                "fabric" => "Fabric",
                "forge" => "Forge",
                "neoforge" => "NeoForge",
                other => other,
            };
            format!("{loader} {}", target.minecraft)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn explain_mod_error(
    error: blockops_server_manager::blockops_mod::InstallError,
    targets: &[blockops_server_manager::blockops_mod::Target],
) -> String {
    use blockops_server_manager::blockops_mod::InstallError;
    match error {
        InstallError::Unsupported if targets.is_empty() => "В этой сборке BlockOps нет мода. Установите BlockOps из сборки, в которую он входит.".into(),
        InstallError::Unsupported => format!("Эта папка не принадлежит сборке из Minecraft Launcher, для которой есть мод BlockOps. Мод есть для: {}.", describe_targets(targets)),
        InstallError::SharedWith(name) => format!("Из этой папки также запускается профиль «{name}», и мод BlockOps может помешать ему. Укажите для этой сборки отдельную папку игры в настройках профиля Minecraft Launcher."),
        InstallError::InUse => "Файл мода занят. Закройте Minecraft и повторите.".into(),
        InstallError::NameTaken(name) => format!("В папке mods уже есть файл {name}, но это не мод BlockOps. Переименуйте или удалите его и повторите."),
        InstallError::Other(_) => "Не удалось изменить папку модов. Проверьте папку сборки и права доступа.".into(),
    }
}

/// The loaders and versions this build carries a mod for.
#[tauri::command]
async fn blockops_mod_targets(
    app: tauri::AppHandle,
) -> Result<Vec<blockops_server_manager::blockops_mod::Target>, String> {
    tokio::task::spawn_blocking(move || mod_catalog(&app).targets())
        .await
        .map_err(|_| "Не удалось прочитать моды BlockOps".to_owned())
}

/// `loader` and `minecraft` name the build as the profile list shows it; they only choose
/// which bundled version to compare with.
#[tauri::command]
async fn blockops_mod_status(
    app: tauri::AppHandle,
    game_dir: String,
    loader: String,
    minecraft: String,
) -> Result<blockops_server_manager::blockops_mod::ModStatus, String> {
    tokio::task::spawn_blocking(move || {
        let catalog = mod_catalog(&app);
        let target = blockops_server_manager::blockops_mod::Target { loader, minecraft };
        blockops_server_manager::blockops_mod::status(
            std::path::Path::new(&game_dir),
            catalog.get(&target),
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
    tokio::task::spawn_blocking(move || {
        let catalog = mod_catalog(&app);
        blockops_server_manager::blockops_mod::install(
            &root,
            std::path::Path::new(&game_dir),
            &catalog,
        )
        .map_err(|error| explain_mod_error(error, &catalog.targets()))
    })
    .await
    .map_err(|_| "Не удалось установить мод BlockOps".to_owned())?
}

#[tauri::command]
async fn remove_blockops_mod(
    app: tauri::AppHandle,
    game_dir: String,
    loader: String,
    minecraft: String,
) -> Result<blockops_server_manager::blockops_mod::ModStatus, String> {
    tokio::task::spawn_blocking(move || {
        let catalog = mod_catalog(&app);
        let target = blockops_server_manager::blockops_mod::Target { loader, minecraft };
        blockops_server_manager::blockops_mod::remove(
            std::path::Path::new(&game_dir),
            catalog.get(&target),
        )
        .map_err(|error| explain_mod_error(error, &catalog.targets()))
    })
    .await
    .map_err(|_| "Не удалось удалить мод BlockOps".to_owned())?
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
        .plugin(tauri_plugin_notification::init())
        .manage(AppState(
            Mutex::new(None),
            Mutex::new(Vec::new()),
            Mutex::new(None),
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
            add_server_entry,
            server_entry_present,
            blockops_mod_status,
            blockops_mod_targets,
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
