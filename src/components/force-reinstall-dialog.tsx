import {Button} from './ui/button';
import {Dialog,DialogContent,DialogHeader,DialogTitle,DialogDescription,DialogFooter} from './ui/dialog';

export function ForceReinstallDialog({open,onOpenChange,onConfirm,clientPath,busy}:{open:boolean;onOpenChange:(open:boolean)=>void;onConfirm:()=>void;clientPath:string|undefined;busy:boolean}) {
  return <Dialog open={open} onOpenChange={value=>{if(!busy)onOpenChange(value);}}>
    <DialogContent className="sm:max-w-lg"><DialogHeader><DialogTitle>Force reinstall Auram?</DialogTitle><DialogDescription>This permanently deletes the entire local client pack, including worlds, screenshots, options, resource packs, shader packs, and custom files. Auram then installs a fresh copy.</DialogDescription></DialogHeader>
      {clientPath&&<code className="rounded-md border border-zinc-800 bg-black p-3 text-xs break-all">{clientPath}</code>}
      <p className="text-sm text-muted-foreground">Back up anything you want to keep first. Cancellation or a failed download will not restore deleted files.</p>
      <p className="text-xs text-muted-foreground">Your sign-in, launcher settings, server instances, and shared download caches are kept.</p>
      <DialogFooter><Button variant="outline" disabled={busy} onClick={()=>onOpenChange(false)}>Cancel</Button><Button variant="destructive" disabled={busy} onClick={onConfirm}>Delete pack & reinstall</Button></DialogFooter>
    </DialogContent>
  </Dialog>;
}
