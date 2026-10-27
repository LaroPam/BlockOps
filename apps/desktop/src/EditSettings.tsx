import { useState, type FormEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';

type Settings = { name: string; memory_mb: number; port: number; max_players: number; difficulty: string };
type Server = { id: string; settings: Settings };

/** The backend locks the server and validates all changes before writing them. */
export function EditSettings({ server, running, desktop, onSaved }: {
  server: Server; running: boolean; desktop: boolean; onSaved: () => Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState<Settings>(server.settings);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  function edit() { setDraft({ ...server.settings }); setError(''); setOpen(true); }
  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true); setError('');
    try {
      await invoke('update_settings', { id: server.id, settings: draft });
      setOpen(false);
      // Refresh has its own error state; it must not turn a successful save into a failure.
      try { await onSaved(); } catch { /* The server list reports refresh errors. */ }
    } catch (failure) { setError(String(failure)); }
    finally { setBusy(false); }
  }
  if (!open) return <button type="button" disabled={!desktop || running} onClick={edit}>Настройки</button>;
  return <form className="card" onSubmit={submit}>
    <h3>Настройки сервера</h3>
    <p>Изменения применятся при следующем запуске. Для безопасности сервер нужно остановить.</p>
    <label htmlFor={`edit-name-${server.id}`}>Название</label>
    <input id={`edit-name-${server.id}`} required maxLength={60} value={draft.name}
      onChange={e => setDraft({ ...draft, name: e.target.value })} />
    <div className="settings-grid">
      <div><label htmlFor={`edit-memory-${server.id}`}>Память, МиБ</label>
        <input id={`edit-memory-${server.id}`} type="number" required min={1024} max={16384} step={256}
          value={draft.memory_mb} onChange={e => setDraft({ ...draft, memory_mb: Number(e.target.value) })} /></div>
      <div><label htmlFor={`edit-port-${server.id}`}>Локальный порт</label>
        <input id={`edit-port-${server.id}`} type="number" required min={1024} max={65535}
          value={draft.port} onChange={e => setDraft({ ...draft, port: Number(e.target.value) })} /></div>
      <div><label htmlFor={`edit-players-${server.id}`}>Максимум игроков</label>
        <input id={`edit-players-${server.id}`} type="number" required min={1} max={100}
          value={draft.max_players} onChange={e => setDraft({ ...draft, max_players: Number(e.target.value) })} /></div>
      <div><label htmlFor={`edit-difficulty-${server.id}`}>Сложность</label>
        <select id={`edit-difficulty-${server.id}`} value={draft.difficulty}
          onChange={e => setDraft({ ...draft, difficulty: e.target.value })}>
          <option value="peaceful">Мирная</option><option value="easy">Лёгкая</option>
          <option value="normal">Обычная</option><option value="hard">Сложная</option>
        </select></div>
    </div>
    {error && <p role="alert" className="error">{error}</p>}
    <div className="actions"><button type="submit" disabled={busy || running || !desktop}>Сохранить</button>
      <button type="button" disabled={busy} onClick={() => setOpen(false)}>Отмена</button></div>
  </form>;
}
