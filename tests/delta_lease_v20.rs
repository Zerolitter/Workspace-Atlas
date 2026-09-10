use rusqlite::{params, Connection};
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::context_ir::{
    ChangeKind, IrEvidenceLeaseV2, IrLeaseDependencyKindV2, IrLeaseDependencyV2,
};
use workspace_atlas::discovery;
use workspace_atlas::generation_delta::{
    compute_generation_delta, create_evidence_lease, verify_declared_lease_dependencies,
    verify_evidence_lease, LeaseVerification,
};
use workspace_atlas::hashing::{content_hash_of_bytes, content_hash_of_file};
use workspace_atlas::workspace::{register_workspace, WorkspaceRecord};

const SOURCE: &str = "export function alpha(): number { return 1; }\nexport function beta(): number { return 2; }\nexport function gamma(): number { return 3; }\n";

struct Fixture {
    _database_directory: tempfile::TempDir,
    workspace_directory: tempfile::TempDir,
    connection: Connection,
    workspace: WorkspaceRecord,
    config: Config,
    first_generation_id: String,
}

fn setup() -> Fixture {
    let database_directory = tempfile::tempdir().unwrap();
    let workspace_directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace_directory.path().join("src")).unwrap();
    std::fs::write(workspace_directory.path().join("src/a.ts"), SOURCE).unwrap();
    let config = Config::parse(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"delta-lease-v20\"\n",
    )
    .unwrap();
    let catalogue = database_directory.path().join("atlas.sqlite");
    let connection = init_catalogue(&catalogue, &config).unwrap();
    let workspace = register_workspace(
        &connection,
        workspace_directory.path(),
        &config,
        &catalogue,
        "1.0.0",
    )
    .unwrap();
    let first_generation_id = discovery::reconcile(&workspace, &connection, &config)
        .unwrap()
        .candidate_generation_id;
    Fixture {
        _database_directory: database_directory,
        workspace_directory,
        connection,
        workspace,
        config,
        first_generation_id,
    }
}

fn reconcile_after(fixture: &Fixture, source: &str) -> String {
    std::fs::write(fixture.workspace_directory.path().join("src/a.ts"), source).unwrap();
    discovery::reconcile(&fixture.workspace, &fixture.connection, &fixture.config)
        .unwrap()
        .candidate_generation_id
}

fn generation_revision_and_run(conn: &Connection, generation_id: &str) -> (String, String) {
    conn.query_row(
        "SELECT gf.revision_id, er.extractor_run_id
         FROM generation_file gf
         JOIN extractor_run er ON er.revision_id = gf.revision_id
         WHERE gf.generation_id = ?1 AND gf.canonical_path = 'src/a.ts'
         ORDER BY er.extractor_run_id LIMIT 1",
        params![generation_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}
fn first_symbol_key(conn: &Connection, generation_id: &str) -> String {
    conn.query_row(
        "SELECT sf.canonical_symbol_key
         FROM generation_file gf
         JOIN symbol_fact sf ON sf.revision_id = gf.revision_id
         WHERE gf.generation_id = ?1
         ORDER BY sf.canonical_symbol_key
         LIMIT 1",
        params![generation_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn insert_relationship(
    conn: &Connection,
    generation_id: &str,
    id: &str,
    relationship_type: &str,
    target: &str,
    resolution_status: &str,
    resolver_policy: &str,
) {
    let (revision_id, extractor_run_id) = generation_revision_and_run(conn, generation_id);
    conn.execute(
        "INSERT INTO relationship_fact (
            relationship_fact_id, extractor_run_id, revision_id, relationship_type,
            source_ref_kind, source_ref_value, target_ref_kind, target_ref_value,
            attributes_json, evidence_method, confidence, evidence_reason
         ) VALUES (?1,?2,?3,?4,'symbol','alpha','symbol',?5,'{}','semantic',1.0,'fixture')",
        params![id, extractor_run_id, revision_id, relationship_type, target],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO relationship_resolution (
            relationship_resolution_id, workspace_id, generation_id, relationship_fact_id,
            resolver_policy_version, status, resolved_ref_kind, resolved_ref_value,
            reason_code, candidate_refs_json, confidence, evidence_json, created_at
         ) VALUES (?1,?2,?3,?4,?5,?6,'symbol',?7,'fixture','[]',1.0,'[]',?8)",
        params![
            format!("resolution-{id}"),
            fixture_workspace_id(conn),
            generation_id,
            id,
            resolver_policy,
            resolution_status,
            target,
            "2026-09-03T00:00:00Z"
        ],
    )
    .unwrap();
}

fn fixture_workspace_id(conn: &Connection) -> String {
    conn.query_row("SELECT workspace_id FROM workspace LIMIT 1", [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn insert_effect(conn: &Connection, generation_id: &str, id: &str, phase: &str, attributes: &str) {
    let (revision_id, extractor_run_id) = generation_revision_and_run(conn, generation_id);
    conn.execute(
        "INSERT INTO effect_fact (
            effect_fact_id, extractor_run_id, revision_id, subject_ref_kind,
            subject_ref_value, effect_type, phase, attributes_json,
            evidence_method, confidence, evidence_reason
         ) VALUES (?1,?2,?3,'symbol','alpha','writes_state',?4,?5,'semantic',1.0,'fixture')",
        params![id, extractor_run_id, revision_id, phase, attributes],
    )
    .unwrap();
}

fn insert_coverage(conn: &Connection, generation_id: &str, id: &str, status: &str) {
    conn.execute(
        "INSERT INTO coverage_record (
            coverage_id, generation_id, scope_kind, scope_key, status,
            capabilities_json, limitations_json, details_json
         ) VALUES (?1,?2,'workspace','all',?3,'[\"symbols\"]','[]','{}')",
        params![id, generation_id, status],
    )
    .unwrap();
}
fn insert_file_coverage(
    conn: &Connection,
    generation_id: &str,
    id: &str,
    canonical_path: &str,
    status: &str,
) {
    let file_id: String = conn
        .query_row(
            "SELECT file_id FROM generation_file
             WHERE generation_id = ?1 AND canonical_path = ?2",
            params![generation_id, canonical_path],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO coverage_record (
            coverage_id, generation_id, file_id, scope_kind, scope_key, status,
            capabilities_json, limitations_json, details_json
         ) VALUES (?1,?2,?3,'file',?4,?5,'[\"symbols\"]','[]','{}')",
        params![id, generation_id, file_id, canonical_path, status],
    )
    .unwrap();
}

fn insert_conflict(conn: &Connection, generation_id: &str, id: &str, status: &str) {
    conn.execute(
        "INSERT INTO evidence_conflict (
            evidence_conflict_id, workspace_id, generation_id, subject_kind, subject_key,
            conflict_type, participating_fact_ids_json, preferred_fact_id,
            projection_policy_version, status, explanation, created_at
         ) VALUES (?1,?2,?3,'symbol','alpha','provider_disagreement','[\"fact-a\",\"fact-b\"]',
                   'fact-a','projection-v1',?4,'fixture','2026-09-03T00:00:00Z')",
        params![id, fixture_workspace_id(conn), generation_id, status],
    )
    .unwrap();
}

#[test]
fn line_only_movement_is_not_a_symbol_semantic_change_and_has_explicit_evidence() {
    let fixture = setup();
    insert_coverage(
        &fixture.connection,
        &fixture.first_generation_id,
        "coverage-before",
        "complete",
    );
    let second_generation_id = reconcile_after(&fixture, &format!("// moved down\n{SOURCE}"));
    insert_coverage(
        &fixture.connection,
        &second_generation_id,
        "coverage-after",
        "complete",
    );

    let delta = compute_generation_delta(
        &fixture.connection,
        &fixture.workspace,
        &fixture.first_generation_id,
        &second_generation_id,
    )
    .unwrap();

    assert_eq!(delta.files.changes.len(), 1);
    assert_eq!(delta.files.changes[0].change_kind, ChangeKind::Modified);
    assert!(
        delta.symbols.changes.is_empty(),
        "line movement is not semantic drift"
    );
    assert!(
        !delta.unchanged_verified_contracts.is_empty(),
        "symbol changes: {:?}; uncertainty: {:?}",
        delta.symbols.changes,
        delta.uncertainty
    );
    for unchanged in &delta.unchanged_verified_contracts {
        assert!(unchanged
            .evidence
            .iter()
            .any(|reason| reason == "semantic_identity_unchanged"));
        assert!(unchanged
            .evidence
            .iter()
            .any(|reason| reason == "source_identity_unchanged"));
        assert!(unchanged
            .evidence
            .iter()
            .any(|reason| reason == "relationship_set_unchanged"));
        assert!(unchanged
            .evidence
            .iter()
            .any(|reason| reason == "coverage_complete"));
    }
}

#[test]
fn semantic_delta_classifies_retarget_state_effect_provider_coverage_and_conflict_drift() {
    let fixture = setup();
    let second_generation_id = reconcile_after(&fixture, &format!("// generation two\n{SOURCE}"));

    insert_relationship(
        &fixture.connection,
        &fixture.first_generation_id,
        "rel-call-before",
        "calls",
        "beta",
        "resolved_symbol",
        "resolver-v1",
    );
    insert_relationship(
        &fixture.connection,
        &second_generation_id,
        "rel-call-after",
        "calls",
        "gamma",
        "resolved_symbol",
        "resolver-v1",
    );
    insert_relationship(
        &fixture.connection,
        &fixture.first_generation_id,
        "rel-ref-before",
        "references",
        "beta",
        "resolved_symbol",
        "resolver-v1",
    );
    insert_relationship(
        &fixture.connection,
        &second_generation_id,
        "rel-ref-after",
        "references",
        "beta",
        "ambiguous",
        "resolver-v1",
    );
    insert_effect(
        &fixture.connection,
        &fixture.first_generation_id,
        "effect-before",
        "direct",
        "{\"scope\":\"local\"}",
    );
    insert_effect(
        &fixture.connection,
        &second_generation_id,
        "effect-after",
        "transitive",
        "{\"scope\":\"local\"}",
    );
    insert_coverage(
        &fixture.connection,
        &fixture.first_generation_id,
        "coverage-before",
        "complete",
    );
    insert_coverage(
        &fixture.connection,
        &second_generation_id,
        "coverage-after",
        "partial",
    );
    insert_conflict(
        &fixture.connection,
        &fixture.first_generation_id,
        "conflict-before",
        "open",
    );
    insert_conflict(
        &fixture.connection,
        &second_generation_id,
        "conflict-after",
        "unresolved",
    );
    fixture
        .connection
        .execute(
            "UPDATE index_generation SET provider_set_hash = ?2 WHERE generation_id = ?1",
            params![second_generation_id, "b".repeat(64)],
        )
        .unwrap();

    let delta = compute_generation_delta(
        &fixture.connection,
        &fixture.workspace,
        &fixture.first_generation_id,
        &second_generation_id,
    )
    .unwrap();

    assert!(
        delta.relationships.changes.iter().any(|change| {
            change.change_kind == ChangeKind::Retargeted
                && change.reason_codes == ["relationship_target_changed"]
        }),
        "relationship changes: {:?}",
        delta.relationships.changes
    );
    assert!(delta.relationships.changes.iter().any(|change| {
        change.change_kind == ChangeKind::StateChanged
            && change.reason_codes == ["relationship_resolution_state_changed"]
    }));
    assert!(delta.effects.changes.iter().any(|change| {
        change.change_kind == ChangeKind::StateChanged
            && change.reason_codes == ["effect_phase_changed"]
    }));
    assert!(delta.coverage.changes.iter().any(|change| {
        change.change_kind == ChangeKind::StateChanged
            && change.reason_codes == ["provider_fingerprint_changed"]
    }));
    assert!(!delta
        .symbols
        .changes
        .iter()
        .any(|change| { change.reason_codes == ["provider_fingerprint_changed"] }));
    assert!(delta.coverage.changes.iter().any(|change| {
        change.change_kind == ChangeKind::StateChanged
            && change.reason_codes == ["coverage_status_changed"]
    }));
    assert!(delta.conflicts.changes.iter().any(|change| {
        change.change_kind == ChangeKind::StateChanged
            && change.reason_codes == ["conflict_status_changed"]
    }));
}

fn dependency(kind: IrLeaseDependencyKindV2, key: &str, digest: String) -> IrLeaseDependencyV2 {
    IrLeaseDependencyV2 {
        kind,
        key: key.to_string(),
        digest,
    }
}

#[test]
fn declared_lease_dependencies_fail_closed_on_policy_and_truth_drift() {
    let fixture = setup();
    insert_coverage(
        &fixture.connection,
        &fixture.first_generation_id,
        "coverage",
        "complete",
    );
    insert_effect(
        &fixture.connection,
        &fixture.first_generation_id,
        "effect",
        "direct",
        "{}",
    );
    insert_relationship(
        &fixture.connection,
        &fixture.first_generation_id,
        "relationship",
        "calls",
        "beta",
        "resolved_symbol",
        "resolver-v1",
    );
    insert_conflict(
        &fixture.connection,
        &fixture.first_generation_id,
        "conflict",
        "open",
    );

    let kinds_and_keys = [
        (
            IrLeaseDependencyKindV2::Relationship,
            "symbol:alpha\u{1f}calls",
        ),
        (
            IrLeaseDependencyKindV2::Effect,
            "symbol:alpha\u{1f}writes_state",
        ),
        (IrLeaseDependencyKindV2::Coverage, "workspace:all"),
        (
            IrLeaseDependencyKindV2::Conflict,
            "symbol:alpha\u{1f}provider_disagreement",
        ),
        (IrLeaseDependencyKindV2::PlannerPolicy, "planner-v1.1.0"),
        (
            IrLeaseDependencyKindV2::ProjectionPolicy,
            "projection-stale",
        ),
        (IrLeaseDependencyKindV2::Estimator, "estimator-stale"),
    ];
    for (kind, key) in kinds_and_keys {
        let lease = IrEvidenceLeaseV2 {
            generation_id: fixture.first_generation_id.clone(),
            dependencies: vec![dependency(kind, key, "0".repeat(64))],
        };
        assert!(matches!(
            verify_declared_lease_dependencies(&fixture.connection, &fixture.workspace, &lease)
                .unwrap(),
            LeaseVerification::DependencyMismatch { .. }
        ));
    }

    let provider_set_hash: String = fixture
        .connection
        .query_row(
            "SELECT provider_set_hash FROM index_generation WHERE generation_id = ?1",
            params![fixture.first_generation_id],
            |row| row.get(0),
        )
        .unwrap();
    let source_hash =
        content_hash_of_file(&fixture.workspace_directory.path().join("src/a.ts")).unwrap();
    let valid = IrEvidenceLeaseV2 {
        generation_id: fixture.first_generation_id.clone(),
        dependencies: vec![
            dependency(
                IrLeaseDependencyKindV2::Estimator,
                workspace_atlas::context_route::DEEP_ESTIMATOR_VERSION,
                content_hash_of_bytes(
                    workspace_atlas::context_route::DEEP_ESTIMATOR_VERSION.as_bytes(),
                ),
            ),
            dependency(
                IrLeaseDependencyKindV2::PlannerPolicy,
                workspace_atlas::task_compiler::PLANNER_POLICY_V2_VERSION,
                content_hash_of_bytes(
                    workspace_atlas::task_compiler::PLANNER_POLICY_V2_VERSION.as_bytes(),
                ),
            ),
            dependency(
                IrLeaseDependencyKindV2::ProjectionPolicy,
                workspace_atlas::context_route::DEEP_PROJECTION_VERSION,
                content_hash_of_bytes(
                    workspace_atlas::context_route::DEEP_PROJECTION_VERSION.as_bytes(),
                ),
            ),
            dependency(
                IrLeaseDependencyKindV2::ProviderFingerprint,
                "provider_set",
                content_hash_of_bytes(provider_set_hash.as_bytes()),
            ),
            dependency(
                IrLeaseDependencyKindV2::SourceDigest,
                "src/a.ts",
                source_hash,
            ),
        ],
    };
    assert_eq!(
        verify_declared_lease_dependencies(&fixture.connection, &fixture.workspace, &valid)
            .unwrap(),
        LeaseVerification::Valid
    );

    fixture
        .connection
        .execute(
            "UPDATE index_generation SET provider_set_hash = ?2 WHERE generation_id = ?1",
            params![fixture.first_generation_id, "f".repeat(64)],
        )
        .unwrap();
    assert!(matches!(
        verify_declared_lease_dependencies(&fixture.connection, &fixture.workspace, &valid).unwrap(),
        LeaseVerification::DependencyMismatch { dependency_kind, .. }
            if dependency_kind == "provider_fingerprint"
    ));
}

#[test]
fn persisted_lease_invalidates_when_stored_fingerprint_drifts_from_truth() {
    let fixture = setup();
    let source_hash =
        content_hash_of_file(&fixture.workspace_directory.path().join("src/a.ts")).unwrap();
    let provider_set_hash: String = fixture
        .connection
        .query_row(
            "SELECT provider_set_hash FROM index_generation WHERE generation_id = ?1",
            params![fixture.first_generation_id],
            |row| row.get(0),
        )
        .unwrap();
    let symbol_key = first_symbol_key(&fixture.connection, &fixture.first_generation_id);
    let lease_id = create_evidence_lease(
        &fixture.connection,
        &fixture.workspace,
        &fixture.first_generation_id,
        "symbol",
        &symbol_key,
        Some(&source_hash),
        Some(&provider_set_hash),
        None,
        None,
    )
    .unwrap();
    fixture
        .connection
        .execute(
            "UPDATE index_generation SET provider_set_hash = ?2 WHERE generation_id = ?1",
            params![fixture.first_generation_id, "e".repeat(64)],
        )
        .unwrap();

    assert!(matches!(
        verify_evidence_lease(
            &fixture.connection,
            &fixture.workspace,
            &lease_id,
            "src/a.ts"
        )
        .unwrap(),
        LeaseVerification::DependencyMismatchAutoInvalidated { dependency_kind, .. }
            if dependency_kind == "provider_fingerprint"
    ));
    let state: String = fixture
        .connection
        .query_row(
            "SELECT state FROM evidence_lease WHERE lease_id = ?1",
            params![lease_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "invalidated");
}
#[test]
fn persisted_policy_leases_follow_the_named_truth_evidence() {
    let fixture = setup();
    let source_hash =
        content_hash_of_file(&fixture.workspace_directory.path().join("src/a.ts")).unwrap();
    insert_relationship(
        &fixture.connection,
        &fixture.first_generation_id,
        "relationship",
        "calls",
        "beta",
        "resolved_symbol",
        "resolver-v1",
    );
    insert_conflict(
        &fixture.connection,
        &fixture.first_generation_id,
        "conflict",
        "open",
    );
    let relationship_lease = create_evidence_lease(
        &fixture.connection,
        &fixture.workspace,
        &fixture.first_generation_id,
        "relationship",
        "symbol:alpha\u{1f}calls",
        Some(&source_hash),
        None,
        Some("resolver-v1"),
        None,
    )
    .unwrap();
    let conflict_lease = create_evidence_lease(
        &fixture.connection,
        &fixture.workspace,
        &fixture.first_generation_id,
        "conflict",
        "symbol:alpha\u{1f}provider_disagreement",
        Some(&source_hash),
        None,
        None,
        Some("projection-v1"),
    )
    .unwrap();
    fixture
        .connection
        .execute(
            "UPDATE relationship_resolution
             SET resolver_policy_version = 'resolver-v2'
             WHERE relationship_fact_id = 'relationship'",
            [],
        )
        .unwrap();
    fixture
        .connection
        .execute(
            "UPDATE evidence_conflict
             SET projection_policy_version = 'projection-v2'
             WHERE evidence_conflict_id = 'conflict'",
            [],
        )
        .unwrap();

    assert!(matches!(
        verify_evidence_lease(
            &fixture.connection,
            &fixture.workspace,
            &relationship_lease,
            "src/a.ts"
        )
        .unwrap(),
        LeaseVerification::DependencyMismatchAutoInvalidated { dependency_kind, .. }
            if dependency_kind == "resolver_policy"
    ));
    assert!(matches!(
        verify_evidence_lease(
            &fixture.connection,
            &fixture.workspace,
            &conflict_lease,
            "src/a.ts"
        )
        .unwrap(),
        LeaseVerification::DependencyMismatchAutoInvalidated { dependency_kind, .. }
            if dependency_kind == "projection_policy"
    ));
}

#[test]
fn adding_one_target_to_an_existing_relationship_is_added_not_retargeted() {
    let fixture = setup();
    let second_generation_id = reconcile_after(&fixture, &format!("// generation two\n{SOURCE}"));
    insert_relationship(
        &fixture.connection,
        &fixture.first_generation_id,
        "before-beta",
        "calls",
        "beta",
        "resolved_symbol",
        "resolver-v1",
    );
    insert_relationship(
        &fixture.connection,
        &second_generation_id,
        "after-beta",
        "calls",
        "beta",
        "resolved_symbol",
        "resolver-v1",
    );
    insert_relationship(
        &fixture.connection,
        &second_generation_id,
        "after-gamma",
        "calls",
        "gamma",
        "resolved_symbol",
        "resolver-v1",
    );

    let delta = compute_generation_delta(
        &fixture.connection,
        &fixture.workspace,
        &fixture.first_generation_id,
        &second_generation_id,
    )
    .unwrap();
    assert!(delta.relationships.changes.iter().any(|change| {
        change.change_kind == ChangeKind::Added && change.reason_codes == ["resolved_edge_added"]
    }));
    assert!(!delta
        .relationships
        .changes
        .iter()
        .any(|change| change.change_kind == ChangeKind::Retargeted));
}

#[test]
fn relationship_evidence_drift_is_not_mislabeled_as_resolution_state() {
    let fixture = setup();
    let second_generation_id = reconcile_after(&fixture, &format!("// generation two\n{SOURCE}"));
    insert_relationship(
        &fixture.connection,
        &fixture.first_generation_id,
        "before",
        "calls",
        "beta",
        "resolved_symbol",
        "resolver-v1",
    );
    insert_relationship(
        &fixture.connection,
        &second_generation_id,
        "after",
        "calls",
        "beta",
        "resolved_symbol",
        "resolver-v1",
    );
    fixture
        .connection
        .execute(
            "UPDATE relationship_fact SET confidence = 0.5 WHERE relationship_fact_id = 'after'",
            [],
        )
        .unwrap();

    let delta = compute_generation_delta(
        &fixture.connection,
        &fixture.workspace,
        &fixture.first_generation_id,
        &second_generation_id,
    )
    .unwrap();
    assert!(delta.relationships.changes.iter().any(|change| {
        change.change_kind == ChangeKind::Modified
            && change.reason_codes == ["relationship_evidence_changed"]
    }));
    assert!(!delta
        .symbols
        .changes
        .iter()
        .any(|change| { change.reason_codes == ["relationship_set_changed"] }));
}

#[test]
fn unchanged_claim_is_omitted_when_live_source_no_longer_matches_generation() {
    let fixture = setup();
    let second_generation_id = reconcile_after(&fixture, &format!("// moved down\n{SOURCE}"));
    std::fs::write(
        fixture.workspace_directory.path().join("src/a.ts"),
        format!("// unreconciled drift\n{SOURCE}"),
    )
    .unwrap();

    let delta = compute_generation_delta(
        &fixture.connection,
        &fixture.workspace,
        &fixture.first_generation_id,
        &second_generation_id,
    )
    .unwrap();
    assert!(delta.unchanged_verified_contracts.is_empty());
    assert!(delta
        .uncertainty
        .iter()
        .any(|reason| reason.contains("live source")));
}
#[test]
fn unchanged_claim_requires_complete_coverage_for_its_source() {
    let fixture = setup();
    insert_coverage(
        &fixture.connection,
        &fixture.first_generation_id,
        "workspace-before",
        "complete",
    );
    let second_generation_id = reconcile_after(&fixture, &format!("// moved down\n{SOURCE}"));
    insert_coverage(
        &fixture.connection,
        &second_generation_id,
        "workspace-after",
        "complete",
    );
    insert_file_coverage(
        &fixture.connection,
        &second_generation_id,
        "file-after",
        "src/a.ts",
        "partial",
    );

    let delta = compute_generation_delta(
        &fixture.connection,
        &fixture.workspace,
        &fixture.first_generation_id,
        &second_generation_id,
    )
    .unwrap();
    assert!(delta.unchanged_verified_contracts.is_empty());
    assert!(delta
        .uncertainty
        .iter()
        .any(|reason| reason.contains("coverage")));
}

#[test]
fn unchanged_claim_requires_matching_coverage_despite_complete_extractors() {
    let fixture = setup();
    let second_generation_id = reconcile_after(&fixture, &format!("// moved down\n{SOURCE}"));
    for (coverage_id, generation_id) in [
        ("unrelated-before", fixture.first_generation_id.as_str()),
        ("unrelated-after", second_generation_id.as_str()),
    ] {
        let extractor_status: String = fixture
            .connection
            .query_row(
                "SELECT er.status
                 FROM generation_file gf
                 JOIN extractor_run er ON er.revision_id = gf.revision_id
                 WHERE gf.generation_id = ?1
                 ORDER BY er.extractor_run_id
                 LIMIT 1",
                params![generation_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(extractor_status, "complete");
        fixture
            .connection
            .execute(
                "INSERT INTO coverage_record (
                    coverage_id, generation_id, scope_kind, scope_key, status,
                    capabilities_json, limitations_json, details_json
                 ) VALUES (?1,?2,'provider','unrelated@0','complete','[\"symbols\"]','[]','{}')",
                params![coverage_id, generation_id],
            )
            .unwrap();
    }

    let delta = compute_generation_delta(
        &fixture.connection,
        &fixture.workspace,
        &fixture.first_generation_id,
        &second_generation_id,
    )
    .unwrap();

    assert!(delta.unchanged_verified_contracts.is_empty());
    assert!(delta
        .uncertainty
        .iter()
        .any(|reason| reason.contains("coverage")));
}

#[test]
fn durable_lease_requires_the_named_truth_evidence_to_exist() {
    let fixture = setup();
    let source_hash =
        content_hash_of_file(&fixture.workspace_directory.path().join("src/a.ts")).unwrap();
    let lease_id = create_evidence_lease(
        &fixture.connection,
        &fixture.workspace,
        &fixture.first_generation_id,
        "symbol",
        "missing-symbol",
        Some(&source_hash),
        None,
        None,
        None,
    )
    .unwrap();
    assert!(matches!(
        verify_evidence_lease(
            &fixture.connection,
            &fixture.workspace,
            &lease_id,
            "src/a.ts"
        )
        .unwrap(),
        LeaseVerification::DependencyMismatchAutoInvalidated { dependency_kind, .. }
            if dependency_kind == "evidence"
    ));
}

#[test]
fn declared_lease_dependencies_require_canonical_order() {
    let fixture = setup();
    let lease = IrEvidenceLeaseV2 {
        generation_id: fixture.first_generation_id.clone(),
        dependencies: vec![
            dependency(
                IrLeaseDependencyKindV2::SourceDigest,
                "src/a.ts",
                "0".repeat(64),
            ),
            dependency(
                IrLeaseDependencyKindV2::PlannerPolicy,
                workspace_atlas::task_compiler::PLANNER_POLICY_V2_VERSION,
                "0".repeat(64),
            ),
        ],
    };
    assert!(
        verify_declared_lease_dependencies(&fixture.connection, &fixture.workspace, &lease)
            .is_err()
    );
}
