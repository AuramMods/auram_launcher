//! Auram's memory preferences, automatic RAM policy and G1 tuning.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

const MIB: u64 = 1024 * 1024;
const MID_SYSTEM_MEMORY_ABOVE_MIB: u64 = 23 * 1024;
const HIGH_SYSTEM_MEMORY_ABOVE_MIB: u64 = 35 * 1024;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub enum MemoryTuning {
    #[serde(rename = "Low memory")]
    Low,
    #[serde(rename = "High memory")]
    High,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticMemory {
    pub system_memory_mib: Option<u64>,
    pub maximum_mib: u64,
    pub initial_mib: u64,
    pub profile: MemoryTuning,
    pub high_system_memory_above_mib: u64,
}
impl AutomaticMemory {
    pub fn from_system_bytes(bytes: u64) -> Self {
        // Compare bytes before rounding so both cutoffs retain their exact boundaries.
        let high_boundary = HIGH_SYSTEM_MEMORY_ABOVE_MIB * MIB;
        let (maximum_gib, initial_gib, profile) = if bytes > high_boundary {
            (20, 12, MemoryTuning::High)
        } else if bytes > MID_SYSTEM_MEMORY_ABOVE_MIB * MIB {
            (12, 9, MemoryTuning::Low)
        } else {
            (9, 9, MemoryTuning::Low)
        };
        Self {
            system_memory_mib: (bytes > 0).then_some(bytes / MIB),
            maximum_mib: maximum_gib * 1024,
            initial_mib: initial_gib * 1024,
            profile,
            high_system_memory_above_mib: HIGH_SYSTEM_MEMORY_ABOVE_MIB,
        }
    }
}
pub fn automatic_memory() -> &'static AutomaticMemory {
    static MEMORY: std::sync::OnceLock<AutomaticMemory> = std::sync::OnceLock::new();
    MEMORY.get_or_init(|| {
        let mut system = sysinfo::System::new();
        system.refresh_memory();
        // A failed/zero reading falls back to the original low-memory preset.
        AutomaticMemory::from_system_bytes(system.total_memory())
    })
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct MemoryPreferences {
    pub automatic: bool,
    pub manual_heap_gib: Option<u8>,
}
impl Default for MemoryPreferences {
    fn default() -> Self {
        Self {
            automatic: true,
            manual_heap_gib: None,
        }
    }
}
impl MemoryPreferences {
    pub fn validate(self) -> Result<()> {
        let profiles = Profiles::load()?;
        match self.manual_heap_gib {
            Some(gib) if !profiles.supports(gib) => bail!(
                "Choose a heap size between {} and {} GB",
                profiles.manual_minimum_gib,
                profiles.manual_maximum_gib
            ),
            None if !self.automatic => bail!("Choose a manual heap size"),
            _ => Ok(()),
        }
    }

    fn manual_mib(self, profiles: &Profiles) -> Option<u64> {
        if self.automatic {
            return None;
        }
        self.manual_heap_gib
            .filter(|&heap| profiles.supports(heap))
            .map(|heap| u64::from(heap) * 1024)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profiles {
    manual_minimum_gib: u8,
    manual_maximum_gib: u8,
    high_memory_at_mib: u64,
    low: Profile,
    high: Profile,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    initial_memory_mib: u64,
    flags: Vec<String>,
}

impl Profiles {
    fn load() -> Result<Self> {
        Ok(serde_json::from_str(include_str!("../jvm-profiles.json"))?)
    }
    fn supports(&self, heap_gib: u8) -> bool {
        (self.manual_minimum_gib..=self.manual_maximum_gib).contains(&heap_gib)
    }
    fn manual_arguments(self, maximum_mib: u64) -> Vec<String> {
        let tuning = if maximum_mib >= self.high_memory_at_mib {
            MemoryTuning::High
        } else {
            MemoryTuning::Low
        };
        self.arguments(maximum_mib, tuning)
    }
    fn configured_arguments(
        self,
        preferences: MemoryPreferences,
        automatic: &AutomaticMemory,
    ) -> Vec<String> {
        match preferences.manual_mib(&self) {
            Some(maximum_mib) => self.manual_arguments(maximum_mib),
            None => self.arguments(automatic.maximum_mib, automatic.profile),
        }
    }
    fn arguments(self, maximum_mib: u64, tuning: MemoryTuning) -> Vec<String> {
        let profile = match tuning {
            MemoryTuning::Low => self.low,
            MemoryTuning::High => self.high,
        };
        let mut arguments = vec![
            format!("-Xmx{maximum_mib}M"),
            format!("-Xms{}M", maximum_mib.min(profile.initial_memory_mib)),
        ];
        arguments.extend(profile.flags);
        arguments
    }
}

pub fn jvm_arguments(memory_mib: u64) -> Result<Vec<String>> {
    Ok(Profiles::load()?.manual_arguments(memory_mib))
}

pub fn configured_jvm_arguments(preferences: MemoryPreferences) -> Result<Vec<String>> {
    Ok(Profiles::load()?.configured_arguments(preferences, automatic_memory()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heap_profiles_retain_original_tuning_and_valid_initial_bounds() {
        for memory in [
            1024, 4096, 9216, 10240, 11264, 12287, 12288, 20480, 21504, 32768,
        ] {
            let arguments = jvm_arguments(memory).unwrap();
            let high = memory >= 12288;
            assert_eq!(arguments[0], format!("-Xmx{memory}M"));
            assert_eq!(
                arguments[1],
                format!("-Xms{}M", memory.min(if high { 12288 } else { 9216 }))
            );
            assert_eq!(
                arguments.iter().filter(|a| a.starts_with("-Xmx")).count(),
                1
            );
            assert_eq!(
                arguments.iter().filter(|a| a.starts_with("-Xms")).count(),
                1
            );
            for flag in [
                "-XX:+UseG1GC",
                "-XX:+ParallelRefProcEnabled",
                "-XX:+DisableExplicitGC",
                "-XX:+AlwaysPreTouch",
                "-XX:InitiatingHeapOccupancyPercent=15",
                "-Dfml.readTimeout=120",
                "-Dfml.loginTimeout=120",
            ] {
                assert!(arguments.iter().any(|a| a == flag));
            }
            assert!(arguments.contains(&format!(
                "-XX:MaxGCPauseMillis={}",
                if high { 16 } else { 50 }
            )));
            assert!(arguments.contains(&format!(
                "-XX:G1HeapWastePercent={}",
                if high { 8 } else { 5 }
            )));
            assert!(arguments.contains(&format!(
                "-XX:G1MixedGCCountTarget={}",
                if high { 8 } else { 4 }
            )));
            assert!(arguments.contains(&format!(
                "-XX:G1ReservePercent={}",
                if high { 15 } else { 10 }
            )));
            assert_eq!(
                arguments
                    .iter()
                    .any(|a| a == "-XX:+UnlockExperimentalVMOptions"),
                high
            );
            if high {
                assert!(arguments.contains(&"-XX:G1MixedGCLiveThresholdPercent=85".into()));
                assert!(arguments.contains(&"-XX:G1RSetUpdatingPauseTimePercent=5".into()));
            }
        }
    }

    #[test]
    fn saved_manual_heap_is_used_only_in_manual_mode() {
        let profiles = Profiles::load().unwrap();
        for heap in 4..=32 {
            let manual = MemoryPreferences {
                automatic: false,
                manual_heap_gib: Some(heap),
            };
            assert!(manual.validate().is_ok());
            assert_eq!(manual.manual_mib(&profiles), Some(u64::from(heap) * 1024));
            let auto = MemoryPreferences {
                automatic: true,
                ..manual
            };
            assert_eq!(auto.manual_mib(&profiles), None);
        }
        for heap in [None, Some(3), Some(33)] {
            let manual = MemoryPreferences {
                automatic: false,
                manual_heap_gib: heap,
            };
            assert!(manual.validate().is_err());
            assert_eq!(manual.manual_mib(&profiles), None);
        }
    }

    #[test]
    fn automatic_heap_and_tuning_preserve_the_exact_system_ram_boundaries() {
        use MemoryTuning::{High, Low};
        let gib = 1024 * MIB;
        for (bytes, maximum, initial, tuning) in [
            (0, 9, 9, Low),
            (23 * gib - 1, 9, 9, Low),
            (23 * gib, 9, 9, Low),
            (23 * gib + 1, 12, 9, Low),
            (32 * gib, 12, 9, Low),
            (35 * gib - 1, 12, 9, Low),
            (35 * gib, 12, 9, Low),
            (35 * gib + 1, 20, 12, High),
            (64 * gib, 20, 12, High),
        ] {
            let memory = AutomaticMemory::from_system_bytes(bytes);
            assert_eq!(memory.maximum_mib, maximum * 1024);
            assert_eq!(memory.initial_mib, initial * 1024);
            assert_eq!(memory.profile, tuning);
            assert_eq!(
                memory.system_memory_mib,
                if bytes == 0 { None } else { Some(bytes / MIB) }
            );
            let arguments = Profiles::load()
                .unwrap()
                .configured_arguments(MemoryPreferences::default(), &memory);
            assert_eq!(arguments[0], format!("-Xmx{}M", memory.maximum_mib));
            assert_eq!(arguments[1], format!("-Xms{}M", memory.initial_mib));
            assert!(arguments.contains(&format!(
                "-XX:MaxGCPauseMillis={}",
                if tuning == High { 16 } else { 50 }
            )));
        }
    }

    #[test]
    fn twelve_gib_keeps_the_low_flags_only_in_automatic_mode() {
        let memory = AutomaticMemory::from_system_bytes(32 * 1024 * MIB);
        let preferences = MemoryPreferences {
            automatic: true,
            manual_heap_gib: Some(12),
        };
        let automatic = Profiles::load()
            .unwrap()
            .configured_arguments(preferences, &memory);
        assert_eq!(automatic[2..], jvm_arguments(9 * 1024).unwrap()[2..]);
        let manual = Profiles::load().unwrap().configured_arguments(
            MemoryPreferences {
                automatic: false,
                ..preferences
            },
            &memory,
        );
        assert_eq!(manual, jvm_arguments(12 * 1024).unwrap());
    }
}
