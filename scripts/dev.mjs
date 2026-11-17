import { spawnSync } from 'node:child_process';
import { prepareVersion } from './version.mjs';
import { existsSync,readFileSync,writeFileSync,rmSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export function runDevelopment({command='pnpm',args,environment=process.env,marker=resolve(`.build/dev-restart-${process.pid}.json`)}) {
  try {
    for (;;) {
      rmSync(marker,{force:true});
      const result=spawnSync(command,args,{stdio:'inherit',env:{...environment,AURAM_DEV:'1',AURAM_DEV_RESTART_FILE:marker},shell:process.platform==='win32'&&command==='pnpm'});
      if(!existsSync(marker))return result.status??1;
      const requested=JSON.parse(readFileSync(marker,'utf8'));
      if(requested.schema!==1||requested.restart!==true)return result.status??1;
      // The native process requested this restart after committing its data
      // location. Let the CLI close Vite, then start both together again.
    }
  }finally{rmSync(marker,{force:true});}
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)) {
  prepareVersion();
  const config=JSON.parse(readFileSync('.build/tauri-version.json','utf8'));config.identifier='art.arcane.auram.dev';writeFileSync('.build/tauri-dev.json',JSON.stringify(config));
  process.exit(runDevelopment({args:['exec','tauri','dev','--config','.build/tauri-dev.json',...process.argv.slice(2)]}));
}
