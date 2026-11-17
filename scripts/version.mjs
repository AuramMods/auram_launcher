import { execFileSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
export function buildInfo(cwd=process.cwd()) {
  const git=(...args)=>execFileSync('git',args,{cwd,encoding:'utf8'}).trim();
  const commit=git('rev-parse','HEAD');
  const build=Number(git('rev-list','--count',commit));
  return {build,version:`0.0.${build}`,commit,tag:`build-${build}`};
}
export function prepareVersion() {
  const info=buildInfo();mkdirSync('.build',{recursive:true});
  writeFileSync('.build/tauri-version.json',JSON.stringify({version:info.version,bundle:{macOS:{bundleVersion:String(info.build)}}},null,2));
  writeFileSync('.build/build.json',JSON.stringify(info,null,2));return info;
}
if (process.argv[1] && resolve(process.argv[1])===fileURLToPath(import.meta.url)) console.log(JSON.stringify(prepareVersion()));
