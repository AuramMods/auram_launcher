use super::Platform;
use anyhow::Result;
use serde_json::Value;
use std::path::Path;

pub(super) fn macos_libraries() -> Result<Vec<Value>> {
    Ok(serde_json::from_str(include_str!(
        "../../macos-libraries.json"
    ))?)
}

/// Minecraft 1.20.1's JNA 5.12.1 aborts on long macOS dlerror() messages.
/// Use Mojang's matched 5.13.0 pair (from 1.20.2), which includes JNA #1456.
/// Patch only in-memory launch metadata; never mutate upstream/cache snapshots.
pub(super) fn client_metadata(mut metadata: Value, platform: Platform) -> Result<Value> {
    if platform != Platform::MacArm {
        return Ok(metadata);
    }
    let replacements = macos_libraries()?;
    if let Some(libraries) = metadata["libraries"].as_array_mut() {
        for library in libraries {
            for replacement in &replacements {
                let name = replacement["name"].as_str().unwrap();
                let identity = name.rsplit_once(':').unwrap().0;
                if library["name"].as_str().is_some_and(|existing| {
                    existing
                        .rsplit_once(':')
                        .is_some_and(|(id, _)| id == identity)
                }) {
                    library["name"] = replacement["name"].clone();
                    library["downloads"]["artifact"] = replacement["downloads"]["artifact"].clone();
                }
            }
        }
    }
    // Forge may reference old JNA filenames even when it inherits Mojang's jars.
    fn patch_argument(value: &mut Value) {
        match value {
            Value::String(argument) => {
                if let Some(modules) = argument.strip_prefix("-DmergeModules=") {
                    let modules = modules
                        .split(',')
                        .map(|module| {
                            for artifact in ["jna-platform", "jna"] {
                                let version = module
                                    .strip_prefix(&format!("{artifact}-"))
                                    .and_then(|v| v.strip_suffix(".jar"));
                                if version.is_some_and(|v| {
                                    !v.is_empty()
                                        && v.chars().all(|c| c.is_ascii_digit() || c == '.')
                                }) {
                                    return format!("{artifact}-5.13.0.jar");
                                }
                            }
                            module.to_owned()
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    *argument = format!("-DmergeModules={modules}");
                }
            }
            Value::Array(values) => values.iter_mut().for_each(patch_argument),
            Value::Object(object) => {
                if let Some(value) = object.get_mut("value") {
                    patch_argument(value);
                }
            }
            _ => {}
        }
    }
    if let Some(arguments) = metadata.get_mut("arguments").and_then(|v| v.get_mut("jvm")) {
        patch_argument(arguments);
    }
    Ok(metadata)
}

/// Keep the original Forge file and substitute only its vanilla JNA pair.
pub(super) fn server_jna_arguments(
    content: &str,
    library_root: &Path,
    replacements: &[Value],
) -> Result<Option<String>> {
    let mut changed = false;
    let mut result = String::new();
    for line in content.lines() {
        let token = line.trim().trim_matches('"');
        if let Some(classpath) = token.strip_prefix("-DlegacyClassPath=") {
            let mut entries = classpath.split(':').map(str::to_owned).collect::<Vec<_>>();
            let mut replaced = false;
            for library in replacements {
                let path = library["downloads"]["artifact"]["path"].as_str().unwrap();
                let old = format!("libraries/{}", path.replace("5.13.0", "5.12.1"));
                for entry in &mut entries {
                    if *entry == old {
                        *entry = crate::storage::safe_join(library_root, path)?
                            .to_string_lossy()
                            .into_owned();
                        replaced = true;
                    }
                }
            }
            if replaced {
                result.push_str(&super::argfile(&[format!(
                    "-DlegacyClassPath={}",
                    entries.join(":")
                )]));
                changed = true;
                continue;
            }
        }
        // Some generated Forge argfiles also merge these modules by filename.
        if token.starts_with("-DmergeModules=") {
            result.push_str(
                &line
                    .replace("jna-5.12.1.jar", "jna-5.13.0.jar")
                    .replace("jna-platform-5.12.1.jar", "jna-platform-5.13.0.jar"),
            );
        } else {
            result.push_str(line);
        }
        result.push('\n');
    }
    Ok(changed.then_some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::{Credentials, Profile},
        download::Downloader,
        runtime::{library_downloads, maven_path, Ready, Runtime, Versions},
        storage::{Layout, Settings},
    };
    use serde_json::json;
    use std::path::PathBuf;

    fn metadata(version: &str) -> Value {
        json!({
            "libraries":[
                {"name":format!("net.java.dev.jna:jna:{version}"),"downloads":{"artifact":{"path":format!("net/java/dev/jna/jna/{version}/jna-{version}.jar"),"url":"https://example.invalid/old.jar","sha1":"old","size":1}}},
                {"name":format!("net.java.dev.jna:jna-platform:{version}"),"downloads":{"artifact":{"path":format!("net/java/dev/jna/jna-platform/{version}/jna-platform-{version}.jar"),"url":"https://example.invalid/old-platform.jar","sha1":"old","size":1}}},
                {"name":"unrelated:library:1","downloads":{"artifact":{"path":"unrelated/library/1/library-1.jar","url":"https://example.invalid/unchanged.jar","sha1":"unchanged","size":9}}}
            ],
            "arguments":{"jvm":["-cp","${classpath}",format!("-DmergeModules=jna-{version}.jar,jna-platform-{version}.jar,other.jar,jna-helper.jar"),{"rules":[{"action":"allow","os":{"name":"osx"}}],"value":[format!("-DmergeModules=jna-{version}.jar,jna-platform-{version}.jar")]}]}
        })
    }

    #[test]
    fn mac_uses_matching_verified_jna_pair_and_preserves_other_libraries() {
        let original = metadata("5.12.1");
        let patched = client_metadata(original.clone(), Platform::MacArm).unwrap();
        assert_eq!(
            original["libraries"][0]["name"],
            "net.java.dev.jna:jna:5.12.1"
        );
        assert_eq!(patched["libraries"][2], original["libraries"][2]);
        assert_eq!(
            patched["libraries"][0]["name"],
            "net.java.dev.jna:jna:5.13.0"
        );
        assert_eq!(
            patched["libraries"][1]["name"],
            "net.java.dev.jna:jna-platform:5.13.0"
        );
        assert_eq!(
            patched["arguments"]["jvm"][2],
            "-DmergeModules=jna-5.13.0.jar,jna-platform-5.13.0.jar,other.jar,jna-helper.jar"
        );
        assert_eq!(
            patched["arguments"]["jvm"][3]["value"][0],
            "-DmergeModules=jna-5.13.0.jar,jna-platform-5.13.0.jar"
        );
        assert_eq!(
            client_metadata(patched.clone(), Platform::MacArm).unwrap(),
            patched
        );
        let downloads =
            library_downloads(&patched, Path::new("libraries"), Platform::MacArm).unwrap();
        assert_eq!(
            downloads[0].target,
            Path::new("libraries/net/java/dev/jna/jna/5.13.0/jna-5.13.0.jar")
        );
        assert_eq!(downloads[0].size, Some(1879325));
        assert_eq!(
            downloads[0].checksum,
            crate::download::Checksum::Sha1("1200e7ebeedbe0d10062093f32925a912020e747".into())
        );
        assert_eq!(
            client_metadata(original.clone(), Platform::WindowsX64).unwrap(),
            original
        );
    }

    #[test]
    fn forge_server_retains_relative_libraries_and_quotes_the_compatible_jna_paths() {
        let content = "-DmergeModules=jna-5.12.1.jar,jna-platform-5.12.1.jar,other.jar\n-DlegacyClassPath=libraries/other.jar:libraries/net/java/dev/jna/jna/5.12.1/jna-5.12.1.jar:libraries/net/java/dev/jna/jna-platform/5.12.1/jna-platform-5.12.1.jar\n--launchTarget forgeserver";
        let cached = Path::new("Auram data 世界/libraries");
        let replacements = macos_libraries().unwrap();
        let revised = server_jna_arguments(content, cached, &replacements)
            .unwrap()
            .unwrap();
        assert!(revised.contains("-DmergeModules=jna-5.13.0.jar,jna-platform-5.13.0.jar,other.jar"));
        assert!(revised.contains(r#""-DlegacyClassPath=libraries/other.jar:Auram data 世界/libraries/net/java/dev/jna/jna/5.13.0/jna-5.13.0.jar:Auram data 世界/libraries/net/java/dev/jna/jna-platform/5.13.0/jna-platform-5.13.0.jar""#));
        assert!(revised.ends_with("--launchTarget forgeserver\n"));
        assert!(server_jna_arguments(&revised, cached, &replacements)
            .unwrap()
            .is_none());
    }

    #[test]
    fn forge_inheritance_cannot_restore_old_jna_or_old_merge_module_names() {
        let temp = tempfile::tempdir().unwrap();
        for platform in [Platform::MacArm, Platform::WindowsX64] {
            let layout = Layout::new(temp.path().join(platform.key())).unwrap();
            let runtime = Runtime {
                net: Downloader::new(&layout).unwrap(),
                layout,
                platform,
            };
            let vanilla = client_metadata(metadata("5.12.1"), platform).unwrap();
            let mut forge = metadata("5.10.0");
            forge["mainClass"] = json!("cpw.mods.bootstraplauncher.BootstrapLauncher");
            let forge = client_metadata(forge, platform).unwrap();
            let ready = Ready {
                java: PathBuf::new(),
                vanilla,
                forge,
                versions: Versions {
                    minecraft: "1.20.1".into(),
                    forge: "47.4.10".into(),
                },
            };
            let session = Credentials {
                profile: Some(Profile {
                    name: "Fixture".into(),
                    uuid: "fixture".into(),
                    xuid: "fixture".into(),
                }),
                ..Default::default()
            };
            let settings = Settings {
                memory: crate::launch_options::MemoryPreferences {
                    automatic: false,
                    manual_heap_gib: Some(20),
                },
                ..Settings::default()
            };
            let args = runtime.arguments(&ready, &session, &settings).unwrap();
            // The test exercises both target policies on either host. PathBuf
            // still serializes using the host's directory separator.
            let classpath =
                args[args.iter().position(|a| a == "-cp").unwrap() + 1].replace('\\', "/");
            let version = if platform == Platform::MacArm {
                "5.13.0"
            } else {
                "5.10.0"
            };
            for artifact in ["jna", "jna-platform"] {
                assert!(classpath.contains(
                    &maven_path(&format!("net.java.dev.jna:{artifact}:{version}")).unwrap()
                ));
            }
            if platform == Platform::MacArm {
                assert!(!classpath.contains("5.12.1"));
                assert!(!classpath.contains("5.10.0"));
                assert!(args
                    .iter()
                    .filter(|a| a.starts_with("-DmergeModules="))
                    .all(|a| !a.contains("5.10.0") && !a.contains("5.12.1")));
            }
            assert_eq!(
                args.iter()
                    .filter(|a| a.as_str() == "-XstartOnFirstThread")
                    .count(),
                usize::from(platform == Platform::MacArm)
            );
            assert!(args.contains(&"-Xmx20480M".into()));
            assert!(args.contains(&"-Xms12288M".into()));
            assert!(args.contains(&"-XX:MaxGCPauseMillis=16".into()));
        }
    }
}
