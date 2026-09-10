# Spec: file classification and immutable revision identity

## Status and authority

This specification records the owner-accepted G1 pre-release repair direction for R5. The repository is private and unreleased, so this scoped local catalogue compatibility upgrade may replace the defective internal revision seam. The accepted direction authorizes the classification-only `H7-C/PRE` implementation and review described below; it does **not** authorize applying migration `0004` to a live catalogue, rewriting or deleting historical rows, unrelated V2 persistence, H6B public surfaces, H8 adoption/release, or publication.

The fixed internal identities are:

- producer classification policy: `file-classification-v2.0.0`;
- immutable revision identity policy: `file-revision-identity-v2.0.0`;
- resulting catalogue schema: `1.3.0`;
- forward migration: `0004_file_classification_identity`, yielding `PRAGMA user_version = 4`.

Changing the classified path set, classification outputs, identity tuple, canonical encoding, digest algorithm, or reuse rule requires a successor identity and renewed compatibility review.

## Producer-owned classification

Discovery owns classification before immutable `file_revision` lookup or insertion. The classifier consumes the already-normalized, repository-relative path with `/` separators. It performs no file-body scan, manifest inference, provider query, compiler inference, or task-conditioned classification.

`file-classification-v2.0.0` adds exactly one rule ahead of the existing extension classifier:

1. the extension, compared with the existing ASCII case-insensitive extension behavior, is `.rs`; and
2. one directory segment, excluding the filename, is exactly the case-sensitive ASCII segment `tests`.

When both predicates hold, discovery emits `artifact_class = 'test'` and `is_test = 1`. Examples include `tests/calculate.rs` and `crates/math/tests/integration.rs`. `src/tests.rs`, `src/unit_test.rs`, `Tests/calculate.rs`, and non-Rust files under `tests/` do not match this rule and retain the existing classifier result. Rust unit-test modules embedded in ordinary source files remain `source`; this policy does not parse `#[cfg(test)]`, infer names, or promise classification for other languages.

Every non-matching path uses the current extension mapping unchanged and emits `is_test = 0`. Existing `is_generated` and language production remain unchanged. Classification must be computed once and the same result must drive revision lookup, insertion, generation reporting, and provider reuse.

## Immutable revision identity

A new revision is needed only when no existing row exactly matches this material identity tuple:

```text
(file_id, content_hash, artifact_class, language-or-empty, is_generated, is_test)
```

Migration `0004` replaces `idx_file_revision_identity` with a unique index over that complete tuple. It does not add a compatibility column, mutate a `file_revision`, relabel a historical generation, or backfill policy text. Policy provenance for generations produced after the cutover is the generation schema `1.3.0`; historical rows retain their original values.

When no matching row exists, `file-revision-identity-v2.0.0` derives the revision ID from a domain-separated, length-prefixed canonical encoding of:

```text
file-revision-identity-v2.0.0
file_id
full 64-hex content_hash
artifact_class
language-or-empty
is_generated as 0 or 1
is_test as 0 or 1
```

The ID is `rev_` plus the full lowercase 64-hex BLAKE3 digest. The full content hash and every classification field participate; the current first-12-content-hash path construction is not used for new rows. Existing IDs are opaque and remain valid foreign-key targets.

## Forward migration and catalogue compatibility

The owner-authorized RP7 implementation increment owns exactly these six files:

1. `migrations/0004_file_classification_identity.sql` (new);
2. `src/migrations.rs`;
3. `src/ids.rs`;
4. `src/discovery.rs`;
5. `tests/compiler_composition.rs`;
6. `src/catalogue.rs`, limited in RP7 to changing the two initialization assertions for applied migration and `PRAGMA user_version` from `3` to `4`.

This list records the post-review RP7 scope amendment. The first five paths
remain the implementation owners frozen before RP7; the sixth preserves the
owner-authorized assertion-only change already present in the RP7 commit.
`src/ids.rs` remains the one typed owner of revision identity rather than an
ad hoc hash in discovery. Migration and discovery tests remain focused unit
tests in their owning Rust modules; no separate fixture file is required.

The migration is forward-only and structural:

1. preflight the existing migration-integrity and catalogue-integrity checks;
2. in one migration transaction, drop only `idx_file_revision_identity`, create its six-field replacement, record migration `0004`, and atomically advance `PRAGMA user_version` to `4` with the committed body;
3. change `CURRENT_SCHEMA_VERSION` to `1.3.0` for generations produced by the upgraded binary;
4. perform no `UPDATE`, `DELETE`, table rebuild, historical generation rewrite, or eager revision creation.

The current runner commits a migration body before setting `user_version`. That commit/header split must be removed or otherwise proven impossible for `0004`: after every injected failure, both the migration row/index and header must be wholly old or wholly new. This is a focused `src/migrations.rs` acceptance requirement, not permission to redesign unrelated migrations.

Opening a catalogue at versions `1`–`3` applies the ordered migrations through `0004`. Existing rows are reused when all six material fields match the new classification. A formerly source-classified Rust file under a `tests` segment does not match: reconcile creates one V2 revision, retains the old source revision and its extractor/fact history, and points only the new generation at the new test revision. Nothing rewrites earlier generations.

After `user_version = 4`, binaries whose maximum migration is `3` must refuse the catalogue before any read-modify-write operation using the existing newer-catalogue guard. No compatibility shim, dual writer, or downgrade mode is added. A V2 binary also rejects a split state where `schema_migration`, migration checksum, index shape, and `user_version` do not agree.

## Backup and rollback

Implementation and tests use disposable catalogue copies; this documentation task applies no migration. Before the first live-catalogue application, `H7-C/POST` requires a reviewed restore rehearsal from an explicit user-owned consistent backup made while Atlas writers are stopped and WAL state has been safely checkpointed. Atlas must not silently create a backup, delete an old catalogue, or imply retention ownership.

Failure before commit leaves migration row, index, and `user_version` unchanged. After a successful migration, rollback means stopping writers and restoring the complete pre-migration backup, then running the old binary against that restored catalogue. Reverting only the executable is prohibited because the old binary must refuse version `4`. Without a verified pre-migration backup, recovery is forward repair with a version-4-capable binary; there is no supported in-place downgrade or historical-row rewrite.

## RP9 compatibility-fence repair

The owner-authorized RP9 repair owns exactly eight tracked files:

1. `migrations/0004_file_classification_identity.sql`;
2. `src/catalogue.rs`;
3. `src/cli.rs`;
4. `src/migrations.rs`;
5. `Cargo.toml`;
6. `tests/context_observation.rs`;
7. this specification;
8. `implementation-plan.md`.

The original four-file RP9 scope expanded only after implementation proved
three pre-existing boundaries: `doctor` and other durable CLI paths did not all
take the writer lease, direct current-binary migration entrypoints can receive
a raw `rusqlite::Connection`, and one integration fixture reopened a writable
schema-4 catalogue without the application connection contract. `src/cli.rs`
therefore routes every post-initialization CLI path that can durably mutate an
opened catalogue through the marker-bearing lease before that mutation.
`init` remains governed by the atomic migration runner: a preserved binary
rejects version `4` before catalogue mutation, while a current binary registers
the connection-local identity before migration and registration.
`Cargo.toml` only enables the existing `rusqlite` dependency's
function-registration feature, and
`src/migrations.rs` only registers the same connection-local schema-writer
identity at the start of `apply_all`. `tests/context_observation.rs` only
replaces that raw writable reopen with `catalogue::open_connection`; all event
assertions and production behavior remain unchanged.

Migration `0004` adds nullable `writer_lease.schema_writer_version` with no
legacy-compatible default. A database trigger rejects an acquisition/upsert
whose inserted marker is not exactly `4` before conflict handling can update an
existing lease. The current `acquire_writer_lease` statement supplies `4`.
Other legacy mutators that did not acquire a lease are stopped at their first
durable statement by schema triggers that call the deterministic zero-argument
connection-local `atlas_schema_writer_version()` function. Current
schema-4-capable connections register that function as exactly `4` before
schema-dependent work; the preserved maximum-migration-3 executable cannot
register it, so absence or any other value fails closed. This is a compatibility
fence, not a compatibility shim, dual writer, downgrade mode, new public
surface, or schema/version change.

`init_catalogue` creates no implicit sibling backup. Migration authority stays
with an explicit user-owned, writer-stopped, WAL-consistent external backup and
the restore contract below. RP9 changes no historical catalogue row and keeps
schema `1.3.0`, migration/user version `4`, both V2 identities, migration
ordering, checksum enforcement, index consistency, and every unrelated gate.

## F2A private legacy-init operational fence

The supported invocation path for the preserved maximum-migration-3 `init` is
the private, package-excluded `scripts/legacy-init-fence.py` wrapper. Direct
invocation of the preserved executable's `init` command is unsupported and is
not represented as fenced. The wrapper accepts only the closed legacy-init
argument set and verifies the preserved executable SHA-256
`c61f71b9e17a46c06aabbc40383de4e549061e860da0bb74f7eaa5eea601fabf`,
and constructs the subprocess arguments itself so no second catalogue or
command can bypass the checked route.

Before any subprocess launch, the wrapper inspects an existing catalogue
read-only and immutable, refuses symlinks and non-regular files, refuses
WAL/SHM/journal sidecars, and requires an internally consistent schema-3
header, ordered migration rows, exact migration checksums, legacy identity
index, integrity check, foreign keys, and registered workspace route. Schema 4
or newer refuses before launch. Schema 0–2, unreadable, corrupt, split,
unregistered, or concurrently changing state also refuses rather than asking
the old executable to open or copy it.

A validated existing schema-3 catalogue returns a deterministic
already-initialized success without launching legacy `init`, because the
preserved executable creates an implicit sibling even when no migration is
pending. Only an absent catalogue path may launch the exact preserved
executable; fresh initialization creates schema 3 and no sibling. Subsequent
schema-3 status/reconcile use remains supported. Backup, retention, and restore
remain explicit caller operations; the fence never creates, redirects,
renames, or deletes a backup. Current schema-4 initialization and migration
behavior is unchanged.

This repair owns only `scripts/legacy-init-fence.py`,
`scripts/test_legacy_init_fence.py`, this specification, and
`implementation-plan.md`. It adds no packaged/public command, compatibility
alias, migration, persisted field, schema/version change, release surface, or
live-catalogue authority. `H7-C/POST`, live use, main checkpoint, H6B, H8,
PR/tag/release/publication, and visibility changes remain closed.

## RED/GREEN acceptance

**RED before repair**

- `tests/calculate.rs` reconciles as `source/is_test=0`.
- Reconcile after fixture SQL changes classification collides because the old revision ID omits classification and truncates the content hash input.
- The old unique index permits `is_generated` or `is_test` to differ without participating in revision uniqueness.
- A body-committed/header-old migration fault can evade the old-binary `user_version` guard.

**GREEN after repair**

- Focused classifier cases prove the exact positive and negative path matrix above and preserve every non-matching extension result.
- A first real reconcile produces `test/is_test=1` for `tests/calculate.rs`; an unchanged second reconcile while a source file changes reuses the test revision without direct SQL mutation.
- Composition obtains `TestContract`/validation evidence from producer-created Truth.
- Identity vectors differ for each material classification field and for full content hashes sharing the first 12 hexadecimal characters; identical tuples yield identical IDs.
- An upgraded populated catalogue retains byte-for-byte historical row values and foreign-key reachability, reuses matching old revisions, creates a distinct V2 test revision only when needed, and passes foreign-key/integrity checks.
- Migration success makes migration row, checksum, six-field index, schema version, and `user_version` agree. Every injected failure leaves the complete old state. A simulated maximum-version-3 binary refuses version `4`.
- Backup/restore rehearsal returns the exact pre-migration catalogue and permits the old binary; no test applies a migration to a live catalogue.

## Gate closure

A fresh review must verify implementation, migration fault behavior, old-catalogue reuse, old-binary refusal, historical-row preservation, and restore evidence. Only an explicit owner `H7-C/POST` approval after that review may authorize first live-catalogue use. H7 remains closed for Context IR, route decisions, execution envelopes, lifecycle references, telemetry vocabularies, and every persistence change unrelated to this classification repair.
