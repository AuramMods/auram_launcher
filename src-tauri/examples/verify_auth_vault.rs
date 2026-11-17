//! Actual Windows Credential Manager regression with disposable dummy tokens.
//! It never opens a browser, reads a real account entry, or prints token values.
use anyhow::Result;
#[cfg_attr(not(windows), allow(dead_code))]
fn verify() -> Result<()> {
    use anyhow::{bail, Context};
    use auram_core::auth::{Credentials, NativeVault, Profile, Vault};
    use std::collections::BTreeSet;
    let service = format!(
        "art.arcane.auram.credential-test.{:032x}",
        rand::random::<u128>()
    );
    let vault = NativeVault::with_service(&service);
    let entry = |name: &str| keyring::Entry::new(&service, name);
    let mut names = BTreeSet::from(["microsoft".to_owned()]);
    let result = (|| {
        let large = Credentials {
            client_id: "fixture-client".into(),
            refresh_token: "dummy-refresh".repeat(1024),
            minecraft_token: "dummy-minecraft-jwt".repeat(2048),
            expires_at: 123456,
            profile: Some(Profile {
                name: "Player 🦄 名".into(),
                uuid: "a".repeat(32),
                xuid: "12345".into(),
            }),
        };
        match entry("microsoft")?.set_password(&serde_json::to_string(&large)?) {
            Err(keyring::Error::TooLong(_, _)) => {
                println!("PASS reproduced the original Windows single-password size rejection")
            }
            Err(error) => return Err(error.into()),
            Ok(()) => bail!("Fixture did not reproduce the original Windows credential limit"),
        }
        let small = Credentials {
            refresh_token: "legacy-refresh".into(),
            minecraft_token: "legacy-token".into(),
            ..large.clone()
        };
        entry("microsoft")?.set_password(&serde_json::to_string(&small)?)?;
        let loaded = vault.load()?.context("Legacy session missing")?;
        if loaded.refresh_token != small.refresh_token {
            bail!("Legacy session mismatch");
        }
        for credentials in [&large, &small] {
            vault.save(credentials)?;
            let reopened = NativeVault::with_service(&service)
                .load()?
                .context("Saved session missing")?;
            if serde_json::to_value(&reopened)? != serde_json::to_value(credentials)? {
                bail!("Native credential roundtrip mismatch");
            }
            let pointer: serde_json::Value =
                serde_json::from_str(&entry("microsoft")?.get_password()?)?;
            if pointer["auram_vault"] != 2 {
                bail!("Windows session was not migrated to size-safe storage");
            }
            for slot in 0..2 {
                let descriptor = format!("microsoft-v2-slot-{slot}");
                names.insert(descriptor.clone());
                match entry(&descriptor)?.get_password() {
                    Ok(value) => {
                        let data: serde_json::Value = serde_json::from_str(&value)?;
                        for index in 0..data["chunks"].as_u64().context("Missing chunk count")? {
                            names.insert(format!("microsoft-v2-{slot}-{index}"));
                        }
                    }
                    Err(keyring::Error::NoEntry) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        println!("PASS native Windows legacy migration, large Unicode session roundtrip, refresh rotation and reopen");
        Ok(())
    })();
    let cleanup = vault.clear();
    result?;
    cleanup?;
    if vault.load()?.is_some() {
        bail!("Native session was not cleared");
    }
    for name in names {
        match entry(&name)?.get_secret() {
            Err(keyring::Error::NoEntry) => {}
            Err(error) => return Err(error.into()),
            Ok(_) => bail!("Disposable credential entry remained after sign-out"),
        }
    }
    println!("PASS native Windows sign-out removed all disposable credential entries");
    Ok(())
}
fn main() -> Result<()> {
    #[cfg(windows)]
    verify()?;
    #[cfg(not(windows))]
    println!("Windows Credential Manager verification runs on Windows; deterministic storage tests run on every platform.");
    Ok(())
}
