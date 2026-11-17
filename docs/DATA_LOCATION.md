# Data location

Use **Settings → Installation & data location → Choose directory** to select a parent folder. Production creates `AuramLauncher` inside that folder. The client instance, worlds, server workspace, downloads, caches, nonsecret settings, backups and logs move together. **Reset to default** moves them back to the normal platform application-data directory. The confirmation shows both exact paths and restarts the launcher when finished. Close Minecraft and any manually started servers using the files before moving them. Launcher jobs exclude conflicting updates, deployments, sign-ins and launches during the move.

The installed application is independent: the normal Windows NSIS installer has an installation-directory page, including writable folders on other drives; macOS apps can be moved with Finder. The updater retains the app's installation location. Settings displays the actual application path. Changing data location does not relocate a running executable.

## Copy and recovery

Relocation copies into a private staging directory on the destination volume, preserving file permissions and links without walking their targets. Source and destination contents are checked with SHA-256. Only the destination-local staging directory is renamed into place; moving between Windows drive letters does not depend on a cross-volume rename. An actual cross-device download-promotion error also falls back to a destination-local atomic copy and checksum check.

A small schema-versioned bootstrap pointer stays beside the original default folder (`.AuramLauncher-location` in production). Its journal identifies the exact source, destination and staging directory; the verified snapshot travels with the prepared copy. Source files stay available until the verified destination has been published and the pointer committed. After that, cleanup removes only originals that still match their recorded content and verified destination. Newly written or changed originals are retained, with a persistent notice identifying the old folder. Existing nonempty, overlapping or unrelated destinations are never overwritten.

An interrupted operation appears in Settings with **Retry move** and **Cancel unfinished move**. Cancellation retains prepared copies beside their destination rather than deleting files changed after an interruption. A crash after destination publication can adopt only that exact verified copy. If activation succeeded but cleanup failed, the new location stays usable and **Finish move** retries cleanup. Cancellation is deferred during activation and cleanup.

A missing custom drive produces a recoverable location error instead of silently creating a new empty installation. Once an ordinary move has finished, **Reset to default** can explicitly start at the default folder without moving or deleting the unavailable drive's data. Unfinished relocation journals require reconnecting their involved drives before recovery.

Microsoft credentials stay in the native vault. Saved SFTP credential namespaces retain their original identity across moves; private keys inside the data folder are rebased. Saved external server directories keep their paths. Launcher-generated local server startup scripts are updated to the relocated Java path, preserving other script contents and executable permissions; external script originals are backed up under `backups/data-location/`. Server worlds, properties, access lists and EULA state are preserved.

Development uses `AuramLauncherDev` and a separate bootstrap pointer. QA and explicit development data overrides remain isolated. `pnpm dev` supervises an explicit native restart request so it restarts the CLI and Vite together after a location change. Normal development exits/errors do not cause an automatic restart loop. Production requires no development marker or signing credentials for this operation.

## Verification

`cargo test --manifest-path src-tauri/Cargo.toml --lib data_location` exercises move/reset, worlds/cache/partial preservation, credential identity and key paths, interrupted copying and activation, damaged prepared copies, safe cancellation, late user writes, unavailable drives, cleanup recovery, symlink boundaries, and generated server scripts/permissions.

`cargo run --manifest-path src-tauri/Cargo.toml --locked --no-default-features --example verify_data_location -- /path/on/another/volume` uses disposable directories only. On Unix it requires distinct actual filesystem device IDs. It verifies moving data and resetting it, actual cross-volume download promotion, integrity and zero-body reuse. Windows uses the same copy/verification implementation; native Windows drive, ACL and link-permission testing still needs a Windows environment. Windows link creation errors retain the original installation.
