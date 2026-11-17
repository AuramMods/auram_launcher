import React from 'react';
import {invoke} from '@tauri-apps/api/core';
import {ArrowUpFromLine,ChevronRight,FolderOpen,HardDrive,LoaderCircle,Plus,RefreshCw,Server,ShieldCheck,Trash2,Wifi} from 'lucide-react';
import {Button} from './ui/button';
import {Input} from './ui/input';
import {Label} from './ui/label';
import {Dialog,DialogContent,DialogHeader,DialogTitle,DialogDescription,DialogFooter} from './ui/dialog';
import {deploymentAction,emptyConnection,serverAddress,type ServerDraft,type ServerInspection,type ServerInstance,type SftpSettings} from '../lib/server-instances';

type Props={servers:ServerInstance[];targetVersion:{name:string;sha:string}|null;busy:boolean;refresh:()=>Promise<unknown>;onBusyChange:(busy:boolean)=>void;onError:(text:string)=>void;onNotice:(text:string)=>void};
const newDraft=():ServerDraft=>({id:null,name:'Local server',target:{kind:'local',directory:''}});

export function ServerInstances({servers,targetVersion,busy,refresh,onBusyChange,onError,onNotice}:Props) {
  const [open,setOpen]=React.useState(false);
  const [draft,setDraft]=React.useState<ServerDraft>(newDraft);
  const [baseline,setBaseline]=React.useState('');
  const [password,setPassword]=React.useState('');
  const [passphrase,setPassphrase]=React.useState('');
  const [inspection,setInspection]=React.useState<ServerInspection|null>(null);
  const [pending,setPending]=React.useState(false);
  const [detailError,setDetailError]=React.useState('');
  const request=React.useRef(0);
  const selected=React.useRef<string|null>(null);
  const mounted=React.useRef(true);
  const inspectionRequest=React.useRef<string|null>(null);
  const cancelInspection=React.useCallback(()=>{
    const requestId=inspectionRequest.current;
    if(!requestId)return;
    inspectionRequest.current=null;
    void invoke('cancel_server_inspection',{requestId}).then(()=>refresh()).catch(error=>{if(mounted.current)onError(`Could not cancel server inspection: ${String(error)}`);});
  },[refresh,onError]);
  React.useEffect(()=>{
    mounted.current=true;
    return ()=>{mounted.current=false;request.current++;selected.current=null;cancelInspection();};
  },[cancelInspection]);
  const saved=servers.find(server=>server.id===draft.id);
  const dirty=JSON.stringify(draft)!==baseline||!!password||!!passphrase;
  const disabled=busy||pending;
  function close(value:boolean) {setOpen(value);if(!value){request.current++;selected.current=null;cancelInspection();setPending(false);setPassword('');setPassphrase('');}}
  function edit(next:ServerDraft) {request.current++;setDraft(next);setInspection(null);setDetailError('');}
  function connection<K extends keyof SftpSettings>(key:K,value:SftpSettings[K]) {
    if(draft.target.kind==='sftp')edit({...draft,target:{kind:'sftp',connection:{...draft.target.connection,[key]:value}}});
  }
  function working(value:boolean) {if(mounted.current)setPending(value);onBusyChange(value);}
  async function inspect(id:string,trustFingerprint:string|null=null) {
    cancelInspection();
    const attempt=++request.current;
    const requestId=Array.from(crypto.getRandomValues(new Uint32Array(4)),value=>value.toString(16)).join('-');
    inspectionRequest.current=requestId;
    setPending(true);setDetailError('');setInspection(null);
    try {
      const result=await invoke<ServerInspection>('inspect_server_instance',{id,requestId,trustFingerprint});
      await refresh();
      if(mounted.current&&attempt===request.current&&selected.current===id){
        setInspection(result);
        if(trustFingerprint&&!result.hostKey){
          const next=draft.target.kind==='sftp'?{...draft,target:{kind:'sftp' as const,connection:{...draft.target.connection,fingerprint:trustFingerprint}}}:draft;
          setDraft(next);setBaseline(JSON.stringify(next));
        }
      }
    } catch(error) {if(mounted.current&&attempt===request.current)setDetailError(String(error));await refresh().catch(()=>{});}
    finally {if(inspectionRequest.current===requestId){inspectionRequest.current=null;if(mounted.current)setPending(false);}}
  }
  function details(server:ServerInstance) {
    if(disabled)return;
    const next={id:server.id,name:server.name,target:server.target};
    selected.current=server.id;setDraft(next);setBaseline(JSON.stringify(next));setPassword('');setPassphrase('');setInspection(null);setDetailError('');setOpen(true);
    void inspect(server.id);
  }
  function add() {
    selected.current=null;request.current++;const next=newDraft();setDraft(next);setBaseline(JSON.stringify(next));setPassword('');setPassphrase('');setInspection(null);setDetailError('');setOpen(true);
  }
  async function chooseDirectory() {
    const attempt=request.current;
    try {
      const directory=await invoke<string|null>('choose_server_directory');
      if(directory&&mounted.current&&attempt===request.current)setDraft(previous=>({...previous,name:previous.name==='Local server'?directory.split(/[\\/]/).filter(Boolean).at(-1)??'Local server':previous.name,target:{kind:'local',directory}}));
    }catch(error){if(mounted.current&&attempt===request.current)setDetailError(String(error));}
  }
  async function save() {
    const attempt=++request.current;
    working(true);setDetailError('');
    let instance:ServerInstance|undefined;
    try {
      instance=await invoke<ServerInstance>('save_server_instance',{input:draft,secret:password||passphrase?{password,passphrase}:null});
      await refresh();
      const next={id:instance.id,name:instance.name,target:instance.target};
      if(mounted.current&&attempt===request.current){selected.current=instance.id;setDraft(next);setBaseline(JSON.stringify(next));setPassword('');setPassphrase('');}
    }catch(error){if(mounted.current&&attempt===request.current)setDetailError(String(error));}
    finally{working(false);}
    if(instance&&mounted.current&&attempt===request.current)await inspect(instance.id);
  }
  async function remove() {
    if(!draft.id)return;
    working(true);setDetailError('');
    try {await invoke('remove_server_instance',{id:draft.id});close(false);await refresh();}
    catch(error){setDetailError(String(error));}finally{working(false);}
  }
  async function deploy(server:ServerInstance,recoverOnly=false) {
    if(server.target.kind==='sftp'&&!server.target.connection.fingerprint){details(server);return;}
    close(false);working(true);onError('');onNotice('');
    try {onNotice(await invoke<string>('deploy_server_instance',{id:server.id,recoverOnly}));}
    catch(error){onError(String(error));}
    finally{await refresh().catch(()=>{});working(false);}
  }
  const version=inspection&&!inspection.hostKey?inspection.version:saved?.knownVersion??null;
  const action=deploymentAction(version,targetVersion);
  const recover=!!inspection?.recoveryRequired;
  const canSave=!!draft.name.trim()&&(draft.target.kind==='local'?!!draft.target.directory:!!draft.target.connection.host&&!!draft.target.connection.username);
  return <>
    <div className="page-heading server-heading"><div><span className="eyebrow">BETTER TOGETHER</span><h1>Your servers<span>.</span></h1><p>Deploy Auram {targetVersion?.name??'from your installed pack'} to a local folder or an SFTP server.</p></div><Button onClick={add} disabled={disabled} aria-label="Add server"><Plus/>Add server</Button></div>
    {servers.length===0?<div className="servers-empty"><Server/><h2>No servers yet</h2><p>Add a local directory or connect your hosted server over SFTP.</p><Button variant="outline" onClick={add} disabled={disabled}><Plus/>Add your first server</Button></div>:<ul className="server-list" aria-label="Saved servers">{servers.map(server=>{
      const primary=deploymentAction(server.knownVersion,targetVersion);
      const Icon=server.target.kind==='local'?HardDrive:Wifi;
      return <li className="server-row" key={server.id}><button className="server-open" onClick={()=>details(server)} disabled={disabled} aria-label={`Open ${server.name}`}><span className="server-icon"><Icon size={20}/></span><span className="server-identity"><strong>{server.name}</strong><span className="server-address">{serverAddress(server.target)}</span><span className="server-version">{server.knownVersion?`Auram ${server.knownVersion.version}`:'Unknown version'}{server.lastError&&<span className="server-check-error"> · Last check failed</span>}</span></span><ChevronRight size={16}/></button><Button className="server-deploy" variant={primary.current?'outline':'default'} disabled={disabled||!targetVersion||primary.current} onClick={()=>void deploy(server)} aria-label={`${primary.label} · ${server.name}`}><ArrowUpFromLine/>{primary.label}</Button></li>;
    })}</ul>}
    <Dialog open={open} onOpenChange={close}><DialogContent className="server-dialog sm:max-w-xl"><DialogHeader><DialogTitle>{draft.id?draft.name:'Add server'}</DialogTitle><DialogDescription>{draft.id?'Deploy the installed pack to this server, or edit its connection.':'Choose where to create or update an Auram server.'}</DialogDescription></DialogHeader>
      <div className="server-dialog-body">
        {!draft.id&&<div className="server-types" role="group" aria-label="Server location"><Button variant={draft.target.kind==='local'?'secondary':'outline'} aria-pressed={draft.target.kind==='local'} disabled={disabled} onClick={()=>edit({id:null,name:'Local server',target:{kind:'local',directory:''}})}><HardDrive/>Local directory</Button><Button variant={draft.target.kind==='sftp'?'secondary':'outline'} aria-pressed={draft.target.kind==='sftp'} disabled={disabled} onClick={()=>edit({id:null,name:'SFTP server',target:{kind:'sftp',connection:emptyConnection()}})}><Wifi/>SFTP</Button></div>}
        {draft.id&&<div className="server-version-card" aria-live="polite"><span>CURRENT SERVER VERSION</span><strong>{pending?<><LoaderCircle className="animate-spin" size={17}/>Checking server…</>:version?`Auram ${version.version}`:'Unknown version'}</strong><p>{pending?'Reading auram.json from this destination…':dirty?'Save changes to check this destination.':detailError?'Version check failed. The last known version is shown above.':inspection&&!inspection.hostKey?(inspection.version?'Read from the server’s auram.json.':'No recognized auram.json found. A full deploy will replace the pack directories.'):'The version has not been checked yet.'}</p></div>}
        {detailError&&<div className="server-dialog-error" role="alert"><p>{detailError}</p>{draft.id&&<Button variant="outline" size="sm" disabled={disabled||dirty} onClick={()=>void inspect(draft.id!)}><RefreshCw/>Retry connection</Button>}</div>}
        {inspection?.hostKey&&<div className="sftp-preview"><div className="section-title"><ShieldCheck/>{inspection.hostKey.changed?'SSH host key changed':'Verify SSH host key'}</div><p className="sftp-help">Compare this fingerprint with your server provider. Credentials are sent only after the host key is trusted.</p><code className="sftp-fingerprint">{inspection.hostKey.fingerprint}</code><Button className="mt-3" variant="secondary" disabled={disabled} onClick={()=>void inspect(draft.id!,inspection.hostKey!.fingerprint)}>Trust key & connect</Button></div>}
        <details className="server-configuration" open={draft.id?undefined:true}><summary>{draft.id?'Server settings':'Connection details'}</summary><div className="server-form">
          <div className="server-form-wide"><Label htmlFor="server-name">Name</Label><Input id="server-name" value={draft.name} disabled={disabled} onChange={event=>edit({...draft,name:event.target.value})}/></div>
          {draft.target.kind==='local'?<div className="server-form-wide"><Label htmlFor="server-directory">Server directory</Label><div className="server-directory-picker"><Input id="server-directory" value={draft.target.directory} placeholder="Choose an empty or existing server folder" readOnly/><Button variant="outline" disabled={disabled} onClick={()=>void chooseDirectory()}><FolderOpen/>Choose</Button></div></div>:<>
            <div><Label htmlFor="server-host">Host</Label><Input id="server-host" value={draft.target.connection.host} disabled={disabled} onChange={event=>connection('host',event.target.value)} placeholder="sftp.example.com" autoCapitalize="none"/></div>
            <div><Label htmlFor="server-port">Port</Label><Input id="server-port" type="number" min={1} max={65535} value={draft.target.connection.port} disabled={disabled} onChange={event=>connection('port',Number(event.target.value))}/></div>
            <div><Label htmlFor="server-user">Username</Label><Input id="server-user" value={draft.target.connection.username} disabled={disabled} onChange={event=>connection('username',event.target.value)} autoComplete="username"/></div>
            <div><Label htmlFor="server-root">Server root directory</Label><Input id="server-root" value={draft.target.connection.directory} disabled={disabled} onChange={event=>connection('directory',event.target.value)} placeholder="/server"/></div>
            <div><Label htmlFor="server-auth">Authentication</Label><select id="server-auth" className="sftp-select" value={draft.target.connection.authentication} disabled={disabled} onChange={event=>connection('authentication',event.target.value)}><option value="password">Password</option><option value="key">Private key file</option></select></div>
            {draft.target.connection.authentication==='password'?<div><Label htmlFor="server-password">Password</Label><Input id="server-password" type="password" value={password} disabled={disabled} onChange={event=>setPassword(event.target.value)} autoComplete="current-password" placeholder={draft.id?'Leave blank to keep saved password':'SFTP password'}/></div>:<><div><Label htmlFor="server-key">Private key path</Label><Input id="server-key" value={draft.target.connection.keyPath} disabled={disabled} onChange={event=>connection('keyPath',event.target.value)}/></div><div><Label htmlFor="server-passphrase">Key passphrase</Label><Input id="server-passphrase" type="password" value={passphrase} disabled={disabled} onChange={event=>setPassphrase(event.target.value)} autoComplete="off" placeholder="Optional · saved securely"/></div></>}
            <p className="sftp-help server-form-wide">Credentials are saved in your operating system’s credential store for this server instance.</p>
          </>}
        </div></details>
        {draft.id&&<><p className="sftp-help">Stop the server before deploying. {draft.target.kind==='sftp'?'Only mods, scripts, config and kubejs are synced. Worlds, properties and runtime files stay on the host.':'Java 17, Forge and start scripts are prepared automatically. Worlds, properties and access lists are preserved.'} If there’s no usable changelist route, the four pack directories are replaced with backups.</p>{recover&&<p className="sftp-help">An interrupted or active deployment owns the server lock. Recover only after the other deployment has stopped.</p>}<div className="server-detail-actions">{draft.target.kind==='local'&&<Button variant="outline" size="sm" disabled={disabled} onClick={()=>void invoke('open_server_instance',{id:draft.id}).catch(error=>setDetailError(String(error)))}><FolderOpen/>Open folder</Button>}<Button variant="ghost" size="sm" disabled={disabled||dirty} onClick={()=>void inspect(draft.id!)}><RefreshCw/>Check version</Button><Button variant="ghost" size="sm" disabled={disabled} onClick={()=>void remove()}><Trash2/>Remove from list</Button></div><p className="server-remove-note">Removing a saved server leaves its files in place.</p></>}
      </div>
      <DialogFooter><Button variant="ghost" onClick={()=>close(false)}>Close</Button>{!draft.id?<Button disabled={disabled||!canSave} onClick={()=>void save()}><Plus/>Add server</Button>:<>{dirty&&<Button variant="outline" disabled={disabled||!canSave} onClick={()=>void save()}>Save changes</Button>}<Button disabled={disabled||dirty||!!detailError||!!inspection?.hostKey||!inspection||(!recover&&(!targetVersion||action.current))} onClick={()=>{if(saved)void deploy(saved,recover);}}><ArrowUpFromLine/>{recover?'Recover deployment':action.label}</Button></>}</DialogFooter>
    </DialogContent></Dialog>
  </>;
}
