import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

export const releaseRepository='AuramMods/auram_launcher';
export function platformFiles(info) {
  return {
    'darwin-aarch64': {updater:'Auram.app.tar.gz',installer:`Auram_${info.version}_aarch64.dmg`},
    'windows-x86_64': {updater:`Auram_${info.version}_x64-setup.exe`,installer:`Auram_${info.version}_x64-setup.exe`},
  };
}
export function prepareRelease(directory,info) {
  const platforms={};const files=[];const signatures=[];
  for(const [platform,{updater,installer}] of Object.entries(platformFiles(info))) {
    const metadataName=`${platform}.json`;
    const meta=JSON.parse(readFileSync(join(directory,metadataName),'utf8'));
    if(meta.platform!==platform||meta.commit!==info.commit||meta.build!==info.build||meta.version!==info.version)throw new Error(`Mismatched build artifact: ${platform}`);
    const required=[...new Set([updater,`${updater}.sig`,installer])].sort();
    if(!Array.isArray(meta.files)||JSON.stringify(meta.files.map(file=>file.name).sort())!==JSON.stringify(required))throw new Error(`Incomplete distribution: ${platform}`);
    for(const file of meta.files) {
      // Names must be one of the exact expected installers/signatures above.
      const bytes=readFileSync(join(directory,file.name));
      if(!bytes.length||bytes.length!==file.size||createHash('sha256').update(bytes).digest('hex')!==file.sha256)throw new Error(`Artifact integrity mismatch: ${file.name}`);
    }
    const signature=readFileSync(join(directory,`${updater}.sig`),'utf8').trim();
    if(!signature||signature!==meta.signature)throw new Error(`Updater signature mismatch: ${platform}`);
    const url=`https://github.com/${releaseRepository}/releases/download/${info.tag}/${encodeURIComponent(updater)}`;
    if(meta.url!==url)throw new Error(`Updater URL does not identify the immutable release: ${platform}`);
    platforms[platform]={url,signature};
    files.push(...required,metadataName);
    signatures.push({updater,signature});
  }
  writeFileSync(join(directory,'latest.json'),JSON.stringify({version:info.version,notes:`Auram Build ${info.build}`,pub_date:new Date().toISOString(),platforms},null,2));
  writeFileSync(join(directory,'build.json'),JSON.stringify(info,null,2));
  files.push('latest.json','build.json');
  writeFileSync(join(directory,'SHA256SUMS'),files.map(name=>`${createHash('sha256').update(readFileSync(join(directory,name))).digest('hex')}  ${name}`).join('\n')+'\n');
  return {files:[...files,'SHA256SUMS'],signatures};
}
