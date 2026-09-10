use rmcp::model::ProtocolVersion;
use workspace_atlas::mcp_adapter::{
    protocol_version, supported_protocol_versions, LEGACY_PROTOCOL_DATE, MODERN_PROTOCOL_DATE,
};

#[test]
fn sdk_boundary_supports_exactly_the_selected_dual_era_protocols() {
    assert_eq!(MODERN_PROTOCOL_DATE, "2026-07-28");
    assert_eq!(LEGACY_PROTOCOL_DATE, "2025-11-25");
    assert_eq!(
        supported_protocol_versions(),
        [ProtocolVersion::V_2026_07_28, ProtocolVersion::V_2025_11_25,]
    );
    assert_eq!(
        protocol_version(MODERN_PROTOCOL_DATE),
        Some(ProtocolVersion::V_2026_07_28)
    );
    assert_eq!(
        protocol_version(LEGACY_PROTOCOL_DATE),
        Some(ProtocolVersion::V_2025_11_25)
    );
    assert_eq!(protocol_version("2025-06-18"), None);
}

#[test]
fn configured_rmcp_boundary_exposes_server_schema_and_stdio_types() {
    fn require_server<T: rmcp::ServerHandler>() {}
    fn require_schema<T: rmcp::schemars::JsonSchema>() {}

    require_server::<workspace_atlas::mcp_adapter::AtlasMcpService>();
    require_schema::<workspace_atlas::mcp_adapter::EmptyArguments>();
    let _stdio = rmcp::transport::stdio;
}
