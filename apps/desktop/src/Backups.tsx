import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

type Backup = { id: string; created_at: number; size_bytes: number; minecraft_version: string };

function sizeLabel(bytes: number): string {
  if (bytes < 1024 * 1024) return `${Math.max(1, Math.ceil(bytes / 1024))} КиБ`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} МиБ`;
}

export function Backups({ serverId, running, desktop }: { serverId: string; running: boolean; desktop: boolean }) {
  const [open, setOpen] = useState(false);
  const [backups, setBackups] = useState<Backup[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [message, setMessage] = useState('');
  const [confirmId, setConfirmId] = useState<string | null>(null);
  const [confirmRecovery, setConfirmRecovery] = useState(false);

  async function refresh() {
    const result = await invoke<Backup[]>('list_backups', { id: serverId });
    setBackups(result);
  }
  async function toggle() {
    if (open) { setOpen(false); setConfirmId(null); setConfirmRecovery(false); return; }
    setOpen(true); setError(''); setMessage('');
    if (!desktop) return;
    setBusy(true);
    try { await refresh(); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  async function create() {
    setBusy(true); setError(''); setMessage('');
    try {
      await invoke<Backup>('create_backup', { id: serverId });
      setMessage('Резервная копия создана.');
      await refresh();
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  async function restore(backupId: string) {
    setBusy(true); setError(''); setMessage('');
    try {
      await invoke('restore_backup', { id: serverId, backupId });
      setConfirmId(null);
      setMessage('Мир восстановлен. Предыдущая версия сохранена в папке сервера.');
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  async function recover() {
    setBusy(true); setError(''); setMessage('');
    try {
      await invoke('recover_restore', { id: serverId });
      setConfirmRecovery(false);
      setMessage('Восстановление после сбоя завершено. Проверьте мир перед запуском.');
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }

  return <div className="backup-section">
    <button type="button" onClick={() => void toggle()} aria-expanded={open}>Резервные копии</button>
    {open && <section className="backup-panel" aria-label="Резервные копии мира">
      <p>Создавайте копии и восстанавливайте мир, когда сервер остановлен. Копии хранятся на этом компьютере.</p>
      {running && <p className="notice">Сначала остановите сервер, чтобы сохранить или восстановить мир.</p>}
      <button type="button" disabled={!desktop || running || busy} onClick={() => void create()}>{busy ? 'Выполняется…' : 'Создать копию мира'}</button>
      {backups === null && !error && <p>{desktop ? 'Загружаем список копий…' : 'Список доступен в приложении.'}</p>}
      {backups !== null && backups.length === 0 && <p>Резервных копий пока нет. Если сервер ещё не запускался, сначала запустите его, чтобы создать мир.</p>}
      {backups?.map(backup => <article className="backup-item" key={backup.id}>
        <div><strong>{new Date(backup.created_at * 1000).toLocaleString('ru-RU')}</strong><p>{sizeLabel(backup.size_bytes)}, Minecraft {backup.minecraft_version}</p></div>
        {confirmId !== backup.id && <button type="button" disabled={!desktop || running || busy} onClick={() => { setConfirmId(backup.id); setConfirmRecovery(false); setError(''); setMessage(''); }}>Восстановить</button>}
        {confirmId === backup.id && <div className="backup-confirm" role="group" aria-label="Подтверждение восстановления">
          <p>Мир будет заменён выбранной копией. Текущая версия не удаляется: она сохраняется в папке сервера с именем, начинающимся на world-before-restore.</p>
          <div className="actions"><button type="button" disabled={running || busy} onClick={() => void restore(backup.id)}>Подтвердить восстановление</button><button type="button" disabled={busy} onClick={() => setConfirmId(null)}>Отмена</button></div>
        </div>}
      </article>)}
      <details><summary>Восстановление после сбоя</summary>
        <p>Если BlockOps сообщил о прерванном восстановлении, эта операция проверит файлы и попытается безопасно завершить его. Для неоднозначного состояния потребуется ручная проверка.</p>
        {!confirmRecovery ? <button type="button" disabled={!desktop || running || busy} onClick={() => { setConfirmRecovery(true); setConfirmId(null); }}>Проверить прерванное восстановление</button> : <div className="backup-confirm"><p>Проверить и восстановить состояние после сбоя?</p><div className="actions"><button type="button" disabled={running || busy} onClick={() => void recover()}>Подтвердить</button><button type="button" disabled={busy} onClick={() => setConfirmRecovery(false)}>Отмена</button></div></div>}
      </details>
      {message && <p role="status" className="success">{message}</p>}
      {error && <p role="alert" className="error">{error}</p>}
    </section>}
  </div>;
}
