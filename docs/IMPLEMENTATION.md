# Tauri migration progress

The implementation follows committed cycles: foundation; persistence/migration; downloads; pack reconciliation/runtime cache; authentication; game runtime; server builder; UI/updater/releases; integrated verification/publication.

The Flutter source remains as migration reference until the replacement passes integrated verification. No production installation is modified by development runs.

Current cycle: native foundation. Production release is not yet configured.

Persistence/download cycle: added non-destructive legacy import, development isolation, cross-platform safe paths, atomic JSON, rollback journals, job cancellation/serialization, verified resumable downloads and bounded queues. Recovery, migration, Git blob hashing, range support, ignored ranges and 416 responses are covered by isolated tests.

Runtime/server/UI cycle: native Java 17 archive and checksum pins; safe ZIP extraction with internal links materialized as independent files; official Forge client/server installation; persistent Mojang assets; architecture-specific LWJGL selection; Java argument files and redacted process logging; real pack-driven server exclusions/inclusions; explicit server EULA state; typed desktop operations and OLED-black shadcn interface. Live isolated Java/Forge client provisioning and complete pack 1.0.7 server build succeeded. Microsoft user interaction, Windows execution, updater transition, and release publication remain outstanding.

Release cycle: commit-derived build metadata, fast-forward release command with real-remote tests, two-target CI/build matrix, draft-only artifact aggregation/publication, signed updater downloads with safe retry state, and startup update checks. Updater private key is stored outside Git and in repository Actions secrets. Local Xcode 27 rejected stripped Rust proc-macro dylibs; release builds explicitly retain build-tool symbols to avoid the reproduced LINKEDIT alignment failure.
