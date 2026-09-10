//! Generation state machine.
//!
//! INV-005 — atomic active generation:
//! "Normal queries see one complete committed generation. A partial or failed
//! candidate generation is never presented as current.
//!
//! `BEGIN IMMEDIATE`
//!   create candidate generation
//!   register generation-file membership
//!   store new immutable file revisions and facts
//!   store diagnostics and coverage
//!   verify required invariants
//!   mark candidate committed
//!   update workspace.active_generation_id
//! `COMMIT`
//!
//! Migration `migrations/0001_foundation.sql` enforces this with
//! `validate_active_generation_insert` and `_update`, which reject an active
//! pointer to any non-committed generation.

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;

use crate::error::{AtlasError, Result};
use crate::ids::generation_id;
use crate::migrations;

/// Allowed generation states; kept identical to the migration's `CHECK` domain.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum GenerationState {
    Candidate,
    Committed,
    Failed,
    Abandoned,
}

impl GenerationState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Committed => "committed",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "candidate" => Some(Self::Candidate),
            "committed" => Some(Self::Committed),
            "failed" => Some(Self::Failed),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }
}

/// Allowed trigger kinds; kept identical to the migration's `CHECK` domain.
#[derive(Debug, Clone, Copy, Serialize)]
pub enum TriggerKind {
    Baseline,
    Watcher,
    Reconcile,
    Manual,
    ProviderInvalidation,
    FullRebuild,
}

impl TriggerKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Watcher => "watcher",
            Self::Reconcile => "reconcile",
            Self::Manual => "manual",
            Self::ProviderInvalidation => "provider_invalidation",
            Self::FullRebuild => "full_rebuild",
        }
    }
}

/// Result of creating a candidate generation.
#[derive(Debug, Clone, Serialize)]
pub struct GenerationRecord {
    pub generation_id: String,
    pub workspace_id: String,
    pub parent_generation_id: Option<String>,
    pub sequence_no: i64,
    pub state: GenerationState,
    pub trigger_kind: TriggerKind,
    pub provider_set_hash: String,
    pub schema_version: String,
    pub created_at: String,
}

/// Begin a candidate generation from a workspace's current active generation.
/// Returns the new candidate's `generation_id`.
pub fn begin_candidate(
    conn: &Connection,
    workspace_id: &str,
    trigger_kind: TriggerKind,
    provider_set_hash: &str,
    schema_version: &str,
) -> Result<GenerationRecord> {
    let parent_id: Option<String> = conn
        .query_row(
            "SELECT generation_id
             FROM index_generation
             WHERE workspace_id = ?1 AND state = 'committed'
             ORDER BY sequence_no DESC LIMIT 1",
            params![workspace_id],
            |r| r.get(0),
        )
        .optional()?;
    // Sequence numbers must never collide, including against abandoned or
    // failed candidates left behind by a crashed prior attempt -- derive
    // the next number from the highest sequence_no across every generation
    // for this workspace, not only committed ones.
    let max_seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(sequence_no), 0) FROM index_generation WHERE workspace_id = ?1",
            params![workspace_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let next_seq = max_seq + 1;
    let gid = generation_id(workspace_id, next_seq);
    let now = migrations::iso8601_now();

    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO index_generation (
            generation_id, workspace_id, parent_generation_id, sequence_no,
            state, trigger_kind, source_tree_hash, provider_set_hash,
            schema_version, created_at, committed_at, failure_code, failure_message,
            eligible_file_count, indexed_file_count, partial_file_count,
            unsupported_file_count, excluded_file_count, failed_file_count
         ) VALUES (?1, ?2, ?3, ?4, 'candidate', ?5, NULL, ?6, ?7, ?8, NULL, NULL, NULL, 0, 0, 0, 0, 0, 0)",
        params![
            gid,
            workspace_id,
            parent_id,
            next_seq,
            trigger_kind.as_str(),
            provider_set_hash,
            schema_version,
            now,
        ],
    )?;
    tx.commit()?;

    Ok(GenerationRecord {
        generation_id: gid,
        workspace_id: workspace_id.to_string(),
        parent_generation_id: parent_id,
        sequence_no: next_seq,
        state: GenerationState::Candidate,
        trigger_kind,
        provider_set_hash: provider_set_hash.to_string(),
        schema_version: schema_version.to_string(),
        created_at: now,
    })
}

/// Atomically activate a candidate generation: mark it `committed`, set its
/// `committed_at`, then update `workspace.active_generation_id`. All three
/// operations share one `BEGIN IMMEDIATE` transaction, and the migration
/// triggers reject any active pointer to a non-committed generation.
pub fn activate_candidate(
    conn: &Connection,
    workspace_id: &str,
    candidate_generation_id: &str,
    source_tree_hash: Option<&str>,
) -> Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    verify_candidate_committable(&tx, workspace_id, candidate_generation_id)?;

    let now = migrations::iso8601_now();
    tx.execute(
        "UPDATE index_generation
         SET state = 'committed',
             committed_at = ?2,
             source_tree_hash = COALESCE(?3, source_tree_hash)
         WHERE generation_id = ?1 AND state = 'candidate'",
        params![candidate_generation_id, now, source_tree_hash],
    )?;

    tx.execute(
        "UPDATE workspace SET active_generation_id = ?2, updated_at = ?3
         WHERE workspace_id = ?1",
        params![workspace_id, candidate_generation_id, now],
    )?;
    crate::task_session::reconcile_active_legacy_sessions(
        &tx,
        workspace_id,
        candidate_generation_id,
    )?;
    tx.commit()?;
    Ok(())
}

fn verify_candidate_committable(
    tx: &Transaction<'_>,
    workspace_id: &str,
    candidate_generation_id: &str,
) -> Result<()> {
    let row: Option<(String, String)> = tx
        .query_row(
            "SELECT state, workspace_id FROM index_generation WHERE generation_id = ?1",
            params![candidate_generation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (state, gen_workspace) = row.ok_or_else(|| AtlasError::GenerationStateInvalid {
        found: "<missing>".into(),
        required: "candidate".into(),
    })?;
    if gen_workspace != workspace_id {
        return Err(AtlasError::GenerationStateInvalid {
            found: format!("workspace {gen_workspace}"),
            required: format!("workspace {workspace_id}"),
        });
    }
    if state != "candidate" {
        return Err(AtlasError::GenerationStateInvalid {
            found: state,
            required: "candidate".into(),
        });
    }
    Ok(())
}

/// Mark a candidate as `failed` with an error code + message. The active
/// generation pointer is NOT changed — this preserves INV-005 ("failed
/// candidates leave the last valid generation authoritative").
pub fn fail_candidate(
    conn: &Connection,
    candidate_generation_id: &str,
    failure_code: &str,
    failure_message: &str,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let updated = tx.execute(
        "UPDATE index_generation
         SET state = 'failed', failure_code = ?2, failure_message = ?3
         WHERE generation_id = ?1 AND state = 'candidate'",
        params![candidate_generation_id, failure_code, failure_message],
    )?;
    if updated == 0 {
        return Err(AtlasError::GenerationStateInvalid {
            found: "<not candidate>".into(),
            required: "candidate".into(),
        });
    }
    tx.commit()?;
    Ok(())
}

/// Mark an in-progress candidate as `abandoned` (e.g. crash recovery). The
pub fn abandon_candidate(conn: &Connection, candidate_generation_id: &str) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let updated = tx.execute(
        "UPDATE index_generation
         SET state = 'abandoned'
         WHERE generation_id = ?1 AND state = 'candidate'",
        params![candidate_generation_id],
    )?;
    if updated == 0 {
        return Err(AtlasError::GenerationStateInvalid {
            found: "<not candidate>".into(),
            required: "candidate".into(),
        });
    }
    tx.commit()?;
    Ok(())
}

/// Look up a generation by id.
pub fn load_generation(conn: &Connection, generation_id: &str) -> Result<Option<GenerationRecord>> {
    let row: Option<GenerationRecord> = conn
        .query_row(
            "SELECT generation_id, workspace_id, parent_generation_id, sequence_no,
                    state, trigger_kind, provider_set_hash, schema_version, created_at
             FROM index_generation WHERE generation_id = ?1",
            params![generation_id],
            |r| {
                let state_str: String = r.get(4)?;
                let trigger_str: String = r.get(5)?;
                Ok(GenerationRecord {
                    generation_id: r.get(0)?,
                    workspace_id: r.get(1)?,
                    parent_generation_id: r.get(2)?,
                    sequence_no: r.get(3)?,
                    state: GenerationState::parse(&state_str).unwrap(),
                    trigger_kind: match trigger_str.as_str() {
                        "baseline" => TriggerKind::Baseline,
                        "watcher" => TriggerKind::Watcher,
                        "reconcile" => TriggerKind::Reconcile,
                        "manual" => TriggerKind::Manual,
                        "provider_invalidation" => TriggerKind::ProviderInvalidation,
                        "full_rebuild" => TriggerKind::FullRebuild,
                        _ => TriggerKind::Manual,
                    },
                    provider_set_hash: r.get(6)?,
                    schema_version: r.get(7)?,
                    created_at: r.get(8)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalogue::init_catalogue;
    use crate::config::Config;
    use crate::workspace::register_workspace;
    use tempfile::tempdir;

    fn fixture() -> (tempfile::TempDir, rusqlite::Connection, String) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("atlas.sqlite");
        let toml = r#"
            schema_version = "1.0.0"
            [workspace]
            display_name = "Example"
            "#;
        let cfg = Config::parse(toml).unwrap();
        let conn = init_catalogue(&path, &cfg).unwrap();
        let ws = register_workspace(
            &conn,
            std::path::Path::new("/projects/example"),
            &cfg,
            &path,
            "1.0.0",
        )
        .unwrap();
        (dir, conn, ws.workspace_id)
    }

    #[test]
    fn begin_and_activate_marks_active_pointer() {
        let (_dir, conn, ws_id) = fixture();
        let gen = begin_candidate(&conn, &ws_id, TriggerKind::Baseline, "p1", "1.0.0").unwrap();
        assert_eq!(gen.state, GenerationState::Candidate);
        assert_eq!(gen.sequence_no, 1);
        activate_candidate(&conn, &ws_id, &gen.generation_id, Some("sth")).unwrap();

        let loaded = load_generation(&conn, &gen.generation_id).unwrap().unwrap();
        assert_eq!(loaded.state, GenerationState::Committed);

        let active: Option<String> = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                params![&ws_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(active.as_deref(), Some(gen.generation_id.as_str()));
    }

    #[test]
    fn fail_candidate_leaves_active_unchanged() {
        let (_dir, conn, ws_id) = fixture();
        let gen1 = begin_candidate(&conn, &ws_id, TriggerKind::Baseline, "p1", "1.0.0").unwrap();
        activate_candidate(&conn, &ws_id, &gen1.generation_id, Some("sth1")).unwrap();

        // A subsequent candidate fails — active pointer must still be gen1.
        let gen2 = begin_candidate(&conn, &ws_id, TriggerKind::Reconcile, "p1", "1.0.0").unwrap();
        fail_candidate(&conn, &gen2.generation_id, "E_TEST", "boom").unwrap();

        let active: Option<String> = conn
            .query_row(
                "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
                params![&ws_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(active.as_deref(), Some(gen1.generation_id.as_str()));

        let g2 = load_generation(&conn, &gen2.generation_id)
            .unwrap()
            .unwrap();
        assert_eq!(g2.state, GenerationState::Failed);
    }

    #[test]
    fn activate_rejects_non_candidate_state() {
        let (_dir, conn, ws_id) = fixture();
        let gen = begin_candidate(&conn, &ws_id, TriggerKind::Baseline, "p1", "1.0.0").unwrap();
        fail_candidate(&conn, &gen.generation_id, "E", "boom").unwrap();
        let err = activate_candidate(&conn, &ws_id, &gen.generation_id, None).unwrap_err();
        assert!(matches!(err, AtlasError::GenerationStateInvalid { .. }));
    }

    #[test]
    fn sequence_increments_per_committed_generation() {
        let (_dir, conn, ws_id) = fixture();
        let g1 = begin_candidate(&conn, &ws_id, TriggerKind::Baseline, "p1", "1.0.0").unwrap();
        activate_candidate(&conn, &ws_id, &g1.generation_id, None).unwrap();
        let g2 = begin_candidate(&conn, &ws_id, TriggerKind::Reconcile, "p1", "1.0.0").unwrap();
        assert_eq!(g2.sequence_no, 2);
        assert_eq!(
            g2.parent_generation_id.as_deref(),
            Some(g1.generation_id.as_str())
        );
    }

    #[test]
    fn abandon_marks_in_progress_candidate() {
        let (_dir, conn, ws_id) = fixture();
        let gen = begin_candidate(&conn, &ws_id, TriggerKind::Watcher, "p1", "1.0.0").unwrap();
        abandon_candidate(&conn, &gen.generation_id).unwrap();
        let loaded = load_generation(&conn, &gen.generation_id).unwrap().unwrap();
        assert_eq!(loaded.state, GenerationState::Abandoned);
    }
}
