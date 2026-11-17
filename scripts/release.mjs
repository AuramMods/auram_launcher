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
  // Query the actual release before rebuilding; published assets remain immutable.
  let published;
  try { published=JSON.parse(gh('release','view',tag,'--repo',repo,'--json','isDraft,url,targetCommitish')); }
  catch(error) { if(!String(error.stderr).includes('release not found'))throw error; }
  if(published&&!published.isDraft) {
    if(published.targetCommitish!==target)throw new Error(`Immutable tag ${tag} already belongs to a different commit`);
    console.log(`Build ${count} already published: ${published.url}\nCommit: ${target}`);return;
  }
  let runs=[];
  for(let attempt=0;attempt<10;attempt++) {
    runs=JSON.parse(gh('run','list','--repo',repo,'--workflow','release.yml','--commit',target,'--json','databaseId,status,conclusion,url','--limit','10'));
    if(runs.length)break;
    await new Promise(resolve=>setTimeout(resolve,1500));
  }
  const running=runs.find(run=>run.status!=='completed');
  const succeeded=runs.find(run=>run.conclusion==='success');
  let workflow=running||succeeded;
  if(!workflow&&runs[0]) {
    run('gh',['run','rerun',String(runs[0].databaseId),'--repo',repo],true);workflow=runs[0];
  } else if(!workflow) {
    run('gh',['workflow','run','release.yml','--repo',repo,'--ref','release','-f',`commit=${target}`],true);
    for(let attempt=0;attempt<20&&!workflow;attempt++) {
      await new Promise(resolve=>setTimeout(resolve,1500));
      workflow=JSON.parse(gh('run','list','--repo',repo,'--workflow','release.yml','--commit',target,'--json','databaseId,status,conclusion,url','--limit','1'))[0];
    }
  }
  if(!workflow)throw new Error('The release workflow was not returned by GitHub. Retry pnpm release to observe it.');
  console.log(`Build ${count}: ${target}\nmacOS ARM64 + Windows x64: ${workflow.url}`);
  run('gh',['run','watch',String(workflow.databaseId),'--repo',repo,'--interval','15','--exit-status'],true);
  const result=JSON.parse(gh('release','view',tag,'--repo',repo,'--json','isDraft,url,targetCommitish'));
  if(result.isDraft||result.targetCommitish!==target)throw new Error(`Workflow finished without publishing the captured commit ${target}`);
  console.log(`Published Build ${count}: ${result.url}\nCommit: ${target}`);
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)) release().catch(error=>{console.error(error.message);process.exitCode=1;});
