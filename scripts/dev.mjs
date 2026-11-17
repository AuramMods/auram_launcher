import { spawnSync } from 'node:child_process';
import { prepareVersion } from './version.mjs';
prepareVersion();
const result=spawnSync('pnpm',['exec','tauri','dev','--config','.build/tauri-version.json',...process.argv.slice(2)],{stdio:'inherit',env:{...process.env,AURAM_DEV:'1'},shell:process.platform==='win32'});
process.exit(result.status??1);
