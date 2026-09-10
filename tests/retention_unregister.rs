use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use tempfile::TempDir;
use workspace_atlas::catalogue::{
    apply_unregister, unregister_preview, UnregisterFault, UnregisterPathKind, UnregisterTarget,
    IRREVERSIBLE_CONFIRMATION,
};
use workspace_atlas::config::Config;
use workspace_atlas::task_session::{
    apply_privacy_compaction, privacy_compaction_preview, PrivacyCompactionAction,
    PrivacyCompactionFault, PRIVACY_DELETION_CONFIRMATION,
};
use workspace_atlas::workspace::{persist_registered_config, register_workspace, WorkspaceRecord};

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    catalogue: PathBuf,
    locator: PathBuf,
    config: Config,
    conn: Connection,
    workspace: WorkspaceRecord,
}

fn new_fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("source.rs"), "pub fn truth() {}\n").unwrap();
    let root = fs::canonicalize(root).unwrap();
    let catalogue = temp.path().join("atlas").join("catalogue.sqlite");
    let config = Config::parse(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"Retention Fixture\"\n",
    )
    .unwrap();
    let conn = workspace_atlas::catalogue::init_catalogue(&catalogue, &config).unwrap();
    let workspace = register_workspace(&conn, &root, &config, &catalogue, "test-policy").unwrap();
    persist_registered_config(&catalogue, &workspace, &config).unwrap();
    conn.execute(
        "INSERT INTO index_generation(
            generation_id, workspace_id, sequence_no, state, trigger_kind,
            source_tree_hash, provider_set_hash, schema_version, created_at, committed_at
         ) VALUES ('gen_1', ?1, 1, 'committed', 'baseline', ?2, ?3, '1.0.0', ?4, ?4)",
        params![
            workspace.workspace_id,
            "11".repeat(32),
            "22".repeat(32),
            "2025-01-01T00:00:00.000Z"
        ],
    )
    .unwrap();
    conn.execute(
        "UPDATE workspace SET active_generation_id = 'gen_1' WHERE workspace_id = ?1",
        params![workspace.workspace_id],
    )
    .unwrap();
    let workspace = workspace_atlas::workspace::load_workspace(&conn, &workspace.workspace_id)
        .unwrap()
        .unwrap();
    let application_catalogue_root = temp.path().join("atlas");
    let locator =
        workspace_atlas::paths::catalogue_locator_path(&application_catalogue_root, &root).unwrap();
    fs::create_dir_all(locator.parent().unwrap()).unwrap();
    fs::write(
        &locator,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "canonical_root": workspace.canonical_root,
            "workspace_id": workspace.workspace_id,
            "catalogue_path": catalogue,
        }))
        .unwrap(),
    )
    .unwrap();
    Fixture {
        _temp: temp,
        root,
        catalogue,
        locator,
        config,
        conn,
        workspace,
    }
}
fn seed_truth_and_history(conn: &Connection, workspace_id: &str) {
    let timestamp = "2025-01-02T00:00:00.000Z";
    conn.execute(
        "INSERT INTO index_generation(
            generation_id, workspace_id, parent_generation_id, sequence_no, state,
            trigger_kind, source_tree_hash, provider_set_hash, schema_version,
            created_at, committed_at
         ) VALUES ('gen_2', ?1, 'gen_1', 2, 'committed', 'reconcile', ?2, ?3,
            '1.0.0', ?4, ?4)",
        params![workspace_id, "77".repeat(32), "88".repeat(32), timestamp],
    )
    .unwrap();
    conn.execute(
        "UPDATE workspace SET active_generation_id = 'gen_2' WHERE workspace_id = ?1",
        params![workspace_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO file_identity(
            file_id, workspace_id, identity_basis, first_seen_at, last_seen_at,
            lifecycle_state, last_known_path
         ) VALUES ('file_1', ?1, 'created', ?2, ?2, 'current', 'source.rs')",
        params![workspace_id, timestamp],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO file_revision(
            revision_id, file_id, content_hash, byte_size, artifact_class, language,
            encoding, newline_style, is_generated, is_test, discovered_at
         ) VALUES ('rev_1', 'file_1', ?1, 18, 'source', 'rust', 'utf-8', 'lf', 0, 0, ?2)",
        params!["99".repeat(32), timestamp],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO generation_file(
            generation_id, file_id, revision_id, canonical_path, presence_state,
            observed_size
         ) VALUES ('gen_2', 'file_1', 'rev_1', 'source.rs', 'present', 18)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO extractor_run(
            extractor_run_id, revision_id, provider_name, provider_version,
            provider_tier, configuration_hash, normalized_schema_version,
            deterministic, status, started_at, finished_at, bytes_total,
            bytes_processed, capabilities_json, limitations_json
         ) VALUES ('run_1', 'rev_1', 'fixture', '1', 'structural', ?1, '1',
            1, 'complete', ?2, ?2, 18, 18, '[]', '[]')",
        params!["aa".repeat(32), timestamp],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO symbol_fact(
            symbol_fact_id, extractor_run_id, revision_id, canonical_symbol_key,
            symbol_kind, display_name, qualified_name, start_byte, end_byte,
            start_line, start_column, end_line, end_column, evidence_method,
            confidence, evidence_reason
         ) VALUES ('sym_1', 'run_1', 'rev_1', 'truth', 'function', 'truth',
            'truth', 0, 10, 1, 1, 1, 11, 'structural', 1.0, 'fixture')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO provider_descriptor(
            provider_key, provider_name, provider_version, provider_tier,
            execution_kind, execution_scope, protocol_version, output_format,
            deterministic, descriptor_hash, registered_at
         ) VALUES ('provider_1', 'fixture', '1', 'structural', 'builtin', 'file',
            '1', 'builtin', 1, ?1, ?2)",
        params!["bb".repeat(32), timestamp],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO workspace_provider(
            workspace_id, provider_key, enabled, required, priority,
            configuration_json, configuration_hash, timeout_ms, max_stdout_bytes,
            max_stderr_bytes, max_output_bytes, network_isolation_policy, updated_at
         ) VALUES (?1, 'provider_1', 1, 0, 1, '{}', ?2, 1000, 1024, 1024,
            1024, 'not_required', ?3)",
        params![workspace_id, "cc".repeat(32), timestamp],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO generation_delta(
            delta_id, workspace_id, from_generation_id, to_generation_id,
            delta_policy_version, state, canonical_json, delta_hash, created_at,
            finished_at
         ) VALUES ('delta_1', ?1, 'gen_1', 'gen_2', '1', 'ready', '{}', ?2, ?3, ?3)",
        params![workspace_id, "dd".repeat(32), timestamp],
    )
    .unwrap();
}

fn truth_snapshot(conn: &Connection, workspace_id: &str) -> (String, Vec<i64>) {
    let active = conn
        .query_row(
            "SELECT active_generation_id FROM workspace WHERE workspace_id = ?1",
            params![workspace_id],
            |row| row.get(0),
        )
        .unwrap();
    let counts = [
        "index_generation",
        "file_identity",
        "file_revision",
        "generation_file",
        "extractor_run",
        "symbol_fact",
        "provider_descriptor",
        "workspace_provider",
        "generation_delta",
    ]
    .into_iter()
    .map(|table| {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    })
    .collect();
    (active, counts)
}

fn insert_session(conn: &Connection, workspace_id: &str, id: &str, completed_at: &str) {
    conn.execute(
        "INSERT INTO task_session(
            task_session_id, workspace_id, start_generation_id, task_hash,
            normalized_goal_hash, raw_task_retention, raw_task, task_kind,
            task_kind_source, planner_policy_version, context_ir_version, state,
            accepted, tests_passed, outcome_code, created_at, completed_at
         ) VALUES (?1, ?2, 'gen_1', ?3, ?4, 'none', NULL, 'bug_fix', 'declared',
            'planner-v1.0.0', '1.0.0', 'completed', 1, 1, 'accepted', ?5, ?5)",
        params![
            id,
            workspace_id,
            "33".repeat(32),
            "44".repeat(32),
            completed_at
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO context_ir(
            context_id, task_session_id, workspace_id, generation_id, context_ir_version,
            planner_policy_version, projection_policy_version, budget_profile, request_hash,
            working_set_status, serving_fallback, max_records, selected_records,
            max_source_bytes, selected_source_bytes, max_estimated_tokens,
            selected_estimated_tokens, soft_latency_ms, hard_latency_ms, elapsed_ms,
            coverage_json, omissions_json, uncertainty_json, validation_plan_json,
            canonical_json, context_hash, created_at
         ) VALUES (?1, ?2, ?3, 'gen_1', '1.0.0', 'planner-v1.0.0', 'projection-v1',
            'default', ?4, 'complete', 0, 1, 1, 0, 0, 1, 1, 0, 1, 0,
            '{}', '[]', '[]', '[]', '{}', ?5, ?6)",
        params![
            format!("ctx_{id}"),
            id,
            workspace_id,
            "55".repeat(32),
            "66".repeat(32),
            completed_at
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO context_use_event(
            event_id, task_session_id, context_id, event_type, observation_source,
            details_json, occurred_at
         ) VALUES (?1, ?2, ?3, 'context_supplied', 'atlas_observed', '{}', ?4)",
        params![format!("evt_{id}"), id, format!("ctx_{id}"), completed_at],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO query_stage_metric(
            metric_id, workspace_id, generation_id, task_session_id, context_id, request_id,
            operation, serving_fallback, truncated, created_at
         ) VALUES (?1, ?2, 'gen_1', ?3, ?4, ?5, 'find', 0, 0, ?6)",
        params![
            format!("metric_{id}"),
            workspace_id,
            id,
            format!("ctx_{id}"),
            format!("req_{id}"),
            completed_at
        ],
    )
    .unwrap();
}

fn as_of() -> DateTime<Utc> {
    "2026-09-04T00:00:00Z".parse().unwrap()
}

#[test]
fn compact_preview_pages_share_the_complete_canonical_manifest_digest() {
    let fixture = new_fixture();
    for (id, completed) in [
        ("old_a", "2026-01-01T00:00:00Z"),
        ("old_b", "2026-01-02T00:00:00Z"),
        ("recent", "2026-09-01T00:00:00Z"),
    ] {
        insert_session(
            &fixture.conn,
            &fixture.workspace.workspace_id,
            id,
            completed,
        );
    }

    let first = privacy_compaction_preview(
        &fixture.conn,
        &fixture.workspace.workspace_id,
        as_of(),
        1,
        None,
    )
    .unwrap();
    let second = privacy_compaction_preview(
        &fixture.conn,
        &fixture.workspace.workspace_id,
        as_of(),
        200,
        first.next_cursor,
    )
    .unwrap();

    assert_eq!(first.manifest_digest, second.manifest_digest);
    assert_eq!(first.total_entries, second.total_entries);
    assert_eq!(first.entries.len(), 1);
    assert!(second
        .entries
        .iter()
        .all(|entry| entry.record_id != "recent"));
}

#[test]
fn compact_manifest_is_bound_to_the_canonical_catalogue_identity() {
    let catalogue_a = new_fixture();
    seed_truth_and_history(&catalogue_a.conn, &catalogue_a.workspace.workspace_id);
    insert_session(
        &catalogue_a.conn,
        &catalogue_a.workspace.workspace_id,
        "old",
        "2026-01-01T00:00:00Z",
    );
    catalogue_a
        .conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();

    let catalogue_b_directory = catalogue_a._temp.path().join("atlas-b");
    fs::create_dir_all(&catalogue_b_directory).unwrap();
    let catalogue_b_path = catalogue_b_directory.join("catalogue.sqlite");
    fs::copy(&catalogue_a.catalogue, &catalogue_b_path).unwrap();
    assert_eq!(
        fs::read(&catalogue_a.catalogue).unwrap(),
        fs::read(&catalogue_b_path).unwrap()
    );
    assert_ne!(
        fs::canonicalize(&catalogue_a.catalogue).unwrap(),
        fs::canonicalize(&catalogue_b_path).unwrap()
    );

    let preview_a = privacy_compaction_preview(
        &catalogue_a.conn,
        &catalogue_a.workspace.workspace_id,
        as_of(),
        50,
        None,
    )
    .unwrap();
    let mut catalogue_b =
        workspace_atlas::catalogue::open_connection(&catalogue_b_path, &catalogue_a.config)
            .unwrap();
    let preview_b = privacy_compaction_preview(
        &catalogue_b,
        &catalogue_a.workspace.workspace_id,
        as_of(),
        50,
        None,
    )
    .unwrap();
    assert_eq!(preview_a.entries, preview_b.entries);
    assert_ne!(preview_a.manifest_digest, preview_b.manifest_digest);

    let logical_state = |conn: &Connection| {
        [
            "task_session",
            "context_ir",
            "context_use_event",
            "query_stage_metric",
        ]
        .map(|table| {
            conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
        })
    };
    let physical_state = |conn: &Connection| {
        (
            conn.query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            conn.query_row("PRAGMA freelist_count", [], |row| row.get::<_, i64>(0))
                .unwrap(),
        )
    };
    let directory_inventory = || {
        let mut inventory = fs::read_dir(&catalogue_b_directory)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name(),
                    entry.metadata().unwrap().file_type().is_file(),
                    entry.metadata().unwrap().len(),
                )
            })
            .collect::<Vec<_>>();
        inventory.sort();
        inventory
    };
    let logical_before = logical_state(&catalogue_b);
    let truth_before = truth_snapshot(&catalogue_b, &catalogue_a.workspace.workspace_id);
    let physical_before = physical_state(&catalogue_b);
    let inventory_before = directory_inventory();

    let error = apply_privacy_compaction(
        &mut catalogue_b,
        &catalogue_a.workspace.workspace_id,
        as_of(),
        &preview_a.manifest_digest,
        PRIVACY_DELETION_CONFIRMATION,
        PrivacyCompactionFault::None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("manifest mismatch"));
    assert_eq!(logical_state(&catalogue_b), logical_before);
    assert_eq!(
        truth_snapshot(&catalogue_b, &catalogue_a.workspace.workspace_id),
        truth_before
    );
    assert_eq!(physical_state(&catalogue_b), physical_before);
    assert_eq!(directory_inventory(), inventory_before);
}
#[test]
fn compact_manifest_applies_the_thousand_session_and_hundred_delete_bounds() {
    let fixture = new_fixture();
    for ordinal in 0..1_101 {
        insert_session(
            &fixture.conn,
            &fixture.workspace.workspace_id,
            &format!("bounded_{ordinal:04}"),
            "2026-08-20T00:00:00Z",
        );
    }
    let preview = privacy_compaction_preview(
        &fixture.conn,
        &fixture.workspace.workspace_id,
        as_of(),
        200,
        None,
    )
    .unwrap();
    assert_eq!(preview.total_entries, 100);
    assert_eq!(preview.entries.len(), 100);
    assert!(preview
        .entries
        .iter()
        .all(|entry| entry.action == PrivacyCompactionAction::DeleteDerived));
}

#[test]
fn compact_apply_rolls_back_faults_and_preserves_truth_and_generation_history() {
    let mut fixture = new_fixture();
    seed_truth_and_history(&fixture.conn, &fixture.workspace.workspace_id);
    insert_session(
        &fixture.conn,
        &fixture.workspace.workspace_id,
        "old",
        "2026-01-01T00:00:00Z",
    );
    let preview = privacy_compaction_preview(
        &fixture.conn,
        &fixture.workspace.workspace_id,
        as_of(),
        50,
        None,
    )
    .unwrap();
    let truth_before = truth_snapshot(&fixture.conn, &fixture.workspace.workspace_id);

    let error = apply_privacy_compaction(
        &mut fixture.conn,
        &fixture.workspace.workspace_id,
        as_of(),
        &preview.manifest_digest,
        PRIVACY_DELETION_CONFIRMATION,
        PrivacyCompactionFault::BeforeCommit,
    )
    .unwrap_err();
    assert!(error.to_string().contains("injected"));
    assert_eq!(
        fixture
            .conn
            .query_row("SELECT COUNT(*) FROM context_ir", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );

    let result = apply_privacy_compaction(
        &mut fixture.conn,
        &fixture.workspace.workspace_id,
        as_of(),
        &preview.manifest_digest,
        PRIVACY_DELETION_CONFIRMATION,
        PrivacyCompactionFault::None,
    )
    .unwrap();
    assert!(result.deleted_contexts > 0);
    assert_eq!(
        truth_snapshot(&fixture.conn, &fixture.workspace.workspace_id),
        truth_before
    );
}

#[test]
fn compact_rejects_wrong_digest_and_literal_confirmation_without_mutation() {
    let mut fixture = new_fixture();
    insert_session(
        &fixture.conn,
        &fixture.workspace.workspace_id,
        "old",
        "2026-01-01T00:00:00Z",
    );
    let preview = privacy_compaction_preview(
        &fixture.conn,
        &fixture.workspace.workspace_id,
        as_of(),
        50,
        None,
    )
    .unwrap();
    for (digest, confirmation) in [
        ("00", PRIVACY_DELETION_CONFIRMATION),
        (preview.manifest_digest.as_str(), "yes"),
    ] {
        assert!(apply_privacy_compaction(
            &mut fixture.conn,
            &fixture.workspace.workspace_id,
            as_of(),
            digest,
            confirmation,
            PrivacyCompactionFault::None,
        )
        .is_err());
        assert_eq!(
            fixture
                .conn
                .query_row("SELECT COUNT(*) FROM context_ir", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn compaction_manifest_distinguishes_derived_and_expired_summary_actions() {
    let mut fixture = new_fixture();
    insert_session(
        &fixture.conn,
        &fixture.workspace.workspace_id,
        "structured",
        "2026-08-01T00:00:00Z",
    );
    insert_session(
        &fixture.conn,
        &fixture.workspace.workspace_id,
        "summary",
        "2026-01-01T00:00:00Z",
    );
    fixture
        .conn
        .execute(
            "INSERT INTO query_stage_metric(
                metric_id, workspace_id, generation_id, task_session_id, context_id,
                request_id, operation, serving_fallback, truncated, created_at
             ) VALUES ('orphan_metric', ?1, 'gen_1', NULL, NULL, 'orphan_request',
                'find', 0, 0, '2026-01-03T00:00:00Z')",
            params![fixture.workspace.workspace_id],
        )
        .unwrap();
    let preview = privacy_compaction_preview(
        &fixture.conn,
        &fixture.workspace.workspace_id,
        as_of(),
        50,
        None,
    )
    .unwrap();
    assert!(preview
        .entries
        .iter()
        .any(|entry| entry.record_id == "structured"
            && entry.action == PrivacyCompactionAction::DeleteDerived));
    assert!(preview
        .entries
        .iter()
        .any(|entry| entry.record_id == "summary"
            && entry.action == PrivacyCompactionAction::DeleteSession));
    assert!(preview
        .entries
        .iter()
        .any(|entry| entry.record_id == "orphan_metric"
            && entry.action == PrivacyCompactionAction::DeleteMetric));
    apply_privacy_compaction(
        &mut fixture.conn,
        &fixture.workspace.workspace_id,
        as_of(),
        &preview.manifest_digest,
        PRIVACY_DELETION_CONFIRMATION,
        PrivacyCompactionFault::None,
    )
    .unwrap();
    let next = privacy_compaction_preview(
        &fixture.conn,
        &fixture.workspace.workspace_id,
        as_of(),
        50,
        None,
    )
    .unwrap();
    assert!(next.entries.is_empty());
}

#[test]
fn compact_apply_excludes_an_active_sqlite_writer() {
    let mut fixture = new_fixture();
    insert_session(
        &fixture.conn,
        &fixture.workspace.workspace_id,
        "old",
        "2026-01-01T00:00:00Z",
    );
    let preview = privacy_compaction_preview(
        &fixture.conn,
        &fixture.workspace.workspace_id,
        as_of(),
        50,
        None,
    )
    .unwrap();
    let writer =
        workspace_atlas::catalogue::open_connection(&fixture.catalogue, &fixture.config).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let error = apply_privacy_compaction(
        &mut fixture.conn,
        &fixture.workspace.workspace_id,
        as_of(),
        &preview.manifest_digest,
        PRIVACY_DELETION_CONFIRMATION,
        PrivacyCompactionFault::None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("locked"));
    writer.execute_batch("ROLLBACK").unwrap();
    assert_eq!(
        fixture
            .conn
            .query_row("SELECT COUNT(*) FROM context_ir", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

fn unregister_target(fixture: &Fixture) -> UnregisterTarget {
    UnregisterTarget::new(
        fixture.root.clone(),
        fixture.catalogue.clone(),
        fixture
            .locator
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf(),
        fixture._temp.path().join("provider-temp"),
    )
    .unwrap()
}

#[test]
fn unregister_preview_digest_is_complete_and_discloses_catalogue_loss() {
    let fixture = new_fixture();
    let target = unregister_target(&fixture);
    let first = unregister_preview(&fixture.conn, &fixture.workspace, &target, 1, None).unwrap();
    let second = unregister_preview(
        &fixture.conn,
        &fixture.workspace,
        &target,
        200,
        first.next_cursor,
    )
    .unwrap();
    assert_eq!(first.manifest_digest, second.manifest_digest);
    assert_eq!(first.total_entries, second.total_entries);
    assert!(first.disclosure.contains("complete Atlas catalogue"));
    assert!(first.disclosure.contains("Truth"));
    assert!(first.disclosure.contains("generation history"));
}

#[test]
fn unregister_manifest_excludes_caller_backups_and_workspace_source() {
    let fixture = new_fixture();
    let backup = fixture.catalogue.with_extension("caller-backup.sqlite");
    fs::write(&backup, "caller owned").unwrap();
    let source = fixture.root.join("source.rs");
    let page = unregister_preview(
        &fixture.conn,
        &fixture.workspace,
        &unregister_target(&fixture),
        200,
        None,
    )
    .unwrap();
    assert!(!page.entries.iter().any(|entry| entry.path == backup));
    assert!(!page.entries.iter().any(|entry| entry.path == source));
}
#[test]
fn unregister_manifest_includes_application_private_provider_temporary_state() {
    let fixture = new_fixture();
    fixture
        .conn
        .execute(
            "INSERT INTO provider_descriptor(
                provider_key, provider_name, provider_version, provider_tier,
                execution_kind, execution_scope, protocol_version, output_format,
                deterministic, descriptor_hash, registered_at
             ) VALUES ('provider_temp', 'fixture', '1', 'structural', 'builtin',
                'file', '1', 'builtin', 1, ?1, ?2)",
            params!["ee".repeat(32), "2026-01-01T00:00:00.000Z"],
        )
        .unwrap();
    fixture
        .conn
        .execute(
            "INSERT INTO provider_execution(
                provider_execution_id, workspace_id, generation_id, provider_key,
                scope_kind, scope_key, input_fingerprint, provider_set_hash,
                normalized_schema_version, mapping_policy_version, status,
                network_isolation_state, created_at
             ) VALUES ('pex_temp', ?1, 'gen_1', 'provider_temp', 'file', 'source.rs',
                ?2, ?3, '1', '1', 'complete', 'not_applicable', ?4)",
            params![
                fixture.workspace.workspace_id,
                "ff".repeat(32),
                "10".repeat(32),
                "2026-01-01T00:00:00.000Z"
            ],
        )
        .unwrap();
    let provider_temp = fixture._temp.path().join("provider-temp").join("pex_temp");
    fs::create_dir_all(&provider_temp).unwrap();
    fs::write(
        provider_temp.join("index.scip"),
        b"temporary provider output",
    )
    .unwrap();
    let provider_temp = fs::canonicalize(provider_temp).unwrap();

    let page = unregister_preview(
        &fixture.conn,
        &fixture.workspace,
        &unregister_target(&fixture),
        200,
        None,
    )
    .unwrap();
    assert!(page.entries.iter().any(|entry| {
        entry.kind == UnregisterPathKind::ProviderTemp && entry.path == provider_temp
    }));
}

#[test]
fn unregister_rejects_replacements_and_never_partially_unregisters_on_faults() {
    let fixture = new_fixture();
    let target = unregister_target(&fixture);
    let preview =
        unregister_preview(&fixture.conn, &fixture.workspace, &target, 200, None).unwrap();
    let original = fs::read(&fixture.locator).unwrap();
    fs::write(&fixture.locator, b"replacement").unwrap();
    assert!(apply_unregister(
        fixture.conn,
        &fixture.workspace,
        target,
        &preview.manifest_digest,
        IRREVERSIBLE_CONFIRMATION,
        UnregisterFault::BeforeQuarantine,
    )
    .is_err());
    assert!(fixture.catalogue.exists());
    assert!(fixture.locator.exists());
    assert!(fixture.root.join("source.rs").exists());
    assert_ne!(fs::read(&fixture.locator).unwrap(), original);
}

#[test]
fn unregister_rejects_non_regular_locator_replacement() {
    let fixture = new_fixture();
    fs::remove_file(&fixture.locator).unwrap();
    fs::create_dir(&fixture.locator).unwrap();
    let error = unregister_preview(
        &fixture.conn,
        &fixture.workspace,
        &unregister_target(&fixture),
        200,
        None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("not a regular file"));
    assert!(fixture.catalogue.exists());
    assert!(fixture.root.join("source.rs").exists());
}

#[test]
fn unregister_excludes_active_writer_and_external_lock_contention() {
    let fixture = new_fixture();
    let target = unregister_target(&fixture);
    fixture
        .conn
        .execute(
            "INSERT INTO writer_lease(
                workspace_id, holder_id, acquired_at, expires_at, heartbeat_at,
                schema_writer_version
             ) VALUES (?1, 'other', ?2, ?2, ?2, 4)",
            params![fixture.workspace.workspace_id, "2026-09-04T00:00:00.000Z"],
        )
        .unwrap();
    let preview =
        unregister_preview(&fixture.conn, &fixture.workspace, &target, 200, None).unwrap();
    let error = apply_unregister(
        fixture.conn,
        &fixture.workspace,
        target,
        &preview.manifest_digest,
        IRREVERSIBLE_CONFIRMATION,
        UnregisterFault::None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("writer is active"));
    assert!(fixture.catalogue.exists());
    assert!(fixture.locator.exists());
    assert!(fixture.root.join("source.rs").exists());

    let fixture = new_fixture();
    let target = unregister_target(&fixture);
    let preview =
        unregister_preview(&fixture.conn, &fixture.workspace, &target, 200, None).unwrap();
    let mut lock_name = fixture.catalogue.as_os_str().to_os_string();
    lock_name.push(".unregister.lock");
    let lock_path = PathBuf::from(lock_name);
    fs::write(&lock_path, "held").unwrap();
    let error = apply_unregister(
        fixture.conn,
        &fixture.workspace,
        target,
        &preview.manifest_digest,
        IRREVERSIBLE_CONFIRMATION,
        UnregisterFault::None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("exclusion lock"));
    assert!(fixture.catalogue.exists());
    assert!(fixture.locator.exists());
    assert!(fixture.root.join("source.rs").exists());
}

#[test]
fn unregister_manifest_change_and_removal_fault_leave_registration_whole() {
    let fixture = new_fixture();
    let target = unregister_target(&fixture);
    let preview =
        unregister_preview(&fixture.conn, &fixture.workspace, &target, 200, None).unwrap();
    let config_path =
        workspace_atlas::workspace::registered_config_path(&fixture.catalogue).unwrap();
    fs::write(&config_path, b"changed").unwrap();
    let error = apply_unregister(
        fixture.conn,
        &fixture.workspace,
        target,
        &preview.manifest_digest,
        IRREVERSIBLE_CONFIRMATION,
        UnregisterFault::None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("manifest mismatch"));
    assert!(fixture.catalogue.exists());
    assert!(fixture.locator.exists());
    assert!(fixture.root.join("source.rs").exists());
    let fixture = new_fixture();
    let target = unregister_target(&fixture);
    let preview =
        unregister_preview(&fixture.conn, &fixture.workspace, &target, 200, None).unwrap();
    let error = apply_unregister(
        fixture.conn,
        &fixture.workspace,
        target,
        &preview.manifest_digest,
        IRREVERSIBLE_CONFIRMATION,
        UnregisterFault::DuringQuarantine,
    )
    .unwrap_err();
    assert!(error.to_string().contains("during quarantine"));
    assert!(fixture.catalogue.exists());
    assert!(fixture.locator.exists());
    assert!(fixture.root.join("source.rs").exists());

    let fixture = new_fixture();
    let target = unregister_target(&fixture);
    let preview =
        unregister_preview(&fixture.conn, &fixture.workspace, &target, 200, None).unwrap();
    let error = apply_unregister(
        fixture.conn,
        &fixture.workspace,
        target,
        &preview.manifest_digest,
        IRREVERSIBLE_CONFIRMATION,
        UnregisterFault::DuringRemoval,
    )
    .unwrap_err();
    assert!(error.to_string().contains("unregister_unavailable"));
    assert!(fixture.catalogue.exists());
    assert!(fixture.locator.exists());
    assert!(fixture.root.join("source.rs").exists());
}

#[cfg(unix)]
#[test]
fn unregister_rejects_symlink_catalogue_replacements() {
    use std::os::unix::fs::symlink;
    let fixture = new_fixture();
    let target = unregister_target(&fixture);
    drop(fixture.conn);
    let replacement = fixture.catalogue.with_extension("replacement");
    fs::rename(&fixture.catalogue, &replacement).unwrap();
    symlink(&replacement, &fixture.catalogue).unwrap();
    assert!(unregister_preview(
        &workspace_atlas::catalogue::init_catalogue(&replacement, &fixture.config).unwrap(),
        &fixture.workspace,
        &target,
        200,
        None,
    )
    .is_err());
    assert!(fixture.root.join("source.rs").exists());
}

#[cfg(windows)]
#[test]
fn unregister_stays_unavailable_when_windows_atomic_removal_cannot_be_proven() {
    let fixture = new_fixture();
    let target = unregister_target(&fixture);
    let preview =
        unregister_preview(&fixture.conn, &fixture.workspace, &target, 200, None).unwrap();
    let error = apply_unregister(
        fixture.conn,
        &fixture.workspace,
        target,
        &preview.manifest_digest,
        IRREVERSIBLE_CONFIRMATION,
        UnregisterFault::None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("unregister_unavailable"));
    assert!(fixture.catalogue.exists());
    let renamed = fixture.catalogue.with_extension("closed-proof");
    fs::rename(&fixture.catalogue, &renamed).unwrap();
    fs::rename(&renamed, &fixture.catalogue).unwrap();
    assert!(fixture.locator.exists());
    assert!(fixture.root.join("source.rs").exists());
}
