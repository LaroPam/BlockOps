import React, { useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { invoke } from '@tauri-apps/api/core';
import './style.css';
import { Servers } from './Servers';
import { Codes } from './Codes';
import { MinecraftProfiles } from './MinecraftProfiles';
import { explainError } from './errors';
import { findCode, invitationText, type CodeStatus } from './invite';

type Session = {
  mode: string;
  endpoint: string | null;
  active_connections: number;
  last_error: string | null;
  transport: string | null;
};

/** What Minecraft with the BlockOps mod reports doing on this computer. */
type LanWorld = { port: number; name: string; version: string | null };
/** The guest's fixed address; a server saved in Minecraft with it works every time. */
const GUEST_ADDRESS = '127.0.0.1:25595';
type LocalMinecraft = { running: number; open_worlds: number; in_menu: number; lan_worlds?: LanWorld[] };

/** "«Steve - New World», Minecraft 1.20.1" from a LAN announcement, when known. */
function describeWorld(name: string | null | undefined, version: string | null | undefined): string {
  return [name && `«${name}»`, version && `Minecraft ${version}`].filter(Boolean).join(', ');
}

const SETTINGS_KEY = 'blockops.connection';
const DEFAULT_COORDINATOR = 'http://127.0.0.1:8080';

/** `direct` meets through the public network; `service` uses a coordinator and relay. */
type Connection = 'direct' | 'service';
type Settings = { coordinator: string; relay: string; connection: Connection };

/** Service addresses are not secret; they are kept in this computer's webview storage. */
function savedSettings(): Settings {
  try {
    const value = JSON.parse(localStorage.getItem(SETTINGS_KEY) ?? 'null');
    if (value && typeof value.coordinator === 'string' && typeof value.relay === 'string') {
      // Before the serverless mode existed, a configured service was the only way to play.
      const connection: Connection = value.connection === 'service' || value.connection === 'direct' ? value.connection
        : value.coordinator === DEFAULT_COORDINATOR ? 'direct' : 'service';
      return { coordinator: value.coordinator, relay: value.relay, connection };
    }
  } catch { /* Storage may be unavailable; fall back to defaults. */ }
  return { coordinator: DEFAULT_COORDINATOR, relay: '', connection: 'direct' };
}

function App() {
  const [mode, setMode] = useState<'host' | 'join'>('host');
  const [lanPort, setLanPort] = useState('');
  const [worldWatchId, setWorldWatchId] = useState<number | null>(null);
  const [hostPairingCode, setHostPairingCode] = useState('');
  const [worldFeedback, setWorldFeedback] = useState('');
  const [worldError, setWorldError] = useState('');
  const [relay, setRelay] = useState(() => savedSettings().relay);
  const [invite, setInvite] = useState('');
  const [pairingCode, setPairingCode] = useState('');
  const [joinFeedback, setJoinFeedback] = useState('');
  const [inviteNotice, setInviteNotice] = useState('');
  const [serverEntry, setServerEntry] = useState('');
  const [coordinator, setCoordinator] = useState(() => savedSettings().coordinator);
  const [connection, setConnection] = useState<Connection>(() => savedSettings().connection);
  const direct = connection === 'direct';
  const [session, setSession] = useState<Session | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [local, setLocal] = useState<LocalMinecraft | null>(null);
  const autoDetectTried = useRef(false);
  const autoJoinTried = useRef(false);
  const sessionRef = useRef<HTMLElement>(null);
  const desktop = '__TAURI_INTERNALS__' in window;
  const watchingMinecraft = desktop && ((mode === 'host' && session === null) || session?.mode === 'join');

  useEffect(() => {
    try { localStorage.setItem(SETTINGS_KEY, JSON.stringify({ coordinator, relay, connection })); }
    catch { /* Not saved; the addresses still apply to this launch. */ }
  }, [coordinator, relay, connection]);

  useEffect(() => {
    if (!desktop) return;
    const poll = () => invoke<Session | null>('status').then(setSession).catch(e => setError(explainError(e)));
    void poll();
    const timer = setInterval(poll, 1000);
    return () => clearInterval(timer);
  }, [desktop]);

  useEffect(() => {
    if (!desktop || worldWatchId === null) return;
    let active = true;
    const check = () => invoke<boolean>('world_watch_alive', { id: worldWatchId }).then(alive => {
      if (active && !alive) {
        setWorldWatchId(null); setLanPort(''); setWorldFeedback('');
        setWorldError('Мир закрыт или связь с модом потеряна. BlockOps остановил соединение и снова найдёт мир, когда вы откроете его для сети.');
      }
    }).catch(() => {
      if (active) {
        setWorldWatchId(null); setLanPort(''); setWorldFeedback('');
        setWorldError('Не удалось проверить состояние мира. Определите порт заново.');
      }
    });
    void check();
    const timer = setInterval(check, 1000);
    return () => { active = false; clearInterval(timer); };
  }, [desktop, worldWatchId]);

  // Reads only the mod's private files and spends no bridge, so frequent polling is cheap.
  useEffect(() => {
    if (!watchingMinecraft) { setLocal(null); return; }
    let active = true;
    const poll = () => invoke<LocalMinecraft | null>('local_minecraft')
      .then(value => { if (active) setLocal(value ?? null); })
      .catch(() => { if (active) setLocal(null); });
    void poll();
    const timer = setInterval(poll, 1500);
    return () => { active = false; clearInterval(timer); };
  }, [watchingMinecraft]);

  // A world newly opened to LAN is picked up once; closing it re-arms detection.
  useEffect(() => {
    if (!local) return;
    if (local.open_worlds === 0) { autoDetectTried.current = false; return; }
    if (mode !== 'host' || session || worldWatchId !== null || lanPort !== '' || busy || autoDetectTried.current) return;
    autoDetectTried.current = true;
    void detectWorld(null);
  }, [local, mode, session, worldWatchId, lanPort, busy]);

  // After connecting, the guest's game joins by itself once it reaches the main menu
  // and the route to the host is up.
  useEffect(() => {
    if (session?.mode !== 'join') { autoJoinTried.current = false; return; }
    if (!local || local.in_menu !== 1 || !session.transport || busy || autoJoinTried.current) return;
    autoJoinTried.current = true;
    void joinMinecraft(null);
  }, [local, session, busy]);

  const refreshSession = async () => {
    try {
      setSession(await invoke<Session | null>('status'));
      setMode('host');
      // Wait for React to insert the invitation panel before navigating to it.
      window.setTimeout(() => sessionRef.current?.scrollIntoView({ behavior: 'auto' }), 100);
    } catch (e) { setError(explainError(e)); }
  };

  async function join(code: string) {
    setBusy(true);
    setError('');
    try { setSession(await invoke<Session>('join_code', { code, coordinator: coordinator.trim() })); }
    catch (e) { setError(explainError(e)); }
    finally { setBusy(false); }
  }

  // A whole pasted invitation is reduced to its code and connects at once.
  function pasteInvitation(e: React.ClipboardEvent<HTMLInputElement>) {
    const code = findCode(e.clipboardData.getData('text'));
    if (!code || busy) return;
    e.preventDefault();
    setInvite(code);
    void join(code);
  }

  // Without a code, BlockOps looks for the mod on this computer by itself.
  async function detectWorld(pairingCode: string | null) {
    setHostPairingCode(''); setBusy(true); setWorldError(''); setWorldFeedback('');
    try {
      const world = await invoke<{id:number;port:number;name?:string|null;version?:string|null}>('detect_open_world', { pairingCode });
      const described = describeWorld(world.name, world.version);
      setWorldWatchId(world.id); setLanPort(String(world.port));
      setWorldFeedback(`Порт получен из Minecraft${described ? `: ${described}` : ''}. BlockOps отслеживает, открыт ли мир.`);
    } catch (failure) { setWorldWatchId(null); setLanPort(''); setWorldError(String(failure)); }
    finally { setBusy(false); }
  }

  async function startExternal(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError('');
    try {
      const port = Number(lanPort);
      if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('Укажите порт от 1 до 65535.');
      setSession(await invoke<Session>('host', { target: `127.0.0.1:${port}`, relay: direct ? null : relay.trim() || null, worldWatchId, direct }));
      window.setTimeout(() => sessionRef.current?.scrollIntoView({ behavior: 'auto' }), 100);
      // One click: the first friend's invitation is ready, and copied, as soon as the connection is.
      let created: CodeStatus | null = null;
      try { created = await invoke<CodeStatus>('create_code', { coordinator: coordinator.trim() }); }
      catch (e) { setError(`Соединение готово, но код для друга не создан: ${explainError(e)}`); }
      if (created) {
        try {
          await navigator.clipboard.writeText(invitationText(created.code, created.expires_at));
          setInviteNotice('Приглашение скопировано: вставьте его в чат с другом.');
        } catch { setInviteNotice('Скопируйте приглашение кнопкой ниже и отправьте его другу.'); }
      }
    }
    catch (e) { setError(explainError(e)); }
    finally { setBusy(false); }
  }

  // Works in any version: the saved server points at the guest's fixed address.
  async function addServerEntry() {
    setBusy(true); setError(''); setServerEntry('');
    try {
      const result = await invoke<{ added: number; present: number; failed: number }>('add_server_entry');
      const failed = result.failed ? ` В ${result.failed} папк${result.failed === 1 ? 'е' : 'ах'} список серверов прочитать не удалось.` : '';
      setServerEntry((result.added > 0
        ? 'Готово: в «Сетевой игре» появился сервер «BlockOps — мир друга». Если этот экран уже открыт, вернитесь в меню и откройте его снова.'
        : result.present > 0 ? 'Сервер «BlockOps — мир друга» уже есть в «Сетевой игре».'
        : 'Папка Minecraft не найдена. Добавьте сервер вручную с адресом выше.') + failed);
    } catch (failure) { setError(String(failure)); }
    finally { setBusy(false); }
  }

  async function joinMinecraft(pairingCode: string | null) {
    setPairingCode(''); setBusy(true); setError(''); setJoinFeedback('');
    try {
      await invoke('join_in_minecraft', { pairingCode });
      setJoinFeedback('Minecraft начал попытку подключения. Результат смотрите в игре.');
    } catch (failure) { setError(String(failure)); }
    finally { setBusy(false); }
  }

  async function stop() {
    setBusy(true);
    setError('');
    try { await invoke('stop'); setSession(null); setInviteNotice(''); setServerEntry(''); setPairingCode(''); setHostPairingCode(''); setWorldWatchId(null); setLanPort(''); setWorldFeedback(''); setJoinFeedback(''); }
    catch (e) { setError(explainError(e)); }
    finally { setBusy(false); }
  }

  const coordinatorIsLocal = (() => {
    try { return ['127.0.0.1', '[::1]', 'localhost'].includes(new URL(coordinator).hostname); }
    catch { return true; }
  })();
  const needsPublicServices = !direct && (coordinatorIsLocal || (mode === 'host' && !relay.trim()));
  const announced = local?.lan_worlds?.[0];
  const hostHint = !local || worldWatchId !== null ? 'BlockOps сам найдёт открытый для сети мир на этом компьютере в любой версии Minecraft Java.'
    : local.open_worlds > 0 ? `Найден открытый для сети мир${announced ? ` ${describeWorld(announced.name, announced.version)}` : ''}.`
    : local.running > 0 ? 'Minecraft запущен. Откройте мир для сети: BlockOps найдёт его сам.'
    : 'Откройте мир в Minecraft и нажмите «Открыть для сети»: BlockOps найдёт его сам, в любой версии.';
  const guestHint = !local ? 'С модом BlockOps откройте главное меню Minecraft на этом компьютере и нажмите кнопку: игра сама подключится к миру друга.'
    : local.running === 0 ? 'Запустите Minecraft с модом BlockOps: в главном меню игра сама подключится к миру друга. Мод можно установить в разделе «Найти мою сборку Minecraft».'
    : local.in_menu > 1 ? 'Запущено несколько копий Minecraft с модом BlockOps. Оставьте одну или войдите по коду мода.'
    : local.in_menu === 0 ? 'Откройте главное меню Minecraft: игра сама подключится к миру друга.'
    : 'Minecraft в главном меню. Если подключение не началось, нажмите кнопку.';

  return <main>
    <header className="app-header">
      <div className="brand"><div className="logo" aria-hidden="true"><svg viewBox="0 0 32 32" fill="none"><path d="M16 3 27 9.2v13.5L16 29 5 22.7V9.2L16 3Z" stroke="currentColor" strokeWidth="2"/><path d="m5.8 9.7 10.2 6 10.2-6M16 15.7v12" stroke="currentColor" strokeWidth="2"/></svg></div><strong>BlockOps</strong></div>
      <div className="header-meta"><span>Minecraft Java</span><span className="badge">Ранняя версия</span></div>
    </header>
    <section className="intro">
      <div><h1>Игра начинается здесь</h1><p>Откройте свой мир Minecraft и пригласите друзей играть вместе.</p></div>
      <span className="edition">Minecraft Java</span>
    </section>
    {!desktop && <p role="alert" className="error">Это просмотр интерфейса в браузере. Для управления сервером необходимо открыть приложение BlockOps.</p>}

    <nav className="main-actions" aria-label="Основные действия">
      <button type="button" className={mode === 'host' ? 'selected' : ''} aria-pressed={mode === 'host'} onClick={() => { setMode('host'); setError(''); }}><span className="action-icon" aria-hidden="true"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7"><path d="M12 4v16M4 12h16"/></svg></span><span className="action-copy"><strong>Открыть свой мир</strong><small>Играйте вместе в уже созданном мире</small></span></button>
      <button type="button" className={mode === 'join' ? 'selected' : ''} aria-pressed={mode === 'join'} onClick={() => { setMode('join'); setError(''); }}><span className="action-icon" aria-hidden="true"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7"><path d="M4 12h13m-5-5 5 5-5 5M19 4v16"/></svg></span><span className="action-copy"><strong>Присоединиться</strong><small>Войти в мир друга по коду</small></span></button>
    </nav>


    {needsPublicServices && <p className="notice">Для игры через интернет пока нужны отдельно развёрнутые сервисы. Их адреса можно указать в дополнительных настройках. Локальная проверка доступна на одном компьютере.</p>}

    <div className="workspace"><div className="workspace-main">
    <div hidden={mode !== 'host'}>
      <section className="card lan-card">
        <h2>Пригласите друга в свой мир</h2>
        <p>Откройте свой мир в Minecraft Java и выберите «Открыть для сети».</p>
        <div className="world-detection">
          <p>{hostHint}</p>
          <button type="button" disabled={!desktop || busy || session !== null} onClick={() => void detectWorld(null)}>Найти открытый мир</button>
          {worldError && <p role="alert" className="error">{worldError}</p>}
          {worldFeedback && <p role="status">{worldFeedback}</p>}
          <details>
            <summary>Ввести код мода вручную</summary>
            <p>Если мир не находится, в открытом мире введите команду /blockops, скопируйте код мода и вставьте его сюда. Код одноразовый, действует 5 минут и предназначен только для этого компьютера.</p>
            <form onSubmit={e => { e.preventDefault(); const code = hostPairingCode.trim(); if (code) void detectWorld(code); }}>
              <label htmlFor="host-pairing-code">Код мода на этом компьютере</label>
              <input id="host-pairing-code" type="password" maxLength={70} autoComplete="off" spellCheck={false} value={hostPairingCode} onChange={e=>setHostPairingCode(e.target.value)} disabled={busy || session !== null}/>
              <button disabled={!desktop || busy || session !== null || !hostPairingCode.trim()}>Определить порт</button>
            </form>
          </details>
        </div>
        <form onSubmit={startExternal}>
          <label htmlFor="lan-port">Порт мира Minecraft</label>
          <input id="lan-port" type="number" min={1} max={65535} step={1} required value={lanPort} onChange={e => {setLanPort(e.target.value);setWorldWatchId(null);setWorldFeedback('');}} placeholder="Из мода или сообщения Minecraft" disabled={busy || session !== null}/>
          <p>Открывая мир для сети, вы также делаете его доступным в вашей локальной сети согласно настройкам Minecraft и Windows. {worldWatchId === null ? 'Если порт указан вручную, отключите соединение после закрытия мира.' : 'BlockOps остановит соединение при обнаружении закрытия мира или потери связи с модом. Проверка выполняется с задержкой, поэтому после выхода из мира остановите соединение сразу.'}</p>
          <button className="primary" disabled={!desktop || busy || session !== null}>{busy ? (direct ? 'Подключаемся к сети, это до минуты…' : 'Готовим приглашение…') : 'Пригласить друга'}</button>
          {session && <p>Чтобы открыть другой мир, сначала отключите текущее соединение.</p>}
        </form>
      </section>
      <details className="managed-servers"><summary>Отдельные серверы BlockOps</summary><Servers desktop={desktop} relay={direct ? '' : relay} direct={direct} createSignal={0} onInvite={() => void refreshSession()}/></details>
    </div>
    {mode === 'join' && !session && <form className="card join-card" onSubmit={e => { e.preventDefault(); void join(invite.trim()); }}>

        <h2>Войдите в мир друга</h2>
        <p>Скопируйте приглашение от друга и вставьте его сюда целиком: BlockOps найдёт код и подключится сам. Код можно ввести и вручную.</p>
        <label htmlFor="invite">Приглашение или код</label>
        <input id="invite" required maxLength={32} autoComplete="off" spellCheck={false} value={invite} onChange={e => setInvite(e.target.value)} onPaste={pasteInvitation} placeholder="Приглашение от друга или код" />
        <p>Код можно использовать один раз. После перезапуска BlockOps потребуется новый.</p>
        <button className="primary" disabled={!desktop || busy || !invite.trim()}>{busy ? 'Подготавливаем подключение…' : 'Подключиться'}</button>
      </form>}
    </div><aside className="guide" aria-label="Как начать">
      <h2>{mode === 'host' ? 'Как начать' : 'Как присоединиться'}</h2>
      {mode === 'host' ? <ol><li><strong>Откройте свой мир</strong><span>Запустите Minecraft как обычно.</span></li><li><strong>Откройте для сети</strong><span>BlockOps найдёт мир сам, в любой версии Minecraft Java.</span></li><li><strong>Пригласите друга</strong><span>Нажмите «Пригласить друга» и отправьте ему код.</span></li></ol> : <ol><li><strong>Получите приглашение</strong><span>Друг пришлёт его в мессенджере.</span></li><li><strong>Вставьте его целиком</strong><span>BlockOps найдёт код и подключится сам.</span></li><li><strong>Откройте Minecraft</strong><span>С модом BlockOps игра подключится сама; без мода выберите сервер «BlockOps» в «Сетевой игре».</span></li></ol>}
    </aside></div>

    {session && mode !== session.mode && <p className="notice">Активное соединение останется открытым. Чтобы создать новое, сначала отключите текущее.</p>}
    {session && <section className="card session-card" ref={sessionRef} aria-live="polite">
      {session.mode === 'host' ? <>
        <h2>Пригласите друзей</h2>
        <p>Соединение подготовлено. Доступность мира проверяется при подключении игрока. Создайте отдельный код для каждого друга и не публикуйте его.</p>
        {inviteNotice && <p role="status" className="success">{inviteNotice}</p>}
        <Codes coordinator={coordinator.trim()} direct={direct}/>
      </> : <>
        <h2>Откройте Minecraft</h2>
        <p role="status">{session.transport === 'direct' ? 'Соединение с другом установлено напрямую.' : session.transport === 'relay' ? 'Соединение с другом установлено через relay.' : 'Устанавливаем соединение с другом…'}</p>
        {session.endpoint && <><label>Адрес сервера для Minecraft</label><code className="endpoint">{session.endpoint}</code></>}
        {session.endpoint && session.endpoint !== GUEST_ADDRESS && <p className="notice">Постоянный адрес {GUEST_ADDRESS} занят другой программой, поэтому сейчас адрес другой. Сохранённый сервер BlockOps заработает, когда адрес освободится.</p>}
        <p>В любой версии Minecraft Java: откройте «Сетевая игра» и выберите сервер «BlockOps — мир друга». Адрес постоянный, поэтому сервер достаточно добавить один раз.</p>
        <button type="button" disabled={!desktop || busy} onClick={() => void addServerEntry()}>Добавить сервер в Minecraft</button>
        {serverEntry && <p role="status">{serverEntry}</p>}
        <div className="minecraft-join">
          <h3>Войти через мод BlockOps</h3>
          <p>{guestHint}</p>
          <button type="button" className="primary" disabled={!desktop || busy} onClick={() => void joinMinecraft(null)}>Войти в мир</button>
          {joinFeedback && <p role="status">{joinFeedback}</p>}
          <details>
            <summary>Ввести код мода вручную</summary>
            <p>Если Minecraft не находится, в главном меню игры нажмите BlockOps, скопируйте код и вставьте его сюда. Код действует один раз — не передавайте его другим.</p>
            <form onSubmit={e => { e.preventDefault(); const code = pairingCode.trim(); if (code) void joinMinecraft(code); }}>
              <label htmlFor="pairing-code">Код сопряжения с Minecraft</label>
              <input id="pairing-code" type="password" maxLength={70} autoComplete="off" spellCheck={false} value={pairingCode} onChange={e => setPairingCode(e.target.value)} disabled={busy}/>
              <button disabled={!desktop || busy || !pairingCode.trim()}>Войти по коду</button>
            </form>
          </details>
        </div>
      </>}
      {session.last_error && <p role="alert" className="error">{explainError(session.last_error)}</p>}
      <details><summary>Состояние соединения</summary>
        <p>Активных игровых подключений: {session.active_connections}</p>
        {session.mode === 'join' && <p>Маршрут: {session.transport === 'relay' ? 'через relay' : session.transport === 'direct' ? 'напрямую' : 'ещё не определён'}</p>}
      </details>
      <button type="button" disabled={busy} onClick={stop}>Отключить соединение</button>
    </section>}
    <MinecraftProfiles desktop={desktop}/>
    <details className="advanced">
      <summary>Дополнительные настройки подключения</summary>
      <fieldset className="connection-choice" disabled={session !== null}>
        <legend>Как соединяться с друзьями</legend>
        <label><input type="radio" name="connection" checked={direct} onChange={() => setConnection('direct')}/> Без своего сервера</label>
        <p>Игроки находят друг друга через публичную сеть libp2p и играют напрямую. Ничего настраивать не нужно. Если сети игроков не позволяют прямое соединение (так бывает с мобильным интернетом), понадобится свой relay-сервер.</p>
        <label><input type="radio" name="connection" checked={!direct} onChange={() => setConnection('service')}/> Через свой сервис координации</label>
      </fieldset>
      {!direct && <>
        <label htmlFor="coordinator">Адрес сервиса координации</label>
        <input id="coordinator" disabled={session !== null} value={coordinator} onChange={e => setCoordinator(e.target.value)} placeholder="https://connect.example.org" spellCheck={false}/>
        <label htmlFor="relay">Адрес relay для вашего сервера</label>
        <input id="relay" disabled={session !== null} value={relay} onChange={e => setRelay(e.target.value)} placeholder="/ip4/…/tcp/4001/p2p/…" spellCheck={false}/>
        <p>Для публичного сервиса нужен HTTPS. Оба игрока указывают один адрес сервиса координации; relay указывает только хозяин. Укажите адреса до создания соединения: BlockOps запомнит их на этом компьютере.</p>
        <button type="button" disabled={session !== null || (coordinator === DEFAULT_COORDINATOR && relay === '')} onClick={() => { setCoordinator(DEFAULT_COORDINATOR); setRelay(''); }}>Вернуть локальные настройки</button>
      </>}
      <p>Приглашение само определяет способ: друг может войти по коду любого вида.</p>
      {session && <p>Чтобы изменить адреса, сначала отключите текущее соединение.</p>}

    </details>


    {error && <p role="alert" className="error">{error}</p>}
    <footer><span>BlockOps</span><span>Сделано для совместной игры</span></footer>
  </main>;
}

createRoot(document.getElementById('root')!).render(<React.StrictMode><App/></React.StrictMode>);
