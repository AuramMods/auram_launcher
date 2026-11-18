# Changelists and sparse pack updates

Auram's schema-1 changelists drive normal client updates. The launcher reads the build advertised by `auram.json` on main, resolves that version's Git tag (including annotated/nested tags), then verifies and uses the metadata at that immutable commit. Unreleased changes on main never become the payload. Older repositories without `auram.json` retain paginated numeric/semantic tag discovery.

The `changelist` field supplies the manifest path. Versions remain strings in installation identities, so legacy `1.0.7` baselines work alongside numeric builds. The chain follows `previousVersion` and verifies each resolved tag against `previousCommit`; `contentCommit` must be the release commit's parent. It is provenance rather than the download revision.

The planner validates the entire chain before activation, keeping the newest operation for each path. It fetches upserts from the final target only. Renames reuse content by Git blob ID, including verified managed files when their cache objects were evicted. It never downloads intermediate jars or infers deletion from a failed HTTP request.

## Sparse transport

This implementation uses sparse HTTP retrieval and a persistent Git-object cache. It does not require a Git executable, clone repository history, or turn the live instance into a Git worktree. Root and immediate Minecraft trees identify selected directories before recursive requests; runtime/user-data folders, tooling, and server-only content are pruned from client traversal. Files use immutable, segment-encoded raw URLs. Git tree JSON is cached by object identity; file bytes use Git blob identities. A truncated recursive response falls back to complete non-recursive subtrees and never establishes absence.

Server source inventories are separate and include `server_config.conf` and `server_files`, even when `.packignore` excludes them from clients. The server builder continues applying its inclusion rules, mod sidedness, dependencies, templates, and preservation policy.

## Rate limits and retries

Metadata and file requests both retry transient connection failures, HTTP 408/5xx responses, and rate limits, with a maximum of five attempts per request. GitHub's HTTP 403 rate limits are recognized from the response headers or rate-limit message; ordinary permission failures and missing files still fail immediately. Backoff grows exponentially and includes random jitter. `Retry-After` (seconds or an HTTP date) and exhausted `X-RateLimit-Reset` times are honored without the old 60-second cap. A secondary limit without retry timing starts with a one-minute wait and doubles on repeated failures, following [GitHub's retry guidance](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api).

All downloader clones share cooldowns per origin. Parallel file downloads pause together, then requests to that origin are spaced 250 ms apart for a one-minute recovery period. Other origins and already verified cache files remain usable. The existing progress bar shows the retry countdown, and cancellation interrupts the wait. Partial files and their resume validators are retained. A rate-limited changelist does not trigger a more expensive full-inventory fallback. Startup retains its bounded remote check and can still offer the installed pack when the service is unavailable; explicit retries respect the outstanding cooldown.

## Baselines, repair, and fallback

The expected release inventory is cached separately from the instance's ownership/base-content inventory. The latter records managed paths and their last applied distribution hashes; it does not certify that user-writable disk files still match. Preserved settings retain their prior base identity, and unknown user files are not claimed or removed.

Normal changelist updates inspect affected paths. **Repair** inspects the complete selected target, including files untouched by recent releases. Fresh installs, missing/invalid chains or caches, changed `.packignore` rules, different installation policies, divergent/downgrade histories, and chains longer than eight releases use complete sparse target reconciliation. Existing launcher installations perform this reconciliation once to establish the new policy baseline, reusing valid local content and runtime caches.

All downloaded files are size/hash checked before activation. Git's blob SHA-1 includes its `blob <length>\0` header. Unsupported symlinks/submodules, unsafe destination names and case/Unicode-normalization collisions fail explicitly. Regular executable modes are applied to private staging copies without modifying shared cache objects.

Transactions back up originals, delete obsolete owned files before installing replacements, handle file/directory transitions, and activate the manifest last. Unknown files blocking a transition cause rollback. Recovery removes staged files before restoring old parent files; it never recursively deletes user directories to make an update fit. Modified managed files also receive retained backups. Worlds, screenshots, local mods and shared Java/Mojang/Forge resources remain preserved.

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

Local HTTP fixtures cover skipped releases, final-only downloads, rename reuse with an empty cache, legacy baselines, tag pagination/peeling, truncated trees, missing/inconsistent/cyclic chains, policy/filter changes, case-only renames, failed downloads, and repair. Filesystem fixtures cover interrupted activation and transitions blocked by user data.

Rate-limit fixtures cover reset/HTTP-date headers and clock skew, primary/secondary limits, transient metadata failures, permission errors, shared metadata/file cooldowns, gradual recovery, independent origins, cancellation, partial-file resume, bounded retries, and suppression of full-inventory fallback after a rate limit.

An explicit live smoke test reads published metadata without downloading pack jar bodies:

```sh
cargo test --manifest-path src-tauri/Cargo.toml --lib published_build145_chains_and_jar_rename_identities -- --ignored --nocapture
```

The live test resolves `139 → 145` through three manifests and verifies that `143 → 145`'s Auram, Imperium and Satiscraftory renames retain their exact blob IDs.
