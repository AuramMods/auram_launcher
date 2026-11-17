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

The launcher reconnects and rebuilds its plan when deployment starts. A nonempty journal directory is atomically renamed into the deployment lock; an existing nonempty lock cannot be overwritten. All required payload bodies are fetched through the verified cache and uploaded to private staging paths before activation. Mods are queued first. Every deployment initially attempts twenty independent SSH/SFTP connections, with brief retries for rejected/closed setup connections. If the provider rejects extra connections, successfully established connections remain usable. If an actual connection-limit response or transport closure occurs during upload, the failing slot is retired and its SSH transport is closed. Its private staged file is retried with backoff on a surviving slot; healthy uploads have no artificial delay. A new deployment tries twenty again. Permission, authentication, host-key, disk and integrity failures do not become concurrency retries. Losing every transport fails with retained recovery state. SSH host-key changes remain errors. Connection setup, metadata requests and upload I/O have cancellation and timeout handling; closing a session is bounded too. SSH authenticates transport integrity; completed SFTP writes and remote sizes are checked. The live verification harness additionally reads uploaded bodies back and checks their Git blob hashes.

Destination checks and staging-directory creation run in bounded parallel batches and report completed/total counts before uploads. The transfer file/byte bar starts only once directories are ready. Destination revalidation, replacing/removing obsolete files, and interrupted-deployment recovery also report counts; completed/skipped step segments are hidden. Full deployment moves the old directories into retained backups instead of recursively deleting thousands of files.

TCP_NODELAY is enabled. Ancestor safety checks use one server-side canonical-path resolution per parent, with leaf symlink/type checks, instead of issuing an LSTAT for every ancestor on every file. Nonexistent parents retain the checked-prefix fallback; permission errors and symlink/alias paths remain errors. Paths are checked afresh rather than cached. The final upload size/type check reuses the validated name. Cached local pack blobs are checksum-verified once per lookup instead of twice. File close acknowledgements, remote size/type checks, and fsync behavior are retained.

Activation retains originals before replacing/removing changed files or swapping the four fully staged directories. Each mutation has a persistent start marker, allowing recovery even when a request succeeded but its acknowledgement was lost. Ownership is written near the end and `auram.json` is installed last. Cancellation is deferred during activation/restoration. Successful or rolled-back journals are atomically moved out of the active lock into retained transaction receipts, avoiding an empty-lock cleanup window.

After a disconnected/interrupted deployment, reconnect, inspect, and choose **Recover deployment** only after the other launcher has stopped. Completed transactions are finalized; incomplete transactions restore the old installation. Backups and journals remain under `.auram-deploy/transactions/`. Recovery preserves files written while a deployment was interrupted rather than silently discarding them. A recovery failure leaves its journal and originals available for another attempt. Remote paths and ancestors are validated; symlinks, unsafe names and directory/file conflicts at action destinations are rejected. Full directory swaps retain nested contents and symlinks without following them; root/ancestor symlinks are refused.

## Local verification

`cargo test --manifest-path src-tauri/Cargo.toml --lib` includes deterministic filesystem/HTTP fixtures for scoped deployment, aliases, same-size changed jars, renamed blobs, untouched configuration preservation, zero-body metadata-only inspection, twenty concurrent uploads, skipped changelists, cancellation/timeouts, lock exclusion, unsafe paths, symlinks, and failures before/after every remote mutation. Recovery is restartable and idempotent in those interruption cases.

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

The fixture connects only to `127.0.0.1`, creates a unique test directory, and uses dummy credentials. Pass `-- --credential-store` to exercise native credential persistence/deletion. An optional `AURAM_SFTP_TEST_KEY` selects a fixture key configured in the container, with the dummy passphrase `fixture-passphrase`. Tests include real password/key authentication, unknown/changed host-key rejection, twenty real simultaneous connections, uploaded content hashes, version publication, alias handling, whole-directory replacement, repeat synchronization, lock semantics, and preservation of server-owned files. They do not replace a test against a user's hosting provider.

## Small-file performance fixture

`bench_sftp_small_files` creates a unique directory on the disposable **loopback** fixture and checks every uploaded body against its Git blob hash. It accepts the same `AURAM_SFTP_TEST_PORT` as the live harness; its optional argument is the number of tiny files (default 128).

For reproducible network latency, run the localhost-only proxy against the Docker port above:

```sh
python3 scripts/fixtures/sftp_latency_proxy.py --upstream-port 65252 --delay-ms 10 \
  --limit-file "$PWD/.qa/sftp-limit"
# In another shell:
AURAM_SFTP_TEST_PORT=1427 cargo run --manifest-path src-tauri/Cargo.toml \
  --locked --no-default-features --example bench_sftp_small_files -- 128
```

To simulate a limit imposed during transfer, start without a limit file and add `AURAM_SFTP_BENCH_LIMIT_FILE="$PWD/.qa/sftp-limit"` to that benchmark command. After establishing twenty connections, the harness writes a limit of three; the proxy drops excess existing transports. Verify the JSON result reports twenty initial connections, three remaining connections and matching uploaded bodies. For an initial connection cap, write `3` to the limit file before starting the benchmark without `AURAM_SFTP_BENCH_LIMIT_FILE`. This fixture does not connect to production servers.

On the development Mac, 128 tiny files across sixteen nested configuration folders with 10 ms added per proxy direction took 13.82 seconds before this change (8 connections) and 4.09 seconds afterward (20 connections), with all uploaded bytes checked. First upload began after 3.24 seconds versus 1.38 seconds. The same fixture completed with verified bodies after a live 20-to-3 connection reduction. These are controlled fixture measurements, not a throughput guarantee for a hosting provider.
