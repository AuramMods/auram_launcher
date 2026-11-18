import assert from 'node:assert/strict';
import test from 'node:test';
import { describeGameMemory, heapNotches, heapPosition, maximumHeapGib, minimumHeapGib } from '../src/lib/game-memory.ts';

test('the heap slider covers 4–32 GB with three labeled positions', () => {
  assert.equal(minimumHeapGib, 4);
  assert.equal(maximumHeapGib, 32);
  assert.deepEqual(heapNotches, [9, 12, 20]);
  assert.equal(heapPosition(4), 0);
  assert.equal(heapPosition(32), 100);
  assert.equal(heapPosition(12), 8 / 28 * 100);
});

test('the heap thresholds are inclusive only for the high tuning profile', () => {
  for (const [heap, expectedInitial, warningTerms] of [
    [4, 4, ['run out of memory', 'disable Distant Horizons']],
    [8, 8, ['run out of memory', 'disable Distant Horizons']],
    [9, 9, ['disable Distant Horizons']],
    [10, 9, ['disable Distant Horizons']],
    [11, 9, ['disable Distant Horizons']],
    [12, 12, []],
    [20, 12, []],
    [21, 12, ['can cause instability']],
    [32, 12, ['can cause instability']],
  ]) {
    const result = describeGameMemory({ automatic: false, manualHeapGib: heap }, 20 * 1024);
    assert.equal(result.heapGib, heap);
    assert.equal(result.totalGib, heap + 2);
    assert.equal(result.initialGib, expectedInitial);
    assert.equal(result.profile, heap >= 12 ? 'High memory' : 'Low memory');
    assert.equal(result.warnings.length, warningTerms.length);
    warningTerms.forEach((term, index) => assert.ok(result.warnings[index].includes(term)));
  }
});

test('automatic selection uses the detected preset and shows its own guidance', () => {
  const auto = { automatic: true, manualHeapGib: 32 };
  const low = describeGameMemory(auto, 9 * 1024);
  assert.equal(low.heapGib, 9);
  assert.equal(low.totalGib, 11);
  assert.match(low.warnings.join(' '), /disable Distant Horizons/);
  const high = describeGameMemory(auto, 20 * 1024);
  assert.equal(high.heapGib, 20);
  assert.equal(high.totalGib, 22);
  assert.deepEqual(high.warnings, []);
});
