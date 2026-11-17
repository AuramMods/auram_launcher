# Native launcher implementation

The Flutter/Dart implementation and bundled ZIP were replaced with Tauri 2, a Rust core, React/TypeScript, pnpm, and an OLED-black shadcn interface.

## Behavior

- Pack tags resolve to immutable GitHub commits. File manifests and Git blob hashes support incremental, verified updates without removing shared runtime caches or user worlds.
- Downloads use persistent partial files, range/validator handling, bounded concurrency, cancellation, retries, and checksum verification.
- Atomic writes, installation journals, rollback, retained backups, and restartable legacy import preserve existing installations.
- The existing Microsoft registration supports device sign-in with offline access. The user explicitly chose to retain this registration rather than change Entra. Sign-in displays a code; **Copy Code** copies it and opens Microsoft in one action. Refresh credentials remain in native credential storage and renew silently. Optional PKCE callback mode is available for appropriately configured registrations.
- Managed Java 17, official Forge installers, platform-specific native libraries, Java argument files, and redacted process output handle game/runtime setup.
- Server builds use pack inclusion/exclusion settings and mod sidedness metadata, restore required common libraries excluded by filename rules, preserve user configuration/worlds, and generate scripts preferring managed Java 17.
- Commit-count versions, a non-forced release branch update, signed startup updates, and two-platform GitHub Actions builds implement distribution. Updater private keys stay outside Git and in repository Actions secrets.

## Verification evidence

- Native ARM64 packaged UI: OLED rendering, navigation, settings persistence, device code presentation, cancellation, and recoverable missing-update errors exercised.
- Real pack 1.0.7 metadata and hashes verified. Official Java 17 / Forge 47.4.10 client provisioned. Repeating provisioning transferred zero download body bytes.
- Isolated signed updater: rejected a mismatched signature, then installed Build 23 over Build 22 and restarted into the exact newer commit. Both builds came from real commits; production data was excluded.
- Rust tests cover refresh rotation/concurrency, revoked credentials, device challenge lifecycle, download corruption/ranges/cancellation/deduplication, path/archive safety, migration, transaction rollback, configuration preservation, launch arguments and server dependencies.
- Real temporary Git remote tests cover reachable commit counts, release retries, unpublished local work and rejection of divergent release pushes.
- Windows and macOS CI passed for committed Build 23. The latest Copy Code/server refinements require a final two-platform run.
- The real pack server reached Minecraft's ready state in an isolated directory. Fresh-directory startup and shutdown verification is the current live check.

## Distribution and interactive limits

Full real-account Minecraft authentication/launch requires the account owner's interaction; protocol behavior and the real registration's device challenge have been exercised without reading private account credentials. macOS distribution currently uses ad-hoc signing, with mandatory updater signatures; Apple notarization and Windows Authenticode credentials are not configured in CI. Initial installation may require an OS trust confirmation. Publication and endpoint verification remain the final work items.
