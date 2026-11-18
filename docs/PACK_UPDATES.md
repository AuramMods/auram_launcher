# Pack bundles and sparse Git updates

The launcher verifies a signed release catalog that contains the exact pinned commit, complete target inventory, release history and immutable archive locations. The pack maintainer builds that catalog and its partial archives locally. Ordinary bundle updates use release asset URLs and need zero GitHub REST calls. See the [distribution contract](PACK_DISTRIBUTION_CONTRACT.md).

With a usable baseline, the launcher composes the final target first. It removes objects already present in verified caches or managed files and counts the distinct archives needed for the remaining objects. Cached archives do not count. Eight or fewer required archives use the bundle route. A split release may require several archives. A metadata-only change can require zero. Independent large files avoid loading superseded jars, while a small grouped archive can contain a few extra members. `state/last-pack-transfer.json` records archive body bytes, unpacked bytes and additional unpacked member bytes for diagnosis.

Older builds that lack catalogs keep the pinned GitHub HTTP path. The launcher resolves the advertised release's tag, checks schema-1 changelist provenance, and fetches final blobs from the immutable commit. It handles annotated tags, legacy versions such as `1.0.7`, and paginated tag discovery. If a signed catalog references an absent archive, verified raw files from that same signed commit can supply the delta. An unsupported signed format, invalid signature or corrupt archive fails with a retryable error.

## Sparse transport

Fresh installs, unknown baselines, missing history routes and updates requiring more than eight uncached archives use one private shallow Git object cache. The launcher retrieves the pinned tree without its file bodies and batches only selected missing blobs. It ships the transport on macOS and Windows and works without a Git account. An explicitly configured Git executable can use saved credentials in the same cache. Git transport failure can fall back to verified raw files while preserving the exact pinned commit.

Server source inventories are separate and include `server_config.conf` and `server_files`, even when `.packignore` excludes them from clients. The server builder continues applying its inclusion rules, mod sidedness, dependencies, templates, and preservation policy.

## Rate limits and retries

Metadata and file requests both retry transient connection failures, HTTP 408/5xx responses, and rate limits, with a maximum of five attempts per request. GitHub's HTTP 403 rate limits are recognized from the response headers or rate-limit message; ordinary permission failures and missing files still fail immediately. Backoff grows exponentially and includes random jitter. `Retry-After` (seconds or an HTTP date) and exhausted `X-RateLimit-Reset` times are honored without the old 60-second cap. A secondary limit without retry timing starts with a one-minute wait and doubles on repeated failures, following [GitHub's retry guidance](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api).

All downloader clones share cooldowns per origin. Parallel file downloads pause together, then requests to that origin are spaced 250 ms apart for a one-minute recovery period. Other origins and already verified cache files remain usable. The existing progress bar shows the retry countdown, and cancellation interrupts the wait. Partial files and their resume validators are retained. A rate-limited changelist does not trigger a more expensive full-inventory fallback. Startup retains its bounded remote check and can still offer the installed pack when the service is unavailable; explicit retries respect the outstanding cooldown.

## Baselines, repair, and fallback

The expected release inventory is cached separately from the instance's ownership/base-content inventory. The latter records managed paths and their last applied distribution hashes; it does not certify that user-writable disk files still match. Preserved settings retain their prior base identity, and unknown user files are not claimed or removed.

Incremental updates inspect the affected managed paths. **Repair** inspects the complete selected target, including files untouched by recent releases. Repair can fetch missing content from Git for the same installed version while leaving other files in place.

All downloaded files are size/hash checked before activation. Git's blob SHA-1 includes its `blob <length>\0` header. Unsupported symlinks/submodules, unsafe destination names and case/Unicode-normalization collisions fail explicitly. Regular executable modes are applied to private staging copies without modifying shared cache objects.

Full replacement stages `mods`, `config`, `scripts` and `kubejs` on the destination volume before activation. Custom files inside those folders move with the originals to `.auram-backups/<transaction>/original` in the instance. To restore a custom file, close Minecraft or the server and copy the needed file from that backup. Client metadata and other pack paths still follow ordinary ownership rules. Worlds, screenshots, options, resource and shader packs, server properties, EULA and access lists are preserved. The manifest activates last. Interrupted replacements roll back from retained originals.

## Explicit force reinstall

**Force reinstall** under **Your installation** is a separate destructive operation. Its confirmation names the client directory and warns that all local client files are permanently deleted, including worlds, screenshots, options, resource/shader packs, custom mods, and the installed manifest. It then installs the currently published pack into the empty client instance and verifies its Java/Minecraft/Forge resources. Valid shared cache content is reused instead of downloaded again.

This action retains launcher settings, Microsoft credentials, SFTP connection details, local/remote server data, shared caches, partial downloads, and earlier retained backups. Cancellation or a failed reinstall does not restore deleted client data. If installation fails after deletion, the UI shows the client as uninstalled so **Install Auram** can retry a fresh install.

A persistent deletion intent is written before removal. If deletion is interrupted, it is resumed before any older client installation journal can replay files. A pending reset never counts as an installed pack. The intent is cleared only after the client and its old transaction data are removed and an empty instance directory is ready. Reset completion is idempotent and cannot erase a later installation on subsequent launches. Deletion never follows symlinks to external user data. Ordinary updates and repair retain their preservation behavior above.

## Verification

Run basic checks locally:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets --features qa -- -D warnings
pnpm check
pnpm build --no-bundle
```

Local HTTP and Git fixtures cover the archive boundary, signature and extraction failures, exact Git object requests, skipped releases, rename reuse, legacy baselines, truncated trees and repair. Filesystem fixtures cover interrupted activation and transitions blocked by user data.

For real committed versions, build the catalogs locally in the pack checkout. Build the headless example with `cargo build --manifest-path src-tauri/Cargo.toml --no-default-features --features qa --example verify_pack_delivery --locked`, then invoke `python3 scripts/verify_pack_delivery.py <pack-repo> <catalog-parent> <example-binary>`. The fixture uses versions 139, 141, 159 and 161, or 145 for `--legacy`. `--public` uses the public endpoint for the final 159-to-161 update. `--boundary` repackages the same committed files into eight and nine local archives using a disposable fixture key. `--runtime-cache` can reuse an earlier isolated runtime cache to measure warm revalidation. All writes stay inside the temporary QA root.

Rate-limit fixtures cover reset/HTTP-date headers and clock skew, primary/secondary limits, transient metadata failures, permission errors, shared metadata/file cooldowns, gradual recovery, independent origins, cancellation, partial-file resume, bounded retries, and suppression of full-inventory fallback after a rate limit.

An explicit live smoke test reads published metadata without downloading pack jar bodies:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --lib published_build145_chains_and_jar_rename_identities -- --ignored --nocapture
```

The live test resolves `139 → 145` through three manifests and verifies that `143 → 145`'s Auram, Imperium and Satiscraftory renames retain their exact blob IDs.
