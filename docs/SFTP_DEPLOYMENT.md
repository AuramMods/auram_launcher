# SFTP server deployment

Build a server in the Server tab. Each successful build writes `auram.json` at its root with the selected pack version, immutable commit, Minecraft version, and required Forge version. A separate local deployment payload records the distribution baseline, independent of writable client/server files and runtime files.

Enter the SFTP hostname, port, username, server root directory, and either a password or an OpenSSH private-key path. Choose **Save & inspect server**. On first connection, compare the actual SHA-256 SSH host-key fingerprint with your server provider, then trust it explicitly. Changed host keys require the same explicit review. Credentials are never sent to an unverified key. Passwords and key passphrases use macOS Keychain / Windows Credential Manager; ordinary settings hold connection details and the trusted fingerprint. **Forget connection** removes the stored connection and its credential. Development and production data use separate credential identities.

Inspection works before a local server is built and shows the remote version. Once a build is available, the launcher shows a file-change preview and **Full deploy** for an unversioned server, **Deploy update** for a different version, or **Sync files** for discrepancies in the same build. A matching installation shows **Up to date**. Missing or unrecognized version metadata selects full deployment. Connection and permission failures remain errors; they do not establish absence.

## Scope and changelists

Only these payload directories are deployed:

- `mods/`
- `scripts/`
- `config/`
- `kubejs/` (including its client/server/startup script subdirectories)

The launcher also maintains `auram.json`, `.auram-deploy.json` (managed-file ownership), and `.auram-deploy/` (transaction journals/backups). It does **not** deploy worlds, `server.properties`, EULA state, operator/whitelist/ban lists, Java, Forge libraries, startup scripts, `defaultconfigs`, schematics, or arbitrary other local folders. A new remote host therefore needs its own matching Java/Forge server runtime. Required Forge changes are reported, but runtime replacement is outside this operation. User-specific local edits are not silently imported: the payload comes from the selected built pack's recorded, verified distribution baseline.

The remote version resolves through the pack's immutable tag. The existing schema-1 changelist parser validates skipped releases, `previousVersion`, `previousCommit`, provenance and final path operations. Server inventories remain separate from client `.packignore` selections. The planner uses these changes to preserve untouched remote configuration and establish old distribution identities. It compares only final target content, never uploads intermediate releases, and falls back to a complete scoped comparison when the chain is missing, inconsistent, divergent, too long, or unavailable. Valid existing cache objects are reused.

Jars named `mods/example.jar` and `mods/example.jar.client` have one canonical identity. Existing disabled names stay disabled during updates. Jar reuse uses the recorded Git blob identity when available, plus the actual remote size. For previously untracked jars without a content identity, equal sizes are accepted. A changed recorded blob is uploaded even when the byte size is unchanged. Identical owned jar renames move the remote file without transferring its body and retain `.client` state. Ambiguous enabled/disabled duplicates fail with a specific path instead of selecting an arbitrary file.

Changed scripts/configuration are compared by Git blob hash over their actual remote bytes. Untouched configuration keeps server-local edits. Incoming replacements retain original backups. Obsolete modified configuration is preserved and reported; unknown files, including custom mods, are retained and reported. Legacy versioned servers can retire changelist-established distribution files only after their actual old blob hash matches. Unversioned servers do not infer ownership of unrelated files merely from their presence.

## Activation and recovery

Stop the remote server before deployment. SFTP cannot stop or restart Minecraft. No remote shell commands are executed.

The launcher reconnects and rebuilds its plan when deployment starts. A nonempty journal directory is atomically renamed into the deployment lock; an existing nonempty lock cannot be overwritten. All required payload bodies are fetched through the verified cache and uploaded to private staging paths before activation. Mods are queued first, with up to four uploads in parallel. SSH authenticates transport integrity; completed SFTP writes and remote sizes are checked. The live verification harness additionally reads uploaded bodies back and checks their Git blob hashes.

Activation retains originals before replacing/removing managed files. Each mutation has a persistent start marker, allowing recovery even when a request succeeded but its acknowledgement was lost. Ownership is written near the end and `auram.json` is installed last. Cancellation is deferred during activation/restoration. Successful or rolled-back journals are atomically moved out of the active lock into retained transaction receipts, avoiding an empty-lock cleanup window.

After a disconnected/interrupted deployment, reconnect, inspect, and choose **Recover interrupted deployment** only after the other launcher has stopped. Completed transactions are finalized; incomplete transactions restore the old installation. Backups and journals remain under `.auram-deploy/transactions/`. Recovery preserves files written while a deployment was interrupted rather than silently discarding them. A recovery failure leaves its journal and originals available for another attempt. Remote paths and ancestors are validated; symlinks, unsafe names and directory/file conflicts are rejected without following or recursively deleting them.

## Local verification

`cargo test --manifest-path src-tauri/Cargo.toml --lib` includes deterministic filesystem/HTTP fixtures for scoped deployment, aliases, same-size changed jars, renamed blobs, configuration preservation, skipped changelists, cancellation, lock exclusion, unsafe paths, symlinks, and failures before/after every remote mutation. Recovery is restartable and idempotent in those interruption cases.

A disposable loopback OpenSSH container exercises the real transport:

```sh
mkdir -p .qa/sftp-live/server
docker run --rm -d --name auram-sftp-qa -p 127.0.0.1::22 \
  -v "$PWD/.qa/sftp-live/server:/home/auram/upload" \
  atmoz/sftp:alpine auram:fixture-only:1000
docker port auram-sftp-qa
# Substitute the reported local port.
AURAM_SFTP_TEST_PORT=65252 cargo run --manifest-path src-tauri/Cargo.toml \
  --no-default-features --example verify_sftp
docker stop auram-sftp-qa
```

The fixture connects only to `127.0.0.1`, creates a unique test directory, and uses dummy credentials. Pass `-- --credential-store` to exercise native credential persistence/deletion. An optional `AURAM_SFTP_TEST_KEY` selects a fixture key configured in the container, with the dummy passphrase `fixture-passphrase`. Tests include real password/key authentication, unknown/changed host-key rejection, uploaded content hashes, version publication, alias handling, rename reuse, repeat synchronization, lock semantics, and preservation of server-owned files. They do not replace a test against a user's hosting provider.
