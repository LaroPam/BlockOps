import { useEffect, useState, type FormEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';

type Profile = { name: string; game_dir: string; version_id: string | null; minecraft_version: string | null; loader: string; metadata_complete: boolean };
type Loader = 'fabric' | 'forge' | 'neoforge' | 'none' | 'unknown';
type Selection = { game_dir: string; loader: Loader };
const loaders: Record<string, string> = { fabric: 'Fabric', forge: 'Forge', neoforge: 'NeoForge', vanilla: 'Vanilla', none: 'Не установлен', ambiguous: 'Обнаружено несколько загрузчиков', unknown: 'Не знаю / не определён' };
type ModStatus = { bundled: string | null; installed: string[]; current: boolean };

/** Install, update or remove the bundled mod in one profile; the backend re-checks the profile. */
function ModControls({ folder, gameDir }: { folder: string; gameDir: string }) {
  const [status, setStatus] = useState<ModStatus | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [done, setDone] = useState('');
  useEffect(() => {
    void invoke<ModStatus>('blockops_mod_status', { gameDir }).then(setStatus).catch(failure => setError(String(failure)));
  }, [gameDir]);
  async function act(command: 'install_blockops_mod' | 'remove_blockops_mod') {
    setBusy(true); setError(''); setDone(''); setConfirming(false);
    try {
      const args = command === 'install_blockops_mod' ? { folder: folder.trim() || null, gameDir } : { gameDir };
      setStatus(await invoke<ModStatus>(command, args));
      setDone(command === 'install_blockops_mod'
        ? 'Мод BlockOps установлен. Если Minecraft открыт, перезапустите его.'
        : 'Мод BlockOps удалён. Если Minecraft открыт, перезапустите его.');
    } catch (failure) { setError(String(failure)); }
    finally { setBusy(false); }
  }
  if (!status) return error ? <p className="error" role="alert">{error}</p> : <p>Проверяем мод BlockOps…</p>;
  const outdated = status.installed.length > 0 && !status.current;
  return <div className="mod-controls">
    {status.current ? <p className="success">Мод BlockOps {status.bundled} установлен.</p>
      : outdated ? <p>Установлена другая версия мода BlockOps: {status.installed.join(', ')}.</p>
      : <p>Мод BlockOps не установлен.</p>}
    {!status.bundled && <p>В этой сборке BlockOps нет мода для установки.</p>}
    {status.bundled && !status.current && !confirming && <button type="button" disabled={busy} onClick={() => setConfirming(true)}>{outdated ? 'Обновить мод BlockOps' : 'Установить мод BlockOps'}</button>}
    {confirming && <div className="mod-confirm" role="group" aria-label="Подтверждение установки мода">
      <p>BlockOps добавит мод BlockOps {status.bundled}{outdated ? ' вместо старой версии' : ''} в папку mods сборки <code>{gameDir}</code>. Другие файлы сборки не изменятся. Fabric API отдельно устанавливать не нужно.</p>
      <div className="code-actions">
        <button type="button" className="primary-inline" disabled={busy} onClick={() => void act('install_blockops_mod')}>Установить</button>
        <button type="button" disabled={busy} onClick={() => setConfirming(false)}>Отмена</button>
      </div>
    </div>}
    {status.installed.length > 0 && !confirming && <button type="button" disabled={busy} onClick={() => void act('remove_blockops_mod')}>Удалить мод BlockOps</button>}
    {done && <p role="status">{done}</p>}
    {error && <p className="error" role="alert">{error}</p>}
  </div>;
}

export function MinecraftProfiles({desktop}:{desktop:boolean}) {
  const [folder, setFolder] = useState('');
  const [profiles, setProfiles] = useState<Profile[] | null>(null);
  const [gameDir, setGameDir] = useState('');
  const [loader, setLoader] = useState<Loader | ''>('');
  const [saved, setSaved] = useState<Selection | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (desktop) void invoke<Selection | null>('get_minecraft_selection').then(setSaved).catch(() => setError('Не удалось прочитать сохранённый выбор.'));
  }, [desktop]);
  async function scan(event:FormEvent) {
    event.preventDefault(); setBusy(true); setError(''); setProfiles(null);
    try { setProfiles(await invoke<Profile[]>('discover_minecraft_profiles', { folder: folder.trim() || null })); }
    catch (failure) { setError(String(failure)); }
    finally { setBusy(false); }
  }
  async function save(event:FormEvent) {
    event.preventDefault();
    if (!loader) return;
    setBusy(true); setError('');
    try { setSaved(await invoke<Selection>('save_minecraft_selection', { selection: { game_dir: gameDir.trim(), loader } })); }
    catch (failure) { setError(String(failure)); }
    finally { setBusy(false); }
  }
  return <details className="profiles-panel">
    <summary>Найти мою сборку Minecraft</summary>
    <p>Проверим профили официального Minecraft Launcher и их метаданные. Название сборки можно менять. Поиск ничего не меняет в Minecraft; мод BlockOps устанавливается в сборку Fabric 1.21.1 только после вашего подтверждения.</p>
    <form onSubmit={scan}>
      <label htmlFor="minecraft-folder">Папка Minecraft Launcher</label>
      <input id="minecraft-folder" value={folder} onChange={e=>setFolder(e.target.value)} placeholder="Пустое поле: стандартная папка Minecraft" disabled={busy} />
      <p>Для другой папки укажите путь, в котором находится launcher_profiles.json. Другие лаунчеры пока могут не определяться.</p>
      <button disabled={!desktop || busy}>{busy ? 'Выполняем…' : 'Найти сборки'}</button>
    </form>
    {profiles && profiles.length === 0 && <p>В этой папке не найдено профилей. Папку сборки можно указать вручную ниже.</p>}
    {profiles && profiles.length > 0 && <ul className="profile-list">{profiles.map((profile, index)=><li key={`${profile.version_id ?? ''}-${index}`}>
      <strong>{profile.name}</strong><p>Загрузчик: {loaders[profile.loader] ?? 'Не удалось определить'} · Minecraft: {profile.minecraft_version ?? 'версия не определена'}</p>
      <p>Папка игры: <code>{profile.game_dir}</code></p>
      {!profile.metadata_complete && <p>Метаданные неполные. Перед установкой понадобится уточнить сборку и загрузчик.</p>}
      {profile.loader === 'fabric' && profile.metadata_complete && profile.minecraft_version === '1.21.1'
        ? <ModControls folder={folder} gameDir={profile.game_dir}/>
        : profile.loader === 'fabric' && <p>Мод BlockOps пока поддерживает только Fabric 1.21.1.</p>}
      <button type="button" disabled={busy} onClick={()=>{setGameDir(profile.game_dir);setLoader('');}}>Выбрать эту папку</button>
    </li>)}</ul>}
    <form className="profile-choice" onSubmit={save}>
      <h3>Выбор сборки</h3>
      <p>Выберите найденную папку или укажите её вручную. Сообщите, какой загрузчик установлен; если не уверены, выберите «Не знаю».</p>
      <label htmlFor="game-dir">Папка игровой сборки</label>
      <input id="game-dir" required value={gameDir} onChange={e=>setGameDir(e.target.value)} placeholder="Полный путь к папке сборки" disabled={busy}/>
      <label htmlFor="reported-loader">Какой загрузчик установлен?</label>
      <select id="reported-loader" required value={loader} onChange={e=>setLoader(e.target.value as Loader | '')} disabled={busy}>
        <option value="">Выберите ответ</option><option value="fabric">Fabric</option><option value="forge">Forge</option><option value="neoforge">NeoForge</option><option value="none">Загрузчик не установлен</option><option value="unknown">Не знаю</option>
      </select>
      <button disabled={!desktop || busy || !gameDir.trim() || !loader}>Сохранить выбор</button>
    </form>
    {error && <p className="error" role="alert">{error}</p>}
    {saved && <p className="server-notice" role="status">Выбрана папка: {saved.game_dir}. Ваш ответ: {loaders[saved.loader] ?? 'Не определён'}. Совместимость ещё не проверена.</p>}
    <p>Выбор сохраняется в BlockOps. Мод BlockOps устанавливается из списка найденных сборок; Fabric Loader BlockOps не устанавливает.</p>
  </details>;
}
