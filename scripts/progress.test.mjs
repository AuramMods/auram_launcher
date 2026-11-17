import test from 'node:test';
import assert from 'node:assert/strict';
import {isNewerProgress,formatBytes,formatPercent,remainingPhases} from '../src/lib/job-progress.ts';
test('late IPC and polling snapshots never rewind the displayed operation',()=>{
 const current={jobId:4,revision:30};
 assert.equal(isNewerProgress({jobId:4,revision:29},current),false);
 assert.equal(isNewerProgress({jobId:3,revision:100},current),false);
 assert.equal(isNewerProgress({jobId:4,revision:30},current),false);
 assert.equal(isNewerProgress({jobId:5,revision:1},current),true);
 assert.equal(isNewerProgress({jobId:4,revision:31},current),true);
});
test('completed and skipped segments disappear while remaining steps keep their original numbers',()=>{
 const steps=['complete','skipped','running','pending','failed','cancelled'].map((state,index)=>({id:String(index),state}));
 assert.deepEqual(remainingPhases(steps).map(({phase,index})=>[phase.id,index]),[['2',2],['3',3],['4',4],['5',5]]);
 assert.deepEqual(remainingPhases(steps.slice(0,2)),[]);
});
test('counts remain readable for small files and incomplete verification never rounds to 100%',()=>{
 assert.equal(formatBytes(128),'128 B');assert.equal(formatBytes(1024),'1.0 KB');
 assert.equal(formatBytes(1024**2),'1.0 MB');assert.equal(formatPercent(99.99),'99%');assert.equal(formatPercent(100),'100%');
});
