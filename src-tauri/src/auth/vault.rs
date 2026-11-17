//! Native credential persistence. Windows limits a generic credential to 2,560
//! bytes, so commit a small pointer only after writing an inactive secure slot.
use super::{Credentials, Vault};
use anyhow::Result;

// Account status polling also reads the vault outside Auth's refresh gate.
// Hold the native snapshot stable until reads, rotation and cleanup finish.
#[cfg(windows)]
static WINDOWS_VAULT_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub struct NativeVault {
    service: String,
}
impl Default for NativeVault {
    fn default() -> Self {
        Self::with_service(if cfg!(feature = "qa") {
            "art.arcane.auram.qa"
        } else if cfg!(debug_assertions) {
            "art.arcane.auram.dev"
        } else {
            "art.arcane.auram.launcher"
        })
    }
}
impl NativeVault {
    /// Separate namespaces let native verification avoid real account entries.
    pub fn with_service(service: &str) -> Self {
        Self {
            service: service.into(),
        }
    }
    fn entry(&self, name: &str) -> Result<keyring::Entry> {
        Ok(keyring::Entry::new(&self.service, name)?)
    }
}
impl Vault for NativeVault {
    fn load(&self) -> Result<Option<Credentials>> {
        #[cfg(windows)]
        {
            let _guard = WINDOWS_VAULT_GATE.lock().unwrap_or_else(|e| e.into_inner());
            chunked::load(self)
        }
        #[cfg(not(windows))]
        {
            match self.entry("microsoft")?.get_password() {
                Ok(value) => Ok(Some(serde_json::from_str(&value)?)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(error.into()),
            }
        }
    }
    fn save(&self, value: &Credentials) -> Result<()> {
        #[cfg(windows)]
        {
            let _guard = WINDOWS_VAULT_GATE.lock().unwrap_or_else(|e| e.into_inner());
            chunked::save(self, value)
        }
        #[cfg(not(windows))]
        {
            self.entry("microsoft")?
                .set_password(&serde_json::to_string(value)?)?;
            Ok(())
        }
    }
    fn clear(&self) -> Result<()> {
        #[cfg(windows)]
        {
            let _guard = WINDOWS_VAULT_GATE.lock().unwrap_or_else(|e| e.into_inner());
            chunked::clear(self)
        }
        #[cfg(not(windows))]
        {
            match self.entry("microsoft")?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(error.into()),
            }
        }
    }
}

#[cfg(any(windows, test))]
mod chunked {
    use super::*;
    use anyhow::{bail, Context};
    use serde::{Deserialize, Serialize};
    use sha2::Digest;

    pub(super) const ROOT: &str = "microsoft";
    const CHUNK_BYTES: usize = 2048;
    const MAX_BYTES: usize = 1024 * 1024;
    pub(super) trait Store {
        fn password(&self, name: &str) -> Result<Option<String>>;
        fn put_password(&self, name: &str, value: &str) -> Result<()>;
        fn secret(&self, name: &str) -> Result<Option<Vec<u8>>>;
        fn put_secret(&self, name: &str, value: &[u8]) -> Result<()>;
        fn delete(&self, name: &str) -> Result<()>;
    }
    #[derive(Serialize, Deserialize)]
    struct Pointer {
        auram_vault: u32,
        slot: u8,
        revision: String,
    }
    #[derive(Serialize, Deserialize)]
    struct Slot {
        revision: String,
        chunks: usize,
        bytes: usize,
        sha256: String,
    }
    fn descriptor(slot: u8) -> String {
        format!("microsoft-v2-slot-{slot}")
    }
    fn part(slot: u8, index: usize) -> String {
        format!("microsoft-v2-{slot}-{index}")
    }
    fn pointer(value: &str) -> Result<Option<Pointer>> {
        let json: serde_json::Value =
            serde_json::from_str(value).context("Read Microsoft credential record")?;
        if json.get("auram_vault").is_none() {
            return Ok(None);
        }
        let pointer: Pointer =
            serde_json::from_value(json).context("Read Microsoft credential pointer")?;
        if pointer.auram_vault != 2
            || pointer.slot > 1
            || pointer.revision.len() != 32
            || !pointer.revision.bytes().all(|c| c.is_ascii_hexdigit())
        {
            bail!("Unsupported Microsoft credential record; sign out and sign in again");
        }
        Ok(Some(pointer))
    }
    fn slot(store: &impl Store, index: u8) -> Result<Option<Slot>> {
        let Some(value) = store.password(&descriptor(index))? else {
            return Ok(None);
        };
        let value: Slot = serde_json::from_str(&value).context("Read Microsoft credential slot")?;
        if value.bytes == 0
            || value.bytes > MAX_BYTES
            || value.chunks != value.bytes.div_ceil(CHUNK_BYTES)
            || value.sha256.len() != 64
            || !value.sha256.bytes().all(|c| c.is_ascii_hexdigit())
        {
            bail!("Invalid Microsoft credential slot; sign out and sign in again");
        }
        Ok(Some(value))
    }
    fn cleanup(store: &impl Store, index: u8) -> Result<()> {
        let name = descriptor(index);
        let Some(value) = store.password(&name)? else {
            return Ok(());
        };
        // A damaged staging descriptor must not make sign-out/retry unusable.
        // Only our bounded, fixed names in the inactive/signed-out slot are erased.
        let chunks = serde_json::from_str::<Slot>(&value)
            .ok()
            .filter(|slot| {
                slot.bytes > 0
                    && slot.bytes <= MAX_BYTES
                    && slot.chunks == slot.bytes.div_ceil(CHUNK_BYTES)
            })
            .map_or(MAX_BYTES / CHUNK_BYTES, |slot| slot.chunks);
        for chunk in 0..chunks {
            store.delete(&part(index, chunk))?;
        }
        // Keep the descriptor if cleanup fails so sign-out/retry can finish.
        store.delete(&name)?;
        Ok(())
    }
    pub(super) fn load(store: &impl Store) -> Result<Option<Credentials>> {
        let Some(value) = store.password(ROOT)? else {
            return Ok(None);
        };
        let Some(pointer) = pointer(&value)? else {
            // Existing single-entry sessions stay usable and migrate on save.
            return Ok(Some(
                serde_json::from_str(&value).context("Read saved Microsoft session")?,
            ));
        };
        let slot = slot(store, pointer.slot)?
            .context("Microsoft credential slot is missing; sign in again")?;
        if slot.revision != pointer.revision {
            bail!("Microsoft credential revision mismatch; sign in again");
        }
        let mut bytes = Vec::with_capacity(slot.bytes);
        for index in 0..slot.chunks {
            let chunk = store
                .secret(&part(pointer.slot, index))?
                .context("Microsoft credential fragment is missing; sign in again")?;
            let expected = (slot.bytes - index * CHUNK_BYTES).min(CHUNK_BYTES);
            if chunk.len() != expected {
                bail!("Microsoft credential fragment is damaged; sign in again");
            }
            bytes.extend_from_slice(&chunk);
        }
        if hex::encode(sha2::Sha256::digest(&bytes)) != slot.sha256 {
            bail!("Microsoft credential integrity check failed; sign in again");
        }
        Ok(Some(
            serde_json::from_slice(&bytes).context("Read saved Microsoft session")?,
        ))
    }
    pub(super) fn save(store: &impl Store, credentials: &Credentials) -> Result<()> {
        let previous = store
            .password(ROOT)?
            .as_deref()
            .map(pointer)
            .transpose()?
            .flatten();
        let index = previous.as_ref().map_or(0, |p| 1 - p.slot);
        let bytes = serde_json::to_vec(credentials)?;
        if bytes.len() > MAX_BYTES {
            bail!("Microsoft credential response exceeds the supported session size");
        }
        cleanup(store, index)?;
        let slot = Slot {
            revision: format!("{:032x}", rand::random::<u128>()),
            chunks: bytes.len().div_ceil(CHUNK_BYTES),
            bytes: bytes.len(),
            sha256: hex::encode(sha2::Sha256::digest(&bytes)),
        };
        // This nonsecret descriptor also allows cleanup of interrupted staging.
        store.put_password(&descriptor(index), &serde_json::to_string(&slot)?)?;
        for (chunk, bytes) in bytes.chunks(CHUNK_BYTES).enumerate() {
            store.put_secret(&part(index, chunk), bytes)?;
        }
        let pointer = Pointer {
            auram_vault: 2,
            slot: index,
            revision: slot.revision,
        };
        let committed = serde_json::to_string(&pointer)?;
        if let Err(error) = store.put_password(ROOT, &committed) {
            // A failed acknowledgement can follow a successful native write.
            // Retain both slots unless reading back proves the commit succeeded.
            if store.password(ROOT).ok().flatten().as_deref() != Some(&committed) {
                return Err(error);
            }
        }
        if let Some(previous) = previous {
            let _ = cleanup(store, previous.slot);
        }
        Ok(())
    }
    pub(super) fn clear(store: &impl Store) -> Result<()> {
        store.delete(ROOT)?;
        let first = cleanup(store, 0);
        let second = cleanup(store, 1);
        first.and(second)
    }
}

#[cfg(any(windows, test))]
impl chunked::Store for NativeVault {
    fn password(&self, name: &str) -> Result<Option<String>> {
        match self.entry(name)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn put_password(&self, name: &str, value: &str) -> Result<()> {
        Ok(self.entry(name)?.set_password(value)?)
    }
    fn secret(&self, name: &str) -> Result<Option<Vec<u8>>> {
        match self.entry(name)?.get_secret() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn put_secret(&self, name: &str, value: &[u8]) -> Result<()> {
        Ok(self.entry(name)?.set_secret(value)?)
    }
    fn delete(&self, name: &str) -> Result<()> {
        match self.entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{chunked::*, *};
    use std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Mutex,
        },
    };
    #[derive(Default)]
    struct MemoryStore {
        entries: Mutex<BTreeMap<String, Vec<u8>>>,
        writes: AtomicUsize,
        fault: Mutex<Option<(usize, bool)>>,
    }
    impl MemoryStore {
        fn mutate(&self, action: impl FnOnce()) -> Result<()> {
            let index = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
            let fault = *self.fault.lock().unwrap();
            if fault == Some((index, false)) {
                anyhow::bail!("Injected native write failure");
            }
            action();
            if fault == Some((index, true)) {
                anyhow::bail!("Lost native write acknowledgement");
            }
            Ok(())
        }
        fn arm(&self, index: usize, after: bool) {
            self.writes.store(0, Ordering::SeqCst);
            *self.fault.lock().unwrap() = Some((index, after));
        }
    }
    impl Store for MemoryStore {
        fn password(&self, name: &str) -> Result<Option<String>> {
            self.entries
                .lock()
                .unwrap()
                .get(name)
                .map(|bytes| {
                    Ok(String::from_utf16(
                        &bytes
                            .chunks_exact(2)
                            .map(|b| u16::from_le_bytes([b[0], b[1]]))
                            .collect::<Vec<_>>(),
                    )?)
                })
                .transpose()
        }
        fn put_password(&self, name: &str, value: &str) -> Result<()> {
            let bytes = value
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>();
            if bytes.len() > 2560 {
                anyhow::bail!("password encoded as UTF-16 exceeds 2560 bytes");
            }
            self.put_secret(name, &bytes)
        }
        fn secret(&self, name: &str) -> Result<Option<Vec<u8>>> {
            Ok(self.entries.lock().unwrap().get(name).cloned())
        }
        fn put_secret(&self, name: &str, value: &[u8]) -> Result<()> {
            if value.len() > 2560 {
                anyhow::bail!("binary secret exceeds 2560 bytes");
            }
            self.mutate(|| {
                self.entries
                    .lock()
                    .unwrap()
                    .insert(name.into(), value.into());
            })
        }
        fn delete(&self, name: &str) -> Result<()> {
            self.mutate(|| {
                self.entries.lock().unwrap().remove(name);
            })
        }
    }
    fn credentials(refresh: &str, large: bool) -> Credentials {
        Credentials {
            client_id: "test-client".into(),
            refresh_token: refresh.repeat(if large { 4096 } else { 1 }),
            minecraft_token: if large {
                "jwt".repeat(4096)
            } else {
                "token".into()
            },
            expires_at: 12345,
            profile: Some(super::super::Profile {
                name: "Player 🦄 名".into(),
                uuid: "a".repeat(32),
                xuid: "12345".into(),
            }),
        }
    }
    fn equal(actual: &Credentials, expected: &Credentials) {
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }
    #[test]
    fn large_unicode_sessions_roundtrip_rotate_and_clear_within_real_windows_limits() {
        let store = MemoryStore::default();
        let original = credentials("refresh", true);
        assert!(
            store
                .put_password(ROOT, &serde_json::to_string(&original).unwrap())
                .is_err(),
            "Reproduce the original single-password failure"
        );
        save(&store, &original).unwrap();
        equal(&load(&store).unwrap().unwrap(), &original);
        let rotated = credentials("rotated🦄", true);
        save(&store, &rotated).unwrap();
        equal(&load(&store).unwrap().unwrap(), &rotated);
        assert!(store
            .entries
            .lock()
            .unwrap()
            .values()
            .all(|value| value.len() <= 2560));
        clear(&store).unwrap();
        assert!(load(&store).unwrap().is_none());
        assert!(store.entries.lock().unwrap().is_empty());
    }
    #[test]
    fn legacy_utf16_sessions_migrate_without_losing_the_refresh_token() {
        let store = MemoryStore::default();
        let original = credentials("legacy", false);
        store
            .put_password(ROOT, &serde_json::to_string(&original).unwrap())
            .unwrap();
        equal(&load(&store).unwrap().unwrap(), &original);
        save(&store, &original).unwrap();
        equal(&load(&store).unwrap().unwrap(), &original);
        assert!(store
            .password(ROOT)
            .unwrap()
            .unwrap()
            .contains("auram_vault"));
        clear(&store).unwrap();
        assert!(store.entries.lock().unwrap().is_empty());
    }
    #[test]
    fn interrupted_rotation_and_migration_keep_one_complete_session_at_every_write_boundary() {
        for legacy in [false, true] {
            let original = credentials("original", !legacy);
            let rotated = credentials("rotated", true);
            let initial = || {
                let store = MemoryStore::default();
                if legacy {
                    store
                        .put_password(ROOT, &serde_json::to_string(&original).unwrap())
                        .unwrap();
                } else {
                    save(&store, &original).unwrap();
                }
                store.writes.store(0, Ordering::SeqCst);
                store
            };
            let reference = initial();
            save(&reference, &rotated).unwrap();
            let count = reference.writes.load(Ordering::SeqCst);
            for after in [false, true] {
                for index in 1..=count {
                    let store = initial();
                    store.arm(index, after);
                    let _ = save(&store, &rotated);
                    *store.fault.lock().unwrap() = None;
                    let actual = load(&store).unwrap().unwrap();
                    let value = serde_json::to_value(&actual).unwrap();
                    assert!(
                        value == serde_json::to_value(&original).unwrap()
                            || value == serde_json::to_value(&rotated).unwrap(),
                        "partial session at write {index}, after {after}"
                    );
                    clear(&store).unwrap();
                    assert!(store.entries.lock().unwrap().is_empty());
                }
            }
        }
    }
    #[test]
    fn corrupt_or_missing_chunks_are_reported_without_deleting_credentials() {
        let store = MemoryStore::default();
        save(&store, &credentials("refresh", true)).unwrap();
        let name = "microsoft-v2-0-0";
        let original = store.secret(name).unwrap().unwrap();
        store.put_secret(name, &vec![b'X'; original.len()]).unwrap();
        assert!(load(&store)
            .err()
            .unwrap()
            .to_string()
            .contains("integrity"));
        assert!(store.password(ROOT).unwrap().is_some());
        store.delete(name).unwrap();
        assert!(load(&store).err().unwrap().to_string().contains("missing"));
        clear(&store).unwrap();
        assert!(store.entries.lock().unwrap().is_empty());
    }
    #[test]
    fn first_sign_in_can_be_interrupted_without_exposing_partial_credentials() {
        let credentials = credentials("first", true);
        let reference = MemoryStore::default();
        save(&reference, &credentials).unwrap();
        let count = reference.writes.load(Ordering::SeqCst);
        for after in [false, true] {
            for index in 1..=count {
                let store = MemoryStore::default();
                store.arm(index, after);
                let _ = save(&store, &credentials);
                *store.fault.lock().unwrap() = None;
                if let Some(actual) = load(&store).unwrap() {
                    equal(&actual, &credentials);
                }
                clear(&store).unwrap();
                assert!(store.entries.lock().unwrap().is_empty());
            }
        }
    }
    #[test]
    fn damaged_staging_metadata_can_be_cleared_without_leaving_credential_fragments() {
        let store = MemoryStore::default();
        save(&store, &credentials("first", true)).unwrap();
        store
            .put_password("microsoft-v2-slot-0", "broken metadata")
            .unwrap();
        assert!(load(&store).is_err());
        clear(&store).unwrap();
        assert!(store.entries.lock().unwrap().is_empty());
    }
}
