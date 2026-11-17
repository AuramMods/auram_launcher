import { readdirSync,readFileSync,writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { join } from 'node:path';
import { buildInfo } from './version.mjs';
const info=buildInfo();const dir='.build/release';const repo='AuramMods/auram_launcher';
const platforms={};
for(const platform of ['darwin-aarch64','windows-x86_64']) {
 const meta=JSON.parse(readFileSync(join(dir,`${platform}.json`),'utf8'));
 if(meta.commit!==info.commit||meta.build!==info.build||meta.version!==info.version)throw new Error(`Mismatched build artifact: ${platform}`);
 platforms[platform]={url:meta.url,signature:meta.signature};
}
writeFileSync(join(dir,'latest.json'),JSON.stringify({version:info.version,notes:`Auram Build ${info.build}`,pub_date:new Date().toISOString(),platforms},null,2));
writeFileSync(join(dir,'build.json'),JSON.stringify(info,null,2));
const files=readdirSync(dir).filter(name=>!name.endsWith('SHA256SUMS'));
writeFileSync(join(dir,'SHA256SUMS'),files.map(name=>`${createHash('sha256').update(readFileSync(join(dir,name))).digest('hex')}  ${name}`).join('\n')+'\n');
const gh=(...args)=>execFileSync('gh',[...args,'--repo',repo],{encoding:'utf8'});
let existing;
try {existing=JSON.parse(gh('release','view',info.tag,'--json','isDraft,url'));}catch(error){if(!String(error.stderr).includes('release not found'))throw error;}
if(existing&&!existing.isDraft) {console.log(`Release already published; assets remain immutable: ${existing.url}`);process.exit(0);}
const notes=`Auram Build ${info.build}\n\nCommit: ${info.commit}\n\nNative Apple Silicon macOS and Windows x64 launcher. Includes incremental pack updates, persistent Microsoft sign-in, server generation, and signed automatic launcher updates.\n\nmacOS: install Auram in Applications before launching. This initial distribution uses ad-hoc platform signing; allow the app in Privacy & Security if macOS requests it. Updater artifacts are cryptographically signed.\n`;
writeFileSync('.build/release-notes.md',notes);
if(!existing)gh('release','create',info.tag,'--target',info.commit,'--title',`Build ${info.build}`,'--notes-file','.build/release-notes.md','--draft');
// A failed draft may be retried; only draft assets can be replaced.
gh('release','upload',info.tag,...readdirSync(dir).map(name=>join(dir,name)),'--clobber');
gh('release','edit',info.tag,'--draft=false','--latest');
console.log(`Published https://github.com/${repo}/releases/tag/${info.tag}`);
