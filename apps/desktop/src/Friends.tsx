import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { explainError } from './errors';

export type FriendsBook = { friends: { peer: string; name: string; added: number }[]; worlds: { host: string; label: string; added: number }[] };

const day = (seconds: number) => new Date(seconds * 1000).toLocaleDateString();

/** The friends book, refreshed whenever `revision` changes. */
export function useFriends(desktop: boolean, revision = 0): [FriendsBook | null, (book: FriendsBook) => void] {
  const [book, setBook] = useState<FriendsBook | null>(null);
  useEffect(() => {
    if (!desktop) return;
    void invoke<FriendsBook | null>('friends').then(value => setBook(value ?? { friends: [], worlds: [] })).catch(() => setBook(null));
  }, [desktop, revision]);
  return [book, setBook];
}

/** The host's list of friends who come in without a code. */
export function HostFriends({ desktop, revision }: { desktop: boolean; revision: number }) {
  const [book, setBook] = useFriends(desktop, revision);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  if (!book) return null;
  async function forget(peer: string) {
    setBusy(true); setError('');
    try { setBook(await invoke<FriendsBook>('forget_friend', { peer })); }
    catch (failure) { setError(explainError(failure)); }
    finally { setBusy(false); }
  }
  return <details className="friends-panel">
    <summary>Друзья, которые заходят без кода ({book.friends.length})</summary>
    {book.friends.length === 0
      ? <p>Когда друг войдёт по коду, нажмите «Запомнить друга»: в следующий раз он зайдёт без кода, пока ваш мир открыт через BlockOps.</p>
      : <><p>Они могут войти, пока ваш мир открыт через BlockOps. «Забыть» сразу отключает друга.</p>
        <ul className="friend-list">{book.friends.map(friend => <li key={friend.peer}>
          <span><strong>{friend.name}</strong><small>с {day(friend.added)}</small></span>
          <button type="button" disabled={busy} onClick={() => void forget(friend.peer)}>Забыть</button>
        </li>)}</ul></>}
    {error && <p role="alert" className="error">{error}</p>}
  </details>;
}

/** Worlds this computer may enter again without a code. */
export function FriendWorlds({ desktop, busy, onJoin }: { desktop: boolean; busy: boolean; onJoin: (host: string) => void }) {
  const [book, setBook] = useFriends(desktop);
  const [error, setError] = useState('');
  if (!book || book.worlds.length === 0) return null;
  async function forget(host: string) {
    setError('');
    try { setBook(await invoke<FriendsBook>('forget_world', { host })); }
    catch (failure) { setError(explainError(failure)); }
  }
  return <div className="friend-worlds" aria-label="Миры друзей">
    <h3>Миры друзей</h3>
    <p>Если друг запомнил вас, войдите без кода, пока его мир открыт через BlockOps.</p>
    <ul className="friend-list">{book.worlds.map(world => <li key={world.host}>
      <span><strong>{world.label || 'Мир друга'}</strong><small>с {day(world.added)}</small></span>
      <span className="friend-actions">
        <button type="button" className="primary-inline" disabled={busy} onClick={() => onJoin(world.host)}>Войти</button>
        <button type="button" disabled={busy} onClick={() => void forget(world.host)}>Забыть</button>
      </span>
    </li>)}</ul>
    {error && <p role="alert" className="error">{error}</p>}
  </div>;
}
