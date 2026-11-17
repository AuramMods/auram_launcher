import test from 'node:test';
import assert from 'node:assert/strict';
import {deploymentAction,serverAddress,emptyConnection} from '../src/lib/server-instances.ts';
test('server actions distinguish unversioned servers, pack updates and exact installed builds',()=>{
  const target={name:'151',sha:'a'.repeat(40)};
  assert.equal(deploymentAction(null,target).label,'Full Deploy');
  assert.equal(deploymentAction({version:'145',commit:''},target).label,'Deploy Update 151');
  assert.equal(deploymentAction({version:'151',commit:''},target).current,true);
  assert.equal(deploymentAction({version:'151',commit:'b'.repeat(40)},target).current,false);
  assert.equal(deploymentAction({version:'151',commit:target.sha},target).label,'Up to date');
  assert.equal(deploymentAction(null,null).label,'Install Auram first');
});
test('server addresses keep local paths distinct from remote roots and each connection has its own form state',()=>{
  const a=emptyConnection(),b=emptyConnection();a.host='changed';assert.equal(b.host,'');
  assert.equal(serverAddress({kind:'local',directory:'/Servers/My 世界'}),'/Servers/My 世界');
  assert.equal(serverAddress({kind:'sftp',connection:{...b,host:'example.com',port:2022,username:'player',directory:'/server'}}),'player@example.com:2022 · /server');
});
