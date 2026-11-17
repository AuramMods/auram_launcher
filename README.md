# Auram Launcher

Native Tauri 2 launcher for the Auram Minecraft 1.20.1 Forge pack. Supports Apple Silicon macOS and Windows x64. React, TypeScript, shadcn/ui, Rust, and pnpm; no Flutter/Dart installation is needed.

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

Install Auram, sign in with Microsoft through your system browser, then launch. Refresh credentials are stored in macOS Keychain or Windows Credential Manager. Account selection and sign-out are available in the launcher. Microsoft can require reauthentication after revocation or policy changes.

Pack versions come from `AuramMods/Auram` tags. Downloads use immutable commit files verified with Git blob checksums. Only changed managed files are applied. Shared Java, Mojang assets, libraries, and Forge runtime files survive pack updates; downloads resume when the server supports ranges. Repair verifies files rather than trusting an installation marker. Unmanaged files are retained, and changed managed files are backed up before replacement. Transactions recover interrupted installations on the next start.

Production data lives at `~/Library/Application Support/AuramLauncher` on macOS and `%LOCALAPPDATA%\AuramLauncher` on Windows. Use the UI to open the actual data, client, server, or log folder. Existing Flutter data can be imported from the Play tab without deleting the original installation. Imported Mojang assets and libraries are checked before reuse; the managed Java distribution is checksum-pinned.

Minecraft stays running when its launcher window closes. The launcher remains in the background while collecting redacted logs; reopening the application shows the existing window. Java failures and early game exits are reported with a log location.

## Servers

Install a client pack first, then choose **Build server**. The builder uses that exact pack commit and `server_config.conf` inclusion, exclusion, whitelist, and template settings. It installs matching Forge server files and omits author-specific worlds/access lists. Rebuilding preserves world and user settings.

Read the Minecraft EULA and explicitly accept it in the Server tab. Open the server folder and run `run.sh` on macOS or `run.bat` on Windows. The scripts use `JAVA_HOME`, available Java, or the launcher's managed Java on the same machine; a copied server requires Java 17 on its destination. Adjust `user_jvm_args.txt` for server memory.

## Microsoft application registration

The default application ID is the registration previously used by Auram's `microshaft` package. It must allow personal Microsoft accounts, public native clients, and the system-browser redirect `http://localhost/oauth/callback`. Microsoft ignores the dynamically chosen port for registered localhost redirects. The authorization request uses PKCE with `XboxLive.signin offline_access`; no client secret belongs in the desktop app.

If that registration needs changes, update its native redirect settings in Entra. An independently registered replacement may require Minecraft service authorization. The application ID can be changed under advanced authentication settings. Authentication errors retain existing credentials on transient failures; account interaction remains necessary to prove real-account login and renewal.

## Releases

Commit, integrate, and push the desired source to `main`, then run:

```sh
pnpm release
```

The command fetches remote refs, captures `origin/main`, and normally pushes that SHA to `origin/release`. Git itself rejects divergence; the command never force-pushes or adds a version-bump/merge commit. Repeating it observes or retries the workflow for the same commit.

Build `N` is the full reachable commit count, `git rev-list --count <commit>`. The UI/release title displays Build N, the tag is `build-N`, and Tauri uses `0.0.N`. GitHub checkout uses complete history. Version configuration and metadata are generated during each build.

The release workflow builds macOS ARM64 DMG/updater archives and Windows x64 per-user NSIS/updater installers. A single publisher assembles signatures, checksums, `build.json`, and `latest.json`, uploads them to a draft, then publishes it. Published release assets remain immutable; failed drafts may be retried.

Production checks the latest GitHub release on startup, verifies the update signature, installs a newer build, and restarts. An unavailable endpoint or invalid signature leaves the installed launcher usable. Automatic attempts are recorded to avoid repeated installation loops; Settings allows an explicit retry. The Flutter launcher needs a one-time manual upgrade because it never contained a self-updater.

Updater signing is independent of OS distribution signing. The initial pipeline uses macOS ad-hoc signing and unsigned Windows installers unless platform credentials are configured. macOS may require allowing the first installation in Privacy & Security, and Windows may display reputation warnings. Developer ID signing/notarization and Authenticode credentials are separate from the required updater private key.

## Isolated live verification

`cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --example inspect_pack` resolves and verifies real pack metadata.

`cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --example verify_runtime -- --client` provisions and verifies Java, Forge, and Mojang resources under a temporary test directory. Repeating it reports transferred download bytes, allowing cache reuse to be verified.

`cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --example verify_server` builds the real pack server in that same isolated directory. It does not accept the EULA or start a server automatically.

The optional Rust `qa` feature requires `AURAM_QA_DATA_DIR`, uses separate credentials, and allows a fixture update endpoint through `AURAM_QA_UPDATE_URL`. It is excluded from published release builds. QA sessions and artifacts stay in ignored `.qa/` storage.
