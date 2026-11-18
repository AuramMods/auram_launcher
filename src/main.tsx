import React from 'react';
import { createRoot } from 'react-dom/client';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { ArrowDownToLine, ArrowRight, Check, ChevronRight, FolderOpen, Gamepad2, HardDrive, LoaderCircle, Play, RefreshCw, Server, Settings2, Terminal, Trash2, X } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { JobProgressPanel } from '@/components/job-progress';
import { ForceReinstallDialog } from '@/components/force-reinstall-dialog';
import { SidebarAccount } from '@/components/sidebar-account';
import { ServerInstances } from '@/components/server-instances';
import { ServerConsole } from '@/components/server-console';
import { DataLocationSettings } from '@/components/data-location';
import { GameMemoryCard } from '@/components/game-memory';
import type { AutomaticMemory, MemoryPreferences } from '@/lib/game-memory';
import type { LocalServerAction, LocalServerStatus, ServerInstance } from '@/lib/server-instances';
import { isNewerProgress, type JobProgress } from '@/lib/job-progress';
import { createStartupTask, type PackPreparation } from '@/lib/startup';
import { Switch } from '@/components/ui/switch';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Notifications, useNotifications } from '@/components/notifications';
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from '@/components/ui/dialog';
import emblem from '../assets/icon.png';
import './style.css';

type Profile = {name:string;uuid:string;xuid:string};
type AuthSettings = {microsoftClientId:string;browserLogin:boolean};
type Settings = AuthSettings & {memory:MemoryPreferences};
type Status = {build:string;pack:{name:string;sha:string}|null;account:Profile|null;accountError:string|null;settings:Settings;memory:AutomaticMemory;packUpdate:PackPreparation;busy:boolean;progress:JobProgress|null;game:{running:boolean;pid:number|null;lastExit:string|null};servers:ServerInstance[];localServers:LocalServerStatus[];dataPath:string;clientPath:string};
type Operation = 'install'|'forceReinstall'|'repair'|'launch'|'signIn'|'signOut'|'buildServer';
function App() {
  const [tab,setTab]=React.useState<'play'|'server'|'settings'|{serverId:string}>('play');
  const [status,setStatus]=React.useState<Status|null>(null);
  const {notifications,notify,remove}=useNotifications();
  const showError=React.useCallback((message:string)=>notify(message,'error'),[notify]);
  const showNotice=React.useCallback((message:string)=>notify(message,'success'),[notify]);
  const [active,setActive]=React.useState(false);
  const [serverActions,setServerActions]=React.useState<Partial<Record<string,LocalServerAction>>>({});
  const [serverErrors,setServerErrors]=React.useState<Partial<Record<string,string>>>({});
  const [confirmReinstall,setConfirmReinstall]=React.useState(false);
  const [progress,setProgress]=React.useState<JobProgress|null>(null);
  const [settings,setSettings]=React.useState<AuthSettings|null>(null);
  const [challenge,setChallenge]=React.useState<{code:string;verificationUri:string;expiresAt:number}|null>(null);
  const [codeCopied,setCodeCopied]=React.useState(false);
  const [advanced,setAdvanced]=React.useState(false);
  const [updateStatus,setUpdateStatus]=React.useState('');
  const [packPreparation,setPackPreparation]=React.useState<PackPreparation>({state:'pending',version:null,message:null,detail:null});
  const lastProgress=React.useRef<Pick<JobProgress,'jobId'|'revision'>|null>(null);
  const receiveProgress=React.useCallback((next:JobProgress)=>{if(isNewerProgress(next,lastProgress.current)){lastProgress.current=next;setProgress(next);}},[]);
  const refresh = React.useCallback(async () => {
    const next = await invoke<Status>('get_status');
    setStatus(next);
    setSettings(previous => previous ?? {
      microsoftClientId: next.settings.microsoftClientId,
      browserLogin: next.settings.browserLogin,
    });
    if (next.progress) receiveProgress(next.progress);
    return next;
  }, [receiveProgress]);
  const acceptPreparation=React.useCallback((next:PackPreparation)=>{
    setPackPreparation(next);
    if(next.state==='failed')showError(next.message??'Pack preparation failed. Retry the update.');
    else if(next.state==='offline')notify(next.message??'Pack updates unavailable. The installed version can be launched.','info');
  },[notify,showError]);
  const startSession=React.useMemo(()=>createStartupTask({
    initialize:refresh,
    updateLauncher:()=>invoke<string>('update_launcher',{manual:false}),
    preparePack:()=>invoke<PackPreparation>('prepare_pack_update',{retry:false}),
    launcherStatus:setUpdateStatus,
  }),[refresh]);
  React.useEffect(()=>{
    let disposed=false;
    const subscription=listen<JobProgress>('progress',event=>{if(!disposed)receiveProgress(event.payload);});
    const challengeSubscription=listen<{code:string;verificationUri:string;expiresAt:number}>('device-challenge',event=>{if(!disposed){setCodeCopied(false);setChallenge(event.payload);}});
    startSession().then(async prepared=>{
      if(disposed)return;
      await refresh();if(!disposed)acceptPreparation(prepared);
    }).catch(error=>{if(!disposed){const message=String(error);showError(message);setPackPreparation({state:'failed',version:null,message,detail:null});}});
    const interval=setInterval(()=>{refresh().catch(()=>{});},1500);
    return ()=>{disposed=true;clearInterval(interval);void subscription.then(unlisten=>unlisten());void challengeSubscription.then(unlisten=>unlisten());};
  },[refresh,receiveProgress,startSession,acceptPreparation,showError]);
  const accountError=status?.accountError??null;
  const reportedAccountError=React.useRef<string|null>(null);
  React.useEffect(()=>{
    if(accountError&&accountError!==reportedAccountError.current)showError(`Credential storage unavailable: ${accountError}`);
    reportedAccountError.current=accountError;
  },[accountError,showError]);
  const preparingPack=packPreparation.state==='pending'||packPreparation.state==='running';
  const packUpdateFailed=packPreparation.state==='failed';
  const busy=active||preparingPack||!!status?.busy||progress?.state==='running'||progress?.state==='cancelling';
  async function retryPackUpdate() {
    setProgress(null);
    setPackPreparation(previous=>({...previous,state:'running',message:null}));
    try {
      await invoke('retry_initialize');await refresh();
      const prepared=await invoke<PackPreparation>('prepare_pack_update',{retry:true});
      await refresh();acceptPreparation(prepared);
    } catch(error) {const message=String(error);showError(message);setPackPreparation({state:'failed',version:null,message,detail:null});}
  }
  async function run(operation:Operation) {
    setActive(true);setProgress(null);
    try {await invoke('run_operation',{operation});const next=await refresh();if((operation==='install'||operation==='forceReinstall')&&next.packUpdate.state==='offline')notify(next.packUpdate.message??'Pack updates unavailable. The installed version can be launched.','info');else showNotice(operation==='launch'?'Minecraft started. Auram will keep your game log.':operation==='buildServer'?'Server files are ready. Open the server folder to start it.':operation==='signIn'?'Signed in. Your session will refresh automatically.':operation==='forceReinstall'?'Auram was freshly reinstalled.':'All set.');}
    catch(e){showError(String(e));}
    finally {setActive(false);setChallenge(null);const next=await refresh().catch(()=>null);if(next&&(operation==='install'||operation==='forceReinstall'))setPackPreparation(next.packUpdate);}
  }
  async function action(name:string,args?:Record<string,unknown>) {try {await invoke(name,args);await refresh();return true;}catch(e){showError(String(e));return false;}}
  async function save() {if(settings&&await action('save_settings',{settings}))showNotice('Settings saved.');}
  async function updateLauncher() {setUpdateStatus('Checking for launcher updates…');try {const message=await invoke<string>('update_launcher',{manual:true});setUpdateStatus(message);notify(message);}catch(e){setUpdateStatus(String(e));showError(String(e));}}
  const localServers=new Map(status?.localServers.map(server=>[server.id,server])??[]);
  const servers=status?.servers??[];
  const consoles=servers.filter(server=>server.target.kind==='local'&&(localServers.has(server.id)||serverActions[server.id]||serverErrors[server.id]));
  const consoleId=typeof tab==='object'?tab.serverId:null;
  const consoleServer=consoles.find(server=>server.id===consoleId);
  async function controlServer(id:string,command:LocalServerAction) {
    if(serverActions[id])return;
    if(command==='start'||command==='restart')setTab({serverId:id});
    setServerActions(previous=>({...previous,[id]:command}));
    setServerErrors(previous=>({...previous,[id]:undefined}));
    try {
      await invoke('manage_local_server',{id,action:command});
      if(command==='close')setTab(previous=>typeof previous==='object'&&previous.serverId===id?'server':previous);
    } catch(error) {
      const message=String(error);
      setServerErrors(previous=>({...previous,[id]:message}));
      if(command==='close'||command==='stop')showError(message);
    } finally {
      await refresh().catch(()=>{});
      setServerActions(previous=>({...previous,[id]:undefined}));
    }
  }
  const installed=!!status?.pack;
  const running=!!status?.game.running;
  const beforeInstall=status!==null&&!installed;
  const browseDuringStartup=beforeInstall&&preparingPack&&!active&&!running;
  function onLocationBusyChange(inProgress:boolean) {
    if(inProgress)setProgress(null);
    setActive(inProgress);
  }
  const memory=status?.memory;
  return <div className="shell dark">
    <aside className="sidebar">
      <div className="brand"><img src={emblem} alt=""/><span>AURAM<span className="brand-label">LAUNCHER</span></span></div>
      <div className="nav-label">YOUR WORKSPACE</div>
      <nav className="sidebar-nav" aria-label="Main navigation">{([{id:'play',label:'Play',icon:Gamepad2},{id:'server',label:'Server',icon:Server},{id:'settings',label:'Settings',icon:Settings2}] as const).map(item=><button key={item.id} className={`nav-item ${tab===item.id?'selected':''}`} onClick={()=>setTab(item.id)}><item.icon size={17}/>{item.label}{tab===item.id&&<ChevronRight size={14} className="nav-chevron"/>}</button>)}
        {consoles.map(server=><div key={server.id} className={`nav-console ${consoleId===server.id?'selected':''}`}>
          <button className="nav-console-open" aria-current={consoleId===server.id?'page':undefined} onClick={()=>setTab({serverId:server.id})} title={server.name}>
            <Terminal size={15}/><span>{server.name}</span>
          </button>
          <button className="nav-console-close" aria-label={`Stop and close ${server.name}`} title="Stop and close"
            disabled={!!serverActions[server.id]} onClick={()=>void controlServer(server.id,'close')}><X size={13}/></button>
        </div>)}
      </nav>
      <div className="sidebar-bottom"><div className="sidebar-meta"><div className="local-label"><span className={`status-dot ${running?'on':''}`}/>{running?'MINECRAFT RUNNING':'DESKTOP EDITION'}</div><span className="build-number">Build {status?.build??'…'}</span><button className="text-action" onClick={()=>void updateLauncher()} disabled={busy}><RefreshCw size={12}/>Check launcher updates</button></div><SidebarAccount account={status?.account??null} disabled={busy||!status} onSignIn={()=>void run('signIn')} onSignOut={()=>void run('signOut')}/></div>
    </aside>
    <div className={`workspace ${busy?'is-working':''}`}>
      <main>
        {tab==='play'&&<>
          <div className="page-heading"><div><span className="eyebrow">THE WORLD IS YOURS</span><h1>Welcome to Auram<span>.</span></h1><p>A world of magic, machines, and everything in between.</p></div></div>
          <section className="pack-panel" aria-labelledby="pack-title">
            <div className="pack-top"><div className="pack-symbol"><img src={emblem} alt="Auram emblem"/></div><Badge variant="outline" className="text-zinc-400 border-zinc-800"><span className="status-dot on"/> {installed?'INSTALLED':'READY TO INSTALL'}</Badge></div>
            <div className="pack-heading"><span className="eyebrow">THE AURAM MODPACK</span><h2 id="pack-title">Make something<br/>extraordinary.</h2><p>Your world. Your way. Everything you need, kept up to date.</p></div>
            {beforeInstall&&<DataLocationSettings beforeInstall disabled={busy||running}
              allowPickerWhileBusy={browseDuringStartup} onBusyChange={onLocationBusyChange}
              onError={showError} onNotice={showNotice}/>}
            <div className="pack-footer"><div className="pack-facts"><div><span>MINECRAFT</span><strong>1.20.1</strong></div><div><span>MOD LOADER</span><strong>Forge</strong></div><div><span>PACK VERSION</span><strong>{status?.pack?.name??'—'}</strong></div></div><Button size="lg" className="play-button" disabled={busy||running||(!status&&!packUpdateFailed)} onClick={()=>{if(packUpdateFailed)void retryPackUpdate();else void run(!installed?'install':!status?.account?'signIn':'launch');}}>{busy?<LoaderCircle className="animate-spin"/>:running?<Check/>:!installed?<ArrowDownToLine/>:<Play fill="currentColor"/>}{running?'Game running':preparingPack?'Updating Auram…':packUpdateFailed?(status?.pack?'Retry pack update':'Retry startup'):!installed?'Install Auram':!status?.account?'Sign in to play':'Launch Auram'}{!busy&&!running&&<ArrowRight/>}</Button></div>
          </section>
          <div className="installation-card">
            <Card>
              <CardHeader><CardTitle className="section-title"><HardDrive/>Your installation</CardTitle><CardDescription>Changelist updates. Cached files reused. Updates and repair preserve your worlds.</CardDescription></CardHeader>
              <CardContent className="installation-actions">
                <Button variant="outline" className="installation-tile" disabled={busy||running||!installed} onClick={()=>void run('install')}>
                  <RefreshCw className="size-5"/><span className="installation-tile-copy"><span>Check pack updates</span><small>Find the latest pack</small></span>
                </Button>
                <Button variant="outline" className="installation-tile" onClick={()=>void action('open_folder',{folder:'client'})}>
                  <FolderOpen className="size-5"/><span className="installation-tile-copy"><span>Open game folder</span><small>Browse client files</small></span>
                </Button>
                <Button variant="outline" className="installation-tile" disabled={busy||running} onClick={()=>setConfirmReinstall(true)}>
                  <Trash2 className="size-5"/><span className="installation-tile-copy"><span>Force reinstall</span><small>Reinstall from scratch</small></span>
                </Button>
              </CardContent>
            </Card>
          </div>
          {status?.game.lastExit&&!running&&<p className="last-exit">{status.game.lastExit}</p>}
        </>}
        {tab==='server'&&<ServerInstances servers={servers} targetVersion={status?.pack??null} busy={busy} clientRunning={running}
          localServers={localServers} serverActions={serverActions}
          onStart={id=>void controlServer(id,'start')} onConsole={id=>setTab({serverId:id})}
          refresh={refresh} onBusyChange={setActive} onError={showError} onNotice={showNotice}/>}
        {consoleServer&&<ServerConsole key={consoleServer.id} server={consoleServer} status={localServers.get(consoleServer.id)}
          pending={serverActions[consoleServer.id]} busy={busy} error={serverErrors[consoleServer.id]}
          onControl={command=>void controlServer(consoleServer.id,command)}
          onOpenFolder={()=>void action('open_server_instance',{id:consoleServer.id})}/>}
        {tab==='settings'&&<>
          <div className="page-heading"><span className="eyebrow">MAKE IT YOURS</span><h1>A little fine tuning<span>.</span></h1><p>Simple controls for the way you play.</p></div>
          <GameMemoryCard automaticMemory={memory??null} preferences={status?.settings.memory??null} disabled={busy}
            onSave={preferences=>action('save_memory_preferences',{preferences})}/>
          <div className="detail-grid mt-5"><Card><CardHeader><CardTitle>Files & diagnostics</CardTitle><CardDescription>Repair checks your managed files and downloads only what is missing or damaged.</CardDescription></CardHeader><CardContent className="flex flex-wrap gap-2"><Button variant="outline" onClick={()=>void action('open_folder',{folder:'data'})}><FolderOpen/>Data folder</Button><Button variant="outline" onClick={()=>void action('open_folder',{folder:'logs'})}>Logs</Button><Button variant="secondary" disabled={busy||running||!installed} onClick={()=>void run('repair')}><RefreshCw/>Repair installation</Button></CardContent></Card><Card><CardHeader><CardTitle>Launcher updates</CardTitle><CardDescription>New launcher builds install automatically on startup.</CardDescription></CardHeader><CardContent><Button variant="outline" disabled={busy} onClick={()=>void updateLauncher()}><RefreshCw/>Check for updates</Button><p className="mt-3 text-xs text-muted-foreground" role="status">{updateStatus||`Installed: Build ${status?.build??'…'}`}</p></CardContent></Card></div>
          <DataLocationSettings disabled={busy||running} allowPickerWhileBusy={browseDuringStartup}
            onBusyChange={onLocationBusyChange} onError={showError} onNotice={showNotice}/>
          <button className="text-action mt-6" onClick={()=>setAdvanced(!advanced)}>{advanced?'Hide':'Show'} advanced authentication settings</button>{advanced&&<Card className="mt-3"><CardContent className="pt-5"><Label htmlFor="client-id">Microsoft application ID</Label><Input id="client-id" value={settings?.microsoftClientId??''} onChange={event=>setSettings(current=>current?{...current,microsoftClientId:event.target.value}:current)} className="mt-2"/><p className="text-xs text-muted-foreground mt-2">Changing the application ID requires signing in again.</p><div className="mt-4 flex items-center justify-between gap-4"><Label htmlFor="browser-login">Use a configured browser callback</Label><Switch id="browser-login" checked={settings?.browserLogin??false} onCheckedChange={browserLogin=>setSettings(current=>current?{...current,browserLogin}:current)}/></div><p className="text-xs text-muted-foreground mt-2">Enable only for an application registration with the desktop callback configured. The existing Auram registration uses the code flow with automatic session renewal.</p><Button className="mt-3" disabled={busy} onClick={()=>void save()}>Save</Button></CardContent></Card>}
        </>}
      </main>
      {busy&&<footer className="activity-bar"><JobProgressPanel progress={progress} onCancel={()=>void action('cancel_operation')}/></footer>}
    </div>
    <Notifications notifications={notifications} onRemove={remove}/>
    <ForceReinstallDialog open={confirmReinstall} onOpenChange={setConfirmReinstall} clientPath={status?.clientPath} busy={busy||running} onConfirm={()=>{setConfirmReinstall(false);void run('forceReinstall');}}/>
    <Dialog open={challenge!==null} onOpenChange={open=>{if(!open){setChallenge(null);void invoke('cancel_operation');}}}>
      <DialogContent className="sm:max-w-md"><DialogHeader><DialogTitle>Sign in with Microsoft</DialogTitle><DialogDescription>Copy your code, then enter it on Microsoft’s sign-in page. Auram will remember your session securely.</DialogDescription></DialogHeader>
      <div className="rounded-lg border border-zinc-800 bg-black p-6 text-center"><code className="select-all text-3xl font-medium tracking-[.2em] text-violet-200">{challenge?.code}</code></div>
      <p className="text-xs text-muted-foreground">The button copies this code and opens your browser in one action. Complete sign-in there, then return to Auram.</p>
      <DialogFooter><Button variant="ghost" onClick={()=>{setChallenge(null);void invoke('cancel_operation');}}>Cancel</Button><Button onClick={async()=>{try{await invoke('copy_code_and_open');setCodeCopied(true);}catch(e){showError(String(e));}}}>{codeCopied?<Check/>:<ArrowRight/>}{codeCopied?'Copy Code again':'Copy Code'}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  </div>;
}
document.documentElement.classList.add('dark');
createRoot(document.getElementById('root')!).render(<React.StrictMode><App/></React.StrictMode>);
