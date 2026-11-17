import test from 'node:test';
import assert from 'node:assert/strict';
import {jvmProfile,highMemoryAboveMib} from '../src/lib/jvm-profile.ts';

test('memory slider previews the same profile boundary and heap bounds as Rust',()=>{
  assert.equal(highMemoryAboveMib,9216);
  for(const memory of [1024,8192,9216,9728,12288,20480,32768]) {
    const profile=jvmProfile(memory);
    const high=memory>highMemoryAboveMib;
    assert.equal(profile.label,high?'High memory':'Low memory');
    assert.equal(profile.initialMemoryMib,Math.min(memory,high?12288:9216));
    assert.ok(profile.flags.includes(`-XX:MaxGCPauseMillis=${high?16:50}`));
  }
});
