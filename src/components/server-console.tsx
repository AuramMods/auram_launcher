import React from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ChevronRight, FolderOpen, Play, RotateCcw, Square, Terminal } from 'lucide-react';
import { Button } from './ui/button';
import { Input } from './ui/input';
import type { LocalServerAction, LocalServerStatus, ServerConsoleLine, ServerInstance } from '../lib/server-instances';

type Props = {
  server: ServerInstance;
  status?: LocalServerStatus;
  pending?: LocalServerAction;
  busy: boolean;
  error?: string;
  onControl: (action: LocalServerAction) => void;
  onOpenFolder: () => void;
};

export function ServerConsole({server,status,pending,busy,error,onControl,onOpenFolder}: Props) {
  const [lines,setLines]=React.useState<ServerConsoleLine[]>([]);
  const [consoleError,setConsoleError]=React.useState('');
  const [command,setCommand]=React.useState('');
  const [commandError,setCommandError]=React.useState('');
  const [sending,setSending]=React.useState(false);
  const output=React.useRef<HTMLDivElement>(null);
  const follow=React.useRef(true);
  const cursor=React.useRef(0);
  const running=status?.state==='running';
  const stopping=pending==='stop'||pending==='close'||status?.state==='stopping';
  const state=pending==='restart'?'Restarting':pending==='start'?'Starting':stopping?'Stopping':running?'Running':'Stopped';

  React.useEffect(()=>{
    let disposed=false;
    let timer:ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const next=await invoke<ServerConsoleLine[]>('local_server_console',{id:server.id,after:cursor.current});
        if(disposed)return;
        setConsoleError('');
        if(next.length){
          cursor.current=next[next.length-1].id;
          setLines(previous=>[...previous,...next].slice(-1200));
        }
      } catch(error) {
        if(!disposed)setConsoleError(String(error));
      } finally {
        if(!disposed)timer=setTimeout(poll,running||stopping||pending?350:1200);
      }
    }
    void poll();
    return ()=>{disposed=true;clearTimeout(timer);};
  },[server.id,running,stopping,pending]);

  React.useLayoutEffect(()=>{
    if(output.current&&follow.current)output.current.scrollTop=output.current.scrollHeight;
  },[lines]);

  async function send(event:React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const text=command.trim();
    if(!text||sending||!running||stopping||pending)return;
    setSending(true);
    setCommandError('');
    try {
      await invoke('send_server_command',{id:server.id,command:text});
      setCommand(previous=>previous.trim()===text?'':previous);
    } catch(error) {
      setCommandError(String(error));
    } finally {
      setSending(false);
    }
  }

  return <>
    <div className="page-heading server-heading">
      <div><h1 className="server-console-name">{server.name}</h1>
        <p aria-live="polite">{state}{server.knownVersion?` · Auram ${server.knownVersion.version}`:''}{status?.exit&&!pending?` · ${status.exit}`:''}</p>
      </div>
      <Button variant="outline" onClick={onOpenFolder}><FolderOpen/>Open folder</Button>
    </div>
    <section className="server-console" aria-label="Dedicated server console">
      <div className="server-console-toolbar">
        <h2><Terminal size={16}/>Console</h2>
        <div className="server-console-actions">
          <Button variant="outline" size="sm" disabled={busy||running||stopping||!!pending} onClick={()=>onControl('start')}><Play/>Start</Button>
          <Button variant="outline" size="sm" disabled={!running||stopping||!!pending} onClick={()=>onControl('stop')}><Square/>Stop</Button>
          <Button variant="outline" size="sm" disabled={busy||!running||stopping||!!pending} onClick={()=>onControl('restart')}><RotateCcw/>Restart</Button>
        </div>
      </div>
      {error&&<p className="server-console-error" role="alert">{error}</p>}
      <div className="server-console-output" ref={output} role="log" aria-label="Server output" aria-live="off" tabIndex={0} onScroll={event=>{
        const node=event.currentTarget;
        follow.current=node.scrollHeight-node.scrollTop-node.clientHeight<24;
      }}>
        {lines.length?lines.map(line=><div className={`server-console-line ${line.source}`} key={line.id}>{line.text.replace(/\u001b\[[0-?]*[ -/]*[@-~]/g,'')}</div>):<p>{pending==='start'?'Preparing Java and starting the server…':'Server output will appear here.'}</p>}
      </div>
      <form className="server-console-command" onSubmit={send}>
        <ChevronRight size={16} aria-hidden="true"/>
        <Input aria-label="Server command" placeholder="Enter a server command" value={command} maxLength={4096}
          disabled={!running||stopping||!!pending} onChange={event=>setCommand(event.target.value)}
          autoComplete="off" autoCapitalize="off" spellCheck={false}/>
        <Button type="submit" size="sm" disabled={!command.trim()||sending||!running||stopping||!!pending}>Send</Button>
      </form>
      {(commandError||consoleError)&&<p className="server-console-error" role="alert">{commandError||consoleError}</p>}
    </section>
  </>;
}
