use crate::storage;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{fs::File, path::Path};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Branding {
    name: String,
    icon: String,
    java_agent: String,
}

pub(crate) fn jvm_arguments(game: &Path) -> Result<Vec<String>> {
    let descriptor = storage::safe_join(game, "branding/branding.json")?;
    let Some(branding) = storage::read_optional::<Branding>(&descriptor)
        .context("Read the Auram game branding descriptor")?
    else {
        return Ok(Vec::new());
    };
    let directory = descriptor
        .parent()
        .context("Branding directory is missing")?;
    if branding.name.trim().is_empty() || branding.name.contains('\0') {
        bail!("Invalid game name in branding.json");
    }
    let icon =
        storage::safe_join(directory, &branding.icon).context("Resolve the game branding icon")?;
    let agent = storage::safe_join(directory, &branding.java_agent)
        .context("Resolve the game branding agent")?;
    File::open(&icon).with_context(|| format!("Open game branding icon {}", icon.display()))?;
    File::open(&agent).with_context(|| format!("Open game branding agent {}", agent.display()))?;
    Ok(vec![
        format!("-Dauram.branding.name={}", branding.name),
        format!("-Dauram.branding.icon={}", icon.display()),
        format!("-javaagent:{}", agent.display()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_follows_relocated_game_and_yields_unquoted_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let game = temp.path().join("Auram 世界");
        let dir = game.join("branding");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(jvm_arguments(&game).unwrap().is_empty());
        std::fs::write(
            dir.join("branding.json"),
            r#"{"name":"Auram","icon":"unicorn 雪.png","javaAgent":"start here.jar"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("unicorn 雪.png"), b"icon").unwrap();
        let error = jvm_arguments(&game).unwrap_err();
        assert!(error.to_string().contains("start here.jar"));
        std::fs::write(dir.join("start here.jar"), b"agent").unwrap();
        assert_eq!(
            jvm_arguments(&game).unwrap(),
            [
                "-Dauram.branding.name=Auram".into(),
                format!(
                    "-Dauram.branding.icon={}",
                    dir.join("unicorn 雪.png").display()
                ),
                format!("-javaagent:{}", dir.join("start here.jar").display()),
            ]
        );
    }

    #[test]
    fn descriptor_cannot_load_an_agent_outside_the_branding_directory() {
        let temp = tempfile::tempdir().unwrap();
        storage::atomic_write(
            &temp.path().join("branding/branding.json"),
            br#"{"name":"Auram","icon":"icon.png","javaAgent":"../external.jar"}"#,
        )
        .unwrap();
        assert!(jvm_arguments(temp.path())
            .unwrap_err()
            .to_string()
            .contains("branding agent"));
    }
}
