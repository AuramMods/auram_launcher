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

Pack versions come from the release advertised by `AuramMods/Auram`’s `auram.json`, pinned to its annotated or lightweight Git tag. The launcher composes changelists across skipped builds and fetches each selected final blob once. Sparse HTTP inventories omit unrelated repository content, bundled runtimes, and user data; unchanged jar renames reuse verified cached or installed bytes. Downloads use immutable commit files verified with Git blob checksums. Only changed managed files are applied. See [pack update behavior](docs/PACK_UPDATES.md) for fallback and ownership rules. Shared Java, Mojang assets, libraries, and Forge runtime files survive pack updates; downloads resume when the server supports ranges. Repair verifies files rather than trusting an installation marker. Unmanaged files are retained, and changed managed files are backed up before replacement. Transactions recover interrupted installations on the next start.

Production data lives at `~/Library/Application Support/AuramLauncher` on macOS and `%LOCALAPPDATA%\AuramLauncher` on Windows. Use the UI to open the actual data, client, server, or log folder. Existing Flutter data can be imported from the Play tab without deleting the original installation. Imported Mojang assets and libraries are checked before reuse; the managed Java distribution is checksum-pinned.

Minecraft stays running when its launcher window closes. The launcher remains in the background while collecting redacted logs; reopening the application shows the existing window. Java failures and early game exits are reported with a log location.

## Servers

Install a client pack first, then choose **Build server**. The builder uses that exact pack commit and `server_config.conf` inclusion, exclusion, whitelist, and template settings. It installs matching Forge server files, restores mandatory server-side dependencies mistakenly excluded by filename rules, and omits author-specific worlds/access lists. Rebuilding preserves world and user settings.

Read the Minecraft EULA and explicitly accept it in the Server tab. Open the server folder and run `run.sh` on macOS or `run.bat` on Windows. The scripts prefer the launcher's managed Java 17 on the same machine, then fall back to `JAVA_HOME` or available Java if that runtime is absent; a copied server requires Java 17 on its destination. Adjust `user_jvm_args.txt` for server memory.

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

The release workflow builds macOS ARM64 DMG/updater archives and Windows x64 per-user NSIS/updater installers. A single publisher assembles signatures, checksums, `build.json`, and `latest.json`, uploads them to a draft, then publishes it. Published release assets remain immutable; failed drafts may be retried.

Production checks the latest GitHub release on startup, verifies the update signature, installs a newer build, and restarts. An unavailable endpoint or invalid signature leaves the installed launcher usable. **Play installed version** launches the current pack without checking GitHub for a pack update; normal Microsoft authentication requirements still apply. Automatic attempts are recorded to avoid repeated installation loops; Settings allows an explicit retry. The Flutter launcher needs a one-time manual upgrade because it never contained a self-updater.

Updater signing is independent of OS distribution signing. The initial pipeline uses macOS ad-hoc signing and unsigned Windows installers unless platform credentials are configured. macOS may require allowing the first installation in Privacy & Security, and Windows may display reputation warnings. Developer ID signing/notarization and Authenticode credentials are separate from the required updater private key.

## Isolated live verification

`cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --example inspect_pack` resolves and verifies real pack metadata.

`cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --example verify_runtime -- --client` provisions and verifies Java, Forge, and Mojang resources under a temporary test directory. Repeating it reports transferred download bytes, allowing cache reuse to be verified.

`cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --example verify_server` builds the real pack server in that same isolated directory. It does not accept the EULA or start a server automatically.

The optional Rust `qa` feature requires `AURAM_QA_DATA_DIR`, uses separate credentials, and allows a fixture update endpoint through `AURAM_QA_UPDATE_URL`. It is excluded from published release builds. QA sessions and artifacts stay in ignored `.qa/` storage.
