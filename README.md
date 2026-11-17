# Auram Launcher

Native Tauri 2 launcher for the Auram Minecraft 1.20.1 Forge pack. Supports Apple Silicon macOS and Windows x64. React, TypeScript, shadcn/ui, Rust, and pnpm; no Flutter/Dart installation is needed.

The desktop icon keeps the original purple Auram mark on an OLED-black background. `pnpm icons` regenerates the native desktop icons from `assets/app-icon.svg`.

## Development

Install Node 24, pnpm 11.15.1, Rust 1.93 or newer, and the native Tauri prerequisites: Xcode Command Line Tools on macOS; Microsoft C++ build tools and WebView2 on Windows.

```sh
pnpm install --frozen-lockfile
pnpm dev
```

`pnpm dev` opens the native application with Vite hot reload. `pnpm frontend:dev` is the internal frontend server. Development stores data in `AuramLauncherDev` under the platform's local application-data directory, uses a separate credential namespace, and never installs production launcher updates. Set `AURAM_DATA_DIR` to override development storage.

```sh
pnpm check
pnpm test
cargo fmt --manifest-path src-tauri/Cargo.toml -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml --lib
pnpm build --no-bundle
```

Bundled production builds require `TAURI_SIGNING_PRIVATE_KEY` and its optional `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. The public key is embedded in the Tauri configuration. Private keys must remain outside Git. Xcode 27 requires release build-tool stripping to stay disabled; the Cargo release profile accounts for this.

## Playing and managing files

Install Auram, choose Sign in with Microsoft, then click **Copy Code**. That click copies the displayed code and opens Microsoft in your browser in one action. Complete the first sign-in, then launch. No browser or clipboard action occurs before the click. Refresh credentials are stored in macOS Keychain or Windows Credential Manager. Account selection and sign-out are available in the launcher. Microsoft can require reauthentication after revocation or policy changes.

On startup, Auram checks launcher updates, then automatically checks and updates an already installed client pack. The Play button stays disabled until the real pack update finishes. A failed request caused by unavailable internet/update services allows the installed version to launch; Microsoft authentication requirements still apply. Cancellation, invalid metadata, integrity failures and local installation errors keep launch blocked and show **Retry pack update** on the Play button. No separate installed-version bypass is offered. A fresh installation still starts with **Install Auram**. Startup preparation is shared across duplicate UI requests and runs once per launcher process; explicit retries start a new attempt.

Pack versions come from the release advertised by `AuramMods/Auram`’s `auram.json`, pinned to its annotated or lightweight Git tag. The launcher composes changelists across skipped builds and fetches each selected final blob once. Sparse HTTP inventories omit unrelated repository content, bundled runtimes, and user data; unchanged jar renames reuse verified cached or installed bytes. Downloads use immutable commit files verified with Git blob checksums. Only changed managed files are applied. See [pack update behavior](docs/PACK_UPDATES.md) for fallback and ownership rules. Shared Java, Mojang assets, libraries, and Forge runtime files survive pack updates; downloads resume when the server supports ranges. Repair verifies files rather than trusting an installation marker. Unmanaged files are retained, and changed managed files are backed up before replacement. Transactions recover interrupted installations on the next start.

Operation progress shows the active and upcoming segmented steps and one aggregate batch bar with ready/total file counts. Completed/skipped segments disappear while the overall step count and original step numbers remain. Counted directory preparation, destination validation, file replacement/removal and recovery show their own progress before or after transfers. Download batches use a fixed byte total when all sizes are known, otherwise a fixed file count. The displayed percentage measures verified, ready content. Reused cache files count as ready without adding network traffic. Metadata, extraction, and installer steps remain indeterminate instead of claiming an invented percentage. Individual task cards and task-list controls are not displayed. Cancellation remains available while work is running.

Local managed-file inspection, Java/Forge snapshot checks, mod metadata reads, runtime-cache preparation and private staging copies use a dedicated worker pool sized to the available CPU cores (up to 32). These blocking operations run off the async executor. File counts advance on completed work; the journaled activation/rollback stays ordered. For profiling, `AURAM_FILE_WORKERS=1` forces a serial run; normal use detects the worker count automatically. Checksum dependencies are optimized in development too, so `pnpm dev` does not spend excessive time in debug hash routines.

Notifications float in the top-right corner on a blurred dark surface. Each has its own three-second lifetime, slides in, and fades out; click anywhere on it to dismiss it early. Concurrent notifications stack without moving the page. Startup failures remain retryable from the Play button after their notification disappears.

**Force reinstall** in **Your installation** permanently deletes the entire client instance, including local worlds and custom files, before installing a fresh pack. Back up anything you want to keep before confirming. It retains your sign-in, settings, server instances, and shared download caches. A failed or cancelled reinstall does not restore deleted client files; use **Install Auram** to retry. Regular updates and repair continue preserving user data.

Production data lives at `~/Library/Application Support/AuramLauncher` on macOS and `%LOCALAPPDATA%\AuramLauncher` on Windows. Use the UI to open the actual data, client, server, or log folder. Existing Flutter data can be imported from the Play tab without deleting the original installation. Imported Mojang assets and libraries are checked before reuse; the managed Java distribution is checksum-pinned.

Minecraft stays running when its launcher window closes. The launcher remains in the background while collecting redacted logs; reopening the application shows the existing window. Java failures and early game exits are reported with a log location.

Game memory is automatic, using the original Flutter launcher's actual rule: systems with **up to 35 GiB** of installed RAM use a **9 GiB maximum / 9 GiB initial** heap; systems **above 35 GiB** use a **20 GiB maximum / 12 GiB initial** heap. (The old constant names said 32 GB, but its condition was `memGiB > 35`.) The launcher detects physical RAM once per run and selects the corresponding original G1 flags. The settings card shows the detected RAM and selected preset, with no slider or memory save control. Previously saved manual allocations are ignored and are omitted from future settings writes. If detection returns no usable value, the low preset is used. Both profiles retain pre-touch, reference processing, explicit-GC and Forge timeout settings.

macOS uses native ARM64 Azul Zulu Java 17.0.21, the same vendor/runtime family as the old launcher's 17.0.18, and retains `-XstartOnFirstThread`. Minecraft 1.20.1's JNA 5.12.1 can abort inside `dispatch.c` when macOS produces a long library-loading error ([upstream fix](https://github.com/java-native-access/jna/pull/1456)). On macOS the launcher uses the matched JNA/JNA-platform 5.13.0 libraries and checksums from [Mojang's 1.20.2 metadata](https://piston-meta.mojang.com/v1/packages/2b1c2be7cf1d1137b77716962b71e147b614c50c/1.20.2.json), including Forge's merge-module references. Only those two libraries change; Minecraft remains 1.20.1. Upstream metadata and existing runtime cache files are retained.

## Servers

The Server tab lists your saved instances. A fresh installation has none. Choose **Add server**, then select an existing/new local directory with the native folder picker or enter an SFTP connection. Each instance remembers its own name, destination, last known version and credentials. Existing single SFTP connections are adopted; no default local server is added. Removing a server from the list leaves its files in place.

Click a row to open its details and automatically read `auram.json` from the local folder or SFTP host. The row and dialog both offer **Full Deploy** for an unknown version, **Deploy Update N** for an older/different build, or **Up to date** for a matching version. The list displays the last successfully observed version; connection errors retain that cache and do not pretend the version file is missing. Edit the server settings in the details dialog and save to inspect the new destination. Closing the dialog or leaving the Server tab cancels that dialog’s version check and releases its busy state; it never cancels an unrelated deployment. Dialogs use transform-free positioning and a plain dimmed backdrop for sharp, reliable rendering in macOS WebKit.

Deploy closes the dialog and reports progress in the bottom bar. It automatically prepares server inputs from the installed client pack’s immutable commit and `server_config.conf` rules. No separate local build/update step is required for SFTP. Matching Forge files, Java 17, cached mod metadata and verified downloads are reused. Local destinations receive Forge runtime files, start scripts and defaults as well as the versioned pack payload; author-specific worlds/access lists are omitted.

For a new local server, read the Minecraft EULA and set `eula=true` in its `eula.txt` before the first start. Open its folder from the details dialog and run `run.sh` on macOS or `run.bat` on Windows. The scripts prefer the launcher's managed Java 17 on the same machine, then fall back to `JAVA_HOME` or available Java if that runtime is absent; a copied server requires Java 17 on its destination. Adjust `user_jvm_args.txt` for server memory.

**SFTP instances** store credentials securely, detect the remote `auram.json` version, plan changelist-based updates without recursively scanning or hashing remote files, and start with twenty separate connections and adapt to actual server limits. It understands `.jar.client` names and preserves worlds, properties and runtime files. If no usable version/update route exists, **Full deploy** replaces only `mods`, `scripts`, `config` and `kubejs`, including custom files inside those directories, while retaining their originals as backups. Stop the remote server before deploying. See [SFTP deployment and recovery](docs/SFTP_DEPLOYMENT.md) for scope, credential storage, comparison rules, and verification.

## Microsoft application registration

The default application ID is the existing registration previously used by Auram's `microshaft` package. It supports device sign-in with `XboxLive.signin offline_access` without changing Entra. The code is needed when adding or reauthorizing an account; subsequent launches silently renew the stored refresh credentials.

An optional advanced browser-callback mode implements authorization-code flow with PKCE. Enable it only for an authorized registration with the native redirect `http://localhost/oauth/callback` configured. The existing registration currently rejects that redirect, so it is not the default. An independently registered replacement may require Minecraft service authorization. No client secret belongs in the desktop app. Authentication errors retain credentials on transient failures; real-account login and renewal still require account interaction to exercise live.

## Releases

Commit, integrate, and push the desired source to `main`, then run:

```sh
pnpm release
```

The command fetches remote refs, captures `origin/main`, and normally pushes that SHA to `origin/release`. Git itself rejects divergence; the command never force-pushes or adds a version-bump/merge commit. Repeating it observes or retries the workflow for the same commit.

Build `N` is the full reachable commit count, `git rev-list --count <commit>`. The UI/release title displays Build N, the tag is `build-N`, and Tauri uses `0.0.N`. GitHub checkout uses complete history. Version configuration and metadata are generated during each build.

Run `pnpm release` on the Apple Silicon development Mac only when publication is wanted. It builds the ARM64 DMG/updater locally from an isolated checkout of the captured remote main commit, while GitHub Actions builds Windows x64. The command waits for Windows, downloads its artifacts, and assembles/publishes both platforms locally through a draft release. Keep the command running through publication. Published assets remain immutable; retries reuse a successful Windows workflow.

Local signing accepts `TAURI_SIGNING_PRIVATE_KEY`, `AURAM_UPDATER_PRIVATE_KEY_FILE`, or an ignored `.auram-release.local.json` containing `{ "updaterKeyFile": "/secure/path/updater.key" }`. Set the optional password through `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Worktrees share the local Cargo build cache; dependency resolution remains frozen. Mac builds and basic checks run locally. The optional Actions CI is Windows-only and manually dispatched.

Production checks the latest GitHub release on startup, verifies the update signature, installs a newer build, and restarts. An unavailable endpoint or invalid signature leaves the installed launcher usable. Automatic attempts are recorded to avoid repeated installation loops; Settings allows an explicit retry. The Flutter launcher needs a one-time manual upgrade because it never contained a self-updater.

Updater signing is independent of OS distribution signing. The initial pipeline uses macOS ad-hoc signing and unsigned Windows installers unless platform credentials are configured. macOS may require allowing the first installation in Privacy & Security, and Windows may display reputation warnings. Developer ID signing/notarization and Authenticode credentials are separate from the required updater private key.

## Isolated live verification

`cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --example inspect_pack` resolves and verifies real pack metadata.

`cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --example verify_runtime -- --client` provisions and verifies Java, Forge, and Mojang resources under a temporary test directory. Repeating it reports transferred download bytes, allowing cache reuse to be verified.

On Apple Silicon, `cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --example verify_macos_compat` runs a headless native regression against the actual managed Java and client libraries: JNA long-error handling, OSHI CPU/memory queries, Unicode/space paths, and both JVM tuning profiles at 1, 9, 9.5, and 20 GiB. It uses isolated temporary runtime data, requires no account, and does not launch Minecraft or exercise gameplay. Set `AURAM_VERIFY_RUNTIME_DIR` to select a separate verification cache.

`cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --example verify_server` builds the real pack server in that same isolated directory. It does not accept the EULA or start a server automatically.

`cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --example verify_auto_memory -- /path/to/isolated/data` starts managed Java headlessly with this computer's automatic preset and checks the actual maximum and initial heap values.

The optional Rust `qa` feature requires `AURAM_QA_DATA_DIR`, uses separate credentials, and allows a fixture update endpoint through `AURAM_QA_UPDATE_URL`. It is excluded from published release builds. QA sessions and artifacts stay in ignored `.qa/` storage.

`cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --example bench_local_files -- /absolute/path/to/server` compares serial and parallel checksum inspection of the same warm-cache files, read-only. The optional second argument limits bytes (default 256 MiB). Existing mutable server files may differ from their recorded hashes; both benchmark modes must produce identical results. `verify_server` reuses the selected installed pack if its isolated data directory already has an installation, allowing repeat-build measurements without changing pack versions.
