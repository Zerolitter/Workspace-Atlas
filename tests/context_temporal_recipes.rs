use serde_json::Value;
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::context_ir::{
    read_deep_context_ir_v2, IrRequirementStateV2, IrTemporalStateV2, ItemRole, SelectionReason,
    TaskKind,
};
use workspace_atlas::discovery;
use workspace_atlas::generation::{begin_candidate, TriggerKind};
use workspace_atlas::task_compiler::{deep_v2_role_recipe, populate_deep_v2_temporal_evidence};
use workspace_atlas::workspace::{register_workspace, WorkspaceRecord};

struct Fixture {
    _database_directory: tempfile::TempDir,
    _workspace_directory: tempfile::TempDir,
    _config: Config,
    connection: rusqlite::Connection,
    workspace: WorkspaceRecord,
    first_generation_id: String,
    active_generation_id: String,
}

impl Fixture {
    fn new() -> Self {
        let database_directory = tempfile::tempdir().unwrap();
        let workspace_directory = tempfile::tempdir().unwrap();
        let source_path = workspace_directory.path().join("src/lib.rs");
        std::fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        std::fs::write(&source_path, "pub fn value() -> i32 { 1 }\n").unwrap();

        let config = Config::parse(
            "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"temporal-recipes\"\n",
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
        std::fs::write(&source_path, "pub fn value() -> i32 { 2 }\n").unwrap();
        let active_generation_id = discovery::reconcile(&workspace, &connection, &config)
            .unwrap()
            .candidate_generation_id;

        Self {
            _database_directory: database_directory,
            _workspace_directory: workspace_directory,
            _config: config,
            connection,
            workspace,
            first_generation_id,
            active_generation_id,
        }
    }

    fn document(&self, task_kind: TaskKind) -> workspace_atlas::context_ir::DeepContextIrV2 {
        let fixture: Value =
            serde_json::from_str(include_str!("fixtures/context_ir/context-ir.example.json"))
                .unwrap();
        let mut document =
            read_deep_context_ir_v2(&serde_json::to_string(&fixture["deep_v2"]).unwrap()).unwrap();
        document.workspace.workspace_id = self.workspace.workspace_id.clone();
        document.workspace.generation_id = self.active_generation_id.clone();
        document.workspace.generation_sequence = self
            .connection
            .query_row(
                "SELECT sequence_no FROM index_generation WHERE generation_id = ?1",
                [&self.active_generation_id],
                |row| row.get(0),
            )
            .unwrap();
        document.task.task_kind = task_kind;
        document.temporal_constraints.current_generation_id = self.active_generation_id.clone();
        document.temporal_constraints.baseline_generation_id =
            Some(self.first_generation_id.clone());
        document.temporal_constraints.evidence_item_ids.clear();
        document.temporal_constraints.state = IrTemporalStateV2::Unavailable;
        document.temporal_constraints.omission_reason = None;
        document.evidence_lease.generation_id = self.active_generation_id.clone();
        for item in &mut document.working_set {
            item.evidence.generation_id = self.active_generation_id.clone();
        }
        for relationship in &mut document.relationships {
            relationship.evidence.generation_id = self.active_generation_id.clone();
        }
        for effect in &mut document.effects {
            effect.evidence.generation_id = self.active_generation_id.clone();
        }
        let recipe = deep_v2_role_recipe(task_kind);
        for role in &mut document.status.role_sufficiency {
            role.required = recipe.required_roles.contains(&role.role);
            if role.role == ItemRole::HistoricalConstraint {
                role.state = IrRequirementStateV2::Unavailable;
                role.evidence_item_ids.clear();
                role.reason = None;
            }
        }
        document
    }
}

#[test]
fn deep_review_bug_api_and_behavior_recipes_consume_bounded_canonical_history() {
    let fixture = Fixture::new();
    let persisted_before: i64 = fixture
        .connection
        .query_row("SELECT COUNT(*) FROM generation_delta", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(persisted_before, 0);

    for task_kind in [
        TaskKind::Review,
        TaskKind::BugFix,
        TaskKind::ApiChange,
        TaskKind::BehaviorChange,
    ] {
        let input = fixture.document(task_kind);
        let document = populate_deep_v2_temporal_evidence(
            &fixture.connection,
            &fixture.workspace,
            input.clone(),
        )
        .unwrap()
        .seal()
        .unwrap();
        let repeated =
            populate_deep_v2_temporal_evidence(&fixture.connection, &fixture.workspace, input)
                .unwrap()
                .seal()
                .unwrap();
        assert_eq!(document.context_hash, repeated.context_hash);

        assert_eq!(
            document.temporal_constraints.state,
            IrTemporalStateV2::VerifiedAncestor
        );
        assert_eq!(
            document
                .temporal_constraints
                .baseline_generation_id
                .as_deref(),
            Some(fixture.first_generation_id.as_str())
        );
        assert!(!document.temporal_constraints.evidence_item_ids.is_empty());
        assert!(document
            .temporal_constraints
            .evidence_item_ids
            .iter()
            .all(|item_id| document.working_set.iter().any(|item| {
                item.item_id == *item_id
                    && item.role == ItemRole::HistoricalConstraint
                    && item.selection_reason == SelectionReason::GenerationDeltaRelevance
            })));
        let historical = document
            .status
            .role_sufficiency
            .iter()
            .find(|entry| entry.role == ItemRole::HistoricalConstraint)
            .unwrap();
        assert_eq!(historical.state, IrRequirementStateV2::Satisfied);
        assert_eq!(
            historical.evidence_item_ids,
            document.temporal_constraints.evidence_item_ids
        );
        assert!(document.cost.selected_records <= document.policy.semantic_budget.max_records);
        assert!(
            document.cost.selected_estimated_tokens
                <= document.policy.semantic_budget.max_estimated_tokens
        );
    }
    let persisted_after: i64 = fixture
        .connection
        .query_row("SELECT COUNT(*) FROM generation_delta", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(persisted_after, 0);
}

#[test]
fn deep_temporal_recipe_routes_stale_live_evidence_through_uncertainty() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture._workspace_directory.path().join("src/lib.rs"),
        "pub fn value() -> i32 { 3 }\n",
    )
    .unwrap();

    let document = populate_deep_v2_temporal_evidence(
        &fixture.connection,
        &fixture.workspace,
        fixture.document(TaskKind::Review),
    )
    .unwrap()
    .seal()
    .unwrap();
    let stale = document
        .working_set
        .iter()
        .find(|item| item.evidence.state == workspace_atlas::context_ir::EvidenceQuality::Stale)
        .unwrap();
    assert_eq!(stale.role, ItemRole::Uncertainty);
    assert_eq!(stale.evidence.preferred, Some(false));
    assert!(document.uncertainty.iter().any(
        |notice| notice.code == workspace_atlas::context_ir::IrOmissionReasonV2::StaleEvidence
    ));
    let uncertainty = document
        .status
        .role_sufficiency
        .iter()
        .find(|entry| entry.role == ItemRole::Uncertainty)
        .unwrap();
    assert_eq!(uncertainty.state, IrRequirementStateV2::Unresolved);
    assert!(uncertainty.evidence_item_ids.contains(&stale.item_id));
}

#[test]
fn deep_temporal_recipe_reports_history_omitted_by_the_shared_record_budget() {
    let fixture = Fixture::new();
    let mut document = fixture.document(TaskKind::Review);
    let ordinary_limit =
        document.policy.semantic_budget.max_records - document.cost.uncertainty_records_reserved;
    while document.cost.selected_records < ordinary_limit {
        let mut item = document.working_set[1].clone();
        item.item_id = format!("current_item_{}", document.working_set.len());
        item.entity_id = format!("symbol:current_{}", document.working_set.len());
        item.rank = document.working_set.len() as u64;
        document.cost.selected_records += 1;
        document.cost.selected_estimated_tokens += item.cost.estimated_tokens;
        document.cost.work_units_consumed += 1;
        document.omissions.candidates_considered += 1;
        document.omissions.selected += 1;
        document.working_set.push(item);
    }

    let document =
        populate_deep_v2_temporal_evidence(&fixture.connection, &fixture.workspace, document)
            .unwrap();
    assert_eq!(
        document.temporal_constraints.state,
        IrTemporalStateV2::VerifiedAncestor
    );
    assert!(document.temporal_constraints.evidence_item_ids.is_empty());
    assert_eq!(
        document.temporal_constraints.omission_reason,
        Some(workspace_atlas::context_ir::IrOmissionReasonV2::RecordBudget)
    );
    let historical = document
        .status
        .role_sufficiency
        .iter()
        .find(|entry| entry.role == ItemRole::HistoricalConstraint)
        .unwrap();
    assert_eq!(historical.state, IrRequirementStateV2::BudgetOmitted);
    assert_eq!(
        historical.reason,
        Some(workspace_atlas::context_ir::IrOmissionReasonV2::RecordBudget)
    );
    document.seal().unwrap();
}

#[test]
fn deep_temporal_recipe_rejects_missing_role_non_ancestor_and_candidate_baselines() {
    let fixture = Fixture::new();
    let mut missing_role = fixture.document(TaskKind::Review);
    missing_role
        .status
        .role_sufficiency
        .retain(|entry| entry.role != ItemRole::HistoricalConstraint);
    let missing_role =
        populate_deep_v2_temporal_evidence(&fixture.connection, &fixture.workspace, missing_role)
            .unwrap_err();
    assert!(missing_role
        .to_string()
        .contains("role matrix does not match planner-v2.0.0"));

    let mut active_as_baseline = fixture.document(TaskKind::Review);
    active_as_baseline
        .temporal_constraints
        .baseline_generation_id = Some(fixture.active_generation_id.clone());
    let non_ancestor = populate_deep_v2_temporal_evidence(
        &fixture.connection,
        &fixture.workspace,
        active_as_baseline,
    )
    .unwrap_err();
    assert!(non_ancestor.to_string().contains("not an ancestor"));

    let candidate = begin_candidate(
        &fixture.connection,
        &fixture.workspace.workspace_id,
        TriggerKind::Manual,
        &"a".repeat(64),
        "1.0.0",
    )
    .unwrap();
    let mut candidate_baseline = fixture.document(TaskKind::BugFix);
    candidate_baseline
        .temporal_constraints
        .baseline_generation_id = Some(candidate.generation_id);
    let non_committed = populate_deep_v2_temporal_evidence(
        &fixture.connection,
        &fixture.workspace,
        candidate_baseline,
    )
    .unwrap_err();
    assert!(non_committed.to_string().contains("not committed"));
}

#[test]
fn deep_temporal_recipe_keeps_baseline_only_history_honest() {
    let fixture = Fixture::new();
    let mut document = fixture.document(TaskKind::Review);
    document.workspace.generation_id = fixture.first_generation_id.clone();
    document.temporal_constraints.current_generation_id = fixture.first_generation_id.clone();
    document.temporal_constraints.baseline_generation_id = None;
    document.workspace.generation_sequence = fixture
        .connection
        .query_row(
            "SELECT sequence_no FROM index_generation WHERE generation_id = ?1",
            [&fixture.first_generation_id],
            |row| row.get(0),
        )
        .unwrap();
    document.evidence_lease.generation_id = fixture.first_generation_id.clone();
    for item in &mut document.working_set {
        item.evidence.generation_id = fixture.first_generation_id.clone();
    }
    for relationship in &mut document.relationships {
        relationship.evidence.generation_id = fixture.first_generation_id.clone();
    }
    for effect in &mut document.effects {
        effect.evidence.generation_id = fixture.first_generation_id.clone();
    }
    fixture
        .connection
        .execute(
            "UPDATE workspace SET active_generation_id = ?1 WHERE workspace_id = ?2",
            rusqlite::params![fixture.first_generation_id, fixture.workspace.workspace_id],
        )
        .unwrap();

    let document =
        populate_deep_v2_temporal_evidence(&fixture.connection, &fixture.workspace, document)
            .unwrap();
    assert_eq!(
        document.temporal_constraints.state,
        IrTemporalStateV2::Unavailable
    );
    assert!(document
        .temporal_constraints
        .baseline_generation_id
        .is_none());
    assert!(document.temporal_constraints.evidence_item_ids.is_empty());
    let historical = document
        .status
        .role_sufficiency
        .iter()
        .find(|entry| entry.role == ItemRole::HistoricalConstraint)
        .unwrap();
    assert_eq!(historical.state, IrRequirementStateV2::Unavailable);
    assert!(historical.evidence_item_ids.is_empty());
    document.seal().unwrap();
}
