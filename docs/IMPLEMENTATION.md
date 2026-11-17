# Tauri migration progress

The implementation follows committed cycles: foundation; persistence/migration; downloads; pack reconciliation/runtime cache; authentication; game runtime; server builder; UI/updater/releases; integrated verification/publication.

The Flutter source remains as migration reference until the replacement passes integrated verification. No production installation is modified by development runs.

Current cycle: native foundation. Production release is not yet configured.

Persistence/download cycle: added non-destructive legacy import, development isolation, cross-platform safe paths, atomic JSON, rollback journals, job cancellation/serialization, verified resumable downloads and bounded queues. Recovery, migration, Git blob hashing, range support, ignored ranges and 416 responses are covered by isolated tests.
