import test from 'node:test';
import assert from 'node:assert/strict';
import {createStartupTask} from '../src/lib/startup.ts';
const ready={state:'ready',version:'151',message:null,detail:null};
test('startup serializes updater jobs and waits for pack completion, once across effect replay',async()=>{
  const calls=[];let finishLauncher,finishPack;
  const launcher=new Promise(resolve=>finishLauncher=resolve),pack=new Promise(resolve=>finishPack=resolve);
  const start=createStartupTask({initialize:async()=>calls.push('initialize'),updateLauncher:()=>{calls.push('launcher');return launcher;},preparePack:()=>{calls.push('pack');return pack;},launcherStatus:()=>{}});
  const first=start(),second=start();assert.equal(first,second);
  await Promise.resolve();assert.deepEqual(calls,['initialize','launcher']);
  finishLauncher('Current');await Promise.resolve();assert.deepEqual(calls,['initialize','launcher','pack']);
  let resolved=false;first.then(()=>resolved=true);await Promise.resolve();assert.equal(resolved,false);
  finishPack(ready);assert.deepEqual(await first,ready);assert.equal(start(),first);
});
test('launcher update failure still attempts the client pack update',async()=>{
  let attempted=0;const messages=[];
  const start=createStartupTask({initialize:async()=>{},updateLauncher:async()=>{throw Error('Launcher endpoint offline');},preparePack:async()=>{attempted++;return ready;},launcherStatus:text=>messages.push(text)});
  assert.deepEqual(await start(),ready);assert.equal(attempted,1);assert.match(messages[0],/endpoint offline/);
});
test('pack failure is retained for a visible retry rather than silently succeeding',async()=>{
  const failed={...ready,state:'failed',message:'Checksum mismatch'};
  const start=createStartupTask({initialize:async()=>{},updateLauncher:async()=>'',preparePack:async()=>failed,launcherStatus:()=>{}});
  assert.deepEqual(await start(),failed);
});
