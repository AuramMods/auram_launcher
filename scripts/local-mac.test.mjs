import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { releaseWorkspace, signingEnvironment } from './local-mac.mjs';

function fixture() {
  const directory=mkdtempSync(join(tmpdir(),'auram-local-release-'));
  const git=(...args)=>execFileSync('git',args,{cwd:directory,encoding:'utf8'}).trim();
  git('init','-b','main');git('config','user.email','test@example.test');git('config','user.name','Test');
  writeFileSync(join(directory,'source'),'committed');git('add','.');git('commit','-m','source');
  return {directory,git,close(){rmSync(directory,{recursive:true,force:true});}};
}
test('local release checkout pins the commit and leaves dirty working files untouched',()=>{
  const f=fixture();let workspace;
  try {
    const commit=f.git('rev-parse','HEAD');writeFileSync(join(f.directory,'source'),'uncommitted');
    workspace=releaseWorkspace(f.directory,commit);
    assert.equal(readFileSync(join(workspace.directory,'source'),'utf8'),'committed');
    assert.equal(execFileSync('git',['rev-parse','HEAD'],{cwd:workspace.directory,encoding:'utf8'}).trim(),commit);
    assert.equal(readFileSync(join(f.directory,'source'),'utf8'),'uncommitted');
    const path=workspace.directory;workspace.close();workspace=null;assert.equal(existsSync(path),false);
    assert.match(f.git('status','--short'),/source/);
  }finally{workspace?.close();f.close();}
});
test('local signing reads a configured file without changing the parent environment',()=>{
  const f=fixture();
  try {
    writeFileSync(join(f.directory,'signing key'),'test-key-material');
    writeFileSync(join(f.directory,'.auram-release.local.json'),JSON.stringify({updaterKeyFile:'signing key'}));
    const original={PATH:'example'};const env=signingEnvironment(f.directory,original);
    assert.equal(env.TAURI_SIGNING_PRIVATE_KEY,'test-key-material');assert.equal(original.TAURI_SIGNING_PRIVATE_KEY,undefined);
    assert.equal(signingEnvironment(f.directory,{TAURI_SIGNING_PRIVATE_KEY:'provided'}).TAURI_SIGNING_PRIVATE_KEY,'provided');
  }finally{f.close();}
});
test('artifact collection excludes older cached Mac distributions',()=>{
  const f=fixture();
  try {
    const target='aarch64-apple-darwin';const cache=join(f.directory,'shared target');
    const bundle=join(cache,target,'release/bundle');mkdirSync(join(bundle,'dmg'),{recursive:true});mkdirSync(join(bundle,'macos'));
    for(const name of ['Auram_0.0.0_aarch64.dmg','Auram_0.0.1_aarch64.dmg'])writeFileSync(join(bundle,'dmg',name),'test-dmg');
    writeFileSync(join(bundle,'macos/Auram.app.tar.gz'),'test-updater');writeFileSync(join(bundle,'macos/Auram.app.tar.gz.sig'),'test-signature');
    execFileSync(process.execPath,[fileURLToPath(new URL('./collect-artifacts.mjs',import.meta.url)),target,'darwin-aarch64'],{cwd:f.directory,env:{...process.env,CARGO_TARGET_DIR:cache}});
    const output=join(f.directory,'.build/artifacts');assert.equal(readdirSync(output).length,4);
    assert.equal(existsSync(join(output,'Auram_0.0.0_aarch64.dmg')),false);
    const meta=JSON.parse(readFileSync(join(output,'darwin-aarch64.json'),'utf8'));
    assert.equal(meta.commit,f.git('rev-parse','HEAD'));assert.equal(meta.version,'0.0.1');
  }finally{f.close();}
});
