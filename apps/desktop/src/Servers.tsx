import { useEffect, useRef, useState, type FormEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Backups } from './Backups';
import { EditSettings } from './EditSettings';
import { Console } from './Console';
import { Whitelist } from './Whitelist';
type Server = {id:string; version:string; settings:{name:string;memory_mb:number;port:number;difficulty:string;max_players:number;whitelist:boolean;game_mode:string|null;pvp:boolean|null}};
type Row = {server:Server;process:null|{phase:string;logs:string[];error:string|null;exit_code:number|null}};
const phases:Record<string,string>={starting:'Запускается',running:'Работает',stopping:'Сохраняется и останавливается',stopped:'Остановлен',failed:'Ошибка запуска или работы'};
function errorHint(message:string):string {
  if (message.includes('authentication keys')) return 'Minecraft не смог получить ключи для проверки игроков. Проверьте доступ к сетевым службам Minecraft.';
  if (message.includes('Check world integrity')) return 'Сервер остановлен принудительно. Проверьте состояние мира перед продолжением игры.';
  if (message.includes('Server port is already occupied')) return 'Этот порт уже занят. Остановите другую программу или выберите другой порт в настройках сервера.';
  if (message.includes('Cannot resolve official Minecraft metadata') || message.includes('Cannot install verified Java')) return 'Не удалось подготовить файлы сервера. Проверьте подключение к интернету и попробуйте снова.';
  if (message.includes('Managed properties changed')) return 'Настройки сервера были изменены вне BlockOps. Верните сохранённые настройки перед запуском.';
  if (message.includes('Interrupted world restore')) return 'Необходимо завершить восстановление мира в разделе резервных копий.';
  return 'Не удалось выполнить действие. Подробности помогут найти причину.';
}
function ActionError({message}:{message:string}) {
  return <div className="error action-error" role="alert"><strong>{errorHint(message)}</strong><details><summary>Технические подробности</summary><code>{message}</code></details></div>;
}
export function Servers({desktop,onInvite,relay,direct,createSignal}:{desktop:boolean;onInvite:()=>void;relay:string;direct:boolean;createSignal:number}) {
  const [rows,setRows]=useState<Row[]>([]);
  const [loaded,setLoaded]=useState(false);
  const [listError,setListError]=useState('');
  const [version,setVersion]=useState('1.21.1');
  const [name,setName]=useState('Мой мир'); const [memory,setMemory]=useState(2048);
  const [port,setPort]=useState(25565); const [players,setPlayers]=useState(10);
  const [whitelist,setWhitelist]=useState(false);
  const [gameMode,setGameMode]=useState('survival'); const [pvp,setPvp]=useState(true);
  const [difficulty,setDifficulty]=useState('normal'); const [accepted,setAccepted]=useState(false);
  const [error,setError]=useState<{message:string;id:string|null}|null>(null); const [busy,setBusy]=useState(false);
  const [notice,setNotice]=useState(''); const requestSequence=useRef(0); const lastApplied=useRef(0);
  const [createOpen,setCreateOpen]=useState(false);
  const [creating,setCreating]=useState(false);
  async function refresh(){const sequence=++requestSequence.current;try{const result=await invoke<Row[]>('servers');if(sequence>lastApplied.current){lastApplied.current=sequence;setRows(result);setListError('');setLoaded(true);}}catch(e){if(sequence>lastApplied.current){lastApplied.current=sequence;setListError(String(e));setLoaded(true);}throw e;}}
  useEffect(()=>{if(createSignal > 0)setCreateOpen(true);},[createSignal]);
  useEffect(()=>{if(!desktop)return;const poll=()=>refresh().catch(()=>{});void poll();const timer=setInterval(poll,1500);return()=>{clearInterval(timer);lastApplied.current=++requestSequence.current;};},[desktop]);
  async function action(command:string,args:Record<string,unknown>={}) {
    setBusy(true);setError(null);setNotice('');try{await invoke(command,args);if(command==='invite_server')onInvite();try{await refresh();}catch{/* Keep a successful operation successful; the list has its own error. */}return true;}catch(e){setError({message:String(e),id:typeof args.id==='string'?args.id:null});return false;}finally{setBusy(false);}
  }
  async function create(e:FormEvent){e.preventDefault();setCreating(true);const ok=await action('create_server',{settings:{name,memory_mb:memory,port,max_players:players,difficulty,whitelist,game_mode:gameMode,pvp},acceptEula:accepted,version});setCreating(false);if(ok){setAccepted(false);setCreateOpen(false);setNotice('Сервер создан. Запустите его, чтобы подготовить мир.');}}
  return <section className="servers"><h2>Ваши серверы</h2><p>Vanilla с Java 21. Каждый мир хранится отдельно.</p>
    {!desktop ? <p>Список серверов доступен в приложении.</p> : !loaded ? <p>Загружаем список серверов…</p> : listError ? <p role="alert" className="error">Не удалось получить список серверов: {listError}</p> : rows.length===0 && <p>У вас пока нет серверов. Создайте первый мир.</p>}
    {notice && <p className="server-notice" role="status">{notice}</p>}
    {rows.map(({server,process})=><article className="card server" key={server.id}>
      <div className="server-heading"><div><h3>{server.settings.name}</h3><p>Vanilla {server.version} · {server.settings.memory_mb} МиБ · порт {server.settings.port}</p></div><span className={`server-status status-${process?.phase ?? 'stopped'}`}>{phases[process?.phase??'stopped'] ?? 'Состояние неизвестно'}</span></div>
      {process?.phase==='starting' && <p role="status">Minecraft готовит мир. Это может занять несколько минут.</p>}
      {process?.phase==='stopping' && <p role="status">Дождитесь завершения сохранения мира.</p>}
      {error?.id===server.id && <ActionError message={error.message}/> }
      <div className="actions"><button disabled={!desktop||busy||!!listError||!!process&&!['stopped','failed'].includes(process.phase)} onClick={()=>action('start_server',{id:server.id})}>Запустить</button>
      <button disabled={busy||!!listError||!process||['stopped','failed','stopping'].includes(process.phase)} onClick={()=>action('stop_server',{id:server.id})}>Остановить</button>
      <button disabled={busy||!!listError||process?.phase!=='running'} onClick={()=>action('invite_server',{id:server.id,relay:relay.trim()||null,direct})}>Пригласить друга</button></div>
      <div className="server-tools"><EditSettings server={server} running={!!listError || !!process && !['stopped','failed'].includes(process.phase)} desktop={desktop} onSaved={refresh}/><Backups serverId={server.id} running={!!listError || !!process && !['stopped','failed'].includes(process.phase)} desktop={desktop}/></div>
      <Whitelist id={server.id} enabled={server.settings.whitelist} running={!listError && process?.phase==='running'}/>
      {process?.error&&<ActionError message={process.error}/>}
      {process&&<details><summary>Консоль, последние 200 строк</summary><pre className="console">{process.logs.join('\n')||'Ожидаем вывод Java…'}</pre><Console id={server.id} running={!listError && process.phase==='running'}/></details>}
    </article>)}
    <button type="button" className="new-server" disabled={creating} onClick={()=>setCreateOpen(!createOpen)} aria-expanded={createOpen}>{createOpen ? "Закрыть создание сервера" : "＋ Создать новый сервер"}</button>
    {createOpen && <form className="card create-card" onSubmit={create}>
      <div className="create-heading"><div><h3>Новый сервер</h3><p>Выберите версию и название. Остальные параметры можно изменить позже.</p></div></div>
      {creating && <p className="server-notice" role="status">Скачиваем и проверяем файлы Minecraft и Java. Не закрывайте приложение до завершения.</p>}
      {error && error.id===null && <ActionError message={error.message}/>}
      <fieldset disabled={creating}>
      <label htmlFor="server-version">Версия Minecraft</label><select id="server-version" value={version} onChange={e=>setVersion(e.target.value)}><option value="1.21.1">Vanilla 1.21.1</option><option value="1.21.4">Vanilla 1.21.4</option></select>
      <label htmlFor="server-name">Название</label><input id="server-name" required maxLength={60} value={name} onChange={e=>setName(e.target.value)}/>
      <details className="create-advanced" onInvalid={e=>{e.currentTarget.open=true;}}><summary>Настройки мира и сервера</summary><div className="settings-grid"><div><label htmlFor="ram">Память, МиБ</label><input id="ram" type="number" min={1024} max={16384} step={256} required value={memory} onChange={e=>setMemory(Number(e.target.value))}/></div>
      <div><label htmlFor="port">Локальный порт</label><input id="port" type="number" min={1024} max={65535} required value={port} onChange={e=>setPort(Number(e.target.value))}/></div>
      <div><label htmlFor="players">Максимум игроков</label><input id="players" type="number" min={1} max={100} required value={players} onChange={e=>setPlayers(Number(e.target.value))}/></div>
      <div><label htmlFor="difficulty">Сложность</label><select id="difficulty" value={difficulty} onChange={e=>setDifficulty(e.target.value)}><option value="peaceful">Мирная</option><option value="easy">Лёгкая</option><option value="normal">Обычная</option><option value="hard">Сложная</option></select></div></div>
      <label htmlFor="game-mode">Режим игры для новых игроков</label><select id="game-mode" value={gameMode} onChange={e=>setGameMode(e.target.value)}><option value="survival">Выживание</option><option value="creative">Творческий</option><option value="adventure">Приключение</option><option value="spectator">Наблюдение</option></select>
      <label className="checkbox"><input type="checkbox" checked={pvp} onChange={e=>setPvp(e.target.checked)}/> Игроки могут наносить урон друг другу (PvP)</label>
      <label className="checkbox"><input type="checkbox" checked={whitelist} onChange={e=>setWhitelist(e.target.checked)}/> Разрешать вход только игрокам из списка допуска</label>
      {whitelist && <p>После запуска добавьте имена игроков в список допуска. Пока он пуст, обычные игроки не смогут войти.</p>}
      </details>
      <p>Скачаем сервер Mojang и Java Eclipse Temurin с проверкой хешей. Потребуется несколько сотен мегабайт; создание мира дополнительно занимает место.</p>
      <p><button type="button" disabled={!desktop} onClick={()=>{void invoke('open_eula').catch(e=>setError({message:String(e),id:null}));}}>Открыть Minecraft EULA в браузере</button></p><code className="eula-url">https://www.minecraft.net/eula</code>
      <label className="checkbox"><input type="checkbox" checked={accepted} onChange={e=>setAccepted(e.target.checked)}/> Я прочитал(а) и принимаю Minecraft EULA для этого сервера.</label>
      </fieldset>
      <button className="primary" disabled={!desktop||busy||!accepted}>{creating?'Загрузка и проверка компонентов…':'Создать сервер'}</button>
    </form>}
    {error && error.id===null && !createOpen && <ActionError message={error.message}/>}
  </section>;
}
