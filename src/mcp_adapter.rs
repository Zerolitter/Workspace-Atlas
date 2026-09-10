//! Official MCP SDK boundary.
//!
//! This module owns protocol/lifecycle/schema mechanics only. Application
//! implementations supplied by transports retain Atlas selection, persistence,
//! source-verification, and error policy.

use std::borrow::Cow;
use std::future::{ready, Future};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ErrorCode, Implementation, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::schemars;
use rmcp::service::{MaybeSendFuture, RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler};
use serde::{de::Error as _, Deserialize, Deserializer};

pub const MODERN_PROTOCOL_DATE: &str = "2026-07-28";
pub const LEGACY_PROTOCOL_DATE: &str = "2025-11-25";

pub fn supported_protocol_versions() -> [ProtocolVersion; 2] {
    [ProtocolVersion::V_2026_07_28, ProtocolVersion::V_2025_11_25]
}

pub fn protocol_version(value: &str) -> Option<ProtocolVersion> {
    match value {
        MODERN_PROTOCOL_DATE => Some(ProtocolVersion::V_2026_07_28),
        LEGACY_PROTOCOL_DATE => Some(ProtocolVersion::V_2025_11_25),
        _ => None,
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EmptyArguments {}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompilerWorkspaceArguments {
    pub workspace_root: String,
    pub catalogue: Option<String>,
}

#[derive(Debug, schemars::JsonSchema)]
pub struct NegotiationVersionVector(#[schemars(length(min = 1, max = 8))] Vec<String>);

impl<'de> Deserialize<'de> for NegotiationVersionVector {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = Vec::<String>::deserialize(deserializer)?;
        if !(1..=8).contains(&values.len()) {
            return Err(D::Error::custom(
                "version vector must contain between 1 and 8 items",
            ));
        }
        Ok(Self(values))
    }
}

impl NegotiationVersionVector {
    pub fn into_inner(self) -> Vec<String> {
        self.0
    }
}

#[derive(Debug, schemars::JsonSchema)]
pub struct AdvertisedIdentifier(
    #[schemars(
        length(min = 1, max = 128),
        extend("x-atlas-utf8-byteLength" = {"minimum": 1, "maximum": 128})
    )]
    String,
);

impl<'de> Deserialize<'de> for AdvertisedIdentifier {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() || value.len() > 128 {
            return Err(D::Error::custom(
                "identifier must contain between 1 and 128 UTF-8 bytes",
            ));
        }
        Ok(Self(value))
    }
}

impl AdvertisedIdentifier {
    pub fn into_inner(self) -> String {
        self.0
    }
}

#[derive(Debug, schemars::JsonSchema)]
pub struct LegacyTaskSessionIdentifier(
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[\u0000-\u007F]+$"),
        extend(
            "x-atlas-utf8-byteLength" = {"minimum": 1, "maximum": 128},
            "x-atlas-canonical-grammar" = "ascii"
        )
    )]
    String,
);

impl<'de> Deserialize<'de> for LegacyTaskSessionIdentifier {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() || value.len() > 128 || !value.is_ascii() {
            return Err(D::Error::custom(
                "legacy task session identifier must contain between 1 and 128 ASCII bytes",
            ));
        }
        Ok(Self(value))
    }
}

impl LegacyTaskSessionIdentifier {
    pub fn into_inner(self) -> String {
        self.0
    }
}

#[derive(Debug, schemars::JsonSchema)]
pub struct GovernorTarget(
    #[schemars(
        length(min = 1, max = 512),
        extend("x-atlas-utf8-byteLength" = {"minimum": 1, "maximum": 512})
    )]
    String,
);

impl<'de> Deserialize<'de> for GovernorTarget {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.is_empty() || value.len() > crate::context_route::MAX_GOVERNOR_TARGET_BYTES {
            return Err(D::Error::custom(
                "governor target must contain between 1 and 512 UTF-8 bytes",
            ));
        }
        Ok(Self(value))
    }
}

impl GovernorTarget {
    pub fn into_inner(self) -> String {
        self.0
    }
}

#[derive(Debug, Default, schemars::JsonSchema)]
pub struct GovernorTargets(#[schemars(length(max = 200))] Vec<GovernorTarget>);

impl<'de> Deserialize<'de> for GovernorTargets {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = Vec::<GovernorTarget>::deserialize(deserializer)?;
        if values.len() > crate::context_route::MAX_GOVERNOR_TARGETS {
            return Err(D::Error::custom(
                "governor target vector must contain at most 200 items",
            ));
        }
        Ok(Self(values))
    }
}

impl GovernorTargets {
    pub fn into_inner(self) -> Vec<String> {
        self.0.into_iter().map(GovernorTarget::into_inner).collect()
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NegotiationArguments {
    pub capability_versions: NegotiationVersionVector,
    pub route_versions: NegotiationVersionVector,
    pub decision_versions: NegotiationVersionVector,
    pub execution_versions: NegotiationVersionVector,
    pub ir_versions: NegotiationVersionVector,
    pub operation_versions: NegotiationVersionVector,
}

impl From<NegotiationArguments> for crate::context_route::NegotiationRequest {
    fn from(value: NegotiationArguments) -> Self {
        Self {
            capability_versions: value.capability_versions.into_inner(),
            route_versions: value.route_versions.into_inner(),
            decision_versions: value.decision_versions.into_inner(),
            execution_versions: value.execution_versions.into_inner(),
            ir_versions: value.ir_versions.into_inner(),
            operation_versions: value.operation_versions.into_inner(),
        }
    }
}

#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TaskKindArgument {
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
#[derive(Ord, PartialOrd, Eq, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum RequiredCapabilityArgument {
    IdentityLookup,
    ExactSource,
    BoundedRelationships,
    ImpactFrontier,
    BasicCoverageConflicts,
    TemporalHistory,
    RequiredRoleClosure,
    ValidationPlan,
}

#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum CallerCapabilityStateArgument {
    Satisfied,
    Unsatisfied,
    Unknown,
}

#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum AtlasIntentArgument {
    None,
    Allow,
    Require,
}

#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
enum RouteArgument {
    #[serde(rename = "DIRECT")]
    Direct,
    #[serde(rename = "ATLAS_LIGHT")]
    AtlasLight,
    #[serde(rename = "ATLAS_DEEP")]
    AtlasDeep,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GovernorDeepLimitArguments {
    #[schemars(range(min = 1, max = 100000))]
    pub max_records: u64,
    #[schemars(range(min = 1, max = 16777216))]
    pub max_source_bytes: u64,
    #[schemars(range(min = 1, max = 4000000))]
    pub max_estimated_tokens: u64,
    #[schemars(range(min = 1, max = 64))]
    pub max_relationship_depth: u32,
    #[schemars(range(min = 1, max = 10000000))]
    pub max_work_units: u64,
    #[schemars(range(min = 1, max = 100))]
    pub uncertainty_reserve_percent: u8,
}

impl From<GovernorDeepLimitArguments> for crate::context_application::GovernorDeepLimits {
    fn from(value: GovernorDeepLimitArguments) -> Self {
        Self {
            max_records: value.max_records,
            max_source_bytes: value.max_source_bytes,
            max_estimated_tokens: value.max_estimated_tokens,
            max_relationship_depth: value.max_relationship_depth,
            max_work_units: value.max_work_units,
            uncertainty_reserve_percent: value.uncertainty_reserve_percent,
        }
    }
}

#[derive(Debug, schemars::JsonSchema)]
#[schemars(
    extend(
        "x-atlas-aggregate-maxItems" = {
            "fields": ["path_targets", "symbol_targets"],
            "maximum": 200
        }
    )
)]
pub struct GovernorSemanticArguments {
    #[schemars(length(min = 1, max = 1048576))]
    pub task: String,
    #[schemars(with = "Option<TaskKindArgument>")]
    pub declared_kind: Option<crate::context_ir::TaskKind>,
    #[serde(default)]
    pub path_targets: GovernorTargets,
    #[serde(default)]
    pub symbol_targets: GovernorTargets,
    #[schemars(with = "std::collections::BTreeMap<
            RequiredCapabilityArgument,
            CallerCapabilityStateArgument
        >")]
    pub caller_capabilities: std::collections::BTreeMap<
        crate::context_route::RequiredCapability,
        crate::context_route::CallerCapabilityState,
    >,
    #[schemars(with = "AtlasIntentArgument")]
    pub atlas_intent: crate::context_route::AtlasIntent,
    #[schemars(with = "RouteArgument")]
    pub route_floor: crate::context_route::Route,
    #[schemars(with = "RouteArgument")]
    pub route_ceiling: crate::context_route::Route,
    #[serde(default)]
    pub legacy_task_session_id: Option<LegacyTaskSessionIdentifier>,
    #[serde(default)]
    pub deep_limits: Option<GovernorDeepLimitArguments>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GovernorSemanticArgumentsWire {
    task: String,
    declared_kind: Option<crate::context_ir::TaskKind>,
    #[serde(default)]
    path_targets: GovernorTargets,
    #[serde(default)]
    symbol_targets: GovernorTargets,
    caller_capabilities: std::collections::BTreeMap<
        crate::context_route::RequiredCapability,
        crate::context_route::CallerCapabilityState,
    >,
    atlas_intent: crate::context_route::AtlasIntent,
    route_floor: crate::context_route::Route,
    route_ceiling: crate::context_route::Route,
    #[serde(default)]
    legacy_task_session_id: Option<LegacyTaskSessionIdentifier>,
    #[serde(default)]
    deep_limits: Option<GovernorDeepLimitArguments>,
}

impl<'de> Deserialize<'de> for GovernorSemanticArguments {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = GovernorSemanticArgumentsWire::deserialize(deserializer)?;
        let target_count = wire
            .path_targets
            .0
            .len()
            .checked_add(wire.symbol_targets.0.len())
            .ok_or_else(|| D::Error::custom("governor aggregate target count overflow"))?;
        if target_count > crate::context_route::MAX_GOVERNOR_TARGETS {
            return Err(D::Error::custom(
                "path_targets and symbol_targets must contain at most 200 aggregate items",
            ));
        }
        Ok(Self {
            task: wire.task,
            declared_kind: wire.declared_kind,
            path_targets: wire.path_targets,
            symbol_targets: wire.symbol_targets,
            caller_capabilities: wire.caller_capabilities,
            atlas_intent: wire.atlas_intent,
            route_floor: wire.route_floor,
            route_ceiling: wire.route_ceiling,
            legacy_task_session_id: wire.legacy_task_session_id,
            deep_limits: wire.deep_limits,
        })
    }
}

impl From<GovernorSemanticArguments> for crate::context_application::GovernorRunSemanticInput {
    fn from(value: GovernorSemanticArguments) -> Self {
        Self {
            task: value.task,
            declared_kind: value.declared_kind,
            path_targets: value.path_targets.into_inner(),
            symbol_targets: value.symbol_targets.into_inner(),
            caller_capabilities: value.caller_capabilities,
            atlas_intent: value.atlas_intent,
            route_floor: value.route_floor,
            route_ceiling: value.route_ceiling,
            legacy_task_session_id: value
                .legacy_task_session_id
                .map(LegacyTaskSessionIdentifier::into_inner),
            deep_limits: value.deep_limits.map(Into::into),
            materialize_source: false,
            max_materialized_bytes: None,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GovernorRunArguments {
    pub workspace_root: String,
    pub catalogue: Option<String>,
    pub supported_versions: NegotiationArguments,
    pub semantic: GovernorSemanticArguments,
}

impl GovernorRunArguments {
    pub fn parse(arguments: &serde_json::Value) -> Result<Self, String> {
        struct BoundedWriter {
            bytes: usize,
        }

        impl std::io::Write for BoundedWriter {
            fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
                self.bytes = self
                    .bytes
                    .checked_add(buffer.len())
                    .ok_or_else(|| std::io::Error::other("governor arguments size overflow"))?;
                if self.bytes > crate::context_route::MAX_GOVERNOR_REQUEST_BYTES {
                    return Err(std::io::Error::other(
                        "governor arguments exceed 1048576 bytes",
                    ));
                }
                Ok(buffer.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        serde_json::to_writer(&mut BoundedWriter { bytes: 0 }, arguments)
            .map_err(|error| error.to_string())?;
        Self::deserialize(arguments).map_err(|error| error.to_string())
    }

    pub fn into_application_request(self) -> crate::context_application::GovernorRunRequest {
        crate::context_application::GovernorRunRequest {
            supported_versions: self.supported_versions.into(),
            semantic: self.semantic.into(),
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskShowArguments {
    pub workspace_root: String,
    pub catalogue: Option<String>,
    pub task_session_id: LegacyTaskSessionIdentifier,
    #[schemars(range(min = 1, max = 200))]
    pub limit: Option<usize>,
    #[schemars(length(min = 1, max = 4096))]
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompiledContextShowArguments {
    pub workspace_root: String,
    pub catalogue: Option<String>,
    pub context_id: Option<AdvertisedIdentifier>,
    pub task_session_id: Option<LegacyTaskSessionIdentifier>,
    #[schemars(range(min = 1, max = 200))]
    pub limit: Option<usize>,
    #[schemars(length(min = 1, max = 4096))]
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServingStatusArguments {
    pub workspace_root: String,
    pub catalogue: Option<String>,
    #[schemars(range(min = 1, max = 200))]
    pub limit: Option<usize>,
    #[schemars(length(min = 1, max = 4096))]
    pub cursor: Option<String>,
}

fn schema_object<T: schemars::JsonSchema>() -> serde_json::Map<String, serde_json::Value> {
    serde_json::to_value(schemars::schema_for!(T))
        .expect("generated schema serializes")
        .as_object()
        .expect("tool input schema is an object")
        .clone()
}

fn compiler_tool<T: schemars::JsonSchema>(name: &'static str, operation: &str) -> Tool {
    Tool::new(
        name,
        format!("Shared application contract for `atlas {operation}`."),
        schema_object::<T>(),
    )
}

/// The exact additive H6B-A0 MCP registry. Existing tool definitions remain
/// owned by the original adapter so their frozen schemas cannot drift.
pub fn compiler_tool_definitions() -> &'static [Tool] {
    static TOOLS: std::sync::LazyLock<Vec<Tool>> = std::sync::LazyLock::new(|| {
        let mut compiled_context_schema = schema_object::<CompiledContextShowArguments>();
        compiled_context_schema.insert(
            "oneOf".into(),
            serde_json::json!([
                {"required": ["context_id"], "not": {"required": ["task_session_id"]}},
                {"required": ["task_session_id"], "not": {"required": ["context_id"]}}
            ]),
        );
        let compiled_context = Tool::new(
            "atlas_compiled_context_show",
            "Shared application contract for `atlas compiled-context show`.",
            compiled_context_schema,
        );
        vec![
            compiler_tool::<CompilerWorkspaceArguments>(
                "atlas_governor_capabilities",
                "governor capabilities",
            ),
            compiler_tool::<GovernorRunArguments>("atlas_governor_run", "governor run"),
            compiler_tool::<TaskShowArguments>("atlas_task_show", "task show"),
            compiled_context,
            compiler_tool::<TaskShowArguments>("atlas_context_yield_show", "context-yield show"),
            compiler_tool::<ServingStatusArguments>("atlas_serving_status", "serving-status"),
        ]
    });
    &TOOLS
}

/// Application-owned operations exposed through the policy-free SDK service.
pub trait AtlasMcpApplication: Clone + Send + Sync + 'static {
    fn tools(&self) -> Vec<Tool>;

    fn call_tool(&self, request: CallToolRequestParams) -> Result<CallToolResponse, ErrorData>;
}

#[derive(Clone, Default)]
pub struct EmptyApplication;

impl AtlasMcpApplication for EmptyApplication {
    fn tools(&self) -> Vec<Tool> {
        Vec::new()
    }

    fn call_tool(&self, request: CallToolRequestParams) -> Result<CallToolResponse, ErrorData> {
        Err(ErrorData::new(
            ErrorCode::METHOD_NOT_FOUND,
            format!("unknown tool {:?}", request.name),
            None,
        ))
    }
}

#[derive(Clone, Default)]
pub struct AtlasMcpService<A = EmptyApplication> {
    application: A,
}

impl<A> AtlasMcpService<A> {
    pub fn new(application: A) -> Self {
        Self { application }
    }
}

impl<A: AtlasMcpApplication> ServerHandler for AtlasMcpService<A> {
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(supported_protocol_versions().into())
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("workspace-atlas", env!("CARGO_PKG_VERSION")),
        )
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        ready(Ok(ListToolsResult::with_all_items(
            self.application.tools(),
        )))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.application
            .tools()
            .into_iter()
            .find(|tool| tool.name == name)
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + MaybeSendFuture + '_ {
        ready(self.application.call_tool(request))
    }
}
