import React, { useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { invoke } from '@tauri-apps/api/core';
import './style.css';
import { Servers } from './Servers';
import { Codes } from './Codes';

type Session = {
  mode: string;
  endpoint: string | null;
  active_connections: number;
  last_error: string | null;
  transport: string | null;
};

function explainError(error: unknown): string {
  const message = String(error);
  if (message.includes('404')) return 'Код не найден, уже использован или срок его действия истёк. Попросите друга создать новый.';
  if (message.includes('429')) return 'Слишком много попыток. Подождите минуту и попробуйте ещё раз.';
  if (message.includes('504')) return 'Сервер друга не подтвердил подключение. Попросите его создать новый код.';
  return message;
}

function App() {
  const [mode, setMode] = useState<'host' | 'join'>('host');
  const [createSignal, setCreateSignal] = useState(0);
  const [target, setTarget] = useState('127.0.0.1:25565');
  const [relay, setRelay] = useState('');
  const [invite, setInvite] = useState('');
  const [coordinator, setCoordinator] = useState('http://127.0.0.1:8080');
  const [session, setSession] = useState<Session | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const sessionRef = useRef<HTMLElement>(null);
  const desktop = '__TAURI_INTERNALS__' in window;

  useEffect(() => {
    if (!desktop) return;
    const poll = () => invoke<Session | null>('status').then(setSession).catch(e => setError(explainError(e)));
    void poll();
    const timer = setInterval(poll, 1000);
    return () => clearInterval(timer);
  }, [desktop]);

  const refreshSession = async () => {
    try {
      setSession(await invoke<Session | null>('status'));
      setMode('host');
      // Wait for React to insert the invitation panel before navigating to it.
      window.setTimeout(() => sessionRef.current?.scrollIntoView({ behavior: 'auto' }), 100);
    } catch (e) { setError(explainError(e)); }
  };

  async function join(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError('');
    try { setSession(await invoke<Session>('join_code', { code: invite.trim(), coordinator: coordinator.trim() })); }
    catch (e) { setError(explainError(e)); }
    finally { setBusy(false); }
  }

  async function startExternal(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError('');
    try { setSession(await invoke<Session>('host', { target, relay: relay.trim() || null })); }
    catch (e) { setError(explainError(e)); }
    finally { setBusy(false); }
  }

  async function stop() {
    setBusy(true);
    setError('');
    try { await invoke('stop'); setSession(null); }
    catch (e) { setError(explainError(e)); }
    finally { setBusy(false); }
  }

  const coordinatorIsLocal = (() => {
    try { return ['127.0.0.1', '[::1]', 'localhost'].includes(new URL(coordinator).hostname); }
    catch { return true; }
  })();
  const needsPublicServices = coordinatorIsLocal || (mode === 'host' && !relay.trim());

  return <main>
    <header className="app-header">
      <div className="brand"><div className="logo" aria-hidden="true"><svg viewBox="0 0 32 32" fill="none"><path d="M16 3 27 9.2v13.5L16 29 5 22.7V9.2L16 3Z" stroke="currentColor" strokeWidth="2"/><path d="m5.8 9.7 10.2 6 10.2-6M16 15.7v12" stroke="currentColor" strokeWidth="2"/></svg></div><strong>BlockOps</strong></div>
      <div className="header-meta"><span>Minecraft Java</span><span className="badge">Ранняя версия</span></div>
    </header>
    <section className="intro">
      <div><h1>Игра начинается здесь</h1><p>Свой сервер для вашей компании. Просто создайте мир и пригласите друзей.</p></div>
      <span className="edition">Vanilla</span>
    </section>
    {!desktop && <p role="alert" className="error">Это просмотр интерфейса в браузере. Для управления сервером необходимо открыть приложение BlockOps.</p>}

    <nav className="main-actions" aria-label="Основные действия">
      <button type="button" className={mode === 'host' ? 'selected' : ''} aria-pressed={mode === 'host'} onClick={() => { setMode('host'); setCreateSignal(value => value + 1); setError(''); }}><span className="action-icon" aria-hidden="true"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7"><path d="M12 4v16M4 12h16"/></svg></span><span className="action-copy"><strong>Создать сервер</strong><small>Новый мир на вашем компьютере</small></span></button>
      <button type="button" className={mode === 'join' ? 'selected' : ''} aria-pressed={mode === 'join'} onClick={() => { setMode('join'); setError(''); }}><span className="action-icon" aria-hidden="true"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7"><path d="M4 12h13m-5-5 5 5-5 5M19 4v16"/></svg></span><span className="action-copy"><strong>Присоединиться</strong><small>Войти в мир друга по коду</small></span></button>
    </nav>


    {needsPublicServices && <p className="notice">Для игры через интернет пока нужны отдельно развёрнутые сервисы. Их адреса можно указать в дополнительных настройках. Локальная проверка доступна на одном компьютере.</p>}

    <div className="workspace"><div className="workspace-main">
    {mode === 'host' ? <Servers desktop={desktop} relay={relay} createSignal={createSignal} onInvite={() => void refreshSession()}/> :
      !session && <form className="card join-card" onSubmit={join}>

        <h2>Войдите в мир друга</h2>
        <p>Попросите друга создать приглашение и передать вам код.</p>
        <label htmlFor="invite">Код приглашения</label>
        <input id="invite" required maxLength={32} autoComplete="off" spellCheck={false} value={invite} onChange={e => setInvite(e.target.value)} placeholder="XXXX-XXXX-XXXX" />
        <p>Код можно использовать один раз. После перезапуска BlockOps потребуется новый.</p>
        <button className="primary" disabled={!desktop || busy || !invite.trim()}>{busy ? 'Подготавливаем подключение…' : 'Подключиться'}</button>
      </form>}
    </div><aside className="guide" aria-label="Как начать">
      <h2>{mode === 'host' ? 'Как начать' : 'Как присоединиться'}</h2>
      {mode === 'host' ? <ol><li><strong>Создайте сервер</strong><span>Выберите настройки для вашего мира.</span></li><li><strong>Запустите мир</strong><span>Дождитесь завершения запуска.</span></li><li><strong>Пригласите друзей</strong><span>Создайте и отправьте им код.</span></li></ol> : <ol><li><strong>Получите код</strong><span>Попросите друга прислать приглашение.</span></li><li><strong>Введите приглашение</strong><span>BlockOps подготовит адрес сервера.</span></li><li><strong>Откройте Minecraft</strong><span>Добавьте полученный адрес в сетевой игре.</span></li></ol>}
    </aside></div>

    {session && mode !== session.mode && <p className="notice">Активное соединение останется открытым. Чтобы создать новое, сначала отключите текущее.</p>}
    {session && <section className="card session-card" ref={sessionRef} aria-live="polite">
      {session.mode === 'host' ? <>
        <h2>Пригласите друзей</h2>
        <p>Создайте отдельный код для каждого друга. Не публикуйте код: его может использовать тот, кто увидит его первым.</p>
        <Codes coordinator={coordinator.trim()}/>
      </> : <>
        <h2>Откройте Minecraft</h2>
        {session.endpoint && <><label>Адрес сервера для Minecraft</label><code className="endpoint">{session.endpoint}</code></>}
        <p>В Minecraft Java откройте «Сетевая игра», добавьте сервер и вставьте этот адрес. Доступность мира проверяется при подключении Minecraft.</p>
      </>}
      {session.last_error && <p role="alert" className="error">{explainError(session.last_error)}</p>}
      <details><summary>Состояние соединения</summary>
        <p>Активных игровых подключений: {session.active_connections}</p>
        {session.mode === 'join' && <p>Маршрут: {session.transport === 'relay' ? 'через relay' : session.transport === 'direct' ? 'напрямую' : 'ещё не определён'}</p>}
      </details>
      <button type="button" disabled={busy} onClick={stop}>Отключить соединение</button>
    </section>}
    <details className="advanced">
      <summary>Дополнительные настройки подключения</summary>
      <p>В этой версии публичные сервисы BlockOps ещё не развёрнуты. Для игры через интернет нужны адреса вашего сервиса координации и relay. Для проверки на одном компьютере можно оставить локальные настройки.</p>
      <label htmlFor="coordinator">Адрес сервиса координации</label>
      <input id="coordinator" disabled={session !== null} value={coordinator} onChange={e => setCoordinator(e.target.value)} placeholder="https://connect.example.org" spellCheck={false}/>
      <label htmlFor="relay">Адрес relay для вашего сервера</label>
      <input id="relay" disabled={session !== null} value={relay} onChange={e => setRelay(e.target.value)} placeholder="/ip4/…/tcp/4001/p2p/…" spellCheck={false}/>
      <p>Для публичного сервиса нужен HTTPS. Оба игрока указывают один адрес сервиса координации; relay указывает только хозяин. Укажите адреса до создания соединения.</p>
      {session && <p>Чтобы изменить адреса, сначала отключите текущее соединение.</p>}
      <details>
        <summary>Подключить уже запущенный сервер</summary>
        <form onSubmit={startExternal}>
          <p>Если сервер запущен отдельно, можно создать для него защищённое соединение. У сервера должны быть приняты условия Minecraft и включён online-mode.</p>
          <label htmlFor="target">Локальный адрес сервера</label>
          <input id="target" required value={target} onChange={e => setTarget(e.target.value)} />
          <button disabled={!desktop || busy || session !== null}>Создать соединение</button>
        </form>
      </details>
    </details>


    {error && <p role="alert" className="error">{error}</p>}
    <footer><span>BlockOps</span><span>Сделано для совместной игры</span></footer>
  </main>;
}

createRoot(document.getElementById('root')!).render(<React.StrictMode><App/></React.StrictMode>);
