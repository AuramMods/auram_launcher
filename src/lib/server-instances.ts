export type ServerVersion={version:string;commit:string;minecraft:string;forge:string};
export type SftpSettings={schema:number;host:string;port:number;username:string;directory:string;authentication:string;keyPath:string;fingerprint:string};
export type ServerTarget={kind:'local';directory:string}|{kind:'sftp';connection:SftpSettings};
export type PackVersion={name:string;sha:string};
export type ServerInstance={id:string;name:string;target:ServerTarget;memoryGib:number;packVersion:PackVersion|null;knownVersion:ServerVersion|null;checkedAt:string|null;lastError:string|null};
export type ServerDraft={id:string|null;name:string;target:ServerTarget;memoryGib:number};
export type ServerInspection={version:ServerVersion|null;hostKey:{fingerprint:string;changed:boolean}|null;recoveryRequired:boolean};
export const emptyConnection=():SftpSettings=>({schema:1,host:'',port:22,username:'',directory:'.',authentication:'password',keyPath:'',fingerprint:''});
export function deploymentAction(version:ServerVersion|null,target:PackVersion|null) {
  if(!target)return {label:'Install Auram first',current:false};
  if(!version)return {label:'Full Deploy',current:false};
  const current=version.version===target.name&&(!version.commit||version.commit===target.sha);
  const components = (value: string) => value.replace(/^v/, '').split('.').map(Number);
  const from = components(version.version);
  const to = components(target.name);
  let direction = 0;
  for (let i = 0; i < Math.max(from.length, to.length) && direction === 0; i++) {
    direction = (to[i] ?? 0) - (from[i] ?? 0);
  }
  const action = direction > 0 ? 'Deploy Update' : 'Switch to';
  return {label:current?'Up to date':`${action} ${target.name}`,current};
}
export function serverAddress(target:ServerTarget) {
  return target.kind==='local'?target.directory:`${target.connection.username}@${target.connection.host}:${target.connection.port} · ${target.connection.directory}`;
}

export const minimumServerMemory=12;
export const maximumServerMemory=30;
export type LocalServerStatus={id:string;state:'running'|'stopping'|'stopped';exit:string|null};
export type LocalServerAction='start'|'stop'|'restart'|'close';
export type ServerConsoleLine={id:number;source:'stdout'|'stderr'|'command'|'launcher';text:string};
