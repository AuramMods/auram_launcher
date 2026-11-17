//! Auram's original G1 tuning, selected by the user's heap allocation.
use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profiles {
    high_memory_above_mib: u64,
    low: Profile,
    high: Profile,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    initial_memory_mib: u64,
    flags: Vec<String>,
}

pub fn jvm_arguments(memory_mib: u64) -> Result<Vec<String>> {
    // Shared with the settings UI so its preview uses the actual launch policy.
    let profiles: Profiles = serde_json::from_str(include_str!("../../src/lib/jvm-profiles.json"))?;
    let profile = if memory_mib > profiles.high_memory_above_mib {
        profiles.high
    } else {
        profiles.low
    };
    let mut arguments = vec![
        format!("-Xmx{memory_mib}M"),
        format!("-Xms{}M", memory_mib.min(profile.initial_memory_mib)),
    ];
    arguments.extend(profile.flags);
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_allocation_selects_original_tuning_without_overriding_the_heap() {
        for memory in [1024, 8192, 9216, 9728, 12288, 20480, 32768] {
            let arguments = jvm_arguments(memory).unwrap();
            let high = memory > 9216;
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
}
