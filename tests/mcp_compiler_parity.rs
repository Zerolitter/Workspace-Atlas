use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{json, Value};
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::context_application::{
    run_catalogue_governor, show_compiled_context, show_context_yield, CompiledContextShowRequest,
    GovernorDeepLimits, GovernorRunRequest, GovernorRunSemanticInput,
};
use workspace_atlas::context_ir::CONTEXT_SCHEMA_VERSION;
use workspace_atlas::context_metrics::MAX_CONTEXT_EXECUTION_SOURCE_BYTES;
use workspace_atlas::context_route::{
    discover_context_capabilities, AtlasIntent, CallerCapabilityState, NegotiationRequest,
    RequiredCapability, Route, CONTEXT_CAPABILITIES_VERSION, CONTEXT_EXECUTION_VERSION,
    CONTEXT_ROUTE_POLICY_VERSION, DEFAULT_APPLICATION_PAGE_LIMIT, MAX_APPLICATION_PAGE_LIMIT,
    MAX_MATERIALIZED_SOURCES,
};
use workspace_atlas::discovery;
use workspace_atlas::serving::serving_status_page;
use workspace_atlas::task_session::{
    show_legacy_task_session, start_legacy_task_session, LegacyTaskShowRequest,
    LegacyTaskStartRequest,
};
use workspace_atlas::workspace::{register_workspace, WorkspaceRecord};

const LEGACY: &str = "2025-11-25";
const EXISTING_TOOLS: [&str; 13] = [
    "atlas_status",
    "atlas_find",
    "atlas_inspect",
    "atlas_trace",
    "atlas_impact",
    "atlas_source",
    "atlas_context",
    "atlas_history",
    "atlas_providers",
    "atlas_context_ir",
    "atlas_serving_build",
    "atlas_generation_delta",
    "atlas_temporal",
];
const NEW_TOOLS: [&str; 6] = [
    "atlas_governor_capabilities",
    "atlas_governor_run",
    "atlas_task_show",
    "atlas_compiled_context_show",
    "atlas_context_yield_show",
    "atlas_serving_status",
];

const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const SILENCE_TIMEOUT: Duration = Duration::from_millis(100);
const JSON_RPC_FRAME_LIMIT_BYTES: usize = 1_048_576;

fn insert_after(frame: &mut Vec<u8>, marker: &[u8], bytes: &[u8]) {
    let offset = frame
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("marker is present")
        + marker.len();
    frame.splice(offset..offset, bytes.iter().copied());
}

fn frame_padded_to(message: &Value, length: usize) -> Vec<u8> {
    let mut frame = serde_json::to_vec(message).unwrap();
    let padding = length.checked_sub(frame.len()).unwrap();
    insert_after(&mut frame, br#""params":"#, &vec![b' '; padding]);
    assert_eq!(frame.len(), length);
    frame
}

enum StdoutEvent {
    Line(String),
    Error(std::io::Error),
    Eof,
}

struct StdinCommand {
    bytes: Vec<u8>,
    completed: mpsc::SyncSender<std::io::Result<()>>,
}

struct McpSession {
    child: Option<Child>,
    stdin_tx: Option<mpsc::Sender<StdinCommand>>,
    stdin_thread: Option<JoinHandle<()>>,
    stdout_rx: Receiver<StdoutEvent>,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<std::io::Result<String>>>,
}

impl McpSession {
    fn spawn() -> Self {
        Self::spawn_command(std::process::Command::cargo_bin("atlas-mcp").unwrap())
    }

    fn spawn_command(mut command: Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (stdin_tx, stdin_rx) = mpsc::channel::<StdinCommand>();
        let stdin_thread = thread::spawn(move || {
            while let Ok(command) = stdin_rx.recv() {
                let result = stdin.write_all(&command.bytes).and_then(|()| stdin.flush());
                let failed = result.is_err();
                let _ = command.completed.send(result);
                if failed {
                    break;
                }
            }
        });
        let (stdout_tx, stdout_rx) = mpsc::channel();
        let stdout_thread = thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match stdout.read_line(&mut line) {
                    Ok(0) => {
                        let _ = stdout_tx.send(StdoutEvent::Eof);
                        break;
                    }
                    Ok(_) => {
                        if stdout_tx.send(StdoutEvent::Line(line)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = stdout_tx.send(StdoutEvent::Error(error));
                        break;
                    }
                }
            }
        });
        let stderr_thread = thread::spawn(move || {
            let mut stderr = BufReader::new(stderr);
            let mut output = String::new();
            stderr.read_to_string(&mut output)?;
            Ok(output)
        });
        Self {
            child: Some(child),
            stdin_tx: Some(stdin_tx),
            stdin_thread: Some(stdin_thread),
            stdout_rx,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
        }
    }

    fn send_bytes(&mut self, identity: &str, bytes: Vec<u8>) {
        self.send_bytes_before_deadline(identity, bytes, WRITE_TIMEOUT, |_| {});
    }

    fn send_bytes_before_deadline<F>(
        &mut self,
        identity: &str,
        bytes: Vec<u8>,
        timeout: Duration,
        before_accept: F,
    ) where
        F: FnOnce(Instant),
    {
        let (completed_tx, completed_rx) = mpsc::sync_channel(1);
        let deadline = Instant::now() + timeout;
        self.stdin_tx
            .as_ref()
            .expect("atlas-mcp stdin writer is closed")
            .send(StdinCommand {
                bytes,
                completed: completed_tx,
            })
            .expect("atlas-mcp stdin writer disconnected");
        let now = Instant::now();
        let completion = if now >= deadline {
            Err(RecvTimeoutError::Timeout)
        } else {
            completed_rx.recv_timeout(deadline.saturating_duration_since(now))
        };
        if completion.is_ok() {
            before_accept(deadline);
            if Instant::now() >= deadline {
                let cleanup = self.abort();
                panic!("{identity}: atlas-mcp did not consume stdin within {timeout:?}; {cleanup}");
            }
        }
        match completion {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let cleanup = self.abort();
                panic!("{identity}: atlas-mcp stdin write failed: {error}; {cleanup}");
            }
            Err(RecvTimeoutError::Disconnected) => {
                let cleanup = self.abort();
                panic!("{identity}: atlas-mcp stdin writer disconnected; {cleanup}");
            }
            Err(RecvTimeoutError::Timeout) => {
                let cleanup = self.abort();
                panic!("{identity}: atlas-mcp did not consume stdin within {timeout:?}; {cleanup}");
            }
        }
    }

    fn request(&mut self, message: Value) -> Value {
        let identity = format!(
            "method={} id={}",
            message["method"].as_str().unwrap_or("<missing>"),
            message["id"]
        );
        let mut bytes = serde_json::to_vec(&message).unwrap();
        bytes.push(b'\n');
        self.send_bytes(&identity, bytes);
        self.response(&identity)
    }

    fn raw_request(&mut self, frame: &[u8]) -> Value {
        assert!(!frame.contains(&b'\n'), "raw MCP frame contains a newline");
        let mut bytes = Vec::with_capacity(frame.len() + 1);
        bytes.extend_from_slice(frame);
        bytes.push(b'\n');
        self.send_bytes("raw request", bytes);
        self.response("raw request")
    }

    fn raw_frame_then_request(
        &mut self,
        case: &str,
        frame: &[u8],
        following_request: Value,
    ) -> Value {
        assert!(!frame.contains(&b'\n'), "raw MCP frame contains a newline");
        let following_id = following_request["id"].clone();
        let following = serde_json::to_vec(&following_request).unwrap();
        let mut bytes = Vec::with_capacity(frame.len() + following.len() + 2);
        bytes.extend_from_slice(frame);
        bytes.push(b'\n');
        bytes.extend_from_slice(&following);
        bytes.push(b'\n');
        let identity = format!("{case}, following id={following_id}");
        self.send_bytes(&identity, bytes);
        self.response(&identity)
    }

    fn response(&mut self, identity: &str) -> Value {
        match self.stdout_rx.recv_timeout(RESPONSE_TIMEOUT) {
            Ok(StdoutEvent::Line(line)) => serde_json::from_str(&line).unwrap_or_else(|error| {
                panic!("{identity}: invalid JSON response: {error}: {line}")
            }),
            Ok(StdoutEvent::Error(error)) => panic!("{identity}: stdout read failed: {error}"),
            Ok(StdoutEvent::Eof) => panic!("{identity}: atlas-mcp closed without a response"),
            Err(RecvTimeoutError::Disconnected) => {
                panic!("{identity}: atlas-mcp stdout reader disconnected")
            }
            Err(RecvTimeoutError::Timeout) => {
                let cleanup = self.abort();
                panic!(
                    "{identity}: atlas-mcp emitted no response within {RESPONSE_TIMEOUT:?}; {cleanup}"
                );
            }
        }
    }

    fn assert_no_response(&mut self, identity: &str) {
        match self.stdout_rx.recv_timeout(SILENCE_TIMEOUT) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(StdoutEvent::Line(line)) => {
                panic!("{identity}: atlas-mcp emitted an unexpected response: {line}")
            }
            Ok(StdoutEvent::Error(error)) => panic!("{identity}: stdout read failed: {error}"),
            Ok(StdoutEvent::Eof) | Err(RecvTimeoutError::Disconnected) => {
                panic!("{identity}: atlas-mcp closed while checking response silence")
            }
        }
    }

    fn initialize(&mut self) {
        let response = self.request(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": LEGACY,
                "capabilities": {},
                "clientInfo": {"name": "t19", "version": "1.0.0"}
            }
        }));
        assert_eq!(response["result"]["protocolVersion"], LEGACY, "{response}");
        let mut notification = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }))
        .unwrap();
        notification.push(b'\n');
        self.send_bytes("notifications/initialized", notification);
    }

    fn call(&mut self, id: usize, name: &str, arguments: Value) -> Value {
        self.request(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
    }

    fn finish(mut self) {
        drop(self.stdin_tx.take());
        let deadline = Instant::now() + EXIT_TIMEOUT;
        let status = loop {
            match self.child.as_mut().unwrap().try_wait().unwrap() {
                Some(status) => break status,
                None if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                None => {
                    let cleanup = self.abort();
                    panic!(
                        "atlas-mcp did not exit within {EXIT_TIMEOUT:?} after stdin closed; {cleanup}"
                    );
                }
            }
        };
        self.child.take();
        let stderr = self.join_activity().expect("pipe activity cleanup failed");
        assert!(status.success(), "{status}: {stderr}");
        assert!(stderr.is_empty(), "atlas-mcp polluted stderr: {stderr}");
    }

    fn terminate(&mut self) -> Result<(), String> {
        drop(self.stdin_tx.take());
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        let _ = child.kill();
        let deadline = Instant::now() + CLEANUP_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    return Err(format!(
                        "child was not reaped within {CLEANUP_TIMEOUT:?} after kill"
                    ));
                }
                Err(error) => return Err(format!("child reap failed: {error}")),
            }
        }
    }

    fn activity_finished(&self) -> bool {
        self.stdin_thread
            .as_ref()
            .is_none_or(JoinHandle::is_finished)
            && self
                .stdout_thread
                .as_ref()
                .is_none_or(JoinHandle::is_finished)
            && self
                .stderr_thread
                .as_ref()
                .is_none_or(JoinHandle::is_finished)
    }

    fn join_activity(&mut self) -> Result<String, String> {
        let deadline = Instant::now() + CLEANUP_TIMEOUT;
        while !self.activity_finished() {
            if Instant::now() >= deadline {
                return Err(format!(
                    "pipe activity did not finish within {CLEANUP_TIMEOUT:?}"
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
        if let Some(stdin_thread) = self.stdin_thread.take() {
            stdin_thread
                .join()
                .map_err(|_| "stdin writer thread panicked".to_string())?;
        }
        if let Some(stdout_thread) = self.stdout_thread.take() {
            stdout_thread
                .join()
                .map_err(|_| "stdout reader thread panicked".to_string())?;
        }
        self.stderr_thread
            .take()
            .map(|thread| {
                thread
                    .join()
                    .map_err(|_| "stderr reader thread panicked".to_string())?
                    .map_err(|error| format!("stderr read failed: {error}"))
            })
            .unwrap_or_else(|| Ok(String::new()))
    }

    fn abort(&mut self) -> String {
        let termination = self.terminate();
        let activity = self.join_activity();
        match (termination, activity) {
            (Ok(()), Ok(stderr)) => format!("cleanup complete; stderr: {stderr}"),
            (termination, activity) => {
                format!("cleanup failed: termination={termination:?}, activity={activity:?}")
            }
        }
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        let _ = self.terminate();
        let _ = self.join_activity();
    }
}

#[test]
fn queued_stdin_completion_at_absolute_deadline_is_rejected_and_cleaned_up() {
    let mut session = McpSession::spawn();
    let timeout = Duration::from_millis(250);
    let completion_received = std::cell::Cell::new(false);
    let started = Instant::now();
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        session.send_bytes_before_deadline(
            "queued stdin deadline regression",
            b"\n".to_vec(),
            timeout,
            |deadline| {
                completion_received.set(true);
                while Instant::now() < deadline {
                    thread::yield_now();
                }
            },
        );
    }))
    .expect_err("a queued completion at the absolute deadline was accepted");
    assert!(
        completion_received.get(),
        "the test seam did not observe a queued completion before the deadline"
    );
    let message = failure
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| failure.downcast_ref::<&str>().copied())
        .unwrap_or("<non-string panic>");
    assert!(message.contains("did not consume stdin"), "{message}");
    assert!(message.contains("cleanup complete"), "{message}");
    assert!(
        started.elapsed() < CLEANUP_TIMEOUT + CLEANUP_TIMEOUT + Duration::from_secs(1),
        "queued-completion cleanup exceeded its independent bound"
    );
}

const STALLED_PROBE_ENV: &str = "ATLAS_MCP_STALLED_STDIN_PROBE";
const STALLED_CHILD_ENV: &str = "ATLAS_MCP_STALLED_STDIN_CHILD";

#[test]
fn stalled_stdin_delivery_is_bounded_by_an_outer_watchdog() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "stalled_stdin_delivery_probe",
            "--nocapture",
        ])
        .env(STALLED_PROBE_ENV, "1")
        .env_remove(STALLED_CHILD_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let watchdog = Duration::from_secs(15);
    let deadline = Instant::now() + watchdog;
    let status = loop {
        match child.try_wait().unwrap() {
            Some(status) => break status,
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            None => {
                let _ = child.kill();
                let reap_deadline = Instant::now() + CLEANUP_TIMEOUT;
                while child.try_wait().unwrap().is_none() {
                    assert!(
                        Instant::now() < reap_deadline,
                        "stalled-stdin probe could not be reaped within {CLEANUP_TIMEOUT:?}"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                panic!("stalled-stdin probe exceeded outer watchdog {watchdog:?}");
            }
        }
    };
    assert!(status.success(), "stalled-stdin probe failed: {status}");
}

#[test]
#[ignore = "subprocess probe invoked by stalled_stdin_delivery_is_bounded_by_an_outer_watchdog"]
fn stalled_stdin_delivery_probe() {
    if std::env::var_os(STALLED_PROBE_ENV).is_none() {
        return;
    }
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "stalled_stdin_non_reader",
            "--nocapture",
        ])
        .env_remove(STALLED_PROBE_ENV)
        .env(STALLED_CHILD_ENV, "1");
    let mut session = McpSession::spawn_command(command);
    let started = Instant::now();
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        session.send_bytes("stalled stdin regression", vec![b'x'; 8 * 1024 * 1024]);
    }))
    .expect_err("a child that never reads stdin unexpectedly consumed the full frame");
    let message = failure
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| failure.downcast_ref::<&str>().copied())
        .unwrap_or("<non-string panic>");
    assert!(message.contains("did not consume stdin"), "{message}");
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "stalled stdin cleanup exceeded its independent bound"
    );
}

#[test]
#[ignore = "subprocess child invoked by stalled_stdin_delivery_probe"]
fn stalled_stdin_non_reader() {
    if std::env::var_os(STALLED_CHILD_ENV).is_some() {
        thread::sleep(Duration::from_secs(30));
    }
}

struct Fixture {
    _database_directory: tempfile::TempDir,
    _workspace_directory: tempfile::TempDir,
    workspace_root: std::path::PathBuf,
    catalogue: std::path::PathBuf,
    connection: rusqlite::Connection,
    workspace: WorkspaceRecord,
}

fn fixture() -> Fixture {
    let database_directory = tempfile::tempdir().unwrap();
    let workspace_directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace_directory.path().join("src")).unwrap();
    std::fs::write(
        workspace_directory.path().join("src/lib.rs"),
        "pub fn answer() -> u8 { 42 }\n",
    )
    .unwrap();
    let workspace_root = workspace_directory.path().canonicalize().unwrap();
    let config =
        Config::parse("schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = \"mcp-t19\"\n")
            .unwrap();
    let catalogue = database_directory.path().join("atlas.sqlite");
    let connection = init_catalogue(&catalogue, &config).unwrap();
    let workspace =
        register_workspace(&connection, &workspace_root, &config, &catalogue, "1.0.0").unwrap();
    discovery::reconcile(&workspace, &connection, &config).unwrap();
    Fixture {
        _database_directory: database_directory,
        _workspace_directory: workspace_directory,
        workspace_root,
        catalogue,
        connection,
        workspace,
    }
}

fn workspace_arguments(fixture: &Fixture) -> Value {
    json!({
        "workspace_root": fixture.workspace_root,
        "catalogue": fixture.catalogue,
    })
}

fn governor_request() -> GovernorRunRequest {
    GovernorRunRequest {
        supported_versions: NegotiationRequest {
            capability_versions: vec![CONTEXT_CAPABILITIES_VERSION.to_string()],
            route_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_string()],
            decision_versions: vec![CONTEXT_ROUTE_POLICY_VERSION.to_string()],
            execution_versions: vec![CONTEXT_EXECUTION_VERSION.to_string()],
            ir_versions: vec!["2.0.0".to_string()],
            operation_versions: vec![
                "query-v1.0.0".to_string(),
                "source-reference-v1.0.0".to_string(),
            ],
        },
        semantic: GovernorRunSemanticInput {
            task: "Answer without Atlas context".to_string(),
            declared_kind: None,
            path_targets: Vec::new(),
            symbol_targets: Vec::new(),
            caller_capabilities: BTreeMap::new(),
            atlas_intent: AtlasIntent::None,
            route_floor: Route::Direct,
            route_ceiling: Route::Direct,
            legacy_task_session_id: None,
            deep_limits: None,
            materialize_source: false,
            max_materialized_bytes: None,
        },
    }
}

fn mcp_governor_request_value(request: GovernorRunRequest) -> Value {
    let mut value = serde_json::to_value(request).unwrap();
    let semantic = value["semantic"].as_object_mut().unwrap();
    semantic.remove("materialize_source");
    semantic.remove("max_materialized_bytes");
    value
}

fn assert_success(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(response["result"]["isError"], false, "{response}");
    &response["result"]["structuredContent"]
}

fn governor_result_without_transient_attempt_timings(result: &Value) -> Value {
    let mut comparable = result.clone();
    let attempts = comparable
        .get_mut("execution")
        .and_then(Value::as_object_mut)
        .and_then(|execution| execution.get_mut("attempts"))
        .and_then(Value::as_array_mut)
        .expect("governor result must contain execution.attempts as an array");
    assert!(
        !attempts.is_empty(),
        "governor result execution.attempts must not be empty"
    );
    for (index, attempt) in attempts.iter_mut().enumerate() {
        let attempt = attempt
            .as_object_mut()
            .unwrap_or_else(|| panic!("governor result attempt {index} must be an object"));
        let elapsed_micros = attempt.remove("elapsed_micros").unwrap_or_else(|| {
            panic!("governor result attempt {index} must contain elapsed_micros")
        });
        assert!(
            elapsed_micros.as_u64().is_some(),
            "governor result attempt {index} elapsed_micros must be a non-negative u64: \
             {elapsed_micros}"
        );
    }
    comparable
}

fn assert_no_mcp_materialized_source_bytes(value: &Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                assert_no_mcp_materialized_source_bytes(item);
            }
        }
        Value::Object(fields) => {
            for (name, field) in fields {
                assert_ne!(name, "bytes", "MCP response exposed source bytes: {value}");
                if name == "materialization" {
                    assert!(
                        field.is_null(),
                        "MCP response exposed source materialization: {value}"
                    );
                }
                assert_no_mcp_materialized_source_bytes(field);
            }
        }
        _ => {}
    }
}

fn resolve_schema<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.strip_prefix("#/$defs/").unwrap();
        return resolve_schema(root, &root["$defs"][name]);
    }
    if let Some(non_null) = schema
        .get("anyOf")
        .and_then(Value::as_array)
        .and_then(|variants| variants.iter().find(|variant| variant["type"] != "null"))
    {
        return resolve_schema(root, non_null);
    }
    schema
}

fn schema_property<'a>(root: &'a Value, schema: &'a Value, name: &str) -> &'a Value {
    let schema = resolve_schema(root, schema);
    resolve_schema(root, &schema["properties"][name])
}

fn governor_arguments(fixture: &Fixture) -> Value {
    let mut arguments = workspace_arguments(fixture);
    arguments.as_object_mut().unwrap().extend(
        mcp_governor_request_value(governor_request())
            .as_object()
            .unwrap()
            .clone(),
    );
    arguments
}

fn assert_invalid_params(response: &Value) {
    assert_eq!(response["error"]["code"], -32602, "{response}");
    assert_eq!(
        response["error"]["data"]["kind"], "invalid_params",
        "{response}"
    );
}

#[test]
fn bounded_compiler_inputs_match_discovery_and_reject_before_application() {
    let fixture = fixture();
    let mut session = McpSession::spawn();
    session.initialize();
    let response = session.request(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}
    }));
    let tools = response["result"]["tools"].as_array().unwrap();

    let governor_schema = &tools
        .iter()
        .find(|tool| tool["name"] == "atlas_governor_run")
        .unwrap()["inputSchema"];
    let negotiation = schema_property(governor_schema, governor_schema, "supported_versions");
    for name in [
        "capability_versions",
        "route_versions",
        "decision_versions",
        "execution_versions",
        "ir_versions",
        "operation_versions",
    ] {
        let versions = schema_property(governor_schema, negotiation, name);
        assert_eq!(versions["minItems"], 1, "{name}: {versions}");
        assert_eq!(versions["maxItems"], 8, "{name}: {versions}");
    }
    let semantic = schema_property(governor_schema, governor_schema, "semantic");
    assert_eq!(
        semantic["x-atlas-aggregate-maxItems"],
        json!({"fields": ["path_targets", "symbol_targets"], "maximum": 200}),
        "{semantic}"
    );
    for name in ["path_targets", "symbol_targets"] {
        let targets = schema_property(governor_schema, semantic, name);
        assert_eq!(targets["maxItems"], 200, "{name}: {targets}");
        let item = resolve_schema(governor_schema, &targets["items"]);
        assert_eq!(item["maxLength"], 512, "{name}: {targets}");
        assert_eq!(
            item["x-atlas-utf8-byteLength"],
            json!({"minimum": 1, "maximum": 512}),
            "{name}: {item}"
        );
    }

    for tool_name in ["atlas_task_show", "atlas_context_yield_show"] {
        let schema = &tools.iter().find(|tool| tool["name"] == tool_name).unwrap()["inputSchema"];
        let identifier = schema_property(schema, schema, "task_session_id");
        assert_eq!(identifier["minLength"], 1, "{tool_name}: {identifier}");
        assert_eq!(identifier["maxLength"], 128, "{tool_name}: {identifier}");
        assert_eq!(identifier["pattern"], r"^[\u0000-\u007F]+$");
        assert_eq!(
            identifier["x-atlas-utf8-byteLength"],
            json!({"minimum": 1, "maximum": 128})
        );
        assert_eq!(identifier["x-atlas-canonical-grammar"], "ascii");
    }
    let compiled_schema = &tools
        .iter()
        .find(|tool| tool["name"] == "atlas_compiled_context_show")
        .unwrap()["inputSchema"];
    for name in ["context_id", "task_session_id"] {
        let identifier = schema_property(compiled_schema, compiled_schema, name);
        assert_eq!(identifier["minLength"], 1, "{name}: {identifier}");
        assert_eq!(identifier["maxLength"], 128, "{name}: {identifier}");
        assert_eq!(
            identifier["x-atlas-utf8-byteLength"],
            json!({"minimum": 1, "maximum": 128})
        );
        if name == "task_session_id" {
            assert_eq!(identifier["pattern"], r"^[\u0000-\u007F]+$");
            assert_eq!(identifier["x-atlas-canonical-grammar"], "ascii");
        }
    }

    for (offset, vector_name) in [
        "capability_versions",
        "route_versions",
        "decision_versions",
        "execution_versions",
        "ir_versions",
        "operation_versions",
    ]
    .into_iter()
    .enumerate()
    {
        let mut arguments = governor_arguments(&fixture);
        arguments["supported_versions"][vector_name] =
            Value::Array((0..9).map(|_| Value::from("unsupported")).collect());
        assert_invalid_params(&session.call(10 + offset, "atlas_governor_run", arguments));
    }

    let mut empty_versions = governor_arguments(&fixture);
    empty_versions["supported_versions"]["capability_versions"] = json!([]);
    assert_invalid_params(&session.call(19, "atlas_governor_run", empty_versions));

    let mut oversized = governor_arguments(&fixture);
    oversized["supported_versions"]["capability_versions"] =
        json!(["x".repeat(1_048_577), CONTEXT_CAPABILITIES_VERSION]);
    let oversized_frame = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 20,
        "method": "tools/call",
        "params": {"name": "atlas_governor_run", "arguments": oversized}
    }))
    .unwrap();
    assert!(oversized_frame.len() > 1_048_576);
    let response = session.raw_frame_then_request(
        "oversized negotiation request",
        &oversized_frame,
        json!({"jsonrpc": "2.0", "id": 120, "method": "tools/list", "params": {}}),
    );
    assert_eq!(response["id"], 120, "{response}");
    assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 19);

    for (offset, tool_name) in [
        "atlas_task_show",
        "atlas_compiled_context_show",
        "atlas_context_yield_show",
    ]
    .into_iter()
    .enumerate()
    {
        for (case, task_session_id) in ["".to_string(), "x".repeat(129)].into_iter().enumerate() {
            let mut arguments = workspace_arguments(&fixture);
            arguments["task_session_id"] = Value::from(task_session_id);
            assert_invalid_params(&session.call(30 + offset * 2 + case, tool_name, arguments));
        }
    }
    for (case, context_id) in ["".to_string(), "x".repeat(129)].into_iter().enumerate() {
        let mut arguments = workspace_arguments(&fixture);
        arguments["context_id"] = Value::from(context_id);
        assert_invalid_params(&session.call(40 + case, "atlas_compiled_context_show", arguments));
    }
    for (offset, target_name) in ["path_targets", "symbol_targets"].into_iter().enumerate() {
        let mut arguments = governor_arguments(&fixture);
        arguments["semantic"][target_name] = json!(["x".repeat(513)]);
        assert_invalid_params(&session.call(50 + offset, "atlas_governor_run", arguments));
    }
    let mut too_many_targets = governor_arguments(&fixture);
    too_many_targets["semantic"]["path_targets"] = Value::Array(
        (0..201)
            .map(|index| Value::from(index.to_string()))
            .collect(),
    );
    assert_invalid_params(&session.call(52, "atlas_governor_run", too_many_targets));
    session.finish();
}
#[test]
fn crlf_exact_frame_limit_is_accepted_and_oversized_recovers() {
    let mut session = McpSession::spawn();
    session.initialize();

    let exact_frame = frame_padded_to(
        &json!({"jsonrpc": "2.0", "id": 200, "method": "tools/list", "params": {}}),
        JSON_RPC_FRAME_LIMIT_BYTES,
    );
    let mut exact_bytes = exact_frame.clone();
    exact_bytes.extend_from_slice(b"\r\n");
    session.send_bytes("exact CRLF frame", exact_bytes);
    let response = session.response("exact CRLF frame");
    assert_eq!(response["id"], 200, "{response}");
    assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 19);

    let mut oversized_frame = exact_frame;
    insert_after(&mut oversized_frame, br#""params":"#, b" ");
    assert_eq!(oversized_frame.len(), JSON_RPC_FRAME_LIMIT_BYTES + 1);
    let following = serde_json::to_vec(
        &json!({"jsonrpc": "2.0", "id": 202, "method": "tools/list", "params": {}}),
    )
    .unwrap();
    let mut bytes = oversized_frame;
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(&following);
    bytes.extend_from_slice(b"\r\n");
    session.send_bytes("oversized CRLF frame followed by request", bytes);
    let response = session.response("request following oversized CRLF frame");
    assert_eq!(response["id"], 202, "{response}");
    assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 19);
    session.assert_no_response("oversized CRLF frame");
    session.finish();
}

#[test]
fn crlf_split_across_stdin_writes_preserves_exact_boundary() {
    let mut session = McpSession::spawn();
    session.initialize();

    let exact_frame = frame_padded_to(
        &json!({"jsonrpc": "2.0", "id": 210, "method": "tools/list", "params": {}}),
        JSON_RPC_FRAME_LIMIT_BYTES,
    );
    let mut before_lf = exact_frame;
    before_lf.push(b'\r');
    session.send_bytes("exact CRLF frame through CR", before_lf);
    session.assert_no_response("exact CRLF frame before split LF");
    session.send_bytes("exact CRLF frame split LF", vec![b'\n']);
    let response = session.response("exact CRLF frame split across writes");
    assert_eq!(response["id"], 210, "{response}");
    assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 19);
    session.finish();
}

#[test]
fn coalesced_crlf_frames_preserve_ids_and_notification_silence() {
    let mut session = McpSession::spawn();
    session.initialize();

    let messages = [
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": {"progressToken": "crlf", "progress": 1}
        }),
        json!({"jsonrpc": "2.0", "id": 220, "method": "tools/list", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 221, "method": "tools/list", "params": {}}),
    ];
    let mut bytes = Vec::new();
    for message in messages {
        bytes.extend_from_slice(&serde_json::to_vec(&message).unwrap());
        bytes.extend_from_slice(b"\r\n");
    }
    session.send_bytes("coalesced CRLF frames", bytes);

    for expected_id in [220, 221] {
        let response = session.response("coalesced CRLF request");
        assert_eq!(response["id"], expected_id, "{response}");
        assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 19);
    }
    session.assert_no_response("coalesced CRLF notification");
    session.finish();
}

#[test]
fn raw_frames_and_utf8_semantics_are_bounded_before_application_access() {
    let fixture = fixture();
    let mut session = McpSession::spawn();
    session.initialize();

    let message = json!({
        "jsonrpc": "2.0",
        "id": 100,
        "method": "tools/call",
        "params": {
            "name": "atlas_governor_run",
            "arguments": governor_arguments(&fixture)
        }
    });
    let exact_frame = frame_padded_to(&message, JSON_RPC_FRAME_LIMIT_BYTES);
    assert_success(&session.raw_request(&exact_frame));

    let mut whitespace_oversized = exact_frame;
    insert_after(&mut whitespace_oversized, br#""params":"#, b" ");
    assert_eq!(whitespace_oversized.len(), JSON_RPC_FRAME_LIMIT_BYTES + 1);
    let response = session.raw_frame_then_request(
        "oversized whitespace frame",
        &whitespace_oversized,
        json!({"jsonrpc": "2.0", "id": 108, "method": "tools/list", "params": {}}),
    );
    assert_eq!(response["id"], 108, "{response}");
    assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 19);

    let oversized_notification = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "method": "tools/list",
        "params": {"padding": "x".repeat(JSON_RPC_FRAME_LIMIT_BYTES)}
    }))
    .unwrap();
    assert!(oversized_notification.len() > JSON_RPC_FRAME_LIMIT_BYTES);
    let response = session.raw_frame_then_request(
        "oversized notification",
        &oversized_notification,
        json!({"jsonrpc": "2.0", "id": 109, "method": "tools/list", "params": {}}),
    );
    assert_eq!(response["id"], 109, "{response}");
    assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 19);
    drop(oversized_notification);

    let mut duplicate_oversized = serde_json::to_vec(&message).unwrap();
    let duplicate = br#""catalogue":null,"#;
    let repeat_count =
        ((JSON_RPC_FRAME_LIMIT_BYTES - duplicate_oversized.len()) / duplicate.len()) + 1;
    insert_after(
        &mut duplicate_oversized,
        br#""arguments":{"#,
        &duplicate.repeat(repeat_count),
    );
    assert!(duplicate_oversized.len() > JSON_RPC_FRAME_LIMIT_BYTES);
    let response = session.raw_frame_then_request(
        "oversized duplicate-member frame",
        &duplicate_oversized,
        json!({"jsonrpc": "2.0", "id": 110, "method": "tools/list", "params": {}}),
    );
    assert_eq!(response["id"], 110, "{response}");
    assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 19);

    for (case, frame, following_id) in [
        ("empty object", b"{}".as_slice(), 111),
        ("empty array", b"[]".as_slice(), 112),
        (
            "wrong-shaped object",
            br#"{"jsonrpc":"2.0","method":17}"#.as_slice(),
            113,
        ),
    ] {
        let response = session.raw_frame_then_request(
            case,
            frame,
            json!({"jsonrpc": "2.0", "id": following_id, "method": "tools/list", "params": {}}),
        );
        assert_eq!(response["id"], following_id, "{case}: {response}");
        assert_eq!(
            response["result"]["tools"].as_array().unwrap().len(),
            19,
            "{case}: {response}"
        );
        session.assert_no_response(case);
    }

    let mut context_at_limit = workspace_arguments(&fixture);
    context_at_limit["context_id"] = json!("é".repeat(64));
    let response = session.call(101, "atlas_compiled_context_show", context_at_limit);
    assert_ne!(response["error"]["code"], -32602, "{response}");

    let mut context_over_limit = workspace_arguments(&fixture);
    context_over_limit["context_id"] = json!("é".repeat(65));
    assert_invalid_params(&session.call(102, "atlas_compiled_context_show", context_over_limit));

    let mut target_at_limit = governor_arguments(&fixture);
    target_at_limit["semantic"]["path_targets"] = json!(["é".repeat(256)]);
    assert_success(&session.call(103, "atlas_governor_run", target_at_limit));

    let mut target_over_limit = governor_arguments(&fixture);
    target_over_limit["semantic"]["symbol_targets"] = json!(["é".repeat(257)]);
    assert_invalid_params(&session.call(104, "atlas_governor_run", target_over_limit));

    let mut aggregate_at_limit = governor_arguments(&fixture);
    aggregate_at_limit["semantic"]["path_targets"] =
        Value::Array((0..100).map(|index| json!(format!("p{index}"))).collect());
    aggregate_at_limit["semantic"]["symbol_targets"] =
        Value::Array((0..100).map(|index| json!(format!("s{index}"))).collect());
    assert_success(&session.call(105, "atlas_governor_run", aggregate_at_limit));

    let mut aggregate_over_limit = governor_arguments(&fixture);
    aggregate_over_limit["workspace_root"] = json!(fixture.workspace_root.join("missing"));
    aggregate_over_limit["catalogue"] = json!(fixture.catalogue.with_extension("missing"));
    aggregate_over_limit["semantic"]["path_targets"] =
        Value::Array((0..100).map(|index| json!(format!("p{index}"))).collect());
    aggregate_over_limit["semantic"]["symbol_targets"] =
        Value::Array((0..101).map(|index| json!(format!("s{index}"))).collect());
    assert_invalid_params(&session.call(106, "atlas_governor_run", aggregate_over_limit));

    let mut non_ascii_legacy_id = governor_arguments(&fixture);
    non_ascii_legacy_id["workspace_root"] = json!(fixture.workspace_root.join("missing"));
    non_ascii_legacy_id["catalogue"] = json!(fixture.catalogue.with_extension("missing"));
    non_ascii_legacy_id["semantic"]["legacy_task_session_id"] = json!("é".repeat(64));
    assert_invalid_params(&session.call(107, "atlas_governor_run", non_ascii_legacy_id));

    session.finish();
}

#[test]
fn registry_is_existing_thirteen_plus_exact_six_without_prohibited_authority() {
    let mut session = McpSession::spawn();
    session.initialize();
    let response = session.request(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}
    }));
    let tools = response["result"]["tools"].as_array().unwrap();
    let names = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(&names[..EXISTING_TOOLS.len()], EXISTING_TOOLS);
    assert_eq!(&names[EXISTING_TOOLS.len()..], NEW_TOOLS);
    assert_eq!(names.len(), 19);
    assert!(EXISTING_TOOLS.contains(&"atlas_serving_build"));
    assert!(!NEW_TOOLS.contains(&"atlas_serving_build"));

    let prohibited = [
        "start",
        "complete",
        "abandon",
        "retention",
        "compact",
        "unregister",
        "backup",
        "export",
        "build",
        "rebuild",
        "materialize",
        "write",
        "path_output",
    ];
    for name in &names[EXISTING_TOOLS.len()..] {
        assert!(!prohibited.iter().any(|word| name.contains(word)), "{name}");
    }
    for tool in &tools[EXISTING_TOOLS.len()..] {
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object", "{}: {schema}", tool["name"]);
        assert_eq!(
            schema["additionalProperties"], false,
            "{}: {schema}",
            tool["name"]
        );
        assert!(schema["properties"].get("workspace_root").is_some());
        let text = schema.to_string();
        for forbidden in [
            "materialize_source",
            "max_materialized_bytes",
            "source_path",
            "caller_path",
            "output_path",
            "destination_path",
            "backup_path",
            "export_path",
            "task_action",
            "reason_code",
            "retention",
            "compact",
            "unregister",
            "confirm_manifest",
            "confirm_privacy_deletion",
            "irreversible",
            "serving_build",
            "serving_rebuild",
            "write",
        ] {
            assert!(
                !text.contains(forbidden),
                "{} exposes {forbidden}: {schema}",
                tool["name"]
            );
        }
    }
    let expected_properties = [
        (
            "atlas_governor_capabilities",
            BTreeSet::from(["catalogue", "workspace_root"]),
        ),
        (
            "atlas_governor_run",
            BTreeSet::from([
                "catalogue",
                "semantic",
                "supported_versions",
                "workspace_root",
            ]),
        ),
        (
            "atlas_task_show",
            BTreeSet::from([
                "catalogue",
                "cursor",
                "limit",
                "task_session_id",
                "workspace_root",
            ]),
        ),
        (
            "atlas_compiled_context_show",
            BTreeSet::from([
                "catalogue",
                "context_id",
                "cursor",
                "limit",
                "task_session_id",
                "workspace_root",
            ]),
        ),
        (
            "atlas_context_yield_show",
            BTreeSet::from([
                "catalogue",
                "cursor",
                "limit",
                "task_session_id",
                "workspace_root",
            ]),
        ),
        (
            "atlas_serving_status",
            BTreeSet::from(["catalogue", "cursor", "limit", "workspace_root"]),
        ),
    ];
    for (name, expected) in expected_properties {
        let schema = &tools.iter().find(|tool| tool["name"] == name).unwrap()["inputSchema"];
        let actual = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected, "{name}: {schema}");
    }
    session.finish();
}

#[test]
fn all_six_calls_match_shared_application_results() {
    let mut fixture = fixture();
    let task_session = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &LegacyTaskStartRequest {
            context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
            task: "Inspect MCP parity".to_string(),
            declared_kind: None,
        },
    )
    .unwrap();
    let show_request = LegacyTaskShowRequest {
        context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
        task_session_id: task_session.task_session_id.clone(),
        limit: Some(DEFAULT_APPLICATION_PAGE_LIMIT),
        cursor: None,
    };
    let direct_task = serde_json::to_value(
        show_legacy_task_session(&fixture.connection, &fixture.workspace, &show_request).unwrap(),
    )
    .unwrap();
    let direct_context = serde_json::to_value(
        show_compiled_context(
            &fixture.connection,
            &fixture.workspace,
            &CompiledContextShowRequest {
                context_id: None,
                task_session_id: Some(task_session.task_session_id.clone()),
                limit: Some(DEFAULT_APPLICATION_PAGE_LIMIT),
                cursor: None,
            },
        )
        .unwrap(),
    )
    .unwrap();
    let direct_yield = serde_json::to_value(
        show_context_yield(&fixture.connection, &fixture.workspace, &show_request).unwrap(),
    )
    .unwrap();
    let direct_serving = serde_json::to_value(
        serving_status_page(
            &fixture.connection,
            &fixture.workspace,
            DEFAULT_APPLICATION_PAGE_LIMIT,
            None,
        )
        .unwrap(),
    )
    .unwrap();
    let direct_capabilities = serde_json::to_value(discover_context_capabilities()).unwrap();
    assert_eq!(
        direct_capabilities["availability"]["progressive_execution"],
        "available"
    );
    assert_eq!(
        direct_capabilities["availability"]["deep_context_ir"],
        "available"
    );
    assert_eq!(
        direct_capabilities["availability"]["materialization"],
        "available"
    );
    assert_eq!(
        direct_capabilities["bounds"]["max_materialized_sources"],
        MAX_MATERIALIZED_SOURCES
    );
    assert_eq!(
        direct_capabilities["bounds"]["max_materialized_bytes"],
        MAX_CONTEXT_EXECUTION_SOURCE_BYTES
    );
    let cli = Command::cargo_bin("atlas")
        .unwrap()
        .args(["governor", "capabilities"])
        .arg(&fixture.workspace_root)
        .arg("--catalogue")
        .arg(&fixture.catalogue)
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert!(cli.stderr.is_empty());
    let cli_capabilities: Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(cli_capabilities, direct_capabilities);
    assert_eq!(
        serde_json::to_vec(&cli_capabilities).unwrap(),
        serde_json::to_vec(&direct_capabilities).unwrap()
    );
    let request = governor_request();
    let direct_governor = serde_json::to_value(
        run_catalogue_governor(request.clone(), &mut fixture.connection, &fixture.workspace)
            .unwrap(),
    )
    .unwrap();

    let mut session = McpSession::spawn();
    session.initialize();
    let mcp_capabilities = session.call(
        2,
        "atlas_governor_capabilities",
        workspace_arguments(&fixture),
    );
    let mcp_capabilities = assert_success(&mcp_capabilities);
    assert_eq!(mcp_capabilities, &direct_capabilities);
    assert_eq!(
        serde_json::to_vec(mcp_capabilities).unwrap(),
        serde_json::to_vec(&direct_capabilities).unwrap()
    );
    let mut governor_args = workspace_arguments(&fixture);
    governor_args.as_object_mut().unwrap().extend(
        mcp_governor_request_value(request)
            .as_object()
            .unwrap()
            .clone(),
    );
    let governor_response = session.call(3, "atlas_governor_run", governor_args);
    let mcp_governor = assert_success(&governor_response);
    assert_eq!(
        governor_result_without_transient_attempt_timings(mcp_governor),
        governor_result_without_transient_attempt_timings(&direct_governor)
    );
    let read_args = |fixture: &Fixture| {
        json!({
            "workspace_root": fixture.workspace_root,
            "catalogue": fixture.catalogue,
            "task_session_id": task_session.task_session_id,
            "limit": DEFAULT_APPLICATION_PAGE_LIMIT,
        })
    };
    assert_eq!(
        assert_success(&session.call(4, "atlas_task_show", read_args(&fixture))),
        &direct_task
    );
    assert_eq!(
        assert_success(&session.call(5, "atlas_compiled_context_show", read_args(&fixture))),
        &direct_context
    );
    assert_eq!(
        assert_success(&session.call(6, "atlas_context_yield_show", read_args(&fixture))),
        &direct_yield
    );
    let mut serving_args = workspace_arguments(&fixture);
    serving_args.as_object_mut().unwrap().insert(
        "limit".to_string(),
        Value::from(DEFAULT_APPLICATION_PAGE_LIMIT),
    );
    assert_eq!(
        assert_success(&session.call(7, "atlas_serving_status", serving_args)),
        &direct_serving
    );

    assert_eq!(direct_governor["execution"]["state"], "completed");
    assert_eq!(direct_governor["execution"]["final_route"], "DIRECT");
    assert_no_mcp_materialized_source_bytes(&direct_governor);
    session.finish();
}
#[test]
fn governor_calls_preserve_direct_light_and_deep_route_states() {
    let mut fixture = fixture();
    let mut session = McpSession::spawn();
    session.initialize();

    let mut direct = governor_request();
    let mut light = governor_request();
    light.semantic.task = "Read exact source through LIGHT".to_string();
    light.semantic.path_targets = vec!["src/lib.rs".to_string()];
    light.semantic.caller_capabilities = BTreeMap::from([(
        RequiredCapability::ExactSource,
        CallerCapabilityState::Unsatisfied,
    )]);
    light.semantic.atlas_intent = AtlasIntent::Require;
    light.semantic.route_ceiling = Route::AtlasLight;

    let mut blocked = governor_request();
    blocked.semantic.task = "Read a missing exact source through LIGHT".to_string();
    blocked.semantic.path_targets = vec!["src/missing.rs".to_string()];
    blocked.semantic.caller_capabilities = BTreeMap::from([(
        RequiredCapability::ExactSource,
        CallerCapabilityState::Unsatisfied,
    )]);
    blocked.semantic.atlas_intent = AtlasIntent::Require;
    blocked.semantic.route_ceiling = Route::AtlasLight;

    let mut deep = governor_request();
    deep.semantic.task = "Request unavailable DEEP validation".to_string();
    deep.semantic.caller_capabilities = BTreeMap::from([(
        RequiredCapability::ValidationPlan,
        CallerCapabilityState::Unsatisfied,
    )]);
    let deep_session = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &LegacyTaskStartRequest {
            context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
            task: deep.semantic.task.clone(),
            declared_kind: None,
        },
    )
    .unwrap();
    deep.semantic.legacy_task_session_id = Some(deep_session.task_session_id);
    deep.semantic.atlas_intent = AtlasIntent::Require;
    deep.semantic.route_ceiling = Route::AtlasDeep;
    deep.semantic.deep_limits = Some(GovernorDeepLimits {
        max_records: 50,
        max_source_bytes: 65_536,
        max_estimated_tokens: 16_384,
        max_relationship_depth: 4,
        max_work_units: 10_000,
        uncertainty_reserve_percent: 12,
    });

    for (id, request, expected_route, expected_state) in [
        (20, &mut direct, "DIRECT", "completed"),
        (21, &mut light, "ATLAS_LIGHT", "completed"),
        (22, &mut blocked, "ATLAS_LIGHT", "blocked"),
        (23, &mut deep, "ATLAS_DEEP", "blocked"),
    ] {
        let mut args = workspace_arguments(&fixture);
        args.as_object_mut().unwrap().extend(
            mcp_governor_request_value(request.clone())
                .as_object()
                .unwrap()
                .clone(),
        );
        let response = session.call(id, "atlas_governor_run", args);
        let output = assert_success(&response);
        assert_eq!(
            output["execution"]["final_route"], expected_route,
            "{output}"
        );
        assert_eq!(output["execution"]["state"], expected_state, "{output}");
        assert_no_mcp_materialized_source_bytes(output);
    }
    session.finish();
}

#[test]
fn schemas_and_parsers_enforce_closed_inputs_bounds_and_materialization_disabled() {
    let mut fixture = fixture();
    let mut session = McpSession::spawn();
    session.initialize();
    let task_session = start_legacy_task_session(
        &mut fixture.connection,
        &fixture.workspace,
        &LegacyTaskStartRequest {
            context_ir_version: CONTEXT_SCHEMA_VERSION.to_string(),
            task: "Exercise MCP page bounds".to_string(),
            declared_kind: None,
        },
    )
    .unwrap();

    for (id, name) in NEW_TOOLS.iter().enumerate() {
        let mut args = workspace_arguments(&fixture);
        args.as_object_mut()
            .unwrap()
            .insert("unknown_argument".to_string(), Value::Bool(true));
        let response = session.call(10 + id, name, args);
        assert_eq!(response["error"]["code"], -32602, "{name}: {response}");
    }

    for (offset, name) in [
        "atlas_task_show",
        "atlas_compiled_context_show",
        "atlas_context_yield_show",
        "atlas_serving_status",
    ]
    .into_iter()
    .enumerate()
    {
        for (case, limit) in [0, MAX_APPLICATION_PAGE_LIMIT + 1].into_iter().enumerate() {
            let mut arguments = workspace_arguments(&fixture);
            arguments
                .as_object_mut()
                .unwrap()
                .insert("limit".to_string(), Value::from(limit));
            if name != "atlas_serving_status" {
                arguments.as_object_mut().unwrap().insert(
                    "task_session_id".to_string(),
                    Value::from(task_session.task_session_id.clone()),
                );
            }
            let response = session.call(30 + offset * 10 + case, name, arguments);
            assert_eq!(
                response["error"]["data"]["kind"], "invalid_config",
                "{name}: {response}"
            );
        }
    }

    for (offset, name) in [
        "atlas_task_show",
        "atlas_compiled_context_show",
        "atlas_context_yield_show",
        "atlas_serving_status",
    ]
    .into_iter()
    .enumerate()
    {
        let mut arguments = workspace_arguments(&fixture);
        arguments
            .as_object_mut()
            .unwrap()
            .insert("cursor".to_string(), Value::from("x".repeat(4097)));
        if name != "atlas_serving_status" {
            arguments.as_object_mut().unwrap().insert(
                "task_session_id".to_string(),
                Value::from(task_session.task_session_id.clone()),
            );
        }
        let response = session.call(70 + offset, name, arguments);
        assert_eq!(
            response["error"]["data"]["kind"], "cursor_invalid",
            "{name}: {response}"
        );
    }

    for (id, field, value) in [
        (50, "materialize_source", Value::Bool(true)),
        (52, "max_materialized_bytes", Value::from(1)),
    ] {
        let mut request = serde_json::to_value(governor_request()).unwrap();
        request["semantic"][field] = value;
        let mut args = workspace_arguments(&fixture);
        args.as_object_mut()
            .unwrap()
            .extend(request.as_object().unwrap().clone());
        let response = session.call(id, "atlas_governor_run", args);
        assert_eq!(response["error"]["code"], -32602, "{field}: {response}");
    }

    let mut oversized = governor_request();
    oversized.semantic.task = "x".repeat(1_048_577);
    let mut args = workspace_arguments(&fixture);
    args.as_object_mut().unwrap().extend(
        mcp_governor_request_value(oversized)
            .as_object()
            .unwrap()
            .clone(),
    );
    let oversized_frame = serde_json::to_vec(&json!({
        "jsonrpc": "2.0",
        "id": 51,
        "method": "tools/call",
        "params": {"name": "atlas_governor_run", "arguments": args}
    }))
    .unwrap();
    assert!(oversized_frame.len() > 1_048_576);
    let response = session.raw_frame_then_request(
        "oversized governor task",
        &oversized_frame,
        json!({"jsonrpc": "2.0", "id": 151, "method": "tools/list", "params": {}}),
    );
    assert_eq!(response["id"], 151, "{response}");
    assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 19);
    session.finish();
}

#[test]
fn typed_application_errors_and_catalogue_selection_are_preserved() {
    let fixture = fixture();
    let mut session = McpSession::spawn();
    session.initialize();

    let response = session.call(
        2,
        "atlas_task_show",
        json!({
            "workspace_root": fixture.workspace_root,
            "catalogue": fixture.catalogue,
            "task_session_id": "missing-session",
        }),
    );
    assert_eq!(response["error"]["data"]["kind"], "other", "{response}");

    let mut request = governor_request();
    request.supported_versions.route_versions = vec!["unsupported-route".to_string()];
    let mut args = workspace_arguments(&fixture);
    args.as_object_mut().unwrap().extend(
        mcp_governor_request_value(request)
            .as_object()
            .unwrap()
            .clone(),
    );
    let response = session.call(3, "atlas_governor_run", args);
    assert_eq!(
        response["error"]["data"]["kind"], "no_common_version",
        "{response}"
    );

    let wrong_catalogue = tempfile::NamedTempFile::new().unwrap();
    let response = session.call(
        4,
        "atlas_governor_capabilities",
        json!({
            "workspace_root": fixture.workspace_root,
            "catalogue": wrong_catalogue.path(),
        }),
    );
    assert!(response.get("error").is_some(), "{response}");
    session.finish();
}

#[test]
fn prohibited_tool_names_are_method_not_found() {
    let fixture = fixture();
    let mut session = McpSession::spawn();
    session.initialize();
    let prohibited = BTreeSet::from([
        "atlas_task_start",
        "atlas_task_complete",
        "atlas_task_abandon",
        "atlas_retention_status",
        "atlas_retention_compact",
        "atlas_unregister",
        "atlas_serving_rebuild",
        "atlas_context_yield_export",
    ]);
    for (id, name) in prohibited.into_iter().enumerate() {
        let response = session.call(100 + id, name, workspace_arguments(&fixture));
        assert_eq!(response["error"]["code"], -32601, "{name}: {response}");
    }
    session.finish();
}
