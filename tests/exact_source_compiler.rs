use rusqlite::{types::Value as SqlValue, Connection, OptionalExtension};
use serde_json::Value;
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::context_ir::{
    read_deep_context_ir_v2, DeepContextIrV2, IrPathConfinementV2, IrRequirementStateV2,
    IrSourceVerificationV2, RawTaskRetention, TaskKind, WorkingSetStatus, CONTEXT_SCHEMA_VERSION,
};
use workspace_atlas::discovery;
use workspace_atlas::hashing::content_hash_of_bytes;
use workspace_atlas::task_compiler::{
    seal_deep_context_ir_v2_with_live_sources, DeepV2SourceMaterializationAuthorization,
};
use workspace_atlas::task_session::create_task_session;
use workspace_atlas::workspace::{register_workspace, WorkspaceRecord};

const SOURCE_PATH: &str = "src/controller/connection.ts";
const SOURCE_BYTES: &[u8] = b"0123456789\n";
const START_BYTE: u64 = 2;
const END_BYTE: u64 = 7;

struct Fixture {
    _database_directory: tempfile::TempDir,
    workspace_directory: tempfile::TempDir,
    connection: Connection,
    workspace: WorkspaceRecord,
    document: DeepContextIrV2,
}

#[derive(Debug, PartialEq)]
struct DurableExactSourceState {
    task_sessions: Vec<Vec<SqlValue>>,
    context_irs: Vec<Vec<SqlValue>>,
    context_use_events: Vec<Vec<SqlValue>>,
    exact_source_ordinals: Vec<(String, i64)>,
}

fn complete_table_rows(connection: &Connection, table: &str, order_by: &str) -> Vec<Vec<SqlValue>> {
    let mut statement = connection
        .prepare(&format!("SELECT * FROM {table} ORDER BY {order_by}"))
        .unwrap();
    let column_count = statement.column_count();
    statement
        .query_map([], |row| {
            (0..column_count)
                .map(|column| row.get(column))
                .collect::<rusqlite::Result<Vec<SqlValue>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn durable_exact_source_state(connection: &Connection) -> DurableExactSourceState {
    let exact_source_ordinals = connection
        .prepare(
            "SELECT task_session_id,
                    CAST(json_extract(details_json, '$.request_ordinal') AS INTEGER)
             FROM context_use_event
             WHERE event_type = 'exact_source_requested'
             ORDER BY task_session_id, 2, event_id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    DurableExactSourceState {
        task_sessions: complete_table_rows(connection, "task_session", "task_session_id"),
        context_irs: complete_table_rows(connection, "context_ir", "context_id"),
        context_use_events: complete_table_rows(
            connection,
            "context_use_event",
            "task_session_id, occurred_at, event_id",
        ),
        exact_source_ordinals,
    }
}

fn fixture() -> Fixture {
    let database_directory = tempfile::tempdir().unwrap();
    let workspace_directory = tempfile::tempdir().unwrap();
    let source_path = workspace_directory.path().join(SOURCE_PATH);
    std::fs::create_dir_all(source_path.parent().unwrap()).unwrap();
    std::fs::write(&source_path, SOURCE_BYTES).unwrap();

    let config =
        Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"exact-source\"\n")
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
    let reconcile = discovery::reconcile(&workspace, &connection, &config).unwrap();
    let session = create_task_session(
        &connection,
        &workspace,
        &reconcile.candidate_generation_id,
        &"33".repeat(32),
        &"44".repeat(32),
        RawTaskRetention::None,
        None,
        TaskKind::BehaviorChange,
        CONTEXT_SCHEMA_VERSION,
    )
    .unwrap();

    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/context_ir/context-ir.example.json")).unwrap();
    let mut document =
        read_deep_context_ir_v2(&serde_json::to_string(&fixture["deep_v2"]).unwrap()).unwrap();
    let generation_id = reconcile.candidate_generation_id;
    document.workspace.workspace_id = workspace.workspace_id.clone();
    document.workspace.generation_id = generation_id.clone();
    document.workspace.generation_sequence = connection
        .query_row(
            "SELECT sequence_no FROM index_generation WHERE generation_id = ?1",
            [&generation_id],
            |row| row.get(0),
        )
        .unwrap();
    document.workspace.source_tree_hash = Some(reconcile.source_tree_hash);
    document.task.task_session_id = session.task_session_id;
    document.temporal_constraints.current_generation_id = generation_id.clone();
    document.evidence_lease.generation_id = generation_id.clone();
    for item in &mut document.working_set {
        item.evidence.generation_id = generation_id.clone();
        item.evidence.source_revision_hash = Some(content_hash_of_bytes(SOURCE_BYTES));
    }
    for relationship in &mut document.relationships {
        relationship.evidence.generation_id = generation_id.clone();
    }
    for effect in &mut document.effects {
        effect.evidence.generation_id = generation_id.clone();
    }
    let primary = &mut document.working_set[0];
    primary.cost.source_bytes = END_BYTE - START_BYTE;
    let source = primary.source.as_mut().unwrap();
    source.canonical_path = SOURCE_PATH.into();
    source.whole_file_sha256 = "00".repeat(32);
    source.start_byte = START_BYTE;
    source.end_byte = END_BYTE;
    source.observed_sha256 = None;
    source.verification_status = IrSourceVerificationV2::Unavailable;
    source.canonical_root_confinement = IrPathConfinementV2::Unavailable;
    source.revalidated_before_seal = false;
    document.cost.selected_source_bytes = END_BYTE - START_BYTE;

    Fixture {
        _database_directory: database_directory,
        workspace_directory,
        connection,
        workspace,
        document,
    }
}

#[test]
fn deep_seal_fills_exact_digest_and_byte_range_without_ir_body_or_persistence() {
    let mut fixture = fixture();
    let durable_before = durable_exact_source_state(&fixture.connection);
    let expected_digest = content_hash_of_bytes(SOURCE_BYTES);

    let output = seal_deep_context_ir_v2_with_live_sources(
        &mut fixture.connection,
        &fixture.workspace,
        fixture.document,
        None,
    )
    .unwrap();

    let source = output.context_ir.working_set[0].source.as_ref().unwrap();
    assert_eq!(source.whole_file_sha256, expected_digest);
    assert_eq!(
        source.observed_sha256.as_deref(),
        Some(expected_digest.as_str())
    );
    assert_eq!((source.start_byte, source.end_byte), (START_BYTE, END_BYTE));
    assert_eq!(source.verification_status, IrSourceVerificationV2::Verified);
    assert_eq!(
        source.canonical_root_confinement,
        IrPathConfinementV2::Confined
    );
    assert!(source.revalidated_before_seal);
    assert!(output.materialization.is_none());

    let serialized = serde_json::to_value(&output.context_ir).unwrap();
    let source_json = &serialized["working_set"][0]["source"];
    assert!(source_json.get("bytes").is_none());
    assert!(source_json.get("body").is_none());
    assert!(source_json.get("excerpt").is_none());
    assert!(!serde_json::to_string(&serialized)
        .unwrap()
        .contains("23456"));
    assert_eq!(
        durable_exact_source_state(&fixture.connection),
        durable_before,
        "V2 live verification must not persist lifecycle state or allocate an ordinal"
    );
}

#[test]
fn explicit_materialization_is_exact_byte_bounded_and_envelope_only() {
    let mut fixture = fixture();
    let durable_before = durable_exact_source_state(&fixture.connection);
    let document_without_authorization = fixture.document.clone();
    let authorization =
        DeepV2SourceMaterializationAuthorization::new(END_BYTE - START_BYTE).unwrap();

    let output = seal_deep_context_ir_v2_with_live_sources(
        &mut fixture.connection,
        &fixture.workspace,
        fixture.document,
        Some(authorization),
    )
    .unwrap();
    let without_authorization = seal_deep_context_ir_v2_with_live_sources(
        &mut fixture.connection,
        &fixture.workspace,
        document_without_authorization,
        None,
    )
    .unwrap();

    let materialization = output.materialization.unwrap();
    assert!(materialization.authorized);
    assert_eq!(materialization.max_bytes, END_BYTE - START_BYTE);
    assert_eq!(materialization.sources.len(), 1);
    assert_eq!(materialization.sources[0].bytes, b"23456");
    assert_eq!(materialization.sources[0].start_byte, START_BYTE);
    assert_eq!(materialization.sources[0].end_byte, END_BYTE);
    assert_eq!(
        output.context_ir.context_hash,
        without_authorization.context_ir.context_hash
    );
    assert_eq!(
        serde_json::to_vec(&output.context_ir).unwrap(),
        serde_json::to_vec(&without_authorization.context_ir).unwrap()
    );
    assert!(without_authorization.materialization.is_none());
    assert!(!serde_json::to_string(&output.context_ir)
        .unwrap()
        .contains("23456"));
    assert_eq!(
        durable_exact_source_state(&fixture.connection),
        durable_before,
        "authorized V2 materialization must remain response-local"
    );
}
#[test]
fn materialization_cap_smaller_than_a_selected_range_fails_closed() {
    let mut fixture = fixture();
    let durable_before = durable_exact_source_state(&fixture.connection);
    let authorization =
        DeepV2SourceMaterializationAuthorization::new(END_BYTE - START_BYTE - 1).unwrap();

    let error = seal_deep_context_ir_v2_with_live_sources(
        &mut fixture.connection,
        &fixture.workspace,
        fixture.document,
        Some(authorization),
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("exceeds its authorized byte bound"),
        "{error}"
    );
    assert_eq!(
        durable_exact_source_state(&fixture.connection),
        durable_before,
        "a rejected materialization cap must not persist lifecycle state"
    );
}

#[test]
fn source_drift_seals_blocked_stale_ir_without_durable_telemetry() {
    let mut fixture = fixture();
    let temporal_before = fixture.document.temporal_constraints.clone();
    let durable_before = durable_exact_source_state(&fixture.connection);
    std::fs::write(
        fixture.workspace_directory.path().join(SOURCE_PATH),
        b"source drift\n",
    )
    .unwrap();

    let output = seal_deep_context_ir_v2_with_live_sources(
        &mut fixture.connection,
        &fixture.workspace,
        fixture.document,
        Some(DeepV2SourceMaterializationAuthorization::new(END_BYTE - START_BYTE).unwrap()),
    )
    .unwrap();

    let source = output.context_ir.working_set[0].source.as_ref().unwrap();
    assert_eq!(source.verification_status, IrSourceVerificationV2::Stale);
    assert_eq!(
        output.context_ir.status.exact_source,
        IrRequirementStateV2::Stale
    );
    assert_eq!(
        output.context_ir.status.working_set_status,
        WorkingSetStatus::Blocked
    );
    assert_eq!(
        serde_json::to_value(&output.context_ir.temporal_constraints).unwrap(),
        serde_json::to_value(temporal_before).unwrap()
    );
    assert!(output.materialization.is_none());
    assert_eq!(
        durable_exact_source_state(&fixture.connection),
        durable_before,
        "V2 source drift status must remain transient"
    );
}

#[test]
fn invalid_byte_range_fails_closed_without_false_source_telemetry() {
    let mut fixture = fixture();
    let source = fixture.document.working_set[0].source.as_mut().unwrap();
    source.end_byte = SOURCE_BYTES.len() as u64 + 1;
    fixture.document.working_set[0].cost.source_bytes = source.end_byte - source.start_byte;
    fixture.document.cost.selected_source_bytes = fixture.document.working_set[0].cost.source_bytes;
    let durable_before = durable_exact_source_state(&fixture.connection);

    let error = seal_deep_context_ir_v2_with_live_sources(
        &mut fixture.connection,
        &fixture.workspace,
        fixture.document,
        None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("byte range"), "{error}");

    assert_eq!(
        durable_exact_source_state(&fixture.connection),
        durable_before,
        "invalid V2 ranges must not persist lifecycle state or allocate an ordinal"
    );
}

#[test]
fn failed_secondary_source_yields_partial_ir_when_primary_source_remains_safe() {
    let mut fixture = fixture();
    let durable_before = durable_exact_source_state(&fixture.connection);
    let mut secondary_source = fixture.document.working_set[0].source.clone().unwrap();
    secondary_source.canonical_path = "src/missing.ts".into();
    secondary_source.start_byte = 0;
    secondary_source.end_byte = 2;
    secondary_source.observed_sha256 = None;
    secondary_source.verification_status = IrSourceVerificationV2::Unavailable;
    secondary_source.canonical_root_confinement = IrPathConfinementV2::Unavailable;
    secondary_source.revalidated_before_seal = false;
    fixture.document.working_set[1].source = Some(secondary_source);
    fixture.document.working_set[1].cost.source_bytes = 2;
    fixture.document.cost.selected_source_bytes += 2;

    let output = seal_deep_context_ir_v2_with_live_sources(
        &mut fixture.connection,
        &fixture.workspace,
        fixture.document,
        None,
    )
    .unwrap();

    assert_eq!(
        output.context_ir.status.exact_source,
        IrRequirementStateV2::Satisfied
    );
    assert_eq!(
        output.context_ir.status.working_set_status,
        WorkingSetStatus::Partial
    );
    assert!(output.context_ir.working_set[1].source.is_none());
    assert_eq!(output.context_ir.working_set[1].cost.source_bytes, 0);
    assert_eq!(
        output.context_ir.cost.selected_source_bytes,
        END_BYTE - START_BYTE
    );
    assert!(output.materialization.is_none());
    assert_eq!(
        durable_exact_source_state(&fixture.connection),
        durable_before,
        "partial V2 verification results must remain transient"
    );
}

#[test]
fn non_committed_active_pointer_fails_closed_as_generation_unavailable() {
    let mut fixture = fixture();
    fixture
        .connection
        .execute(
            "UPDATE index_generation SET state = 'failed' WHERE generation_id = ?1",
            [&fixture.document.workspace.generation_id],
        )
        .unwrap();

    let error = seal_deep_context_ir_v2_with_live_sources(
        &mut fixture.connection,
        &fixture.workspace,
        fixture.document,
        None,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        workspace_atlas::error::AtlasError::GenerationStateInvalid { .. }
    ));
}
#[test]
fn path_escape_fails_closed_without_materializing_external_bytes() {
    let mut fixture = fixture();
    let outside_name = format!(
        "{}-outside-materialization.txt",
        fixture
            .workspace_directory
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
    );
    let outside = fixture
        .workspace_directory
        .path()
        .parent()
        .unwrap()
        .join(&outside_name);
    std::fs::write(&outside, SOURCE_BYTES).unwrap();
    let escaping_path = format!("../{outside_name}");
    let changed = fixture
        .connection
        .execute(
            "UPDATE generation_file
             SET canonical_path = ?1
             WHERE generation_id = ?2 AND canonical_path = ?3",
            rusqlite::params![
                escaping_path,
                fixture.document.workspace.generation_id,
                SOURCE_PATH
            ],
        )
        .unwrap();
    assert_eq!(changed, 1);
    fixture.document.working_set[0]
        .source
        .as_mut()
        .unwrap()
        .canonical_path = escaping_path.clone();
    let indexed_escape: Option<String> = fixture
        .connection
        .query_row(
            "SELECT content_hash FROM current_file
             WHERE generation_id = ?1 AND canonical_path = ?2",
            rusqlite::params![fixture.document.workspace.generation_id, escaping_path],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(
        indexed_escape.as_deref(),
        Some(content_hash_of_bytes(SOURCE_BYTES).as_str()),
        "the confinement regression must reach canonical-root verification after indexed lookup"
    );
    let durable_before = durable_exact_source_state(&fixture.connection);

    let output = seal_deep_context_ir_v2_with_live_sources(
        &mut fixture.connection,
        &fixture.workspace,
        fixture.document,
        Some(DeepV2SourceMaterializationAuthorization::new(END_BYTE - START_BYTE).unwrap()),
    )
    .unwrap();
    std::fs::remove_file(outside).unwrap();

    assert_eq!(
        output.context_ir.status.working_set_status,
        WorkingSetStatus::Blocked
    );
    assert_eq!(
        output.context_ir.status.exact_source,
        IrRequirementStateV2::Unavailable
    );
    let escaped_source = output
        .context_ir
        .working_set
        .iter()
        .filter_map(|item| item.source.as_ref())
        .find(|source| source.canonical_path == escaping_path)
        .expect("indexed escaping source remains attributable in the blocked IR");
    assert_eq!(
        escaped_source.canonical_root_confinement,
        IrPathConfinementV2::SymlinkEscape
    );
    assert_eq!(
        escaped_source.verification_status,
        IrSourceVerificationV2::Unavailable
    );
    assert!(output.materialization.is_none());
    assert_eq!(
        durable_exact_source_state(&fixture.connection),
        durable_before
    );
}

#[test]
fn generation_drift_fails_closed_before_any_source_is_sealed() {
    let mut fixture = fixture();
    fixture.document.workspace.generation_id = "different_generation".into();

    let error = seal_deep_context_ir_v2_with_live_sources(
        &mut fixture.connection,
        &fixture.workspace,
        fixture.document,
        Some(DeepV2SourceMaterializationAuthorization::new(END_BYTE - START_BYTE).unwrap()),
    )
    .unwrap_err();

    assert!(matches!(
        error,
        workspace_atlas::error::AtlasError::ActiveGenerationMismatch { .. }
    ));
}
