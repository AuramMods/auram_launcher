import { mkdtempSync,readFileSync,rmSync,writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { buildInfo } from './version.mjs';
import { prepareRelease,releaseRepository as repo } from './release-assets.mjs';

const info=buildInfo();const dir='.build/release';
const gh=(...args)=>execFileSync('gh',[...args,'--repo',repo],{encoding:'utf8'});
let existing;
try {existing=JSON.parse(gh('release','view',info.tag,'--json','isDraft,url,targetCommitish'));}catch(error){if(!String(error.stderr).includes('release not found'))throw error;}
if(existing&&existing.targetCommitish!==info.commit)throw new Error(`Immutable tag ${info.tag} already belongs to a different commit`);
if(existing&&!existing.isDraft) {console.log(`Release already published; assets remain immutable: ${existing.url}`);process.exit(0);}

const {files,signatures}=prepareRelease(dir,info);
// Verify the actual updater bodies against the embedded public key before any
// release is exposed. Private signing material is needed only by build jobs.
const temporary=mkdtempSync(join(tmpdir(),'auram-release-signatures-'));
try {
  const config=JSON.parse(readFileSync('src-tauri/tauri.conf.json','utf8'));
  const publicKey=join(temporary,'updater.pub');
  writeFileSync(publicKey,Buffer.from(config.plugins.updater.pubkey,'base64'));
  for(const {updater,signature} of signatures) {
    const signatureFile=join(temporary,'artifact.sig');
    writeFileSync(signatureFile,Buffer.from(signature,'base64'));
    execFileSync('minisign',['-V','-p',publicKey,'-x',signatureFile,'-m',join(dir,updater)],{stdio:'inherit'});
  }
} finally {rmSync(temporary,{recursive:true,force:true});}

// Retrying an older failed build must not demote a newer published build.
let latest;
try {latest=JSON.parse(gh('release','view','--json','tagName'));}catch(error){if(!String(error.stderr).includes('release not found'))throw error;}
const latestBuild=Number(latest?.tagName.match(/^build-(\d+)$/)?.[1]??0);
const notes=`Auram Build ${info.build}\n\nCommit: ${info.commit}\n\nApple Silicon macOS DMG and Windows x64 per-user installer, both built from this exact commit by GitHub Actions.\n\nIncludes the revised server list and SFTP deployments, parallel file activation, automatic client pack updates, automatic memory selection, simplified account controls, and signed automatic launcher updates.\n\nInstall the Mac app in Applications before launching. macOS uses ad-hoc platform signing without notarization; Windows is not Authenticode signed. Initial installation may require OS trust confirmation. Both updater artifacts are cryptographically signed and verified before publication.\n`;
writeFileSync('.build/release-notes.md',notes);
if(!existing)gh('release','create',info.tag,'--target',info.commit,'--title',`Build ${info.build}`,'--notes-file','.build/release-notes.md','--draft');
// Only drafts may replace assets. Publish once both complete sets are present.
gh('release','upload',info.tag,...files.map(name=>join(dir,name)),'--clobber');
gh('release','edit',info.tag,'--draft=false',`--latest=${info.build>=latestBuild}`);
console.log(`Published https://github.com/${repo}/releases/tag/${info.tag}`);
