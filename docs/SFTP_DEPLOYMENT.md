# SFTP server deployment

The Server tab lists saved local and SFTP instances. A fresh installation starts empty; an existing singleton SFTP connection is adopted with its credential reference. Use **Add server → SFTP** to save a named destination. Passwords and key passphrases are kept separately per instance in macOS Keychain / Windows Credential Manager. Ordinary `state/servers.json` holds only connection details, trusted host-key fingerprints and cached version/error observations. Changing the host or port resets its pin; changing the destination clears the known version. Removing an instance deletes its saved credentials, while server files and backups stay in place. Development and production use separate credential identities.

Opening a server’s details automatically connects and reads only `auram.json` and the deployment lock. There is no recursive scan, jar hash pass, local build prerequisite, or separate inspection button to find. The list shows the last known version, or **Unknown version**. A connection/permission failure retains the cached version and reports the actual error; it never establishes absence. Invalid/unrecognized JSON is treated as an unknown version. Local instances read the corresponding local files.

On first SFTP connection, compare the observed SHA-256 SSH host-key fingerprint with your provider and choose **Trust key & connect**. Changed keys require the same review. Credentials are never sent to an unverified host. Connection settings can be edited in the details dialog; blank password/passphrase fields preserve the current credential for the same connection identity.

Each row and details dialog has a primary deploy action: **Full Deploy** for an unknown version, **Deploy Update N** for a different build, and disabled **Up to date** for a matching build. Clicking Deploy closes the dialog so the shared bottom progress bar reports preparation, transfer and activation. The launcher automatically prepares a versioned server payload from the currently installed client pack, including server rules and dependency filtering. A separate build/update of a staging folder is no longer a user step. Every successful deployment writes `auram.json` at the selected root; local and remote versions are independent. The actual plan is rebuilt at deployment time, so a stale cached list entry does not control what gets changed.

Local deployments provision matching Forge runtime files and startup scripts as well as the four payload directories. Runtime/default files reconcile separately with retained backups and a destination-specific recovery journal; user settings and worlds are preserved. The four payload directories use the same changelist transaction, disabled-jar handling and full replacement fallback as SFTP. Accept the Minecraft EULA in `eula.txt` before starting a new local server with `run.sh` or `run.bat`.

## Scope and changelists

Only these payload directories are deployed:

- `mods/`
- `scripts/`
- `config/`
- `kubejs/` (including its client/server/startup script subdirectories)

The launcher also maintains `auram.json`, `.auram-deploy.json` (managed-file ownership), and `.auram-deploy/` (transaction journals/backups). It does **not** deploy worlds, `server.properties`, EULA state, operator/whitelist/ban lists, Java, Forge libraries, startup scripts, `defaultconfigs`, schematics, or arbitrary other local folders. A new remote host therefore needs its own matching Java/Forge server runtime. Required Forge changes are reported, but runtime replacement is outside this operation. User-specific local edits are not silently imported: the payload comes from the selected built pack's recorded, verified distribution baseline.

The remote `auram.json` version is authoritative. A matching selected version is an immediate no-op, including legacy numeric version files without a commit field. Otherwise the version resolves through its immutable tag. The schema-1 changelist parser validates skipped releases, `previousVersion`, `previousCommit`, provenance and final path operations. The old pack's server inclusion rules translate distribution paths into their server destinations. Saved ownership metadata is used when it matches the remote version. Only final changed/added/deleted server paths are planned; intermediate release bodies are never uploaded.

**Inspection never recursively walks the server or hashes remote payload files.** Unchanged configuration and unrelated custom files are untouched during a changelist update. Incoming replacements and deletions retain original backups, including locally edited files at paths changed/deleted by the release. A version file is not a full integrity/repair check: manual damage to an otherwise current server does not trigger a scan.

If there is no recognized remote version or the actual changelist lookup cannot establish a valid route, deployment uses the full replacement fallback described in the details dialog. It stages fresh `mods`, `scripts`, `config` and `kubejs` directories from the selected built payload, then swaps all four directories. Extra/custom files inside these four directories leave the live server and remain in the retained backups. Empty target directories replace nonempty old directories too. Everything outside those four directories (apart from launcher version/transaction metadata) stays untouched. There is no expensive per-file comparison fallback.

Jars named `mods/example.jar` and `mods/example.jar.client` have one canonical identity. Small, targeted mod-directory listings preserve existing disabled names without reading jar bodies. Changed recorded blobs are uploaded even at the same byte size. Identical jar renames established by the validated version baseline can move existing remote bytes after a size check, retaining `.client` state. Ambiguous enabled/disabled duplicates on affected paths fail with a specific path instead of selecting an arbitrary file.

## Activation and recovery

Stop the remote server before deployment. SFTP cannot stop or restart Minecraft. No remote shell commands are executed.

The launcher reconnects and rebuilds its plan when deployment starts. A nonempty journal directory is atomically renamed into the deployment lock; an existing nonempty lock cannot be overwritten. All required payload bodies are fetched through the verified cache and uploaded to private staging paths before activation. Mods are queued first, with up to eight uploads on eight separate SSH/SFTP connections. If a provider actually rejects extra connections, successfully established connections remain usable. SSH host-key changes remain errors. Connection setup, metadata requests and upload I/O have cancellation and timeout handling; closing a session is bounded too. SSH authenticates transport integrity; completed SFTP writes and remote sizes are checked. The live verification harness additionally reads uploaded bodies back and checks their Git blob hashes.

Activation retains originals before replacing/removing changed files or swapping the four fully staged directories. Each mutation has a persistent start marker, allowing recovery even when a request succeeded but its acknowledgement was lost. Ownership is written near the end and `auram.json` is installed last. Cancellation is deferred during activation/restoration. Successful or rolled-back journals are atomically moved out of the active lock into retained transaction receipts, avoiding an empty-lock cleanup window.

After a disconnected/interrupted deployment, reconnect, inspect, and choose **Recover deployment** only after the other launcher has stopped. Completed transactions are finalized; incomplete transactions restore the old installation. Backups and journals remain under `.auram-deploy/transactions/`. Recovery preserves files written while a deployment was interrupted rather than silently discarding them. A recovery failure leaves its journal and originals available for another attempt. Remote paths and ancestors are validated; symlinks, unsafe names and directory/file conflicts at action destinations are rejected. Full directory swaps retain nested contents and symlinks without following them; root/ancestor symlinks are refused.

## Local verification

`cargo test --manifest-path src-tauri/Cargo.toml --lib` includes deterministic filesystem/HTTP fixtures for scoped deployment, aliases, same-size changed jars, renamed blobs, untouched configuration preservation, zero-body metadata-only inspection, eight concurrent uploads, skipped changelists, cancellation/timeouts, lock exclusion, unsafe paths, symlinks, and failures before/after every remote mutation. Recovery is restartable and idempotent in those interruption cases.

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

The fixture connects only to `127.0.0.1`, creates a unique test directory, and uses dummy credentials. Pass `-- --credential-store` to exercise native credential persistence/deletion. An optional `AURAM_SFTP_TEST_KEY` selects a fixture key configured in the container, with the dummy passphrase `fixture-passphrase`. Tests include real password/key authentication, unknown/changed host-key rejection, eight real simultaneous connections, uploaded content hashes, version publication, alias handling, whole-directory replacement, repeat synchronization, lock semantics, and preservation of server-owned files. They do not replace a test against a user's hosting provider.
