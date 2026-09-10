//! Exact-evidence file lifecycle tracking.
//!
//! Two responsibilities, both evaluated against the generation that is
//! about to be superseded (still visible pre-activation via
//! `workspace.active_generation_id`):
//!
//! - **Rename detection**: correlate paths that disappeared from disk this
//!   cycle with paths that newly appeared, by exact content hash only.
//!   Ambiguous matches (more than one candidate on either side) are left as
//!   a deletion plus a creation rather than guessed (INV-024's "no hidden
//!   inference" — a rename is only ever asserted from exact-hash evidence).
//! - **Deletion tombstones**: for every previously-present path that is
//!   neither walked this cycle nor consumed by a rename, mark its identity
//!   `deleted`, record a `lifecycle_event`, and write a metadata-only
//!   `tombstone`. Atlas never stores raw file bytes, so tombstones always use
//!   `raw_snapshot_state = 'not_retained'`, regardless of content.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, OptionalExtension, Transaction};

use crate::error::Result;
use crate::migrations;

/// A previously-present file's identity + last-known content, as of the
/// generation that is about to be superseded.
#[derive(Debug, Clone)]
pub struct PrevFile {
    pub file_id: String,
    pub content_hash: String,
}

/// Snapshot every file present in the workspace's currently-active
/// (about-to-be-superseded) generation. Empty for a workspace's first
/// reconcile cycle (no active generation yet).
pub fn snapshot_previous_present(
    tx: &Transaction<'_>,
    workspace_id: &str,
) -> Result<HashMap<String, PrevFile>> {
    let mut stmt = tx.prepare(
        "SELECT gf.canonical_path, gf.file_id, fr.content_hash
         FROM workspace w
         JOIN generation_file gf ON gf.generation_id = w.active_generation_id
         JOIN file_revision fr ON fr.revision_id = gf.revision_id
         WHERE w.workspace_id = ?1 AND gf.presence_state = 'present'",
    )?;
    let rows = stmt.query_map(params![workspace_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            PrevFile {
                file_id: r.get(1)?,
                content_hash: r.get(2)?,
            },
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (path, prev) = row?;
        out.insert(path, prev);
    }
    Ok(out)
}

/// One detected rename: exact-hash match between a path that disappeared
/// and a path that newly appeared this cycle.
#[derive(Debug, Clone)]
pub struct RenamePair {
    pub old_path: String,
    pub new_path: String,
    pub file_id: String,
    pub content_hash: String,
}

/// Detect renames between the previous generation's present set and this
/// cycle's walk. `new_present_hashes` maps every currently-`Present` path to
/// its content hash. Exact-hash only: a match is asserted only when exactly
/// one disappeared path and exactly one appeared path share a hash.
pub fn detect_renames(
    prev_present: &HashMap<String, PrevFile>,
    walked_paths: &HashSet<String>,
    new_present_hashes: &HashMap<String, String>,
) -> Vec<RenamePair> {
    let disappeared: Vec<(&String, &PrevFile)> = prev_present
        .iter()
        .filter(|(path, _)| !walked_paths.contains(*path))
        .collect();
    let appeared: Vec<(&String, &String)> = new_present_hashes
        .iter()
        .filter(|(path, _)| !prev_present.contains_key(*path))
        .collect();

    let mut disappeared_by_hash: HashMap<&str, Vec<(&String, &PrevFile)>> = HashMap::new();
    for &(path, pf) in &disappeared {
        disappeared_by_hash
            .entry(pf.content_hash.as_str())
            .or_default()
            .push((path, pf));
    }
    let mut appeared_by_hash: HashMap<&str, Vec<&String>> = HashMap::new();
    for &(path, hash) in &appeared {
        appeared_by_hash
            .entry(hash.as_str())
            .or_default()
            .push(path);
    }

    let mut out = Vec::new();
    for (hash, olds) in &disappeared_by_hash {
        if olds.len() != 1 {
            continue; // Ambiguous on the old side -- leave as deletions.
        }
        let Some(news) = appeared_by_hash.get(hash) else {
            continue;
        };
        if news.len() != 1 {
            continue; // Ambiguous on the new side -- leave as creations.
        }
        let (old_path, prev) = olds[0];
        let new_path = news[0];
        out.push(RenamePair {
            old_path: old_path.clone(),
            new_path: new_path.clone(),
            file_id: prev.file_id.clone(),
            content_hash: prev.content_hash.clone(),
        });
    }
    out
}

/// Record a detected rename: move the identity's `last_known_path` to the
/// new path (so the caller's normal present-file upsert finds the existing
/// identity by its new path) and insert a `renamed` lifecycle event.
pub fn record_rename(
    tx: &Transaction<'_>,
    workspace_id: &str,
    generation_id: &str,
    r: &RenamePair,
) -> Result<()> {
    let now = migrations::iso8601_now();
    tx.execute(
        "UPDATE file_identity SET last_known_path = ?2, last_seen_at = ?3 WHERE file_id = ?1",
        params![r.file_id, r.new_path, now],
    )?;
    let event_id = crate::ids::file_id_from_path(
        workspace_id,
        &format!("rename@{}@{}@{}", generation_id, r.old_path, r.new_path),
    );
    tx.execute(
        "INSERT INTO lifecycle_event (event_id, workspace_id, generation_id, event_type,
            occurred_at, actor, file_id, old_path, new_path, old_content_hash,
            new_content_hash, evidence_method, confidence, details_json)
         VALUES (?1, ?2, ?3, 'renamed', ?4, 'atlas', ?5, ?6, ?7, ?8, ?8, 'exact_content_hash', 1.0, '{}')",
        params![
            event_id,
            workspace_id,
            generation_id,
            now,
            r.file_id,
            r.old_path,
            r.new_path,
            r.content_hash
        ],
    )?;
    Ok(())
}

/// Record deletions: every previously-present path that was neither walked
/// this cycle nor consumed by a rename is gone from disk. Marks the
/// identity `deleted`, inserts a `deleted` lifecycle event, and writes a
/// metadata-only tombstone (current facts are removed by simply not
/// re-inserting `generation_file` membership for it; the tombstone retains
/// only the last path, hash, artifact class, and symbol/relationship
/// *summaries* -- never raw content).
pub fn record_deletions(
    tx: &Transaction<'_>,
    workspace_id: &str,
    generation_id: &str,
    prev_present: &HashMap<String, PrevFile>,
    walked_paths: &HashSet<String>,
    renamed_old_paths: &HashSet<String>,
) -> Result<i64> {
    let mut count = 0i64;
    for (path, prev) in prev_present {
        if walked_paths.contains(path) || renamed_old_paths.contains(path) {
            continue;
        }
        let now = migrations::iso8601_now();
        tx.execute(
            "UPDATE file_identity SET lifecycle_state = 'deleted', last_seen_at = ?2 WHERE file_id = ?1",
            params![prev.file_id, now],
        )?;
        let event_id = crate::ids::file_id_from_path(
            workspace_id,
            &format!("delete@{}@{}", generation_id, path),
        );
        tx.execute(
            "INSERT INTO lifecycle_event (event_id, workspace_id, generation_id, event_type,
                occurred_at, actor, file_id, old_path, new_path, old_content_hash,
                new_content_hash, evidence_method, confidence, details_json)
             VALUES (?1, ?2, ?3, 'deleted', ?4, 'filesystem', ?5, ?6, NULL, ?7, NULL,
                     'path_absent_from_walk', 1.0, '{}')",
            params![
                event_id,
                workspace_id,
                generation_id,
                now,
                prev.file_id,
                path,
                prev.content_hash
            ],
        )?;

        let (revision_id, artifact_class): (Option<String>, String) = tx
            .query_row(
                "SELECT revision_id, artifact_class FROM file_revision
                 WHERE file_id = ?1 AND content_hash = ?2
                 ORDER BY discovered_at DESC LIMIT 1",
                params![prev.file_id, prev.content_hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((None, "other".to_string()));

        let symbol_summary_json = match &revision_id {
            Some(rid) => {
                let mut stmt = tx.prepare(
                    "SELECT qualified_name, symbol_kind FROM symbol_fact WHERE revision_id = ?1",
                )?;
                let names: Vec<serde_json::Value> = stmt
                    .query_map(params![rid], |r| {
                        Ok(serde_json::json!({
                            "qualified_name": r.get::<_, String>(0)?,
                            "symbol_kind": r.get::<_, String>(1)?,
                        }))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string())
            }
            None => "[]".to_string(),
        };
        let relationship_summary_json = match &revision_id {
            Some(rid) => {
                let mut stmt = tx.prepare(
                    "SELECT relationship_type FROM relationship_fact WHERE revision_id = ?1",
                )?;
                let rels: Vec<serde_json::Value> = stmt
                    .query_map(params![rid], |r| {
                        Ok(serde_json::json!({ "relationship_type": r.get::<_, String>(0)? }))
                    })?
                    .collect::<rusqlite::Result<_>>()?;
                serde_json::to_string(&rels).unwrap_or_else(|_| "[]".to_string())
            }
            None => "[]".to_string(),
        };

        let tombstone_id = crate::ids::file_id_from_path(
            workspace_id,
            &format!("tombstone@{}@{}", generation_id, path),
        );
        tx.execute(
            "INSERT INTO tombstone (tombstone_id, workspace_id, file_id, deletion_event_id,
                last_path, last_revision_id, last_content_hash, artifact_class,
                symbol_summary_json, relationship_summary_json, raw_snapshot_state,
                raw_snapshot_locator, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'not_retained', NULL, ?11)",
            params![
                tombstone_id,
                workspace_id,
                prev.file_id,
                event_id,
                path,
                revision_id,
                prev.content_hash,
                artifact_class,
                symbol_summary_json,
                relationship_summary_json,
                now
            ],
        )?;
        count += 1;
    }
    Ok(count)
}
