import { useEffect, useState, type ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';

type Profile = { name: string; game_dir: string; minecraft_version: string | null; loader: string; metadata_complete: boolean };
type Target = { loader: string; minecraft: string };
type ModStatus = { bundled: string | null; installed: string[]; current: boolean };
/** A launcher profile the bundled mod fits, with what is installed there now. */
type ModProfile = { profile: Profile; target: Target; status: ModStatus };
/** What the main screen knows about the world opened to LAN; null while it cannot tell. */
export type WorldReadiness = { found: boolean; label: string; hosting: boolean } | null;

const LOADERS: Record<string, string> = { fabric: 'Fabric', forge: 'Forge', neoforge: 'NeoForge', vanilla: 'Vanilla' };
const MARKS = { ok: ['✓', 'Готово'], todo: ['!', 'Нужно сделать'], optional: ['○', 'Необязательно'], wait: ['…', 'Проверяем'] } as const;
type Mark = keyof typeof MARKS;

/** "Fabric 1.21.1", or the profile's own name when its build is unknown. */
function describeProfile(profile: Profile): string {
  return [LOADERS[profile.loader], profile.minecraft_version].filter(Boolean).join(' ') || profile.name;
}

function Item({ mark, title, children }: { mark: Mark; title: string; children?: ReactNode }) {
  const [symbol, meaning] = MARKS[mark];
  return <li className={`ready-item ready-${mark}`}>
    <span className="ready-mark" aria-hidden="true">{symbol}</span>
    <div><strong>{title}<span className="sr-only">: {meaning}</span></strong>{children}</div>
  </li>;
}

/** One look at everything a game needs: Minecraft, the world, the server entry and the mod. */
export function Readiness({ desktop, mode, world, revision }: { desktop: boolean; mode: 'host' | 'join'; world: WorldReadiness; revision: number }) {
  const [profiles, setProfiles] = useState<Profile[] | null>(null);
  const [targets, setTargets] = useState<Target[]>([]);
  const [mods, setMods] = useState<ModProfile[]>([]);
  const [entry, setEntry] = useState<boolean | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');

  async function check() {
    const [found, bundled, present] = await Promise.all([
      invoke<Profile[] | null>('discover_minecraft_profiles', { folder: null }).then(value => value ?? []).catch(() => [] as Profile[]),
      invoke<Target[] | null>('blockops_mod_targets').then(value => value ?? []).catch(() => [] as Target[]),
      invoke<boolean>('server_entry_present').catch(() => null),
    ]);
    const seen = new Set<string>();
    const fitting = found.flatMap(profile => {
      const target = bundled.find(t => t.loader === profile.loader && t.minecraft === profile.minecraft_version);
      if (!target || !profile.metadata_complete || seen.has(profile.game_dir)) return [];
      seen.add(profile.game_dir);
      return [{ profile, target }];
    });
    const statuses = await Promise.all(fitting.map(({ profile, target }) =>
      invoke<ModStatus>('blockops_mod_status', { gameDir: profile.game_dir, ...target }).then(status => ({ profile, target, status })).catch(() => null)));
    setProfiles(found); setTargets(bundled); setEntry(present);
    setMods(statuses.filter((value): value is ModProfile => value !== null));
  }

  // Checked at start, after changes made elsewhere in the app, and whenever the window comes
  // back, e.g. after installing a loader.
  useEffect(() => {
    if (!desktop) return;
    void check();
    const refresh = () => { if (document.visibilityState === 'visible') void check(); };
    window.addEventListener('focus', refresh);
    return () => window.removeEventListener('focus', refresh);
  }, [desktop, revision]);

  async function install(item: ModProfile) {
    setBusy(true); setError(''); setMessage(''); setConfirming(null);
    try {
      const status = await invoke<ModStatus>('install_blockops_mod', { folder: null, gameDir: item.profile.game_dir });
      setMods(current => current.map(m => m.profile.game_dir === item.profile.game_dir ? { ...m, status } : m));
      setMessage('Мод BlockOps установлен. Если Minecraft открыт, перезапустите его.');
    } catch (failure) { setError(String(failure)); }
    finally { setBusy(false); }
  }

  async function addServer() {
    setBusy(true); setError(''); setMessage('');
    try {
      const result = await invoke<{ added: number; present: number }>('add_server_entry');
      if (result.added + result.present === 0) setError('Папка Minecraft не найдена. Добавьте сервер вручную с адресом 127.0.0.1:25595.');
      else setEntry(true);
    } catch (failure) { setError(String(failure)); }
    finally { setBusy(false); }
  }

  if (!desktop) return null;
  const missing = mods.filter(m => !m.status.current);
  const builds = [...new Set((profiles ?? []).map(describeProfile))];
  const steps: Mark[] = [
    profiles === null ? 'wait' : profiles.length > 0 ? 'ok' : 'optional',
    mode === 'host' ? (world === null ? 'wait' : world.found ? 'ok' : 'todo') : entry === null ? 'wait' : entry ? 'ok' : 'todo',
  ];
  const remaining = steps.filter(step => step === 'todo').length;

  return <section className="readiness" aria-label="Готовность к игре">
    <h2>Готовность к игре</h2>
    <p className="ready-summary" role="status">{steps.includes('wait') ? 'Проверяем…' : remaining === 0 ? 'Всё готово к игре.' : mode === 'host' ? 'Осталось открыть мир для сети.' : 'Осталось добавить сервер в Minecraft.'}</p>
    <ul className="ready-list">
      {profiles === null ? <Item mark="wait" title="Minecraft Java"><span>Ищем сборки…</span></Item>
        : profiles.length > 0 ? <Item mark="ok" title="Minecraft Java"><span>Сборок: {profiles.length}{builds.length ? ` — ${builds.slice(0, 3).join(', ')}${builds.length > 3 ? '…' : ''}` : ''}</span></Item>
        : <Item mark="optional" title="Minecraft Java"><span>Minecraft Launcher не найден. С другим лаунчером тоже можно играть.</span></Item>}

      {mode === 'host' ? (world === null ? <Item mark="wait" title="Мир открыт для сети"><span>Проверим, когда приложение увидит Minecraft.</span></Item>
        : world.hosting ? <Item mark="ok" title="Мир открыт для сети"><span>Друзья могут подключаться.</span></Item>
        : world.found ? <Item mark="ok" title="Мир открыт для сети"><span>{world.label ? `Найден ${world.label}.` : 'Мир найден.'}</span></Item>
        : <Item mark="todo" title="Мир открыт для сети"><span>В игре: Esc → «Открыть для сети».</span></Item>)
      : entry === null ? <Item mark="wait" title="Сервер в Minecraft"><span>Проверяем список серверов…</span></Item>
        : entry ? <Item mark="ok" title="Сервер в Minecraft"><span>«BlockOps — мир друга» есть в «Сетевой игре».</span></Item>
        : <Item mark="todo" title="Сервер в Minecraft"><span>Добавьте один раз, и вход к другу будет в два клика в любой версии.</span>
          <button type="button" className="ready-action" disabled={busy} onClick={() => void addServer()}>Добавить в Minecraft</button></Item>}

      {targets.length > 0 && (mods.length === 0
        ? <Item mark="optional" title="Мод BlockOps"><span>Необязателен. {mode === 'host' ? 'Мир находится и без него.' : 'С ним игра подключается к другу сама.'} Есть для {targets.map(t => `${LOADERS[t.loader] ?? t.loader} ${t.minecraft}`).join(', ')}.</span></Item>
        : missing.length === 0 ? <Item mark="ok" title="Мод BlockOps"><span>Установлен: {mods.map(m => describeProfile(m.profile)).join(', ')}.</span></Item>
        : <Item mark="optional" title="Мод BlockOps"><span>{mode === 'host' ? 'Необязателен: мир находится и без него.' : 'С ним игра подключается к другу сама.'}</span>
          {missing.slice(0, 3).map(m => confirming === m.profile.game_dir
            ? <div key={m.profile.game_dir} className="ready-confirm" role="group" aria-label="Подтверждение установки мода">
                <span>BlockOps добавит мод {m.status.bundled} в папку mods сборки «{m.profile.name}». Другие файлы не изменятся.</span>
                <button type="button" className="ready-action" disabled={busy} onClick={() => void install(m)}>Установить</button>
                <button type="button" className="ready-action" disabled={busy} onClick={() => setConfirming(null)}>Отмена</button>
              </div>
            : <button key={m.profile.game_dir} type="button" className="ready-action" disabled={busy || !m.status.bundled} onClick={() => setConfirming(m.profile.game_dir)}>
                {m.status.installed.length ? 'Обновить' : 'Установить'} в «{m.profile.name}» ({describeProfile(m.profile)})
              </button>)}
        </Item>)}
    </ul>
    {message && <p role="status" className="success">{message}</p>}
    {error && <p role="alert" className="error">{error}</p>}
  </section>;
}
