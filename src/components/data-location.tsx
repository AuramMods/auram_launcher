import React from 'react';
import {invoke} from '@tauri-apps/api/core';
import {FolderOpen,RefreshCw,Undo2} from 'lucide-react';
import {Button} from './ui/button';
import {Card,CardContent,CardDescription,CardHeader,CardTitle} from './ui/card';
import {Dialog,DialogContent,DialogDescription,DialogFooter,DialogHeader,DialogTitle} from './ui/dialog';

type Location={currentPath:string;defaultPath:string;folderName:string;isDefault:boolean;pendingPath:string|null;cleanupPending:boolean;unavailable:string|null;notice:string|null;applicationPath:string;platform:'windows'|'macos'};
type Change={kind:'move';parentDirectory:string}|{kind:'reset'|'retry'|'cancel'|'resetUnavailable'};
type Choice={parentDirectory:string;dataDirectory:string};
type Props={disabled:boolean;onBusyChange:(busy:boolean)=>void;onError:(message:string)=>void;onNotice:(message:string)=>void};
export function DataLocationSettings({disabled,onBusyChange,onError,onNotice}:Props) {
  const [location,setLocation]=React.useState<Location|null>(null);
  const [failure,setFailure]=React.useState<string|null>(null);
  const [choosing,setChoosing]=React.useState(false);
  const [confirmation,setConfirmation]=React.useState<{change:Change;destination:string}|null>(null);
  const refresh=React.useCallback(async()=>{try {setLocation(await invoke<Location>('get_data_location'));setFailure(null);}catch(error){setFailure(String(error));}},[]);
  React.useEffect(()=>{void refresh();},[refresh]);
  async function choose() {
    setChoosing(true);
    try {const selected=await invoke<Choice|null>('choose_data_directory');if(selected)setConfirmation({change:{kind:'move',parentDirectory:selected.parentDirectory},destination:selected.dataDirectory});}
    catch(error){onError(String(error));}finally{setChoosing(false);}
  }
  async function apply(change:Change) {
    setConfirmation(null);onBusyChange(true);
    try {const message=await invoke<string|null>('change_data_location',{change});if(message)onNotice(message);await refresh();}
    catch(error){onError(String(error));await refresh();}finally{onBusyChange(false);}
  }
  const locked=disabled||choosing||!location;
  return <>
    <Card className="mt-5"><CardHeader><CardTitle>Installation & data location</CardTitle><CardDescription>Client files, worlds, shared downloads, server workspace, settings, and logs live here.</CardDescription></CardHeader><CardContent className="space-y-4">
      {failure&&<p className="text-xs text-destructive" role="alert">{failure} <button className="underline" onClick={()=>void refresh()}>Retry</button></p>}
      <div><p className="text-xs text-muted-foreground">Data folder{location?.isDefault?' · Default':''}</p><code className="mt-1 block break-all text-xs text-zinc-300">{location?.currentPath??'Loading location…'}</code></div>
      <p className="text-xs text-muted-foreground">Choose a parent directory. Auram creates <span className="text-zinc-300">{location?.folderName??'AuramLauncher'}</span> inside it. The app and its data can be on different drives.</p>
      <div className="flex flex-wrap gap-2"><Button variant="outline" disabled={locked||!!location?.unavailable||!!location?.pendingPath} onClick={()=>void choose()}><FolderOpen/>{choosing?'Choosing…':'Choose directory'}</Button><Button variant="ghost" disabled={locked||!!location?.isDefault||!!location?.pendingPath} onClick={()=>location&&setConfirmation({change:{kind:location.unavailable?'resetUnavailable':'reset'},destination:location.defaultPath})}><Undo2/>Reset to default</Button></div>
      {location?.unavailable&&<p className="text-xs text-muted-foreground" role="alert">{location.unavailable}</p>}
      {location?.notice&&<p className="text-xs text-muted-foreground" role="status">{location.notice}</p>}
      {location?.pendingPath&&<div className="space-y-2 border-t border-zinc-800 pt-3"><p className="text-xs text-muted-foreground">{location.cleanupPending?'The new location is active. Finish removing the verified original copies.':'An interrupted data move is ready to retry.'}</p><code className="block break-all text-xs">{location.pendingPath}</code><div className="flex flex-wrap gap-2"><Button variant="outline" disabled={locked} onClick={()=>setConfirmation({change:{kind:'retry'},destination:location.pendingPath!})}><RefreshCw/>{location.cleanupPending?'Finish move':'Retry move'}</Button>{!location.cleanupPending&&<Button variant="ghost" disabled={locked} onClick={()=>void apply({kind:'cancel'})}>Cancel unfinished move</Button>}</div></div>}
      <div className="border-t border-zinc-800 pt-3"><p className="text-xs text-muted-foreground">Application folder</p><code className="mt-1 block break-all text-xs text-zinc-400">{location?.applicationPath??'…'}</code><p className="mt-2 text-xs text-muted-foreground">{location?.platform==='windows'?'Choose the application’s installation folder in the Windows installer.':'Move Auram.app to your preferred folder in Finder.'}</p></div>
    </CardContent></Card>
    <Dialog open={confirmation!==null} onOpenChange={open=>{if(!open)setConfirmation(null);}}><DialogContent className="sm:max-w-lg"><DialogHeader><DialogTitle>{confirmation?.change.kind==='resetUnavailable'?'Use the default data folder?':location?.cleanupPending?'Finish moving data?':'Move launcher data?'}</DialogTitle><DialogDescription>{confirmation?.change.kind==='resetUnavailable'?'The unavailable drive’s files will be left in place. Auram will restart using the default folder.':location?.cleanupPending?'Finish removing unchanged original copies. Changed files stay in their old location.':'Close any Minecraft servers or other programs using these files. Auram will copy and verify your data, remove unchanged originals, and restart.'}</DialogDescription></DialogHeader><div className="space-y-3 text-xs"><div><p className="text-muted-foreground">From</p><code className="block break-all mt-1">{location?.currentPath}</code></div><div><p className="text-muted-foreground">To</p><code className="block break-all mt-1 text-violet-200">{confirmation?.destination}</code></div></div><DialogFooter><Button variant="ghost" onClick={()=>setConfirmation(null)}>Cancel</Button><Button disabled={disabled} onClick={()=>{if(confirmation)void apply(confirmation.change);}}>{location?.cleanupPending?'Finish move':confirmation?.change.kind==='resetUnavailable'?'Reset & restart':'Move & restart'}</Button></DialogFooter></DialogContent></Dialog>
  </>;
}
