import { readdirSync, mkdirSync, copyFileSync, readFileSync,writeFileSync } from 'node:fs';
import { join, basename } from 'node:path';
import { buildInfo } from './version.mjs';
import { createHash } from 'node:crypto';
const [target,platform]=process.argv.slice(2);const info=buildInfo();
const walk=dir=>readdirSync(dir,{withFileTypes:true}).flatMap(entry=>entry.isDirectory()?walk(join(dir,entry.name)):[join(dir,entry.name)]);
const files=walk(join(process.env.CARGO_TARGET_DIR||'src-tauri/target',target,'release/bundle')).filter(file=>{
 const name=basename(file);
 return platform==='darwin-aarch64'
  ? /\.app\.tar\.gz(?:\.sig)?$/.test(name)||(name.includes(`_${info.version}_`)&&name.endsWith('.dmg'))
  : name.includes(`_${info.version}_`)&&/-setup\.exe(?:\.sig)?$/.test(name);
});
if(platform==='darwin-aarch64'&&!files.some(file=>file.endsWith('.dmg')))throw new Error('The Mac build did not produce its DMG');
const dir='.build/artifacts';mkdirSync(dir,{recursive:true});
for(const file of files) copyFileSync(file,join(dir,basename(file)));
const updater=files.find(file=>platform==='darwin-aarch64'?file.endsWith('.app.tar.gz'):file.endsWith('-setup.exe'));
if(!updater)throw new Error(`Missing updater bundle for ${platform}`);
const signature=readFileSync(`${updater}.sig`,'utf8').trim();
writeFileSync(join(dir,`${platform}.json`),JSON.stringify({platform,build:info.build,commit:info.commit,version:info.version,url:`https://github.com/AuramMods/auram_launcher/releases/download/${info.tag}/${encodeURIComponent(basename(updater))}`,signature,files:files.map(file=>({name:basename(file),size:readFileSync(file).length,sha256:createHash('sha256').update(readFileSync(file)).digest('hex')}))},null,2));
