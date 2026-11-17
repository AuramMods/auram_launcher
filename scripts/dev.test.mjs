import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,readFileSync,rmSync,writeFileSync,existsSync} from 'node:fs';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {runDevelopment} from './dev.mjs';
test('development restarts the CLI and frontend only for an explicit native restart request',()=>{
 const dir=mkdtempSync(join(tmpdir(),'auram-dev-'));
 try {
  const count=join(dir,'count');const fixture=join(dir,'launcher.mjs');const marker=join(dir,'restart.json');
  writeFileSync(fixture,`import fs from 'node:fs';const count=Number(fs.existsSync(process.env.COUNT)?fs.readFileSync(process.env.COUNT,'utf8'):0);fs.writeFileSync(process.env.COUNT,String(count+1));if(count===0)fs.writeFileSync(process.env.AURAM_DEV_RESTART_FILE,JSON.stringify({schema:1,restart:true}));`);
  assert.equal(runDevelopment({command:process.execPath,args:[fixture],environment:{...process.env,COUNT:count},marker}),0);
  assert.equal(readFileSync(count,'utf8'),'2');assert.equal(existsSync(marker),false);
 }finally{rmSync(dir,{recursive:true,force:true});}
});
test('ordinary development failures are returned without an automatic restart loop',()=>{
 const dir=mkdtempSync(join(tmpdir(),'auram-dev-error-'));
 try {assert.equal(runDevelopment({command:process.execPath,args:['-e','process.exit(9)'],marker:join(dir,'restart.json')}),9);}
 finally{rmSync(dir,{recursive:true,force:true});}
});
