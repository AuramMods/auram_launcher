import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync,writeFileSync,rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { buildInfo } from './version.mjs';
import { advanceRelease } from './release.mjs';
test('build number follows real reachable commits including merge history',()=>{
 const dir=mkdtempSync(join(tmpdir(),'auram-version-'));
 const git=(...args)=>execFileSync('git',args,{cwd:dir,encoding:'utf8'}).trim();
 try {git('init','-b','main');git('config','user.email','test@example.test');git('config','user.name','Test');
 writeFileSync(join(dir,'a'),'one');git('add','.');git('commit','-m','first');git('checkout','-b','feature');writeFileSync(join(dir,'b'),'two');git('add','.');git('commit','-m','second');git('checkout','main');writeFileSync(join(dir,'c'),'three');git('add','.');git('commit','-m','third');git('merge','feature','--no-ff','-m','merge');
 const info=buildInfo(dir);assert.equal(info.build,4);assert.equal(info.version,'0.0.4');assert.equal(info.tag,'build-4');assert.equal(info.commit,git('rev-parse','HEAD'));
 }finally{rmSync(dir,{recursive:true,force:true});}
});
test('release advances to remote main, retries without commits, and rejects divergence',()=>{
 const dir=mkdtempSync(join(tmpdir(),'auram-release-'));const remote=join(dir,'remote.git');const work=join(dir,'work');
 const run=(args,cwd=dir)=>execFileSync('git',args,{cwd,encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
 try {
  run(['init','--bare',remote]);run(['clone',remote,work]);run(['checkout','-b','main'],work);run(['config','user.email','test@example.test'],work);run(['config','user.name','Test'],work);
  writeFileSync(join(work,'a'),'one');run(['add','.'],work);run(['commit','-m','first'],work);run(['push','origin','main'],work);
  const first=advanceRelease(work);assert.equal(first.count,'1');assert.equal(first.target,run(['rev-parse','HEAD'],work));
  assert.deepEqual(advanceRelease(work),first);
  writeFileSync(join(work,'b'),'local-unpublished');run(['add','.'],work);run(['commit','-m','unpublished'],work);
  assert.deepEqual(advanceRelease(work),first);
  run(['push','origin','HEAD:release'],work);
  assert.throws(()=>advanceRelease(work));
 }finally{rmSync(dir,{recursive:true,force:true});}
});
