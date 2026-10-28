import {useEffect,useState} from 'react';
import {invoke} from '@tauri-apps/api/core';
type Code={id:string;code:string;expires_at:number;phase:string;peer:string|null;error:string|null};
export function Codes({coordinator}:{coordinator:string}){
 const [codes,setCodes]=useState<Code[]>([]);const[busy,setBusy]=useState(false);const[error,setError]=useState('');const[copied,setCopied]=useState('');
 const refresh=()=>invoke<Code[]>('codes').then(setCodes);
 useEffect(()=>{void refresh().catch(e=>setError(String(e)));const timer=setInterval(()=>{void refresh().catch(e=>setError(String(e)));},1000);return()=>clearInterval(timer);},[]);
 const copy=(code:Code)=>navigator.clipboard.writeText(code.code).then(()=>setCopied(code.id),()=>setError('Не удалось скопировать код. Выделите его и скопируйте вручную.'));
 async function act(command:string,args:Record<string,string>){setBusy(true);setError('');try{await invoke(command,args);await refresh();}catch(e){setError(String(e));}finally{setBusy(false);}}
 const labels:Record<string,string>={waiting:'Ожидает друга',active:'Доступ выдан',revoked:'Отозвано / истекло',failed:'Доступ закрыт: ошибка связи'};
 return <section><h3>Пригласите друга</h3><p>Каждый код действует до 10 минут и погашается один раз. Друг должен выбрать тот же сервис координации.</p>
 {codes.map(code=><article key={code.id} className="invite-code"><code>{code.phase==='waiting'?code.code:'Код использован или закрыт'}</code>
 <p>{labels[code.phase]}. Срок кода: {new Date(code.expires_at*1000).toLocaleTimeString()}</p>
 {code.peer&&<p className="peer">Identity друга: {code.peer}</p>}{code.error&&<p role="alert" className="error">{code.error}</p>}
 <div className="code-actions">{code.phase==='waiting'&&<button type="button" onClick={()=>void copy(code)}>{copied===code.id?'Скопировано':'Копировать код'}</button>}
 <button disabled={busy} onClick={()=>act('revoke_code',{id:code.id})}>Отозвать и отключить</button></div></article>)}
 <button disabled={busy||!coordinator} onClick={()=>act('create_code',{coordinator})}>{codes.length?'Создать код для ещё одного друга':'Создать одноразовый код'}</button>
 {error&&<p role="alert" className="error">{error}</p>}</section>;
}
