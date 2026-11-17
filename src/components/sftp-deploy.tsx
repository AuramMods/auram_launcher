import React from 'react';
import {invoke} from '@tauri-apps/api/core';
import {ArrowUpFromLine,RefreshCw,ShieldCheck} from 'lucide-react';
import {Button} from './ui/button';
import {Card,CardHeader,CardTitle,CardDescription,CardContent} from './ui/card';
import {Input} from './ui/input';
import {Label} from './ui/label';
import {formatBytes} from '../lib/job-progress';

export type SftpSettings={schema:number;host:string;port:number;username:string;directory:string;authentication:string;keyPath:string;fingerprint:string};
type Version={version:string;commit:string;minecraft:string;forge:string};
type Plan={remoteVersion:Version|null;targetVersion:Version;strategy:string;changelists:number;uploads:number;removals:number;renames:number;reused:number;replacements:number;bytes:number;recoveryRequired:boolean;notes:string[];actions:{kind:string;path:string;from:string|null}[]};
type Check={remoteVersion:Version|null;buildRequired:boolean;hostKey:{fingerprint:string;changed:boolean}|null;plan:Plan|null};
const empty:SftpSettings={schema:1,host:'',port:22,username:'',directory:'.',authentication:'password',keyPath:'',fingerprint:''};

export function SftpDeploy({saved,builtVersion,busy,refresh,onError,onNotice}:{saved:SftpSettings|undefined;builtVersion:string|null;busy:boolean;refresh:()=>Promise<unknown>;onError:(text:string)=>void;onNotice:(text:string)=>void}) {
  const [settings,setSettings]=React.useState(saved??empty);
  const [password,setPassword]=React.useState('');
  const [passphrase,setPassphrase]=React.useState('');
  const [pending,setPending]=React.useState(false);
  const [check,setCheck]=React.useState<Check|null>(null);
  const [showFiles,setShowFiles]=React.useState(false);
  const savedJson=JSON.stringify(saved);
  React.useEffect(()=>{if(savedJson)setSettings(JSON.parse(savedJson));},[savedJson]);
  React.useEffect(()=>{setCheck(null);},[builtVersion]);
  function edit<K extends keyof SftpSettings>(key:K,value:SftpSettings[K]) {setSettings(old=>({...old,[key]:value}));setCheck(null);}
  const disabled=busy||pending;
  async function save() {
    await invoke('save_sftp',{settings,secret:password||passphrase?{password,passphrase}:null});
    setPassword('');setPassphrase('');await refresh();
  }
  async function inspect(trustFingerprint:string|null=null) {
    setPending(true);onError('');setCheck(null);
    try {if(!trustFingerprint)await save();setCheck(await invoke<Check>('check_sftp',{trustFingerprint}));await refresh();}
    catch(error){onError(String(error));}finally{setPending(false);}
  }
  async function deploy(recoverOnly:boolean) {
    setPending(true);onError('');
    try {onNotice(await invoke<string>('deploy_sftp',{recoverOnly}));setCheck(await invoke<Check>('check_sftp',{trustFingerprint:null}));await refresh();}
    catch(error){onError(String(error));setCheck(null);}finally{setPending(false);}
  }
  async function forget() {setPending(true);try{await invoke('forget_sftp');setSettings(empty);setPassword('');setPassphrase('');setCheck(null);await refresh();}catch(error){onError(String(error));}finally{setPending(false);}}
  const plan=check?.plan;
  const sameVersion=plan?.remoteVersion?.version===plan?.targetVersion.version&&(!plan?.remoteVersion?.commit||plan.remoteVersion.commit===plan.targetVersion.commit);
  const changed=plan?plan.uploads+plan.removals+plan.renames+plan.replacements:0;
  return <Card className="mt-5"><CardHeader><CardTitle className="section-title"><ArrowUpFromLine/>Deploy over SFTP</CardTitle><CardDescription>Send the built server’s mods, scripts, config, and kubejs. Worlds, server.properties, access lists, runtime files, and everything else stay on the server.</CardDescription></CardHeader><CardContent>
    <div className="sftp-fields">
      <div><Label htmlFor="sftp-host">Host</Label><Input id="sftp-host" value={settings.host} onChange={e=>edit('host',e.target.value)} disabled={disabled} placeholder="sftp.example.com" autoCapitalize="none"/></div>
      <div><Label htmlFor="sftp-port">Port</Label><Input id="sftp-port" type="number" min={1} max={65535} value={settings.port} onChange={e=>edit('port',Number(e.target.value))} disabled={disabled}/></div>
      <div><Label htmlFor="sftp-user">Username</Label><Input id="sftp-user" value={settings.username} onChange={e=>edit('username',e.target.value)} disabled={disabled} autoComplete="username"/></div>
      <div><Label htmlFor="sftp-directory">Server root directory</Label><Input id="sftp-directory" value={settings.directory} onChange={e=>edit('directory',e.target.value)} disabled={disabled} placeholder="/server"/></div>
      <div><Label htmlFor="sftp-auth">Authentication</Label><select id="sftp-auth" className="sftp-select" value={settings.authentication} onChange={e=>edit('authentication',e.target.value)} disabled={disabled}><option value="password">Password</option><option value="key">Private key file</option></select></div>
      {settings.authentication==='password'?<div><Label htmlFor="sftp-password">Password</Label><Input id="sftp-password" type="password" value={password} onChange={e=>{setPassword(e.target.value);setCheck(null);}} disabled={disabled} autoComplete="current-password" placeholder="Leave blank to keep saved password"/></div>:<><div><Label htmlFor="sftp-key">Private key path</Label><Input id="sftp-key" value={settings.keyPath} onChange={e=>edit('keyPath',e.target.value)} disabled={disabled} placeholder="Full path to your OpenSSH private key"/></div><div><Label htmlFor="sftp-passphrase">Key passphrase</Label><Input id="sftp-passphrase" type="password" value={passphrase} onChange={e=>{setPassphrase(e.target.value);setCheck(null);}} disabled={disabled} autoComplete="off" placeholder="Optional · saved securely"/></div></>}
    </div>
    <p className="sftp-help">Connection details are remembered. Passwords and key passphrases are saved in your operating system’s credential store.</p>
    <div className="flex flex-wrap gap-2 mt-4"><Button variant="outline" disabled={disabled||!settings.host||!settings.username} onClick={()=>void inspect()}><RefreshCw/>Save & inspect server</Button><Button variant="ghost" disabled={disabled||!saved?.host} onClick={()=>void forget()}>Forget connection</Button><span className="sftp-built">Built locally: {builtVersion??'build a server first'}</span></div>
    {!builtVersion&&<p className="sftp-help">Build the server above to prepare a versioned deployment payload.</p>}
    {check?.hostKey&&<div className="sftp-preview"><div className="section-title"><ShieldCheck/>{check.hostKey.changed?'SSH host key changed':'Verify SSH host key'}</div><p className="sftp-help">Compare this fingerprint with your server provider before trusting it. No password has been sent to this unverified host.</p><code className="sftp-fingerprint">{check.hostKey.fingerprint}</code><Button className="mt-3" variant="secondary" disabled={disabled} onClick={()=>void inspect(check.hostKey!.fingerprint)}>Trust this key & inspect</Button></div>}
    {check?.buildRequired&&<div className="sftp-preview"><div className="section-title">Remote Auram: {check.remoteVersion?.version??'No version detected'}</div><p className="sftp-help">Connection verified. Build the server locally to prepare the file-change preview and deployment.</p></div>}
    {plan&&<div className="sftp-preview"><div className="sftp-version-row"><div><span>REMOTE AURAM</span><strong>{plan.remoteVersion?.version??'No version detected'}</strong></div><span className="text-muted-foreground">→</span><div><span>BUILT SERVER</span><strong>{plan.targetVersion.version}</strong></div></div>
      {plan.recoveryRequired?<><p className="sftp-help">A deployment lock is present. Recover only after the other launcher’s deployment has stopped. Recovery restores retained originals before you retry.</p><Button variant="secondary" disabled={disabled} onClick={()=>void deploy(true)}>Recover interrupted deployment</Button></>:<><p className="sftp-help">{plan.strategy==='changelists'?`${plan.changelists} validated changelist${plan.changelists===1?'':'s'} · `:'Full directory replacement · '}{plan.uploads} uploads ({formatBytes(plan.bytes)}) · {plan.replacements?`${plan.replacements} directories replaced`:`${plan.renames} renames · ${plan.removals} removals · ${plan.reused} unchanged`}</p><p className="sftp-help">{plan.replacements>0&&<strong>Full deployment replaces mods, scripts, config and kubejs, including custom files inside them. Original directories are backed up. </strong>}Stop the remote Minecraft server before deploying. SFTP does not stop or restart it. Replaced files are backed up; the version is updated last.</p><div className="flex flex-wrap gap-2"><Button disabled={disabled||(!!sameVersion&&changed===0)} onClick={()=>void deploy(false)}><ArrowUpFromLine/>{plan.replacements>0?'Full deploy':sameVersion?'Up to date':'Deploy update'}</Button><Button variant="ghost" onClick={()=>setShowFiles(!showFiles)}>{showFiles?'Hide':'Show'} file changes</Button></div>{showFiles&&<ul className="sftp-changes">{plan.actions.length?plan.actions.map((action,i)=><li key={i}><span>{action.kind==='replaceDirectory'?'Replace':action.kind}</span><code>{action.from?`${action.from} → `:''}{action.path}</code></li>):<li>No payload files need changing.</li>}</ul>}</>}
      {plan.notes.map((note,i)=><p className="sftp-help" key={i}>{note}</p>)}
    </div>}
  </CardContent></Card>;
}
