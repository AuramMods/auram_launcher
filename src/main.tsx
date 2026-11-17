import React from 'react';
import { createRoot } from 'react-dom/client';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { ArrowDownToLine, ArrowRight, Check, ChevronRight, CircleAlert, FolderOpen, Gamepad2, HardDrive, LoaderCircle, LogOut, Play, RefreshCw, Server, Settings2, ShieldCheck, UserRound, X } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Badge } from '@/components/ui/badge';
import { Progress } from '@/components/ui/progress';
import { Slider } from '@/components/ui/slider';
import { Switch } from '@/components/ui/switch';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter } from '@/components/ui/dialog';
import emblem from '../assets/icon.png';
import './style.css';

type Profile = {name:string;uuid:string;xuid:string};
type Settings = {schema:number;memoryMib:number;microsoftClientId:string;browserLogin:boolean};
type Status = {build:string;pack:{name:string;sha:string}|null;account:Profile|null;accountError:string|null;settings:Settings;busy:boolean;game:{running:boolean;pid:number|null;lastExit:string|null};legacyAvailable:boolean;serverReady:boolean;eulaAccepted:boolean;dataPath:string};
type JobProgress = {operation:string;message:string;completed:number;total:number;bytes:number;totalBytes:number};
type Operation = 'install'|'repair'|'launch'|'launchInstalled'|'signIn'|'signOut'|'buildServer'|'importLegacy';
const bytes = (n:number) => n>=1024**3 ? `${(n/1024**3).toFixed(1)} GB` : `${(n/1024**2).toFixed(1)} MB`;
function App() {
  const [tab,setTab]=React.useState<'play'|'server'|'settings'>('play');
  const [status,setStatus]=React.useState<Status|null>(null);
  const [error,setError]=React.useState('');
  const [notice,setNotice]=React.useState('');
  const [active,setActive]=React.useState(false);
  const [progress,setProgress]=React.useState<JobProgress|null>(null);
  const [settings,setSettings]=React.useState<Settings|null>(null);
  const [challenge,setChallenge]=React.useState<{code:string;verificationUri:string;expiresAt:number}|null>(null);
  const [codeCopied,setCodeCopied]=React.useState(false);
  const [advanced,setAdvanced]=React.useState(false);
  const [updateStatus,setUpdateStatus]=React.useState('');
  const refresh=React.useCallback(async()=>{const next=await invoke<Status>('get_status');setStatus(next);setSettings(previous=>previous??next.settings);return next;},[]);
  React.useEffect(()=>{
    let disposed=false;
    refresh().then(()=>invoke<string>('update_launcher',{manual:false}).then(setUpdateStatus).catch(e=>setUpdateStatus(String(e)))).catch(e=>setError(String(e)));
    const subscription=listen<JobProgress>('progress',event=>{if(!disposed)setProgress(event.payload);});
    const challengeSubscription=listen<{code:string;verificationUri:string;expiresAt:number}>('device-challenge',event=>{if(!disposed){setCodeCopied(false);setChallenge(event.payload);}});
    const interval=setInterval(()=>{refresh().catch(()=>{});},1500);
    return ()=>{disposed=true;clearInterval(interval);void subscription.then(unlisten=>unlisten());void challengeSubscription.then(unlisten=>unlisten());};
  },[refresh]);
  const busy=active||!!status?.busy;
  async function run(operation:Operation) {
    setActive(true);setError('');setNotice('');setProgress(null);
    try {await invoke('run_operation',{operation});await refresh();setNotice((operation==='launch'||operation==='launchInstalled')?'Minecraft started. Auram will keep your game log.':operation==='buildServer'?'Server files are ready. Open the server folder to start it.':operation==='signIn'?'Signed in. Your session will refresh automatically.':'All set.');}
    catch(e){setError(String(e));}
    finally {setActive(false);setProgress(null);setChallenge(null);}
  }
  async function action(name:string,args?:Record<string,unknown>) {try {setError('');await invoke(name,args);await refresh();}catch(e){setError(String(e));}}
  async function save() {if(settings){await action('save_settings',{settings});setNotice('Settings saved.');}}
  async function updateLauncher() {setUpdateStatus('Checking for launcher updates…');try {setUpdateStatus(await invoke<string>('update_launcher',{manual:true}));}catch(e){setUpdateStatus(String(e));}}
  const installed=!!status?.pack;
  const running=!!status?.game.running;
  const percent=progress ? (progress.totalBytes>0?progress.bytes/progress.totalBytes*100:progress.total>0?progress.completed/progress.total*100:null):null;
  return <div className="shell dark">
    <aside className="sidebar">
      <div className="brand"><img src={emblem} alt=""/><span>AURAM<span className="brand-label">LAUNCHER</span></span></div>
      <div className="nav-label">YOUR WORKSPACE</div>
      <nav aria-label="Main navigation">{([{id:'play',label:'Play',icon:Gamepad2},{id:'server',label:'Server',icon:Server},{id:'settings',label:'Settings',icon:Settings2}] as const).map(item=><button key={item.id} className={`nav-item ${tab===item.id?'selected':''}`} onClick={()=>setTab(item.id)}><item.icon size={17}/>{item.label}{tab===item.id&&<ChevronRight size={14} className="nav-chevron"/>}</button>)}</nav>
      <div className="sidebar-bottom"><div className="local-label"><span className={`status-dot ${running?'on':''}`}/>{running?'MINECRAFT RUNNING':'DESKTOP EDITION'}</div><span className="build-number">Build {status?.build??'…'}</span><button className="text-action" onClick={()=>void updateLauncher()} disabled={busy}><RefreshCw size={12}/>Check launcher updates</button></div>
    </aside>
    <div className="workspace">
      <header className="topbar"><span>{tab==='play'?'Your library':tab==='server'?'Server workspace':'Launcher preferences'}</span><div className="account-pill"><span className="account-avatar"><UserRound size={14}/></span>{status?.account?.name??'Not signed in'}</div></header>
      <main>
        {error&&<Alert variant="destructive" className="mb-5"><CircleAlert/><AlertTitle>Something needs attention</AlertTitle><AlertDescription><p className="break-words">{error}</p><div className="flex gap-2 mt-3"><Button size="sm" variant="outline" onClick={()=>{setError('');void action('retry_initialize');}}>Retry</Button><Button size="sm" variant="ghost" onClick={()=>void action('open_folder',{folder:'logs'})}>Open logs</Button><Button size="sm" variant="ghost" onClick={()=>setError('')}>Dismiss</Button></div></AlertDescription></Alert>}
        {notice&&!error&&<div className="notice" role="status"><Check size={15}/>{notice}<button aria-label="Dismiss notification" onClick={()=>setNotice('')}><X size={14}/></button></div>}
        {status?.accountError&&<Alert className="mb-4"><ShieldCheck/><AlertTitle>Credential storage unavailable</AlertTitle><AlertDescription>{status.accountError}</AlertDescription></Alert>}
        {tab==='play'&&<>
          <div className="page-heading"><div><span className="eyebrow">THE WORLD IS YOURS</span><h1>Welcome to Auram<span>.</span></h1><p>A world of magic, machines, and everything in between.</p></div></div>
          <section className="pack-panel" aria-labelledby="pack-title">
            <div className="pack-top"><div className="pack-symbol"><img src={emblem} alt="Auram emblem"/></div><Badge variant="outline" className="text-zinc-400 border-zinc-800"><span className="status-dot on"/> {installed?'INSTALLED':'READY TO INSTALL'}</Badge></div>
            <div className="pack-heading"><span className="eyebrow">THE AURAM MODPACK</span><h2 id="pack-title">Make something<br/>extraordinary.</h2><p>Your world. Your way. Everything you need, kept up to date.</p></div>
            <div className="pack-footer"><div className="pack-facts"><div><span>MINECRAFT</span><strong>1.20.1</strong></div><div><span>MOD LOADER</span><strong>Forge</strong></div><div><span>PACK VERSION</span><strong>{status?.pack?.name??'—'}</strong></div></div><Button size="lg" className="play-button" disabled={busy||running||!status} onClick={()=>void run(!installed?'install':!status?.account?'signIn':'launch')}>{busy?<LoaderCircle className="animate-spin"/>:running?<Check/>:!installed?<ArrowDownToLine/>:<Play fill="currentColor"/>}{running?'Game running':!installed?'Install Auram':!status?.account?'Sign in to play':'Launch Auram'}{!busy&&!running&&<ArrowRight/>}</Button></div>
          </section>
          <div className="detail-grid">
            <Card><CardHeader><CardTitle className="section-title"><ShieldCheck/>Microsoft account</CardTitle><CardDescription>{status?.account?'Ready for your next session.':'Sign in once. Come back and play.'}</CardDescription></CardHeader><CardContent><div className="account-row"><div className="identity-icon"><UserRound/></div><div><strong>{status?.account?.name??'Your adventure starts here'}</strong><p>{status?.account?'Session saved securely on this device':'Use your Minecraft Java account'}</p></div></div><div className="flex gap-2 mt-5"><Button variant="secondary" disabled={busy} onClick={()=>void run('signIn')}>{status?.account?'Change account':'Sign in with Microsoft'}<ArrowRight/></Button>{status?.account&&<Button variant="ghost" size="icon" aria-label="Sign out" disabled={busy} onClick={()=>void run('signOut')}><LogOut/></Button>}</div></CardContent></Card>
            <Card><CardHeader><CardTitle className="section-title"><HardDrive/>Your installation</CardTitle><CardDescription>Changelist updates. Cached files reused. Your worlds preserved.</CardDescription></CardHeader><CardContent className="flex flex-col gap-2"><Button variant="outline" className="justify-between" disabled={busy||running||!installed} onClick={()=>void run('install')}><span className="flex items-center gap-2"><RefreshCw/>Check pack updates</span><ChevronRight/></Button><Button variant="ghost" className="justify-between" onClick={()=>void action('open_folder',{folder:'client'})}><span className="flex items-center gap-2"><FolderOpen/>Open game folder</span><ChevronRight/></Button>{installed&&<Button variant="ghost" className="justify-between" disabled={busy||running} onClick={()=>void run('launchInstalled')}><span className="flex items-center gap-2"><Play/>Play installed version</span><ChevronRight/></Button>}{status?.legacyAvailable&&<Button variant="secondary" disabled={busy||running} onClick={()=>void run('importLegacy')}>Import existing Flutter installation</Button>}</CardContent></Card>
          </div>
          {status?.game.lastExit&&!running&&<p className="last-exit">{status.game.lastExit}</p>}
        </>}
        {tab==='server'&&<>
          <div className="page-heading"><span className="eyebrow">BETTER TOGETHER</span><h1>Your own corner<br/>of the world<span>.</span></h1><p>Build an Auram server from your installed pack version.</p></div>
          <Card><CardHeader><div className="flex justify-between items-center"><CardTitle className="section-title"><Server/>Auram server</CardTitle><Badge variant="outline">{status?.serverReady?'Built':'Not built yet'}</Badge></div><CardDescription>Forge runtime, server mods, configuration, and start scripts in one place.</CardDescription></CardHeader><CardContent><div className="server-description"><p>Client-only mods are excluded using the pack’s server rules. Rebuilding preserves your worlds, access lists, and server settings.</p><div className="flex gap-3 mt-6"><Button disabled={busy||running||!installed} onClick={()=>void run('buildServer')}><Server/>{status?.serverReady?'Update server files':'Build server'}</Button><Button variant="outline" disabled={!status?.serverReady} onClick={()=>void action('open_folder',{folder:'server'})}><FolderOpen/>Open server folder</Button></div>{!installed&&<p className="mt-3 text-xs">Install Auram in the Play tab first.</p>}</div>{status?.serverReady&&<div className="eula-section"><div><Label htmlFor="eula">Minecraft EULA</Label><p>Read the agreement before accepting. Start your server with run.sh or run.bat from its folder.</p><button className="text-action" onClick={()=>void action('open_eula')}>Read the Minecraft EULA <ArrowRight size={12}/></button></div><Switch id="eula" checked={status.eulaAccepted} disabled={busy} onCheckedChange={accepted=>void action('set_server_eula',{accepted})}/></div>}</CardContent></Card>
        </>}
        {tab==='settings'&&<>
          <div className="page-heading"><span className="eyebrow">MAKE IT YOURS</span><h1>A little fine tuning<span>.</span></h1><p>Simple controls for the way you play.</p></div>
          <Card><CardHeader><CardTitle>Game memory</CardTitle><CardDescription>Leave enough memory available for your operating system.</CardDescription></CardHeader><CardContent><div className="memory-value">{((settings?.memoryMib??4096)/1024).toFixed(1)} <span>GiB</span></div><Slider aria-label="Game memory in MiB" min={1024} max={32768} step={512} value={[settings?.memoryMib??4096]} disabled={busy} onValueChange={([memoryMib])=>setSettings(current=>current?{...current,memoryMib}:current)}/><div className="slider-labels"><span>1 GiB</span><span>32 GiB</span></div><Button className="mt-5" disabled={busy||!settings} onClick={()=>void save()}>Save preferences</Button></CardContent></Card>
          <div className="detail-grid mt-5"><Card><CardHeader><CardTitle>Files & diagnostics</CardTitle><CardDescription>Repair checks your managed files and downloads only what is missing or damaged.</CardDescription></CardHeader><CardContent className="flex flex-wrap gap-2"><Button variant="outline" onClick={()=>void action('open_folder',{folder:'data'})}><FolderOpen/>Data folder</Button><Button variant="outline" onClick={()=>void action('open_folder',{folder:'logs'})}>Logs</Button><Button variant="secondary" disabled={busy||running||!installed} onClick={()=>void run('repair')}><RefreshCw/>Repair installation</Button></CardContent></Card><Card><CardHeader><CardTitle>Launcher updates</CardTitle><CardDescription>New launcher builds install automatically on startup.</CardDescription></CardHeader><CardContent><Button variant="outline" disabled={busy} onClick={()=>void updateLauncher()}><RefreshCw/>Check for updates</Button><p className="mt-3 text-xs text-muted-foreground" role="status">{updateStatus||`Installed: Build ${status?.build??'…'}`}</p></CardContent></Card></div>
          <button className="text-action mt-6" onClick={()=>setAdvanced(!advanced)}>{advanced?'Hide':'Show'} advanced authentication settings</button>{advanced&&<Card className="mt-3"><CardContent className="pt-5"><Label htmlFor="client-id">Microsoft application ID</Label><Input id="client-id" value={settings?.microsoftClientId??''} onChange={event=>setSettings(current=>current?{...current,microsoftClientId:event.target.value}:current)} className="mt-2"/><p className="text-xs text-muted-foreground mt-2">Changing the application ID requires signing in again.</p><div className="mt-4 flex items-center justify-between gap-4"><Label htmlFor="browser-login">Use a configured browser callback</Label><Switch id="browser-login" checked={settings?.browserLogin??false} onCheckedChange={browserLogin=>setSettings(current=>current?{...current,browserLogin}:current)}/></div><p className="text-xs text-muted-foreground mt-2">Enable only for an application registration with the desktop callback configured. The existing Auram registration uses the code flow with automatic session renewal.</p><Button className="mt-3" disabled={busy} onClick={()=>void save()}>Save</Button></CardContent></Card>}
        </>}
      </main>
      <footer className="activity-bar" aria-live="polite">{busy?<><LoaderCircle className="animate-spin text-violet-300" size={15}/><div className="activity-content"><div className="activity-text"><span>{progress?.message??'Getting things ready…'}</span>{progress?.totalBytes? <span>{bytes(progress.bytes)} / {bytes(progress.totalBytes)}</span>:percent!==null?<span>{Math.round(percent)}%</span>:null}</div><Progress value={percent??undefined} className={percent===null?'indeterminate':''}/></div><Button size="sm" variant="ghost" onClick={()=>void action('cancel_operation')}>Cancel</Button></>:<><span className="status-dot on"/><span>{running?'Minecraft is running. Closing this window keeps your game open.':'Ready when you are.'}</span><span className="activity-right">AURAM / {status?.pack?.name??'NEW INSTALLATION'}</span></>}</footer>
    </div>
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
