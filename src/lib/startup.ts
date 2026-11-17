export type PackPreparation = {
  state:'pending'|'running'|'ready'|'offline'|'notInstalled'|'failed';
  version:string|null;
  message:string|null;
  detail:string|null;
};
type StartupActions = {
  initialize:()=>Promise<unknown>;
  updateLauncher:()=>Promise<string>;
  preparePack:()=>Promise<PackPreparation>;
  launcherStatus:(message:string)=>void;
};
// React's development effect replay shares the same actual startup attempt.
export function createStartupTask(actions:StartupActions) {
  let task:Promise<PackPreparation>|undefined;
  return ()=>task??=(async()=>{
    await actions.initialize();
    // Launcher and pack updates share the backend job lease. Run in order;
    // launcher service/signature failures must not suppress the pack attempt.
    try {actions.launcherStatus(await actions.updateLauncher());}
    catch(error) {actions.launcherStatus(String(error));}
    return actions.preparePack();
  })();
}
