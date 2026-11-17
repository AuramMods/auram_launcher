import React from 'react';
import { createRoot } from 'react-dom/client';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { ArrowDownToLine, ArrowRight, Check, ChevronRight, CircleAlert, FolderOpen, Gamepad2, HardDrive, LoaderCircle, Play, RefreshCw, Server, Settings2, ShieldCheck, Trash2, X } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { JobProgressPanel } from '@/components/job-progress';
import { ForceReinstallDialog } from '@/components/force-reinstall-dialog';
import { SidebarAccount } from '@/components/sidebar-account';
import { ServerInstances } from '@/components/server-instances';
import type { ServerInstance } from '@/lib/server-instances';
import { isNewerProgress, type JobProgress } from '@/lib/job-progress';
import { createStartupTask, type PackPreparation } from '@/lib/startup';
import { Switch } from '@/components/ui/switch';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from '@/components/ui/dialog';
import emblem from '../assets/icon.png';
import './style.css';

type Profile = {name:string;uuid:string;xuid:string};
type Settings = {schema:number;microsoftClientId:string;browserLogin:boolean};
type AutomaticMemory = {systemMemoryMib:number|null;maximumMib:number;initialMib:number;profile:string;highSystemMemoryAboveMib:number};
type Status = {build:string;pack:{name:string;sha:string}|null;account:Profile|null;accountError:string|null;settings:Settings;memory:AutomaticMemory;packUpdate:PackPreparation;busy:boolean;progress:JobProgress|null;game:{running:boolean;pid:number|null;lastExit:string|null};legacyAvailable:boolean;servers:ServerInstance[];dataPath:string;clientPath:string};
type Operation = 'install'|'forceReinstall'|'repair'|'launch'|'signIn'|'signOut'|'buildServer'|'importLegacy';
function App() {
  const [tab,setTab]=React.useState<'play'|'server'|'settings'>('play');
  const [status,setStatus]=React.useState<Status|null>(null);
  const [error,setError]=React.useState('');
  const [notice,setNotice]=React.useState('');
  const [active,setActive]=React.useState(false);
  const [confirmReinstall,setConfirmReinstall]=React.useState(false);
  const [progress,setProgress]=React.useState<JobProgress|null>(null);
  const [settings,setSettings]=React.useState<Settings|null>(null);
  const [challenge,setChallenge]=React.useState<{code:string;verificationUri:string;expiresAt:number}|null>(null);
  const [codeCopied,setCodeCopied]=React.useState(false);
  const [advanced,setAdvanced]=React.useState(false);
  const [updateStatus,setUpdateStatus]=React.useState('');
  const [packPreparation,setPackPreparation]=React.useState<PackPreparation>({state:'pending',version:null,message:null,detail:null});
  const lastProgress=React.useRef<Pick<JobProgress,'jobId'|'revision'>|null>(null);
  const receiveProgress=React.useCallback((next:JobProgress)=>{if(isNewerProgress(next,lastProgress.current)){lastProgress.current=next;setProgress(next);}},[]);
  const refresh=React.useCallback(async()=>{const next=await invoke<Status>('get_status');setStatus(next);setSettings(previous=>previous??next.settings);if(next.progress)receiveProgress(next.progress);return next;},[receiveProgress]);
  const acceptPreparation=React.useCallback((next:PackPreparation)=>{
    setPackPreparation(next);
    if(next.state==='failed')setError(next.message??'Pack preparation failed. Retry the update.');
    else if(next.state==='offline')setNotice(next.message??'Pack updates unavailable. The installed version can be launched.');
  },[]);
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
    }).catch(error=>{if(!disposed){const message=String(error);setError(message);setPackPreparation({state:'failed',version:null,message,detail:null});}});
    const interval=setInterval(()=>{refresh().catch(()=>{});},1500);
    return ()=>{disposed=true;clearInterval(interval);void subscription.then(unlisten=>unlisten());void challengeSubscription.then(unlisten=>unlisten());};
  },[refresh,receiveProgress,startSession,acceptPreparation]);
  const preparingPack=packPreparation.state==='pending'||packPreparation.state==='running';
  const packUpdateFailed=packPreparation.state==='failed';
  const busy=active||preparingPack||!!status?.busy||progress?.state==='running'||progress?.state==='cancelling';
  async function retryPackUpdate() {
    setError('');setNotice('');setProgress(null);
    setPackPreparation(previous=>({...previous,state:'running',message:null}));
    try {
      await invoke('retry_initialize');await refresh();
      const prepared=await invoke<PackPreparation>('prepare_pack_update',{retry:true});
      await refresh();acceptPreparation(prepared);
    } catch(error) {const message=String(error);setError(message);setPackPreparation({state:'failed',version:null,message,detail:null});}
  }
  async function run(operation:Operation) {
    setActive(true);setError('');setNotice('');setProgress(null);
    try {await invoke('run_operation',{operation});await refresh();setNotice(operation==='launch'?'Minecraft started. Auram will keep your game log.':operation==='buildServer'?'Server files are ready. Open the server folder to start it.':operation==='signIn'?'Signed in. Your session will refresh automatically.':operation==='forceReinstall'?'Auram was freshly reinstalled.':'All set.');}
    catch(e){setError(String(e));}
    finally {setActive(false);setChallenge(null);const next=await refresh().catch(()=>null);if(next&&(operation==='install'||operation==='forceReinstall'))acceptPreparation(next.packUpdate);}
  }
  async function action(name:string,args?:Record<string,unknown>) {try {setError('');await invoke(name,args);await refresh();}catch(e){setError(String(e));}}
  async function save() {if(settings){await action('save_settings',{settings});setNotice('Settings saved.');}}
  async function updateLauncher() {setUpdateStatus('Checking for launcher updates…');try {setUpdateStatus(await invoke<string>('update_launcher',{manual:true}));}catch(e){setUpdateStatus(String(e));}}
  const installed=!!status?.pack;
  const running=!!status?.game.running;
  const memory=status?.memory;
  return <div className="shell dark">
    <aside className="sidebar">
      <div className="brand"><img src={emblem} alt=""/><span>AURAM<span className="brand-label">LAUNCHER</span></span></div>
      <div className="nav-label">YOUR WORKSPACE</div>
      <nav aria-label="Main navigation">{([{id:'play',label:'Play',icon:Gamepad2},{id:'server',label:'Server',icon:Server},{id:'settings',label:'Settings',icon:Settings2}] as const).map(item=><button key={item.id} className={`nav-item ${tab===item.id?'selected':''}`} onClick={()=>setTab(item.id)}><item.icon size={17}/>{item.label}{tab===item.id&&<ChevronRight size={14} className="nav-chevron"/>}</button>)}</nav>
      <div className="sidebar-bottom"><div className="sidebar-meta"><div className="local-label"><span className={`status-dot ${running?'on':''}`}/>{running?'MINECRAFT RUNNING':'DESKTOP EDITION'}</div><span className="build-number">Build {status?.build??'…'}</span><button className="text-action" onClick={()=>void updateLauncher()} disabled={busy}><RefreshCw size={12}/>Check launcher updates</button></div><SidebarAccount account={status?.account??null} disabled={busy||!status} onSignIn={()=>void run('signIn')} onSignOut={()=>void run('signOut')}/></div>
    </aside>
    <div className={`workspace ${busy?'is-working':''}`}>
      <main>
        {error&&<Alert variant="destructive" className="mb-5"><CircleAlert/><AlertTitle>Something needs attention</AlertTitle><AlertDescription><p className="break-words">{error}</p><div className="flex gap-2 mt-3"><Button size="sm" variant="outline" onClick={()=>{setError('');if(packUpdateFailed)void retryPackUpdate();else void action('retry_initialize');}}>Retry</Button><Button size="sm" variant="ghost" onClick={()=>void action('open_folder',{folder:'logs'})}>Open logs</Button><Button size="sm" variant="ghost" onClick={()=>setError('')}>Dismiss</Button></div></AlertDescription></Alert>}
        {notice&&!error&&<div className="notice" role="status"><Check size={15}/>{notice}<button aria-label="Dismiss notification" onClick={()=>setNotice('')}><X size={14}/></button></div>}
        {status?.accountError&&<Alert className="mb-4"><ShieldCheck/><AlertTitle>Credential storage unavailable</AlertTitle><AlertDescription>{status.accountError}</AlertDescription></Alert>}
        {tab==='play'&&<>
          <div className="page-heading"><div><span className="eyebrow">THE WORLD IS YOURS</span><h1>Welcome to Auram<span>.</span></h1><p>A world of magic, machines, and everything in between.</p></div></div>
          <section className="pack-panel" aria-labelledby="pack-title">
            <div className="pack-top"><div className="pack-symbol"><img src={emblem} alt="Auram emblem"/></div><Badge variant="outline" className="text-zinc-400 border-zinc-800"><span className="status-dot on"/> {installed?'INSTALLED':'READY TO INSTALL'}</Badge></div>
            <div className="pack-heading"><span className="eyebrow">THE AURAM MODPACK</span><h2 id="pack-title">Make something<br/>extraordinary.</h2><p>Your world. Your way. Everything you need, kept up to date.</p></div>
            <div className="pack-footer"><div className="pack-facts"><div><span>MINECRAFT</span><strong>1.20.1</strong></div><div><span>MOD LOADER</span><strong>Forge</strong></div><div><span>PACK VERSION</span><strong>{status?.pack?.name??'—'}</strong></div></div><Button size="lg" className="play-button" disabled={busy||running||!status} onClick={()=>{if(installed&&packUpdateFailed)void retryPackUpdate();else void run(!installed?'install':!status?.account?'signIn':'launch');}}>{busy?<LoaderCircle className="animate-spin"/>:running?<Check/>:!installed?<ArrowDownToLine/>:<Play fill="currentColor"/>}{running?'Game running':preparingPack?'Updating Auram…':installed&&packUpdateFailed?'Retry pack update':!installed?'Install Auram':!status?.account?'Sign in to play':'Launch Auram'}{!busy&&!running&&<ArrowRight/>}</Button></div>
          </section>
          <div className="installation-card">
            <Card><CardHeader><CardTitle className="section-title"><HardDrive/>Your installation</CardTitle><CardDescription>Changelist updates. Cached files reused. Updates and repair preserve your worlds.</CardDescription></CardHeader><CardContent className="flex flex-col gap-2"><Button variant="outline" className="justify-between" disabled={busy||running||!installed} onClick={()=>void run('install')}><span className="flex items-center gap-2"><RefreshCw/>Check pack updates</span><ChevronRight/></Button><Button variant="ghost" className="justify-between" onClick={()=>void action('open_folder',{folder:'client'})}><span className="flex items-center gap-2"><FolderOpen/>Open game folder</span><ChevronRight/></Button>{status?.legacyAvailable&&<Button variant="secondary" disabled={busy||running} onClick={()=>void run('importLegacy')}>Import existing Flutter installation</Button>}<Button variant="destructive" className="justify-between mt-2" disabled={busy||running} onClick={()=>setConfirmReinstall(true)}><span className="flex items-center gap-2"><Trash2/>Force reinstall</span><ChevronRight/></Button><p className="text-xs text-muted-foreground">Force reinstall deletes local worlds and all client pack files.</p></CardContent></Card>
          </div>
          {status?.game.lastExit&&!running&&<p className="last-exit">{status.game.lastExit}</p>}
        </>}
        {tab==='server'&&<ServerInstances servers={status?.servers??[]} targetVersion={status?.pack??null} busy={busy||running} refresh={refresh} onBusyChange={setActive} onError={setError} onNotice={setNotice}/>}
        {tab==='settings'&&<>
          <div className="page-heading"><span className="eyebrow">MAKE IT YOURS</span><h1>A little fine tuning<span>.</span></h1><p>Simple controls for the way you play.</p></div>
          <Card><CardHeader><div className="flex items-center justify-between"><CardTitle>Game memory</CardTitle><Badge variant="outline">Automatic</Badge></div><CardDescription>Auram selects its original memory preset for your computer.</CardDescription></CardHeader><CardContent><div className="memory-value">{memory?(memory.maximumMib/1024).toFixed(1):'…'} <span>GiB</span></div>{memory?<><p className="text-xs text-muted-foreground"><span className="text-violet-200">{memory.profile} tuning</span> · {(memory.initialMib/1024).toFixed(1)} GiB initial allocation. {memory.systemMemoryMib!==null?`${(memory.systemMemoryMib/1024).toFixed(1)} GiB system memory detected.`:'System memory detection unavailable; using the low-memory preset.'}</p><p className="mt-2 text-xs text-muted-foreground">9 GiB maximum on systems with up to {memory.highSystemMemoryAboveMib/1024} GiB of RAM; 20 GiB maximum above that.</p></>:<p className="text-xs text-muted-foreground">Detecting system memory…</p>}</CardContent></Card>
          <div className="detail-grid mt-5"><Card><CardHeader><CardTitle>Files & diagnostics</CardTitle><CardDescription>Repair checks your managed files and downloads only what is missing or damaged.</CardDescription></CardHeader><CardContent className="flex flex-wrap gap-2"><Button variant="outline" onClick={()=>void action('open_folder',{folder:'data'})}><FolderOpen/>Data folder</Button><Button variant="outline" onClick={()=>void action('open_folder',{folder:'logs'})}>Logs</Button><Button variant="secondary" disabled={busy||running||!installed} onClick={()=>void run('repair')}><RefreshCw/>Repair installation</Button></CardContent></Card><Card><CardHeader><CardTitle>Launcher updates</CardTitle><CardDescription>New launcher builds install automatically on startup.</CardDescription></CardHeader><CardContent><Button variant="outline" disabled={busy} onClick={()=>void updateLauncher()}><RefreshCw/>Check for updates</Button><p className="mt-3 text-xs text-muted-foreground" role="status">{updateStatus||`Installed: Build ${status?.build??'…'}`}</p></CardContent></Card></div>
          <button className="text-action mt-6" onClick={()=>setAdvanced(!advanced)}>{advanced?'Hide':'Show'} advanced authentication settings</button>{advanced&&<Card className="mt-3"><CardContent className="pt-5"><Label htmlFor="client-id">Microsoft application ID</Label><Input id="client-id" value={settings?.microsoftClientId??''} onChange={event=>setSettings(current=>current?{...current,microsoftClientId:event.target.value}:current)} className="mt-2"/><p className="text-xs text-muted-foreground mt-2">Changing the application ID requires signing in again.</p><div className="mt-4 flex items-center justify-between gap-4"><Label htmlFor="browser-login">Use a configured browser callback</Label><Switch id="browser-login" checked={settings?.browserLogin??false} onCheckedChange={browserLogin=>setSettings(current=>current?{...current,browserLogin}:current)}/></div><p className="text-xs text-muted-foreground mt-2">Enable only for an application registration with the desktop callback configured. The existing Auram registration uses the code flow with automatic session renewal.</p><Button className="mt-3" disabled={busy} onClick={()=>void save()}>Save</Button></CardContent></Card>}
        </>}
      </main>
      {busy&&<footer className="activity-bar"><JobProgressPanel progress={progress} onCancel={()=>void action('cancel_operation')}/></footer>}
    </div>
    <ForceReinstallDialog open={confirmReinstall} onOpenChange={setConfirmReinstall} clientPath={status?.clientPath} busy={busy||running} onConfirm={()=>{setConfirmReinstall(false);void run('forceReinstall');}}/>
    <Dialog open={challenge!==null} onOpenChange={open=>{if(!open){setChallenge(null);void invoke('cancel_operation');}}}>
      <DialogContent className="sm:max-w-md"><DialogHeader><DialogTitle>Sign in with Microsoft</DialogTitle><DialogDescription>Copy your code, then enter it on Microsoft’s sign-in page. Auram will remember your session securely.</DialogDescription></DialogHeader>
      <div className="rounded-lg border border-zinc-800 bg-black p-6 text-center"><code className="select-all text-3xl font-medium tracking-[.2em] text-violet-200">{challenge?.code}</code></div>
      <p className="text-xs text-muted-foreground">The button copies this code and opens your browser in one action. Complete sign-in there, then return to Auram.</p>
      <DialogFooter><Button variant="ghost" onClick={()=>{setChallenge(null);void invoke('cancel_operation');}}>Cancel</Button><Button onClick={async()=>{try{await invoke('copy_code_and_open');setCodeCopied(true);}catch(e){setError(String(e));}}}>{codeCopied?<Check/>:<ArrowRight/>}{codeCopied?'Copy Code again':'Copy Code'}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  </div>;
}
document.documentElement.classList.add('dark');
createRoot(document.getElementById('root')!).render(<React.StrictMode><App/></React.StrictMode>);
