use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{json, Value};
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::workspace::register_workspace;

const MODERN: &str = "2026-07-28";
const LEGACY: &str = "2025-11-25";
const TOOL_COUNT: usize = 19;
const ORIGINAL_TOOLS: [&str; 13] = [
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
const T19_TOOLS: [&str; 6] = [
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
        let mut command = std::process::Command::cargo_bin("atlas-mcp").unwrap();
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

    fn send(&mut self, message: Value) {
        let mut bytes = serde_json::to_vec(&message).unwrap();
        bytes.push(b'\n');
        let identity = format!(
            "method={} id={}",
            message["method"].as_str().unwrap_or("<missing>"),
            message["id"]
        );
        self.send_bytes(&identity, bytes);
    }

    fn request(&mut self, message: Value) -> Value {
        let identity = format!(
            "method={} id={}",
            message["method"].as_str().unwrap_or("<missing>"),
            message["id"]
        );
        self.send(message);
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
        assert!(
            status.success(),
            "atlas-mcp did not shut down cleanly: {status}: {stderr}"
        );
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

fn modern_meta(version: &str) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": version,
        "io.modelcontextprotocol/clientInfo": {"name": "atlas-test", "version": "1.0.0"},
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

fn initialize_legacy(session: &mut McpSession) {
    let response = session.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": LEGACY,
            "capabilities": {},
            "clientInfo": {"name": "atlas-test", "version": "1.0.0"}
        }
    }));
    assert_eq!(response["result"]["protocolVersion"], LEGACY, "{response}");
    session.send(json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    }));
}

#[test]
fn modern_discovery_advertises_exact_dual_era_boundary_and_closed_schemas() {
    let mut session = McpSession::spawn();
    let discover = session.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {"_meta": modern_meta(MODERN)}
    }));
    assert_eq!(
        discover["result"]["supportedVersions"],
        json!([MODERN, LEGACY]),
        "{discover}"
    );

    let listed = session.request(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {"_meta": modern_meta(MODERN)}
    }));
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), TOOL_COUNT);
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(&names[..ORIGINAL_TOOLS.len()], ORIGINAL_TOOLS);
    assert_eq!(&names[ORIGINAL_TOOLS.len()..], T19_TOOLS);
    for tool in tools {
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object", "{}: {schema}", tool["name"]);
        assert_eq!(
            schema["additionalProperties"], false,
            "{} schema must be closed: {schema}",
            tool["name"]
        );
        assert!(schema["properties"].get("workspace_root").is_some());
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "workspace_root"));
    }
    let inspect = tools
        .iter()
        .find(|tool| tool["name"] == "atlas_inspect")
        .unwrap();
    assert_eq!(
        inspect["inputSchema"]["anyOf"],
        json!([{"required": ["path"]}, {"required": ["symbol"]}])
    );
    let find = tools
        .iter()
        .find(|tool| tool["name"] == "atlas_find")
        .unwrap();
    assert_eq!(find["inputSchema"]["properties"]["limit"]["minimum"], 1);
    let impact = tools
        .iter()
        .find(|tool| tool["name"] == "atlas_impact")
        .unwrap();
    assert_eq!(impact["inputSchema"]["properties"]["paths"]["minItems"], 1);
    session.finish();
}

#[test]
fn unsupported_modern_version_reports_both_supported_dates() {
    let mut session = McpSession::spawn();
    let response = session.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {"_meta": modern_meta("2027-01-01")}
    }));
    assert_eq!(response["error"]["code"], -32022, "{response}");
    let text = response.to_string();
    assert!(text.contains(MODERN), "{response}");
    assert!(text.contains(LEGACY), "{response}");
}

#[test]
fn legacy_lifecycle_valid_call_and_every_tool_rejects_unknown_arguments() {
    let database_directory = tempfile::tempdir().unwrap();
    let workspace_directory = tempfile::tempdir().unwrap();
    let workspace_root = workspace_directory.path().canonicalize().unwrap();
    let config = Config::parse(
        "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"mcp-conformance\"\n",
    )
    .unwrap();
    let catalogue = database_directory.path().join("atlas.sqlite");
    let connection = init_catalogue(&catalogue, &config).unwrap();
    register_workspace(&connection, &workspace_root, &config, &catalogue, "1.0.0").unwrap();

    let mut session = McpSession::spawn();
    initialize_legacy(&mut session);
    let listed = session.request(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), TOOL_COUNT);

    let valid = session.request(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "atlas_status",
            "arguments": {
                "workspace_root": &workspace_root,
                "catalogue": catalogue
            }
        }
    }));
    assert!(valid.get("error").is_none(), "{valid}");
    assert_eq!(valid["result"]["isError"], false, "{valid}");
    assert!(valid["result"]["structuredContent"].is_object(), "{valid}");

    for (offset, tool) in tools.iter().enumerate() {
        let response = session.request(json!({
            "jsonrpc": "2.0",
            "id": 10 + offset,
            "method": "tools/call",
            "params": {
                "name": tool["name"],
                "arguments": {
                    "workspace_root": &workspace_root,
                    "unknown_argument": true
                }
            }
        }));
        assert_eq!(
            response["error"]["code"], -32602,
            "{} accepted an unknown argument: {response}",
            tool["name"]
        );
        let malformed = session.request(json!({
            "jsonrpc": "2.0",
            "id": 100 + offset,
            "method": "tools/call",
            "params": {
                "name": tool["name"],
                "arguments": {"workspace_root": 7}
            }
        }));
        assert_eq!(
            malformed["error"]["code"], -32602,
            "{} accepted malformed arguments: {malformed}",
            tool["name"]
        );
    }
    session.finish();
}
