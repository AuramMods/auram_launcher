export type ServerVersion={version:string;commit:string;minecraft:string;forge:string};
export type SftpSettings={schema:number;host:string;port:number;username:string;directory:string;authentication:string;keyPath:string;fingerprint:string};
export type ServerTarget={kind:'local';directory:string}|{kind:'sftp';connection:SftpSettings};
export type ServerInstance={id:string;name:string;target:ServerTarget;knownVersion:ServerVersion|null;checkedAt:string|null;lastError:string|null};
export type ServerDraft={id:string|null;name:string;target:ServerTarget};
export type ServerInspection={version:ServerVersion|null;hostKey:{fingerprint:string;changed:boolean}|null;recoveryRequired:boolean};
export const emptyConnection=():SftpSettings=>({schema:1,host:'',port:22,username:'',directory:'.',authentication:'password',keyPath:'',fingerprint:''});
export function deploymentAction(version:ServerVersion|null,target:{name:string;sha:string}|null) {
  if(!target)return {label:'Install Auram first',current:false};
  if(!version)return {label:'Full Deploy',current:false};
  const current=version.version===target.name&&(!version.commit||version.commit===target.sha);
  return {label:current?'Up to date':`Deploy Update ${target.name}`,current};
}
export function serverAddress(target:ServerTarget) {
  return target.kind==='local'?target.directory:`${target.connection.username}@${target.connection.host}:${target.connection.port} · ${target.connection.directory}`;
}
