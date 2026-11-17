import React from 'react';
import {createPortal} from 'react-dom';
import {Check, CircleAlert, Info} from 'lucide-react';

type Kind='success'|'error'|'info';
type Notification={id:number;message:string;kind:Kind;expiresAt:number};
const DURATION=3000;
const EXIT_DURATION=180;

export function useNotifications() {
  const [notifications,setNotifications]=React.useState<Notification[]>([]);
  const nextId=React.useRef(0);
  const notify=React.useCallback((message:string,kind:Kind='info')=>{
    if(!message.trim())return;
    const notification={id:++nextId.current,message,kind,expiresAt:Date.now()+DURATION};
    setNotifications(current=>[...current,notification]);
  },[]);
  const remove=React.useCallback((id:number)=>setNotifications(current=>current.filter(item=>item.id!==id)),[]);
  return {notifications,notify,remove};
}

function NotificationItem({notification,onRemove}:{notification:Notification;onRemove:(id:number)=>void}) {
  const [leaving,setLeaving]=React.useState(false);
  React.useEffect(()=>{
    if(leaving)return;
    const timer=setTimeout(()=>setLeaving(true),Math.max(0,notification.expiresAt-EXIT_DURATION-Date.now()));
    return ()=>clearTimeout(timer);
  },[notification.expiresAt,leaving]);
  React.useEffect(()=>{
    if(!leaving)return;
    const timer=setTimeout(()=>onRemove(notification.id),EXIT_DURATION);
    return ()=>clearTimeout(timer);
  },[leaving,notification.id,onRemove]);
  const Icon=notification.kind==='success'?Check:notification.kind==='error'?CircleAlert:Info;
  return <div className="notification" data-kind={notification.kind} data-state={leaving?'leaving':'visible'} role={notification.kind==='error'?'alert':'status'}>
    <button className="notification-button" onClick={()=>setLeaving(true)} aria-label={`Dismiss: ${notification.message}`} title={notification.message} tabIndex={leaving?-1:0}>
      <Icon size={16} aria-hidden="true"/><span>{notification.message}</span>
    </button>
  </div>;
}

export function Notifications({notifications,onRemove}:{notifications:Notification[];onRemove:(id:number)=>void}) {
  return createPortal(<section className="notifications" aria-label="Notifications">{notifications.map(notification=><NotificationItem key={notification.id} notification={notification} onRemove={onRemove}/>)}</section>,document.body);
}
