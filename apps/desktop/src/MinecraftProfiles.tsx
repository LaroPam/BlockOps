import { useState, type FormEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';

type Profile = { name: string; game_dir: string; version_id: string | null; minecraft_version: string | null; loader: string; metadata_complete: boolean };
const loaders: Record<string, string> = { fabric: 'Fabric', forge: 'Forge', neoforge: 'NeoForge', vanilla: 'Vanilla', ambiguous: 'Обнаружено несколько загрузчиков', unknown: 'Не удалось определить' };

export function MinecraftProfiles({desktop}:{desktop:boolean}) {
  const [folder, setFolder] = useState('');
  const [profiles, setProfiles] = useState<Profile[] | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  async function scan(event:FormEvent) {
    event.preventDefault(); setBusy(true); setError(''); setProfiles(null);
    try { setProfiles(await invoke<Profile[]>('discover_minecraft_profiles', { folder: folder.trim() || null })); }
    catch (failure) { setError(String(failure)); }
    finally { setBusy(false); }
  }
  return <details className="profiles-panel">
    <summary>Найти мои сборки Minecraft</summary>
    <p>Проверим профили официального Minecraft Launcher и их метаданные. Название сборки можно менять. Ничего не устанавливаем и не изменяем.</p>
    <form onSubmit={scan}>
      <label htmlFor="minecraft-folder">Папка Minecraft</label>
      <input id="minecraft-folder" value={folder} onChange={e=>setFolder(e.target.value)} placeholder="Пустое поле: стандартная папка Minecraft" disabled={busy} />
      <p>Для другой папки укажите путь, в котором находится launcher_profiles.json. Другие лаунчеры пока могут не определяться.</p>
      <button disabled={!desktop || busy}>{busy ? 'Ищем сборки…' : 'Найти сборки'}</button>
    </form>
    {error && <p className="error" role="alert">{error}</p>}
    {profiles && profiles.length === 0 && <p>В этой папке не найдено профилей. Проверьте папку или лаунчер.</p>}
    {profiles && profiles.length > 0 && <ul className="profile-list">{profiles.map((profile, index)=><li key={`${profile.version_id ?? ''}-${index}`}>
      <strong>{profile.name}</strong><p>Загрузчик: {loaders[profile.loader] ?? 'Не удалось определить'} · Minecraft: {profile.minecraft_version ?? 'версия не определена'}</p>
      <p>Папка игры: <code>{profile.game_dir}</code></p>
      {!profile.metadata_complete && <p>Метаданные неполные. Перед установкой понадобится уточнить сборку и загрузчик.</p>}
    </li>)}</ul>}
    <p>Установка мода и загрузчика пока не доступна. Если загрузчик не определён, BlockOps не считает его отсутствующим.</p>
  </details>;
}
