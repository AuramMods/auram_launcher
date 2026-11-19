import profiles from '../../src-tauri/jvm-profiles.json' with { type: 'json' };

export type MemoryPreferences = { automatic: boolean; manualHeapGib: number | null };
export type AutomaticMemory = {
  systemMemoryMib: number | null;
  maximumMib: number;
  initialMib: number;
  profile: 'Low memory' | 'High memory';
};

const mibPerGib = 1024;
const highTuningAtGib = profiles.highMemoryAtMib / mibPerGib;
const recommendedMinimumGib = profiles.recommendedMinimumGib;
const recommendedMaximumGib = profiles.recommendedMaximumGib;
export const minimumHeapGib = profiles.manualMinimumGib;
export const maximumHeapGib = profiles.manualMaximumGib;
export const heapNotches = [recommendedMinimumGib, highTuningAtGib, recommendedMaximumGib];
export const heapOverheadGib = profiles.estimatedOffHeapGib;

export function heapPosition(heapGib: number) {
  return (heapGib - minimumHeapGib) / (maximumHeapGib - minimumHeapGib) * 100;
}

export function describeGameMemory(preferences: MemoryPreferences, automatic: AutomaticMemory) {
  const manual = preferences.manualHeapGib;
  const useManual = !preferences.automatic && manual !== null && Number.isInteger(manual)
    && manual >= minimumHeapGib && manual <= maximumHeapGib;
  const heapGib = useManual ? manual : automatic.maximumMib / mibPerGib;
  const profile = heapGib >= highTuningAtGib ? profiles.high : profiles.low;
  const warnings: string[] = [];
  if (heapGib < recommendedMinimumGib) {
    warnings.push(`The modpack may run out of memory below ${recommendedMinimumGib} GB.`);
  }
  if (heapGib < highTuningAtGib) {
    warnings.push(`Below ${highTuningAtGib} GB, disable Distant Horizons rendering to save memory.`);
  }
  if (heapGib > recommendedMaximumGib) {
    warnings.push(`Heaps above ${recommendedMaximumGib} GB can cause instability over time in demanding workloads.`);
  }

  return {
    heapGib,
    initialGib: useManual ? Math.min(heapGib, profile.initialMemoryMib / mibPerGib) : automatic.initialMib / mibPerGib,
    profile: useManual ? profile.label : automatic.profile,
    totalGib: heapGib + heapOverheadGib,
    warnings,
  };
}
