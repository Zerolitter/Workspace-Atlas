//! `atlas-mcp` — stdio JSON-RPC 2.0 adapter (ADR-017).
//!
//! Transport is one JSON-RPC message per line on stdin and one response per
//! line on stdout. No indexing or ranking logic lives in this binary: every
//! tool dispatches to the same application functions used by the `atlas` CLI.
//!
//! Tools: `atlas_status`, `atlas_find`, `atlas_inspect`, `atlas_trace`,
//! `atlas_impact`, `atlas_source`, `atlas_context`, `atlas_history`,
//! `atlas_providers`, `atlas_context_ir`, `atlas_serving_build`,
//! `atlas_generation_delta`, `atlas_temporal`, `atlas_governor_capabilities`,
//! `atlas_governor_run`, `atlas_task_show`, `atlas_compiled_context_show`,
//! `atlas_context_yield_show`, `atlas_serving_status`.
//! Resources: `atlas://catalogues/<workspace_id>/status`.

use std::borrow::Cow;
use std::future::{ready, Future};
use std::path::PathBuf;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ErrorCode, ErrorData, Implementation,
    JsonRpcMessage, ListResourceTemplatesResult, ListToolsResult, MetaObject,
    PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, ResourceContents, ResourceTemplate, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::schemars::{self, JsonSchema};
use rmcp::service::{
    serve_directly, MaybeSendFuture, RequestContext, RoleServer, RxJsonRpcMessage, TxJsonRpcMessage,
};
use rmcp::transport::Transport;
use rmcp::{ServerHandler, ServiceExt};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use workspace_atlas::cli;
use workspace_atlas::error::AtlasError;
use workspace_atlas::mcp_adapter;
use workspace_atlas::{
    catalogue, config::Config, context_application, serving, task_session, workspace,
};

/// Contract versions this adapter implements (ADR-017).
const NORMALIZED_INDEX_BATCH_SCHEMA_VERSION: &str = "1.0.0";
const CONTEXT_PACKET_SCHEMA_VERSION: &str = "1.0.0";
const LIFECYCLE_EVENT_SCHEMA_VERSION: &str = "1.0.0";
const ATLAS_CAPABILITY_PROTOCOL_VERSION: &str = "1.3";

fn capability_versions() -> Value {
    json!({
        "normalized_index_batch_schema_version": NORMALIZED_INDEX_BATCH_SCHEMA_VERSION,
        "context_packet_schema_version": CONTEXT_PACKET_SCHEMA_VERSION,
        "lifecycle_event_schema_version": LIFECYCLE_EVENT_SCHEMA_VERSION,
        "temporal_report_schema_version": workspace_atlas::temporal::TEMPORAL_SCHEMA_VERSION,
        "atlas_capability_protocol_version": ATLAS_CAPABILITY_PROTOCOL_VERSION,
    })
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WorkspaceArgs {
    workspace_root: String,
    catalogue: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FindArgs {
    workspace_root: String,
    catalogue: Option<String>,
    query: String,
    task_session_id: Option<String>,
    #[schemars(range(min = 1))]
    limit: Option<i64>,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InspectArgs {
    workspace_root: String,
    catalogue: Option<String>,
    path: Option<String>,
    symbol: Option<String>,
    task_session_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TraceDirection {
    Outbound,
    Inbound,
    Both,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TraceArgs {
    workspace_root: String,
    catalogue: Option<String>,
    path: String,
    direction: Option<TraceDirection>,
    #[schemars(range(min = 1))]
    max_depth: Option<i64>,
    #[schemars(range(min = 1))]
    max_records: Option<i64>,
    task_session_id: Option<String>,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImpactArgs {
    workspace_root: String,
    catalogue: Option<String>,
    #[schemars(length(min = 1))]
    paths: Vec<String>,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SourceArgs {
    workspace_root: String,
    catalogue: Option<String>,
    path: String,
    task_session_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ContextMode {
    Map,
    Change,
    Audit,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ContextArgs {
    workspace_root: String,
    catalogue: Option<String>,
    task: String,
    mode: Option<ContextMode>,
    #[serde(default, rename = "path")]
    paths: Vec<String>,
    #[serde(default, rename = "symbol")]
    symbols: Vec<String>,
    #[schemars(range(min = 1))]
    max_records: Option<i64>,
    #[schemars(range(min = 1))]
    max_source_bytes: Option<i64>,
    #[schemars(range(min = 1))]
    max_token_estimate: Option<i64>,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HistoryArgs {
    workspace_root: String,
    catalogue: Option<String>,
    path: Option<String>,
    #[schemars(range(min = 1))]
    limit: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ContextTaskKind {
    Explore,
    BugFix,
    BehaviorChange,
    ApiChange,
    Refactor,
    ConfigurationChange,
    TestChange,
    Review,
    Audit,
    Unknown,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ContextIrArgs {
    workspace_root: String,
    catalogue: Option<String>,
    task: String,
    task_kind: Option<ContextTaskKind>,
    #[serde(default, rename = "path")]
    paths: Vec<String>,
    #[serde(default, rename = "symbol")]
    symbols: Vec<String>,
    #[schemars(range(min = 1))]
    max_records: Option<i64>,
    #[schemars(range(min = 1))]
    max_source_bytes: Option<i64>,
    #[schemars(range(min = 1))]
    max_estimated_tokens: Option<i64>,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GenerationDeltaArgs {
    workspace_root: String,
    catalogue: Option<String>,
    from: String,
    to: String,
}

#[allow(dead_code)]
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TemporalArgs {
    workspace_root: String,
    catalogue: Option<String>,
    from: Option<String>,
    #[schemars(range(min = 1))]
    max_records: Option<i64>,
}

fn schema_object<T: JsonSchema>() -> Map<String, Value> {
    serde_json::to_value(schemars::schema_for!(T))
        .expect("generated schema serializes")
        .as_object()
        .expect("tool input schema is an object")
        .clone()
}

fn tool<T: JsonSchema>(name: &'static str) -> Tool {
    Tool::new(
        name,
        format!(
            "Same input/output contract as `atlas {}`.",
            name.trim_start_matches("atlas_")
        ),
        schema_object::<T>(),
    )
}

fn inspect_tool() -> Tool {
    let mut schema = schema_object::<InspectArgs>();
    schema.insert(
        "anyOf".into(),
        json!([{"required": ["path"]}, {"required": ["symbol"]}]),
    );
    Tool::new(
        "atlas_inspect",
        "Same input/output contract as `atlas inspect`.",
        schema,
    )
}

fn tool_definitions() -> &'static [Tool] {
    static TOOLS: std::sync::LazyLock<Vec<Tool>> = std::sync::LazyLock::new(|| {
        let mut tools = vec![
            tool::<WorkspaceArgs>("atlas_status"),
            tool::<FindArgs>("atlas_find"),
            inspect_tool(),
            tool::<TraceArgs>("atlas_trace"),
            tool::<ImpactArgs>("atlas_impact"),
            tool::<SourceArgs>("atlas_source"),
            tool::<ContextArgs>("atlas_context"),
            tool::<HistoryArgs>("atlas_history"),
            tool::<WorkspaceArgs>("atlas_providers"),
            tool::<ContextIrArgs>("atlas_context_ir"),
            tool::<WorkspaceArgs>("atlas_serving_build"),
            tool::<GenerationDeltaArgs>("atlas_generation_delta"),
            tool::<TemporalArgs>("atlas_temporal"),
        ];
        tools.extend_from_slice(mcp_adapter::compiler_tool_definitions());
        tools
    });
    &TOOLS
}

fn validate_as<T: for<'de> Deserialize<'de>>(arguments: &Value) -> Result<(), RpcError> {
    T::deserialize(arguments)
        .map(|_| ())
        .map_err(|error| RpcError::invalid_params(error.to_string()))
}

fn parse_as<T: for<'de> Deserialize<'de>>(arguments: &Value) -> Result<T, RpcError> {
    T::deserialize(arguments).map_err(|error| RpcError::invalid_params(error.to_string()))
}

fn validate_tool_arguments(name: &str, arguments: &Value) -> Result<(), RpcError> {
    match name {
        "atlas_status" | "atlas_providers" | "atlas_serving_build" => {
            validate_as::<WorkspaceArgs>(arguments)
        }
        "atlas_find" => validate_as::<FindArgs>(arguments),
        "atlas_inspect" => validate_as::<InspectArgs>(arguments),
        "atlas_trace" => validate_as::<TraceArgs>(arguments),
        "atlas_impact" => validate_as::<ImpactArgs>(arguments),
        "atlas_source" => validate_as::<SourceArgs>(arguments),
        "atlas_context" => validate_as::<ContextArgs>(arguments),
        "atlas_history" => validate_as::<HistoryArgs>(arguments),
        "atlas_context_ir" => validate_as::<ContextIrArgs>(arguments),
        "atlas_generation_delta" => validate_as::<GenerationDeltaArgs>(arguments),
        "atlas_temporal" => validate_as::<TemporalArgs>(arguments),
        "atlas_governor_capabilities"
        | "atlas_governor_run"
        | "atlas_task_show"
        | "atlas_compiled_context_show"
        | "atlas_context_yield_show"
        | "atlas_serving_status" => Ok(()),
        other => Err(RpcError::method_not_found(other)),
    }
}

/// A typed JSON-RPC error. `kind` is always present in `data.kind` so every
/// error response is machine-distinguishable.
struct RpcError {
    code: i64,
    kind: &'static str,
    message: String,
}

impl RpcError {
    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            kind: "method_not_found",
            message: format!("unknown method or tool: {method}"),
        }
    }
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            kind: "invalid_params",
            message: message.into(),
        }
    }
    fn budget_invalid(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            kind: "budget_invalid",
            message: message.into(),
        }
    }
    fn from_atlas(e: AtlasError) -> Self {
        Self {
            code: -32000,
            kind: cli::error_kind(&e),
            message: e.to_string(),
        }
    }

    fn from_application(error: context_application::CatalogueContextApplicationError) -> Self {
        use context_application::{
            ApplicationBoundaryError, ApplicationCursorError, CatalogueContextApplicationError,
        };
        use workspace_atlas::context_route::ContextRouteError;
        use workspace_atlas::task_session::LegacyLifecycleError;

        let kind = match &error {
            CatalogueContextApplicationError::Route(error) => match error {
                ContextRouteError::UnsupportedVersion { .. } => "unsupported_version",
                ContextRouteError::NoCommonVersion { .. } => "no_common_version",
                ContextRouteError::FloorAboveCeiling => "floor_above_ceiling",
                ContextRouteError::IntentFloorConflict => "intent_floor_conflict",
                ContextRouteError::IntentCeilingConflict => "intent_ceiling_conflict",
                ContextRouteError::DeepBudgetRequired => "deep_budget_required",
                ContextRouteError::InvalidDeepBudget => "invalid_deep_budget",
                ContextRouteError::InvalidDeepBudgetDigest => "invalid_deep_budget_digest",
                ContextRouteError::InvalidIdentity => "invalid_identity",
                ContextRouteError::InvalidLightOperations => "invalid_light_operations",
                ContextRouteError::UnknownProfile => "unknown_profile",
                ContextRouteError::InvalidExecution => "invalid_execution",
            },
            CatalogueContextApplicationError::Atlas(error) => cli::error_kind(error),
            CatalogueContextApplicationError::Cursor(error) => match error {
                ApplicationCursorError::Invalid => "cursor_invalid",
                ApplicationCursorError::Stale => "cursor_stale",
                ApplicationCursorError::Atlas(error) => cli::error_kind(error),
                ApplicationCursorError::Sqlite(_) => "sqlite",
            },
            CatalogueContextApplicationError::Lifecycle(error) => match error {
                LegacyLifecycleError::Atlas(error) => cli::error_kind(error),
                LegacyLifecycleError::Sqlite(_) => "sqlite",
                LegacyLifecycleError::DurableContractUnavailable => "durable_contract_unavailable",
                LegacyLifecycleError::CursorInvalid => "cursor_invalid",
                LegacyLifecycleError::CursorStale => "cursor_stale",
            },
            CatalogueContextApplicationError::Sqlite(_) => "sqlite",
            CatalogueContextApplicationError::Serde(_) => "serde",
            CatalogueContextApplicationError::Boundary(error) => match error {
                ApplicationBoundaryError::LiteralConfirmation { .. } => "confirmation_required",
                ApplicationBoundaryError::ManifestMismatch { .. } => "manifest_mismatch",
                ApplicationBoundaryError::UnregisterUnavailable { .. } => "unregister_unavailable",
            },
        };
        let message = capped_additive_error(error.to_string());
        Self {
            code: -32000,
            kind,
            message,
        }
    }
}

fn capped_additive_error(mut message: String) -> String {
    const MAX_BYTES: usize = 1024;
    if message.len() <= MAX_BYTES {
        return message;
    }
    let mut end = MAX_BYTES;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message
}

struct ApplicationContext {
    connection: rusqlite::Connection,
    workspace: workspace::WorkspaceRecord,
}

fn application_context(
    workspace_root: &std::path::Path,
    catalogue_override: Option<&std::path::Path>,
) -> Result<ApplicationContext, RpcError> {
    let status = cli::build_status_output(workspace_root, catalogue_override)
        .map_err(RpcError::from_atlas)?;
    let catalogue_path = catalogue_override
        .map_or_else(
            || workspace_atlas::hashing::default_catalogue_dir(&status.workspace_id),
            |path| Ok(path.to_path_buf()),
        )
        .map_err(RpcError::from_atlas)?;
    let config =
        Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"_internal\"\n")
            .expect("static MCP config is valid");
    let connection =
        catalogue::open_connection(&catalogue_path, &config).map_err(RpcError::from_atlas)?;
    let workspace = workspace::load_workspace(&connection, &status.workspace_id)
        .map_err(RpcError::from_atlas)?
        .ok_or_else(|| {
            RpcError::from_atlas(AtlasError::WorkspaceNotFound {
                workspace_id: status.workspace_id,
            })
        })?;
    Ok(ApplicationContext {
        connection,
        workspace,
    })
}

fn get_str(v: &Value, key: &str) -> Result<String, RpcError> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| RpcError::invalid_params(format!("missing required string param: {key}")))
}

fn get_opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

fn get_optional_str(v: &Value, key: &str) -> Result<Option<String>, RpcError> {
    match v.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(RpcError::invalid_params(format!(
            "{key} must be a string when provided"
        ))),
    }
}

fn get_str_vec(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn get_i64_or(v: &Value, key: &str, default: i64) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(default)
}

/// A bound (limit/max_records/...) must be positive -- a zero or negative
/// budget is a client error, not a silently-degraded empty result.
fn require_positive(name: &str, n: i64) -> Result<i64, RpcError> {
    if n <= 0 {
        return Err(RpcError::budget_invalid(format!(
            "{name} must be > 0, got {n}"
        )));
    }
    Ok(n)
}

fn catalogue_override(v: &Value) -> Option<PathBuf> {
    get_opt_str(v, "catalogue").map(PathBuf::from)
}

fn call_tool(name: &str, args: &Value) -> Result<Value, RpcError> {
    validate_tool_arguments(name, args)?;
    let workspace_root = PathBuf::from(get_str(args, "workspace_root")?);
    let cat = catalogue_override(args);

    let value = match name {
        "atlas_status" => {
            let out = cli::build_status_output(&workspace_root, cat.as_deref())
                .map_err(RpcError::from_atlas)?;
            serde_json::to_value(out)
        }
        "atlas_find" => {
            let query = get_str(args, "query")?;
            let limit = require_positive("limit", get_i64_or(args, "limit", 50))?;
            let task_session_id = get_optional_str(args, "task_session_id")?;
            let out = cli::build_find_output_with_task_session(
                &workspace_root,
                &query,
                limit,
                task_session_id.as_deref(),
                cat.as_deref(),
            )
            .map_err(RpcError::from_atlas)?;
            serde_json::to_value(out)
        }
        "atlas_inspect" => {
            let path = get_opt_str(args, "path");
            let symbol = get_opt_str(args, "symbol");
            let task_session_id = get_optional_str(args, "task_session_id")?;
            let out = match (path, symbol) {
                (_, Some(symbol)) => cli::build_inspect_symbol_output_with_task_session(
                    &workspace_root,
                    &symbol,
                    task_session_id.as_deref(),
                    cat.as_deref(),
                ),
                (Some(path), None) => cli::build_inspect_output_with_task_session(
                    &workspace_root,
                    &path,
                    task_session_id.as_deref(),
                    cat.as_deref(),
                ),
                (None, None) => {
                    return Err(RpcError::invalid_params(
                        "atlas_inspect requires either 'path' or 'symbol'",
                    ));
                }
            }
            .map_err(RpcError::from_atlas)?;
            serde_json::to_value(out)
        }
        "atlas_trace" => {
            let path = get_str(args, "path")?;
            let direction =
                get_opt_str(args, "direction").unwrap_or_else(|| "outbound".to_string());
            let max_depth = require_positive("max_depth", get_i64_or(args, "max_depth", 3))?;
            let max_records =
                require_positive("max_records", get_i64_or(args, "max_records", 200))?;
            let task_session_id = get_optional_str(args, "task_session_id")?;
            let out = cli::build_trace_output_with_task_session(
                &workspace_root,
                &path,
                &direction,
                max_depth,
                max_records,
                task_session_id.as_deref(),
                cat.as_deref(),
            )
            .map_err(RpcError::from_atlas)?;
            serde_json::to_value(out)
        }
        "atlas_impact" => {
            let paths = get_str_vec(args, "paths");
            if paths.is_empty() {
                return Err(RpcError::invalid_params(
                    "paths must be a non-empty array of strings",
                ));
            }
            let out = cli::build_impact_output(&workspace_root, &paths, cat.as_deref())
                .map_err(RpcError::from_atlas)?;
            serde_json::to_value(out)
        }
        "atlas_source" => {
            let path = get_str(args, "path")?;
            let task_session_id = get_optional_str(args, "task_session_id")?;
            let out = cli::build_source_output_with_task_session(
                &workspace_root,
                &path,
                task_session_id.as_deref(),
                cat.as_deref(),
            )
            .map_err(RpcError::from_atlas)?;
            serde_json::to_value(out)
        }
        "atlas_context" => {
            let task = get_str(args, "task")?;
            let mode = get_opt_str(args, "mode").unwrap_or_else(|| "map".to_string());
            let paths = get_str_vec(args, "path");
            let symbols = get_str_vec(args, "symbol");
            let max_records = require_positive("max_records", get_i64_or(args, "max_records", 40))?;
            let max_source_bytes = require_positive(
                "max_source_bytes",
                get_i64_or(args, "max_source_bytes", 32768),
            )?;
            let max_token_estimate = require_positive(
                "max_token_estimate",
                get_i64_or(args, "max_token_estimate", 8000),
            )?;
            let out = cli::build_context_output(
                &workspace_root,
                task,
                &mode,
                paths,
                symbols,
                max_records,
                max_source_bytes,
                max_token_estimate,
                cat.as_deref(),
            )
            .map_err(RpcError::from_atlas)?;
            serde_json::to_value(out)
        }
        "atlas_history" => {
            let path = get_opt_str(args, "path");
            let limit = require_positive("limit", get_i64_or(args, "limit", 50))?;
            let out =
                cli::build_history_output(&workspace_root, path.as_deref(), limit, cat.as_deref())
                    .map_err(RpcError::from_atlas)?;
            serde_json::to_value(out)
        }
        "atlas_providers" => {
            let out = cli::build_providers_output(&workspace_root, cat.as_deref())
                .map_err(RpcError::from_atlas)?;
            serde_json::to_value(out)
        }
        "atlas_context_ir" => {
            let task = get_str(args, "task")?;
            let task_kind = get_opt_str(args, "task_kind");
            let paths = get_str_vec(args, "path");
            let symbols = get_str_vec(args, "symbol");
            let max_records = require_positive("max_records", get_i64_or(args, "max_records", 40))?;
            let max_source_bytes = require_positive(
                "max_source_bytes",
                get_i64_or(args, "max_source_bytes", 32768),
            )?;
            let max_estimated_tokens = require_positive(
                "max_estimated_tokens",
                get_i64_or(args, "max_estimated_tokens", 8000),
            )?;
            let out = cli::build_context_ir_output(
                &workspace_root,
                task,
                task_kind,
                paths,
                symbols,
                max_records,
                max_source_bytes,
                max_estimated_tokens,
                cat.as_deref(),
            )
            .map_err(RpcError::from_atlas)?;
            serde_json::to_value(out)
        }
        "atlas_serving_build" => {
            let out = cli::build_serving_build_output(&workspace_root, cat.as_deref())
                .map_err(RpcError::from_atlas)?;
            serde_json::to_value(out)
        }
        "atlas_generation_delta" => {
            let from = get_str(args, "from")?;
            let to = get_str(args, "to")?;
            let out =
                cli::build_generation_delta_output(&workspace_root, &from, &to, cat.as_deref())
                    .map_err(RpcError::from_atlas)?;
            serde_json::to_value(out)
        }
        "atlas_temporal" => {
            let from = get_optional_str(args, "from")?;
            let max_records = require_positive("max_records", get_i64_or(args, "max_records", 50))?;
            let out = cli::build_temporal_output(
                &workspace_root,
                from.as_deref(),
                max_records,
                cat.as_deref(),
            )
            .map_err(RpcError::from_atlas)?;
            serde_json::to_value(out)
        }
        "atlas_governor_capabilities" => {
            let _: mcp_adapter::CompilerWorkspaceArguments = parse_as(args)?;
            cli::build_status_output(&workspace_root, cat.as_deref())
                .map_err(RpcError::from_atlas)?;
            serde_json::to_value(workspace_atlas::context_route::discover_context_capabilities())
        }
        "atlas_governor_run" => {
            let request =
                mcp_adapter::GovernorRunArguments::parse(args).map_err(RpcError::invalid_params)?;
            let mut context = application_context(&workspace_root, cat.as_deref())?;
            let out = context_application::run_catalogue_governor(
                request.into_application_request(),
                &mut context.connection,
                &context.workspace,
            )
            .map_err(RpcError::from_application)?;
            serde_json::to_value(out)
        }
        "atlas_task_show" => {
            let request: mcp_adapter::TaskShowArguments = parse_as(args)?;
            let context = application_context(&workspace_root, cat.as_deref())?;
            let out = task_session::show_legacy_task_session(
                &context.connection,
                &context.workspace,
                &task_session::LegacyTaskShowRequest {
                    context_ir_version: task_session::LEGACY_LIFECYCLE_CONTRACT_VERSION.to_string(),
                    task_session_id: request.task_session_id.into_inner(),
                    limit: request.limit,
                    cursor: request.cursor,
                },
            )
            .map_err(|error| RpcError::from_application(error.into()))?;
            serde_json::to_value(out)
        }
        "atlas_compiled_context_show" => {
            let request: mcp_adapter::CompiledContextShowArguments = parse_as(args)?;
            let context = application_context(&workspace_root, cat.as_deref())?;
            let out = context_application::show_compiled_context(
                &context.connection,
                &context.workspace,
                &context_application::CompiledContextShowRequest {
                    context_id: request
                        .context_id
                        .map(mcp_adapter::AdvertisedIdentifier::into_inner),
                    task_session_id: request
                        .task_session_id
                        .map(mcp_adapter::LegacyTaskSessionIdentifier::into_inner),
                    limit: request.limit,
                    cursor: request.cursor,
                },
            )
            .map_err(RpcError::from_application)?;
            serde_json::to_value(out)
        }
        "atlas_context_yield_show" => {
            let request: mcp_adapter::TaskShowArguments = parse_as(args)?;
            let context = application_context(&workspace_root, cat.as_deref())?;
            let out = context_application::show_context_yield(
                &context.connection,
                &context.workspace,
                &task_session::LegacyTaskShowRequest {
                    context_ir_version: task_session::LEGACY_LIFECYCLE_CONTRACT_VERSION.to_string(),
                    task_session_id: request.task_session_id.into_inner(),
                    limit: request.limit,
                    cursor: request.cursor,
                },
            )
            .map_err(RpcError::from_application)?;
            serde_json::to_value(out)
        }
        "atlas_serving_status" => {
            let request: mcp_adapter::ServingStatusArguments = parse_as(args)?;
            let context = application_context(&workspace_root, cat.as_deref())?;
            let out = serving::serving_status_page(
                &context.connection,
                &context.workspace,
                request
                    .limit
                    .unwrap_or(workspace_atlas::context_route::DEFAULT_APPLICATION_PAGE_LIMIT),
                request.cursor.as_deref(),
            )
            .map_err(RpcError::from_application)?;
            serde_json::to_value(out)
        }
        other => return Err(RpcError::method_not_found(other)),
    };
    value.map_err(|e| RpcError::from_atlas(AtlasError::Serde(e)))
}

fn read_resource(uri: &str) -> Result<Value, RpcError> {
    // atlas://catalogues/<workspace_id>/status
    let rest = uri
        .strip_prefix("atlas://catalogues/")
        .ok_or_else(|| RpcError::invalid_params(format!("unsupported resource uri: {uri}")))?;
    let mut parts = rest.splitn(2, '/');
    let workspace_id = parts.next().unwrap_or("");
    let sub = parts.next().unwrap_or("");
    if workspace_id.is_empty() || sub != "status" {
        return Err(RpcError::invalid_params(format!(
            "unsupported resource uri: {uri} (expected atlas://catalogues/<workspace_id>/status)"
        )));
    }
    let out = cli::status_by_workspace_id(workspace_id).map_err(RpcError::from_atlas)?;
    serde_json::to_value(out).map_err(|e| RpcError::from_atlas(AtlasError::Serde(e)))
}

#[derive(Clone, Copy)]
struct AtlasApplication;

fn rpc_error(error: RpcError) -> ErrorData {
    ErrorData::new(
        ErrorCode(error.code as i32),
        error.message,
        Some(json!({"kind": error.kind})),
    )
}

impl ServerHandler for AtlasApplication {
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(mcp_adapter::supported_protocol_versions().into())
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("atlas-mcp", env!("CARGO_PKG_VERSION")));
        let mut metadata = Map::new();
        metadata.insert(
            "com.workspace-atlas/capabilities".into(),
            capability_versions(),
        );
        info.meta = Some(MetaObject(metadata));
        info
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        ready(Ok(ListToolsResult::with_all_items(
            tool_definitions().to_vec(),
        )))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tool_definitions()
            .iter()
            .find(|tool| tool.name == name)
            .cloned()
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + MaybeSendFuture + '_ {
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        ready(
            call_tool(&request.name, &arguments)
                .map(CallToolResult::structured)
                .map(CallToolResponse::Complete)
                .map_err(rpc_error),
        )
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, ErrorData>> + MaybeSendFuture + '_
    {
        let template = ResourceTemplate::new(
            "atlas://catalogues/{workspace_id}/status",
            "workspace status",
        )
        .with_description("Same result as atlas_status, addressed by workspace_id alone.")
        .with_mime_type("application/json");
        ready(Ok(ListResourceTemplatesResult::with_all_items(vec![
            template,
        ])))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResponse, ErrorData>> + MaybeSendFuture + '_ {
        let uri = request.uri;
        ready(read_resource(&uri).map_or_else(
            |error| Err(rpc_error(error)),
            |value| {
                Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
                    vec![ResourceContents::text(value.to_string(), uri)
                        .with_mime_type("application/json")],
                )))
            },
        ))
    }
}
const MAX_JSON_RPC_FRAME_BYTES: usize = workspace_atlas::context_route::MAX_GOVERNOR_REQUEST_BYTES;
const MAX_JSON_RPC_BUFFER_BYTES: usize = MAX_JSON_RPC_FRAME_BYTES + 1;

enum FrameRead {
    End,
    Complete,
    Oversized,
}

struct BoundedStdioTransport<R, W> {
    read: BufReader<R>,
    frame: Vec<u8>,
    is_discarding: bool,
    write: Arc<tokio::sync::Mutex<Option<W>>>,
    prefetched: Option<RxJsonRpcMessage<RoleServer>>,
}

impl<R, W> BoundedStdioTransport<R, W>
where
    R: AsyncRead + Send + Unpin,
    W: AsyncWrite + Send + Unpin + 'static,
{
    fn new(read: R, write: W) -> Self {
        Self {
            read: BufReader::new(read),
            frame: Vec::with_capacity(MAX_JSON_RPC_BUFFER_BYTES),
            write: Arc::new(tokio::sync::Mutex::new(Some(write))),
            is_discarding: false,
            prefetched: None,
        }
    }

    async fn read_frame(&mut self) -> std::io::Result<FrameRead> {
        loop {
            let available = self.read.fill_buf().await?;
            if available.is_empty() {
                self.frame.clear();
                self.is_discarding = false;
                return Ok(FrameRead::End);
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(available.len(), |offset| offset + 1);
            let content_len = newline.unwrap_or(available.len());
            if !self.is_discarding {
                let remaining = MAX_JSON_RPC_BUFFER_BYTES.saturating_sub(self.frame.len());
                let copied = content_len.min(remaining);
                self.frame.extend_from_slice(&available[..copied]);
                self.is_discarding = content_len > remaining;
            }
            self.read.consume(consumed);
            if newline.is_some() {
                let delimiter_len = usize::from(self.frame.ends_with(b"\r"));
                let message_len = self.frame.len() - delimiter_len;
                let oversized = std::mem::take(&mut self.is_discarding)
                    || message_len > MAX_JSON_RPC_FRAME_BYTES;
                if oversized {
                    self.frame.clear();
                    return Ok(FrameRead::Oversized);
                }
                return Ok(FrameRead::Complete);
            }
        }
    }

    async fn write_message(
        write: Arc<tokio::sync::Mutex<Option<W>>>,
        message: TxJsonRpcMessage<RoleServer>,
    ) -> std::io::Result<()> {
        let encoded = serde_json::to_vec(&message).map_err(std::io::Error::other)?;
        let mut write = write.lock().await;
        let write = write.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotConnected, "transport is closed")
        })?;
        write.write_all(&encoded).await?;
        write.write_all(b"\n").await?;
        write.flush().await
    }

    async fn next_message(&mut self) -> std::io::Result<Option<RxJsonRpcMessage<RoleServer>>> {
        loop {
            match self.read_frame().await? {
                FrameRead::End => return Ok(None),
                FrameRead::Oversized => continue,
                FrameRead::Complete if self.frame.is_empty() => continue,
                FrameRead::Complete => {}
            }
            let frame = self
                .frame
                .strip_suffix(b"\r")
                .unwrap_or(self.frame.as_slice());
            let frame = frame.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(frame);
            let parsed = serde_json::from_slice(frame);
            self.frame.clear();
            match parsed {
                Ok(message) => return Ok(Some(message)),
                Err(_) => continue,
            }
        }
    }

    async fn prime(&mut self) -> std::io::Result<Option<bool>> {
        let Some(message) = self.next_message().await? else {
            return Ok(None);
        };
        let uses_lifecycle = matches!(
            &message,
            JsonRpcMessage::Request(request)
                if matches!(request.request.method(), "initialize" | "server/discover")
        );
        self.prefetched = Some(message);
        Ok(Some(uses_lifecycle))
    }
}

impl<R, W> Transport<RoleServer> for BoundedStdioTransport<R, W>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        Self::write_message(self.write.clone(), item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        if let Some(message) = self.prefetched.take() {
            return Some(message);
        }
        self.next_message().await.ok().flatten()
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        let mut write = self.write.lock().await;
        if let Some(mut stream) = write.take() {
            stream.flush().await?;
        }
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut transport = BoundedStdioTransport::new(tokio::io::stdin(), tokio::io::stdout());
    let Some(uses_lifecycle) = transport.prime().await? else {
        return Ok(());
    };
    let service = if uses_lifecycle {
        AtlasApplication.serve(transport).await?
    } else {
        serve_directly::<RoleServer, _, _, _, _>(AtlasApplication, transport, None)
    };
    service.waiting().await?;
    Ok(())
}
