use rusqlite::{params, Connection};
use serde_json::Value;
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::context_ir::{EvidenceQuality, IrOmissionReasonV2, ItemRole, SelectionReason};
use workspace_atlas::discovery;
use workspace_atlas::task_compiler::resolve_deep_v2_seed_candidates;
use workspace_atlas::workspace::{register_workspace, WorkspaceRecord};

struct Fixture {
    _database_directory: tempfile::TempDir,
    _workspace_directory: tempfile::TempDir,
    connection: Connection,
    workspace: WorkspaceRecord,
    generation_id: String,
    canonical_alpha: String,
    canonical_shared_a: String,
}

impl Fixture {
    fn new() -> Self {
        let database_directory = tempfile::tempdir().unwrap();
        let workspace_directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace_directory.path().join("src")).unwrap();
        std::fs::write(
            workspace_directory.path().join("src/a.ts"),
            "export function alpha() { return 1; }\nexport function shared() { return alpha(); }\n",
        )
        .unwrap();
        std::fs::write(
            workspace_directory.path().join("src/b.ts"),
            "export function beta() { return 2; }\nexport function shared() { return beta(); }\n",
        )
        .unwrap();
        std::fs::write(
            workspace_directory.path().join("src/c.ts"),
            "export function gamma() { return 3; }\n",
        )
        .unwrap();
        std::fs::write(workspace_directory.path().join("src/empty.ts"), "").unwrap();

        let config = Config::parse(
            "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"seed-resolution\"\n",
        )
        .unwrap();
        let database_path = database_directory.path().join("atlas.sqlite");
        let connection = init_catalogue(&database_path, &config).unwrap();
        let workspace = register_workspace(
            &connection,
            workspace_directory.path(),
            &config,
            &database_path,
            "1.0.0",
        )
        .unwrap();
        let generation_id = discovery::reconcile(&workspace, &connection, &config)
            .unwrap()
            .candidate_generation_id;

        connection
            .execute(
                "UPDATE symbol_fact SET qualified_name = CASE display_name
                    WHEN 'alpha' THEN 'pkg::alpha'
                    WHEN 'beta' THEN 'pkg::beta'
                    WHEN 'gamma' THEN 'pkg::gamma'
                    WHEN 'shared' THEN 'pkg::' || revision_id || '::shared'
                    ELSE qualified_name END",
                [],
            )
            .unwrap();
        let canonical_alpha: String = connection
            .query_row(
                "SELECT sf.canonical_symbol_key FROM current_file cf
                 JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
                 WHERE cf.generation_id = ?1 AND sf.display_name = 'alpha'",
                [&generation_id],
                |row| row.get(0),
            )
            .unwrap();
        let canonical_shared_a: String = connection
            .query_row(
                "SELECT sf.canonical_symbol_key FROM current_file cf
                 JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
                 WHERE cf.generation_id = ?1
                   AND cf.canonical_path = 'src/a.ts'
                   AND sf.display_name = 'shared'",
                [&generation_id],
                |row| row.get(0),
            )
            .unwrap();

        for display_name in ["alpha", "beta", "gamma"] {
            let (fact_id, canonical_path, qualified_name): (String, String, String) = connection
                .query_row(
                    "SELECT sf.symbol_fact_id, cf.canonical_path, sf.qualified_name
                     FROM current_file cf JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
                     WHERE cf.generation_id = ?1 AND sf.display_name = ?2",
                    params![generation_id, display_name],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO atlas_fts (
                        record_kind, record_id, workspace_id, canonical_path,
                        display_name, qualified_name, documentation, searchable_text
                     ) VALUES ('symbol', ?1, ?2, ?3, ?4, ?5, '', 'needle')",
                    params![
                        fact_id,
                        workspace.workspace_id,
                        canonical_path,
                        display_name,
                        qualified_name
                    ],
                )
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO atlas_fts (
                    record_kind, record_id, workspace_id, canonical_path,
                    display_name, qualified_name, documentation, searchable_text
                 )
                 SELECT record_kind, record_id, workspace_id, canonical_path,
                        display_name, qualified_name, documentation, searchable_text
                 FROM atlas_fts WHERE display_name = 'alpha'",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO atlas_fts (
                    record_kind, record_id, workspace_id, canonical_path,
                    display_name, qualified_name, documentation, searchable_text
                 )
                 SELECT 'symbol', sf.symbol_fact_id, ?1, cf.canonical_path,
                        sf.display_name, sf.qualified_name, '', 'aliasneedle'
                 FROM current_file cf
                 JOIN symbol_fact sf ON sf.revision_id = cf.revision_id
                 WHERE cf.generation_id = ?2 AND sf.display_name = 'shared'",
                params![workspace.workspace_id, generation_id],
            )
            .unwrap();

        Self {
            _database_directory: database_directory,
            _workspace_directory: workspace_directory,
            connection,
            workspace,
            generation_id,
            canonical_alpha,
            canonical_shared_a,
        }
    }

    fn resolve(
        &self,
        seed: &str,
        limit: u64,
    ) -> workspace_atlas::task_compiler::DeepV2SeedResolution {
        self.resolve_with_work(seed, limit, 100)
    }

    fn resolve_with_work(
        &self,
        seed: &str,
        limit: u64,
        max_work_units: u64,
    ) -> workspace_atlas::task_compiler::DeepV2SeedResolution {
        resolve_deep_v2_seed_candidates(
            &self.connection,
            &self.workspace,
            &self.generation_id,
            &[seed.to_string()],
            limit,
            max_work_units,
        )
        .unwrap()
    }
}

fn contract() -> Value {
    serde_json::from_str(include_str!(
        "fixtures/context_ir/seed-resolution.example.json"
    ))
    .unwrap()
}

#[test]
fn exact_resolution_order_is_canonical_then_path_then_qualified() {
    let fixture = Fixture::new();
    let expected = contract();

    let canonical = fixture.resolve(&fixture.canonical_alpha, 2);
    assert_eq!(
        canonical.candidates.len() as u64,
        expected["canonical"]["candidate_count"].as_u64().unwrap()
    );
    assert_eq!(
        canonical.candidates[0].item.selection_reason,
        SelectionReason::ExactSymbolMatch
    );
    assert_eq!(
        canonical.candidates[0].item.evidence.state,
        EvidenceQuality::Supported
    );

    let path = fixture.resolve("src/a.ts", 2);
    assert_eq!(
        path.candidates.len() as u64,
        expected["path"]["candidate_count"].as_u64().unwrap()
    );
    assert_eq!(
        path.candidates[0].item.selection_reason,
        SelectionReason::ExactPathMatch
    );
    assert_eq!(
        path.candidates[0].item.evidence.state,
        EvidenceQuality::Verified
    );

    let qualified = fixture.resolve("pkg::beta", 2);
    assert_eq!(
        qualified.candidates.len() as u64,
        expected["qualified"]["candidate_count"].as_u64().unwrap()
    );
    assert_eq!(
        qualified.candidates[0].item.selection_reason,
        SelectionReason::ExactSymbolMatch
    );
    assert_eq!(
        qualified.candidates[0].item.evidence.state,
        EvidenceQuality::Supported
    );
}

#[test]
fn duplicate_display_names_are_typed_ambiguity_and_never_primary() {
    let fixture = Fixture::new();
    let expected = contract();
    let outcome = fixture.resolve("shared", 2);

    assert_eq!(
        outcome.candidates.len() as u64,
        expected["ambiguous_display"]["candidate_count"]
            .as_u64()
            .unwrap()
    );
    assert!(outcome.candidates.iter().all(|candidate| {
        candidate.item.role == ItemRole::Uncertainty
            && candidate.item.evidence.state == EvidenceQuality::Ambiguous
            && candidate.item.evidence.preferred == Some(false)
    }));
    assert!(outcome
        .uncertainty
        .iter()
        .any(|notice| notice.code == IrOmissionReasonV2::AmbiguousSeed));
    assert_eq!(outcome.omissions.selected, 2);
    assert_eq!(outcome.omissions.omitted, 0);
}

#[test]
fn missing_exact_match_uses_only_bounded_indexed_fts_candidates() {
    let fixture = Fixture::new();
    let expected = contract();
    let limit = expected["fts_candidate_limit"].as_u64().unwrap();
    let outcome = fixture.resolve("needle", limit);

    assert_eq!(outcome.candidates.len() as u64, limit);
    assert!(outcome.candidates.iter().all(|candidate| {
        candidate.item.role == ItemRole::Uncertainty
            && candidate.item.selection_reason == SelectionReason::UnresolvedRelevance
            && candidate.item.evidence.state == EvidenceQuality::Unresolved
            && candidate.item.evidence.preferred == Some(false)
    }));
    assert_eq!(
        outcome
            .omissions
            .by_reason
            .get(&IrOmissionReasonV2::FtsCandidateLimit),
        Some(&expected["bounded_fts"]["omitted"].as_u64().unwrap())
    );
    assert_eq!(
        outcome.omissions.selected,
        expected["bounded_fts"]["selected"].as_u64().unwrap()
    );
    assert_eq!(outcome.omissions.omitted, 2);
    assert_eq!(outcome.omissions.candidates_considered, 4);
    assert_eq!(outcome.work_units_consumed, 4);
    assert_eq!(outcome.sql_rows_fetched, 4);
    assert_eq!(
        outcome
            .omissions
            .by_reason
            .get(&IrOmissionReasonV2::UnresolvedEvidence),
        Some(&1)
    );
    assert!(outcome.omissions.truncated);
}

#[test]
fn aliases_and_duplicate_fts_rows_do_not_duplicate_stable_candidates() {
    let fixture = Fixture::new();
    let outcome = resolve_deep_v2_seed_candidates(
        &fixture.connection,
        &fixture.workspace,
        &fixture.generation_id,
        &[fixture.canonical_alpha.clone(), "alpha".to_string()],
        3,
        100,
    )
    .unwrap();
    assert_eq!(outcome.candidates.len(), 1);
    assert_eq!(outcome.omissions.candidates_considered, 2);
    assert_eq!(outcome.omissions.selected, 1);
    assert_eq!(outcome.omissions.omitted, 1);
    assert_eq!(
        outcome
            .omissions
            .by_reason
            .get(&IrOmissionReasonV2::UnresolvedEvidence),
        Some(&1),
        "the duplicate evidence was examined but could not add independent evidence"
    );
    assert_eq!(outcome.work_units_consumed, 2);
    assert_eq!(outcome.sql_rows_fetched, 2);

    let fts = fixture.resolve("needle", 3);
    let identities: std::collections::HashSet<_> = fts
        .candidates
        .iter()
        .map(|candidate| candidate.item.entity_id.as_str())
        .collect();
    assert_eq!(fts.candidates.len(), 3);
    assert_eq!(identities.len(), 3);
    assert!(fts.omissions.truncated);
    assert_eq!(fts.omissions.candidates_considered, 4);
    assert_eq!(fts.omissions.selected, 3);
    assert_eq!(fts.omissions.omitted, 1);
    assert_eq!(fts.work_units_consumed, 4);
    assert_eq!(fts.sql_rows_fetched, 4);
    assert_eq!(
        fts.omissions
            .by_reason
            .get(&IrOmissionReasonV2::UnresolvedEvidence),
        Some(&1)
    );
}

#[test]
fn tight_work_limit_pays_for_duplicate_and_sentinel_rows() {
    let fixture = Fixture::new();
    let outcome = fixture.resolve_with_work("needle", 10, 5);

    assert_eq!(outcome.candidates.len(), 1);
    assert_eq!(outcome.work_units_consumed, 3);
    assert_eq!(outcome.sql_rows_fetched, 3);
    assert_eq!(outcome.omissions.candidates_considered, 3);
    assert_eq!(outcome.omissions.selected, 1);
    assert_eq!(outcome.omissions.omitted, 2);
    assert_eq!(
        outcome
            .omissions
            .by_reason
            .get(&IrOmissionReasonV2::UnresolvedEvidence),
        Some(&1)
    );
    assert_eq!(
        outcome
            .omissions
            .by_reason
            .get(&IrOmissionReasonV2::WorkUnitBudget),
        Some(&1)
    );
}

#[test]
fn exact_canonical_alias_replaces_its_ambiguous_display_candidate() {
    let fixture = Fixture::new();
    let outcome = resolve_deep_v2_seed_candidates(
        &fixture.connection,
        &fixture.workspace,
        &fixture.generation_id,
        &["shared".to_string(), fixture.canonical_shared_a.clone()],
        2,
        100,
    )
    .unwrap();
    assert_eq!(outcome.candidates.len(), 2);
    let exact = outcome
        .candidates
        .iter()
        .find(|candidate| {
            candidate.item.entity_id == format!("symbol:{}", fixture.canonical_shared_a)
        })
        .unwrap();
    assert_eq!(exact.item.evidence.preferred, Some(true));
    assert_ne!(exact.item.role, ItemRole::Uncertainty);
    assert!(exact.item.source.is_some());
    assert_eq!(outcome.uncertainty.len(), 1);
    assert_eq!(outcome.uncertainty[0].entity_ids.len(), 1);
}

#[test]
fn ambiguity_notice_survives_candidates_already_seen_through_fts() {
    let fixture = Fixture::new();
    let outcome = resolve_deep_v2_seed_candidates(
        &fixture.connection,
        &fixture.workspace,
        &fixture.generation_id,
        &["aliasneedle".to_string(), "shared".to_string()],
        2,
        100,
    )
    .unwrap();
    let notice = outcome
        .uncertainty
        .iter()
        .find(|notice| notice.code == IrOmissionReasonV2::AmbiguousSeed)
        .unwrap();
    assert_eq!(notice.entity_ids.len(), 2);
}

#[test]
fn zero_byte_exact_path_never_emits_an_invalid_source_range() {
    let fixture = Fixture::new();
    let outcome = fixture.resolve("src/empty.ts", 2);
    assert_eq!(outcome.candidates.len(), 1);
    assert_eq!(
        outcome.candidates[0].item.selection_reason,
        SelectionReason::ExactPathMatch
    );
    assert_eq!(outcome.candidates[0].item.cost.source_bytes, 0);
    assert!(outcome.candidates[0].item.source.is_none());
}

#[test]
fn zero_fts_bound_fails_closed() {
    let fixture = Fixture::new();
    let error = resolve_deep_v2_seed_candidates(
        &fixture.connection,
        &fixture.workspace,
        &fixture.generation_id,
        &["needle".to_string()],
        0,
        100,
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("FTS candidate limit must be > 0"));
}
