import { useEffect, useState, type FormEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';

type Profile = { name: string; game_dir: string; version_id: string | null; minecraft_version: string | null; loader: string; metadata_complete: boolean };
type Loader = 'fabric' | 'forge' | 'neoforge' | 'none' | 'unknown';
type Selection = { game_dir: string; loader: Loader };
const loaders: Record<string, string> = { fabric: 'Fabric', forge: 'Forge', neoforge: 'NeoForge', vanilla: 'Vanilla', none: 'Не установлен', ambiguous: 'Обнаружено несколько загрузчиков', unknown: 'Не знаю / не определён' };

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
    <p>Проверим профили официального Minecraft Launcher и их метаданные. Название сборки можно менять. Ничего не устанавливаем и не изменяем в Minecraft.</p>
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
    <p>Выбор сохраняется в BlockOps. Установка мода и загрузчика пока недоступна; перед ней потребуется проверить совместимость и получить ваше согласие.</p>
  </details>;
}
