import { spawnSync } from 'node:child_process';
import { prepareVersion } from './version.mjs';
import { readFileSync,writeFileSync } from 'node:fs';
prepareVersion();
const config=JSON.parse(readFileSync('.build/tauri-version.json','utf8'));config.identifier='art.arcane.auram.dev';writeFileSync('.build/tauri-dev.json',JSON.stringify(config));
const result=spawnSync('pnpm',['exec','tauri','dev','--config','.build/tauri-dev.json',...process.argv.slice(2)],{stdio:'inherit',env:{...process.env,AURAM_DEV:'1'},shell:process.platform==='win32'});
process.exit(result.status??1);
