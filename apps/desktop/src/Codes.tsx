import {useEffect,useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
type Code={id:string;code:string;expires_at:number;phase:string;peer:string|null;error:string|null};
export function Codes({coordinator}:{coordinator:string}){
 const [codes,setCodes]=useState<Code[]>([]);const[busy,setBusy]=useState(false);const[error,setError]=useState('');
 const refresh=()=>invoke<Code[]>('codes').then(setCodes);
 useEffect(()=>{void refresh().catch(e=>setError(String(e)));const timer=setInterval(()=>{void refresh().catch(e=>setError(String(e)));},1000);return()=>clearInterval(timer);},[]);
 async function act(command:string,args:Record<string,string>){setBusy(true);setError('');try{await invoke(command,args);await refresh();}catch(e){setError(String(e));}finally{setBusy(false);}}
 const labels:Record<string,string>={waiting:'Ожидает друга',active:'Доступ выдан',revoked:'Отозвано / истекло',failed:'Доступ закрыт: ошибка связи'};
 return <section><h3>Пригласите друга</h3><p>Каждый код действует до 10 минут и погашается один раз. Друг должен выбрать тот же сервис координации.</p>
 <button disabled={busy||!coordinator} onClick={()=>act('create_code',{coordinator})}>Создать одноразовый код</button>
 {codes.map(code=><article key={code.id}><code>{code.phase==='waiting'?code.code:'Код использован или закрыт'}</code><p>{labels[code.phase]}. Срок кода: {new Date(code.expires_at*1000).toLocaleTimeString()}</p>
 {code.peer&&<p className="peer">Identity друга: {code.peer}</p>}{code.error&&<p role="alert" className="error">{code.error}</p>}
 <button disabled={busy} onClick={()=>act('revoke_code',{id:code.id})}>Отозвать и отключить</button></article>)}
 {error&&<p role="alert" className="error">{error}</p>}</section>;
}
