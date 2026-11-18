# Auram local distribution, schema 1

`auram-pack.json` is an envelope containing base64 `payload` bytes and a detached
Minisign `signature`. Verify those exact decoded bytes before interpreting them.
The payload is canonical UTF-8 JSON (sorted keys, compact separators, trailing LF).
Public trust roots are separate from launcher updater keys. Private keys never
enter Git, archives, logs, or GitHub secrets. Generate and sign only locally.

The payload identifies `repository`, `schema`, `policy`, exact `target` (name/SHA),
and `producer` commit. `files` is the complete selected source inventory; each
entry supplies Git blob ID, SHA-256, byte size and regular/executable Git mode.
`packIgnore` is the exact target's client filter. Server source inputs are included
independently. `policy.json` is mirrored in the launcher and shared fixtures test
scope/validation. Profiles map repository paths only after source verification.

`history` is newest-to-oldest linked transitions containing exact target/base
identities and before/after entries for every selected change. It supports final
plan composition and reconstruction without remote Git API traversal. Null sides
mean addition/deletion; matching blob IDs preserve rename reuse. A missing route
requests a complete target snapshot, never an incomplete delta installation.

`objects` maps Git blob IDs to keys in `bundles`. Each bundle key is its archive
SHA-256, with size, immutable numeric/legacy-tag release URL, and exact `members`
(Git blob ID -> SHA-256/size). ZIP members are only `objects/<Git blob ID>`.
Verify the archive and each member before cache promotion. Duplicate/unknown
members, links, wrong sizes/modes/hashes, unsafe names and expansion beyond the
declared member bounds are errors. No archive writes directly into an instance.

Only new distinct target objects compared with the previous release are bundled.
Objects inherit immutable locations from already verified older catalogs when
available. An object without a bundle locator is available from the pinned Git
snapshot. Fresh installation and routes needing more than eight uncached payload
archives use a single persistent shallow/partial Git cache and sparse selection.
Large objects are separate payloads; small objects share bounded archives. Counts
refer to actually required uncached archives after final-state/cache reuse, not
numeric version differences. Metadata-only transitions need no payload archive.

Auram ships its own Git transport and fetches public snapshots anonymously by
default. To use an existing Git installation and its saved credentials, set
`AURAM_PACK_GIT` to the absolute path of the installed Git executable before
starting the launcher. Use a real installation, such as Homebrew Git on macOS,
or Git for Windows on Windows. Git is invoked without interactive prompts or
hooks in the same private object cache. If it cannot fetch, Auram retries with
its bundled transport. The launcher never reads or copies the credentials.
See [Git fetch](https://git-scm.com/docs/git-fetch) and
[credential helpers](https://git-scm.com/docs/gitcredentials).

The latest endpoint is the complete envelope at
`https://github.com/AuramMods/Auram/releases/latest/download/auram-pack.json`.
Exact-target discovery uses `/releases/download/<tag>/auram-pack.json`. No normal
path needs REST tag/tree/commit lookups. Publish all objects/metadata to a draft,
verify the asset set, then publish once. Published assets and tags are immutable.
Legacy tags without this envelope retain the HTTP/changelist/tree source.
Transport failures are not absence; signature/hash failures never downgrade trust.

Producer builds use raw object bytes from the tagged commit, never live Prism
files. Artifact metadata is generated after release commit creation, avoiding a
self-referencing SHA. Existing schema-1 pack metadata/tag semantics are unchanged.
