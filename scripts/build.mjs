import { spawnSync } from 'node:child_process';
import { prepareVersion } from './version.mjs';
const info=prepareVersion();
console.log(`Building Auram Build ${info.build} (${info.commit})`);
const result=spawnSync('pnpm',['exec','tauri','build','--config','.build/tauri-version.json',...process.argv.slice(2)],{stdio:'inherit',shell:process.platform==='win32'});
process.exit(result.status??1);
