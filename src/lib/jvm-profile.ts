import profiles from './jvm-profiles.json' with {type:'json'};

export const highMemoryAboveMib=profiles.highMemoryAboveMib;

export function jvmProfile(memoryMib:number) {
  const profile=memoryMib>profiles.highMemoryAboveMib?profiles.high:profiles.low;
  return {...profile,initialMemoryMib:Math.min(memoryMib,profile.initialMemoryMib)};
}
