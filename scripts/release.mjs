import { execFileSync } from 'node:child_process';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
export function advanceRelease(cwd=process.cwd()) {
  const run=(args,inherit=false)=>execFileSync('git',args,{cwd,encoding:'utf8',stdio:inherit?'inherit':undefined});
  run(['fetch','--prune','origin']);
  const target=run(['rev-parse','origin/main']).trim();
  const count=run(['rev-list','--count',target]).trim();
  run(['push','origin',`${target}:refs/heads/release`]);
  return {target,count,tag:`build-${count}`};
}
export async function release() {
  const run=(command,args,inherit=false)=>execFileSync(command,args,{encoding:'utf8',stdio:inherit?'inherit':undefined});
  const {target,count,tag}=advanceRelease();
  const gh=(...args)=>run('gh',args).trim();
  const repo='AuramMods/auram_launcher';
  let runs=[];
  for(let attempt=0;attempt<10;attempt++) {
    runs=JSON.parse(gh('run','list','--repo',repo,'--workflow','release.yml','--commit',target,'--json','databaseId,status,conclusion,url','--limit','10'));
    if(runs.length)break;
    await new Promise(resolve=>setTimeout(resolve,1500));
  }
  const running=runs.find(run=>run.status!=='completed');
  if(running){console.log(`Build ${count}: ${target}\n${running.url}`);return;}
  const succeeded=runs.find(run=>run.conclusion==='success');
  if(succeeded){console.log(`Build ${count} already published: https://github.com/${repo}/releases/tag/${tag}`);return;}
  if(runs[0]) {run('gh',['run','rerun',String(runs[0].databaseId),'--repo',repo],true);console.log(`Retrying ${runs[0].url}`);}
  else {run('gh',['workflow','run','release.yml','--repo',repo,'--ref','release','-f',`commit=${target}`],true);console.log('Dispatched release workflow.');}
  console.log(`Build ${count}: ${target}\nhttps://github.com/${repo}/actions/workflows/release.yml`);
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)) release().catch(error=>{console.error(error.message);process.exitCode=1;});
