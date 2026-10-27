import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';

export function Whitelist({ id, enabled, running }: { id: string; enabled: boolean; running: boolean }) {
  const [player, setPlayer] = useState('');
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  async function act(action: 'add' | 'remove' | 'list') {
    setBusy(true); setMessage(''); setError('');
    try {
      await invoke('whitelist_player', { id, action, player: action === 'list' ? null : player.trim() });
      setMessage('Команда отправлена. Ответ сервера можно посмотреть в консоли ниже.');
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  }
  return <details>
    <summary>Список допуска</summary>
    <p>{enabled ? 'Вход ограничен списком допуска. Операторы сервера также могут входить.' : 'Список допуска выключен. Его можно включить в настройках остановленного сервера.'}</p>
    <p>Укажите имя игрока в Minecraft Java. Добавление и удаление выполняет сам Minecraft-сервер.</p>
    {!running && <p>Для управления списком запустите сервер.</p>}
    <label htmlFor={`whitelist-player-${id}`}>Имя игрока</label>
    <input id={`whitelist-player-${id}`} value={player} onChange={e => setPlayer(e.target.value)} maxLength={16} autoComplete="off" disabled={!running || busy} />
    <div className="actions">
      <button type="button" disabled={!running || busy || !player.trim()} onClick={() => void act('add')}>Добавить</button>
      <button type="button" disabled={!running || busy || !player.trim()} onClick={() => void act('remove')}>Удалить</button>
      <button type="button" disabled={!running || busy} onClick={() => void act('list')}>Показать список</button>
    </div>
    {message && <p role="status" className="success">{message}</p>}
    {error && <p role="alert" className="error">{error}</p>}
  </details>;
}
