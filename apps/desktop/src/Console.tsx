import { useState, type FormEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';

export function Console({id, running}:{id:string; running:boolean}) {
  const [command,setCommand] = useState('');
  const [busy,setBusy] = useState(false);
  const [error,setError] = useState('');
  async function submit(event:FormEvent) {
    event.preventDefault(); setBusy(true); setError('');
    try { await invoke('send_console', {id, command}); setCommand(''); }
    catch (e) { setError(String(e)); } finally { setBusy(false); }
  }
  return <form onSubmit={submit}>
    <label htmlFor={`command-${id}`}>Команда сервера</label>
    <div className="actions"><input id={`command-${id}`} value={command} onChange={e=>setCommand(e.target.value)}
      maxLength={1024} disabled={!running||busy} placeholder="Например: list" />
      <button disabled={!running||busy||!command.trim()}>Отправить</button></div>
    <p>Результат команды появится в журнале выше. Для остановки используйте кнопку «Остановить».</p>
    {error&&<p role="alert" className="error">{error}</p>}
  </form>;
}
