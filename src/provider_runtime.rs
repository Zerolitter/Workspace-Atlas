//! External-process provider runtime.
//!
//! Security controls implemented here: direct spawn without a shell, fixed
//! canonical working directory, minimal environment allowlist, wall-clock
//! timeout with process-tree termination, bounded stdout/stderr/output
//! capture, and output-path confinement plus hashing before decode.
//!
//! This module owns process lifecycle only. Provider adapters never receive a
//! database handle; persistence remains in the core.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{AtlasError, Result};
use crate::paths;

/// One external-process invocation plan. Every field is computed by Atlas;
/// provider stdout cannot alter command, scope, budgets, or output ownership.
#[derive(Debug, Clone)]
pub struct SpawnPlan {
    pub command: String,
    pub arguments: Vec<String>,
    pub cwd: PathBuf,
    /// Explicit `(key, value)` pairs — never the parent's full environment
    /// (RUN-006).
    pub environment: Vec<(String, String)>,
    pub timeout: Duration,
    pub graceful_cancel: Duration,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
}

/// Build a minimal explicit environment. With inheritance disabled, only
/// parent variables named in `allowed` are copied. Inheritance is an explicit
/// operator escape hatch; the public configuration keeps it disabled.
/// Credential-shaped names are dropped in both modes as defense in depth.
pub fn build_environment(allowed: &[String], inherit_environment: bool) -> Vec<(String, String)> {
    const CREDENTIAL_NAME_FRAGMENTS: &[&str] = &[
        "token",
        "secret",
        "password",
        "passwd",
        "apikey",
        "api_key",
        "credential",
        "auth",
        "private_key",
        "ssh_key",
    ];
    let is_credential_shaped = |key: &str| {
        let lower = key.to_ascii_lowercase();
        CREDENTIAL_NAME_FRAGMENTS
            .iter()
            .any(|frag| lower.contains(frag))
    };

    let mut out = BTreeMap::new();
    if inherit_environment {
        for (k, v) in std::env::vars() {
            if !is_credential_shaped(&k) {
                out.insert(k, v);
            }
        }
    } else {
        for key in allowed {
            if is_credential_shaped(key) {
                continue;
            }
            if let Ok(value) = std::env::var(key) {
                out.insert(key.clone(), value);
            }
        }
    }
    out.into_iter().collect()
}

/// Terminal outcome of one spawn attempt. Mirrors the subset of
/// `provider_contract::ProviderOutcome` reachable purely from process
/// observation (decode/mapping outcomes are layered on top by S4/S5).
#[derive(Debug)]
pub enum SpawnOutcome {
    /// Process exited (any code) before the timeout elapsed.
    Exited {
        exit_code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdout_truncated: bool,
        stderr_truncated: bool,
    },
    /// The executable could not be spawned at all (not found, not
    /// executable, permission denied).
    Unavailable { reason: String },
    /// Wall-clock timeout elapsed; the process tree was terminated.
    TimedOut {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdout_truncated: bool,
        stderr_truncated: bool,
    },
    /// Caller-requested cancellation terminated the process tree. The outcome
    /// remains `Cancelled` even if the child had already begun exiting zero.
    Cancelled {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdout_truncated: bool,
        stderr_truncated: bool,
    },
}

/// Spawn `plan.command` directly (RUN-001: never through `sh`, `cmd /c`, or
/// PowerShell — `std::process::Command` never invokes a shell unless told
/// to), wait up to `plan.timeout`, and terminate the whole process tree on
/// timeout or when `cancel` is set.
pub fn spawn_and_wait(plan: &SpawnPlan, cancel: &AtomicBool) -> Result<SpawnOutcome> {
    const REAP_TIMEOUT: Duration = Duration::from_secs(2);
    const CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);

    #[cfg(windows)]
    let (effective_command, prefix_arguments) =
        match resolve_execution_command(&plan.command, &plan.cwd, &plan.environment) {
            Some(resolved) => resolved,
            None => {
                return Ok(SpawnOutcome::Unavailable {
                    reason: format!(
                        "failed to resolve {:?} to a real directly executable Windows program",
                        plan.command
                    ),
                });
            }
        };
    #[cfg(windows)]
    let mut command = {
        let mut command = Command::new(&effective_command);
        command.args(prefix_arguments);
        command
    };
    #[cfg(not(windows))]
    let mut command = Command::new(&plan.command);
    // On Windows these caller arguments follow the validated `node <script>`
    // prefix; the loop is intentionally cross-platform and preserves argv boundaries.
    for arg in &plan.arguments {
        command.arg(arg); // each argument passed separately (RUN-001)
    }
    command
        .current_dir(&plan.cwd)
        .env_clear()
        .envs(plan.environment.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0); // isolate into a new process group for tree-kill
    }
    #[cfg(windows)]
    let job = {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
        windows_job::Job::new().map_err(|e| {
            AtlasError::Other(format!("failed to configure provider Job Object: {e}"))
        })?
    };

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Ok(SpawnOutcome::Unavailable {
                reason: format!("failed to spawn {:?}: {e}", plan.command),
            })
        }
    };

    #[cfg(windows)]
    if let Err(containment_error) = job.assign_and_resume(&child) {
        let _ = job.terminate();
        let _ = child.kill();
        drop(child.stdout.take());
        drop(child.stderr.take());
        let cleanup = reap_child(&mut child, REAP_TIMEOUT);
        return Err(AtlasError::Other(match cleanup {
            Ok(()) => format!(
                "provider was terminated before launch because Windows containment failed: \
                 {containment_error}"
            ),
            Err(cleanup_error) => format!(
                "Windows containment failed ({containment_error}); suspended provider cleanup \
                 also failed: {cleanup_error}"
            ),
        }));
    }

    let stdout_handle = child.stdout.take().expect("piped stdout");
    let stderr_handle = child.stderr.take().expect("piped stderr");
    let stdout_reader = spawn_capture_thread(stdout_handle, plan.max_stdout_bytes);
    let stderr_reader = spawn_capture_thread(stderr_handle, plan.max_stderr_bytes);

    let deadline = Instant::now() + plan.timeout;
    let mut timed_out = false;
    let mut cancelled = false;
    let mut lifecycle_error = None;
    let exit_code: Option<i32> = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {}
            Err(error) => {
                lifecycle_error = Some(format!("failed to poll provider process: {error}"));
                break None;
            }
        }
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            break None;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    if timed_out || cancelled || lifecycle_error.is_some() {
        terminate_tree(&mut child, plan.graceful_cancel);
    }
    #[cfg(windows)]
    if let Err(error) = job.terminate() {
        lifecycle_error.get_or_insert_with(|| format!("failed to terminate provider job: {error}"));
    }
    if let Err(error) = reap_child(&mut child, REAP_TIMEOUT) {
        lifecycle_error
            .get_or_insert_with(|| format!("failed to boundedly reap provider process: {error}"));
    }

    let capture_deadline = Instant::now() + CAPTURE_TIMEOUT;
    let (stdout, stdout_truncated) = finish_capture(stdout_reader, capture_deadline);
    let (stderr, stderr_truncated) = finish_capture(stderr_reader, capture_deadline);

    if let Some(error) = lifecycle_error {
        return Err(AtlasError::Other(error));
    }
    if cancelled {
        return Ok(SpawnOutcome::Cancelled {
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        });
    }
    if timed_out {
        return Ok(SpawnOutcome::TimedOut {
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        });
    }
    Ok(SpawnOutcome::Exited {
        exit_code,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    })
}

/// Read from `handle` up to `cap` bytes into the returned buffer, continuing
/// to drain (and discard) anything beyond `cap` so the child never blocks on
/// a full pipe. Returns `(captured_bytes, truncated)`.
fn spawn_capture_thread(
    mut handle: impl Read + Send + 'static,
    cap: usize,
) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut buf = Vec::with_capacity(cap.min(64 * 1024));
        let mut truncated = false;
        let mut scratch = [0u8; 64 * 1024];
        loop {
            match handle.read(&mut scratch) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() < cap {
                        let take = (cap - buf.len()).min(n);
                        buf.extend_from_slice(&scratch[..take]);
                        if take < n {
                            truncated = true;
                        }
                    } else {
                        truncated = true;
                    }
                }
                Err(_) => break,
            }
        }
        (buf, truncated)
    })
}

fn finish_capture(
    reader: std::thread::JoinHandle<(Vec<u8>, bool)>,
    deadline: Instant,
) -> (Vec<u8>, bool) {
    while !reader.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if !reader.is_finished() {
        return (Vec::new(), true);
    }
    reader.join().unwrap_or((Vec::new(), true))
}

fn reap_child(child: &mut std::process::Child, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("child PID {} did not exit within {timeout:?}", child.id()),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Two-stage termination: graceful signal, bounded grace period, then a hard
/// kill of the whole process tree.
fn terminate_tree(child: &mut std::process::Child, graceful_cancel: Duration) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        // Negative pid targets the whole process group created via
        // `process_group(0)`.
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
        let deadline = Instant::now() + graceful_cancel;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    #[cfg(windows)]
    {
        // The Job Object (assigned at spawn time) is terminated by the
        // caller immediately after this returns, which kills the entire
        // tree unconditionally — Windows has no SIGTERM-equivalent graceful
        // stop for an arbitrary console process, so the grace window here is
        // a best-effort pause via `child.kill()` on the direct process only.
        let _ = child.kill();
        std::thread::sleep(graceful_cancel.min(Duration::from_millis(200)));
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = graceful_cancel;
        let _ = child.kill();
    }
}
#[cfg(windows)]
mod windows_job {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_NO_MORE_FILES, FILETIME, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        GetThreadTimes, OpenThread, ResumeThread, THREAD_QUERY_LIMITED_INFORMATION,
        THREAD_SUSPEND_RESUME,
    };

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// Owns a configured Job Object. Closing it kills every process still
    /// assigned to it, including descendants (RUN-008 / S-013).
    pub struct Job {
        handle: OwnedHandle,
    }

    impl Job {
        pub fn new() -> io::Result<Self> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let job = OwnedHandle(job);
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let configured = SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if configured == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Self { handle: job })
            }
        }

        pub fn assign_and_resume(&self, child: &Child) -> io::Result<()> {
            unsafe {
                let process = child.as_raw_handle() as HANDLE;
                if AssignProcessToJobObject(self.handle.0, process) == 0 {
                    return Err(io::Error::last_os_error());
                }
                resume_only_thread(child.id())
            }
        }

        pub fn terminate(&self) -> io::Result<()> {
            unsafe {
                if TerminateJobObject(self.handle.0, 1) == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            }
        }
    }

    unsafe fn resume_only_thread(process_id: u32) -> io::Result<()> {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let snapshot = OwnedHandle(snapshot);
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        if Thread32First(snapshot.0, &mut entry) == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut primary: Option<(u64, OwnedHandle)> = None;
        loop {
            if entry.th32OwnerProcessID == process_id {
                let thread = OpenThread(
                    THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
                    0,
                    entry.th32ThreadID,
                );
                if thread.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let thread = OwnedHandle(thread);
                let mut creation: FILETIME = std::mem::zeroed();
                let mut exit: FILETIME = std::mem::zeroed();
                let mut kernel: FILETIME = std::mem::zeroed();
                let mut user: FILETIME = std::mem::zeroed();
                if GetThreadTimes(thread.0, &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
                    return Err(io::Error::last_os_error());
                }
                let created =
                    ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
                if primary
                    .as_ref()
                    .is_none_or(|(earliest, _)| created < *earliest)
                {
                    primary = Some((created, thread));
                }
            }
            if Thread32Next(snapshot.0, &mut entry) == 0 {
                let error = GetLastError();
                if error != ERROR_NO_MORE_FILES {
                    return Err(io::Error::from_raw_os_error(error as i32));
                }
                break;
            }
        }

        let (_, primary) = primary.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no primary thread found for suspended process PID {process_id}"),
            )
        })?;
        if ResumeThread(primary.0) == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Output ownership + validation (RUN-005, RUN-010, S-001, S-002)
// ---------------------------------------------------------------------------

/// Create a private, Atlas-owned output directory for one execution. Provider
/// stdout never selects this path.
pub fn make_execution_output_dir(temp_root: &Path, execution_id: &str) -> Result<PathBuf> {
    let dir = temp_root.join(execution_id);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[derive(Debug, Clone)]
pub struct OutputArtifact {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}

/// Validate a provider's declared output file before any decode is
/// attempted: it must resolve inside `temp_root` (path confinement, S-002)
/// and must not exceed `max_output_bytes` (checked via metadata length —
/// RUN-010 "reject before decode" — the file is never read into memory to
/// discover it is oversized).
pub fn validate_output_artifact(
    temp_root: &Path,
    declared_path: &Path,
    max_output_bytes: u64,
) -> Result<OutputArtifact> {
    let candidate_abs = if declared_path.is_absolute() {
        declared_path.to_path_buf()
    } else {
        temp_root.join(declared_path)
    };
    let confined = paths::confine_to_root(temp_root, &candidate_abs)?;
    let full_path = temp_root.join(&confined);
    let metadata = std::fs::metadata(&full_path).map_err(|e| {
        AtlasError::Other(format!(
            "provider output {full_path:?} missing or unreadable: {e}"
        ))
    })?;
    if metadata.len() > max_output_bytes {
        return Err(AtlasError::Other(format!(
            "provider output {full_path:?} is {} bytes, exceeds max_output_bytes {max_output_bytes} \
             (rejected before decode)",
            metadata.len()
        )));
    }
    let sha256 = crate::hashing::content_hash_of_file(&full_path)?;
    Ok(OutputArtifact {
        path: full_path,
        sha256,
        bytes: metadata.len(),
    })
}

// ---------------------------------------------------------------------------
// Executable resolution + probing (RUN-002, RUN-003, RUN-013)
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
pub fn resolve_executable_path(command: &str) -> Option<PathBuf> {
    let candidate = Path::new(command);
    if candidate.components().count() > 1 {
        return if candidate.is_file() {
            Some(candidate.to_path_buf())
        } else {
            None
        };
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(command))
        .find(|path| path.is_file())
}

#[cfg(windows)]
fn environment_value<'a>(environment: &'a [(String, String)], name: &str) -> Option<&'a str> {
    environment
        .iter()
        .rev()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[cfg(windows)]
fn resolve_executable_path(
    command: &str,
    cwd: &Path,
    environment: &[(String, String)],
) -> Option<PathBuf> {
    let candidate = Path::new(command);
    let path_value = environment_value(environment, "PATH");
    let path_ext = environment_value(environment, "PATHEXT")
        .unwrap_or(".COM;.EXE;.BAT;.CMD")
        .split(';')
        .filter(|extension| !extension.is_empty())
        .collect::<Vec<_>>();

    if candidate.is_absolute() || candidate.components().count() > 1 {
        let explicit = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            cwd.join(candidate)
        };
        return resolve_windows_candidate(&explicit, &path_ext);
    }

    let path_value = path_value?;
    std::env::split_paths(path_value).find_map(|entry| {
        let directory = if entry.as_os_str().is_empty() || entry.is_relative() {
            cwd.join(entry)
        } else {
            entry
        };
        resolve_windows_candidate(&directory.join(candidate), &path_ext)
    })
}

#[cfg(windows)]
fn resolve_windows_candidate(candidate: &Path, path_ext: &[&str]) -> Option<PathBuf> {
    if candidate.extension().is_some() {
        return is_real_executable_file(candidate).then(|| candidate.to_path_buf());
    }
    path_ext.iter().find_map(|extension| {
        let mut executable_name = candidate.as_os_str().to_os_string();
        executable_name.push(extension);
        let executable = PathBuf::from(executable_name);
        is_real_executable_file(&executable).then_some(executable)
    })
}

#[cfg(windows)]
fn candidate_metadata_is_real(is_file: bool, file_attributes: u32, len: u64) -> bool {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    is_file && (file_attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 || len != 0)
}

#[cfg(windows)]
fn is_real_executable_file(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;

    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    candidate_metadata_is_real(
        metadata.is_file(),
        metadata.file_attributes(),
        metadata.len(),
    )
}

#[cfg(windows)]
fn is_windows_batch_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

/// Parse only the two npm `cmd-shim` invocation lines Atlas explicitly
/// supports: direct `node` and the current generated `%_prog%` tail.
#[cfg(windows)]
fn parse_npm_cmd_shim_invocation<'a>(
    line: &'a str,
    exact_program_prefix: &str,
    allow_parent_script: bool,
) -> Option<&'a str> {
    let quoted_script = line.strip_prefix(exact_program_prefix)?.strip_prefix('"')?;
    let closing_quote = quoted_script.find('"')?;
    if &quoted_script[closing_quote + 1..] != " %*" {
        return None;
    }
    let script_token = &quoted_script[..closing_quote];
    let relative_script = script_token.strip_prefix("%dp0%\\")?;
    let mut components = relative_script.split('\\');
    if components.clone().next() == Some("..") {
        if !allow_parent_script {
            return None;
        }
        components.next();
    }
    if relative_script.is_empty()
        || !relative_script.ends_with(".js")
        || !relative_script.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '\\' | '@' | '_' | '-' | '.' | ' ')
        })
        || components.any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component.ends_with(' ')
                || component.ends_with('.')
        })
    {
        return None;
    }
    Some(relative_script)
}

#[cfg(windows)]
fn parse_npm_cmd_shim(text: &str, allow_parent_script: bool) -> Option<&str> {
    const GENERATED_PROGRAM: &str =
        "endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  ";

    let framed = text.strip_suffix("\r\n")?;
    let lines = framed.split("\r\n");
    if lines
        .clone()
        .any(|line| line.as_bytes().contains(&b'\r') || line.as_bytes().contains(&b'\n'))
    {
        return None;
    }

    let mut direct = lines.clone();
    if direct.next() == Some("@ECHO off") {
        if let Some(invocation) = direct.next() {
            if direct.next().is_none() {
                if let Some(script) =
                    parse_npm_cmd_shim_invocation(invocation, "node ", allow_parent_script)
                {
                    return Some(script);
                }
            }
        }
    }

    let mut generated = lines;
    for expected in [
        "@ECHO off",
        "GOTO start",
        ":find_dp0",
        "SET dp0=%~dp0",
        "EXIT /b",
        ":start",
        "SETLOCAL",
        "CALL :find_dp0",
        "",
        "IF EXIST \"%dp0%\\node.exe\" (",
        "  SET \"_prog=%dp0%\\node.exe\"",
        ") ELSE (",
        "  SET \"_prog=node\"",
        "  SET PATHEXT=%PATHEXT:;.JS;=;%",
        ")",
        "",
    ] {
        if generated.next() != Some(expected) {
            return None;
        }
    }
    let invocation = generated.next()?;
    if generated.next().is_some() {
        return None;
    }
    parse_npm_cmd_shim_invocation(invocation, GENERATED_PROGRAM, allow_parent_script)
}

#[cfg(windows)]
fn is_dos_safe_path_component(component: &std::ffi::OsStr) -> bool {
    let Some(component) = component.to_str() else {
        return false;
    };
    if component.is_empty()
        || component.ends_with([' ', '.'])
        || component.contains(['<', '>', ':', '"', '|', '?', '*'])
    {
        return false;
    }
    let stem = component.split('.').next().unwrap_or_default();
    let reserved_name = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"]
        .iter()
        .any(|reserved| stem.eq_ignore_ascii_case(reserved));
    let reserved_port = stem.len() == 4
        && stem.get(..3).is_some_and(|prefix| {
            prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT")
        })
        && matches!(stem.as_bytes()[3], b'1'..=b'9');
    !reserved_name && !reserved_port
}

#[cfg(windows)]
fn canonical_windows_command_argument(path: &Path) -> Option<String> {
    let normal_path = path.to_str()?.strip_prefix(r"\\?\")?;
    if normal_path.starts_with("UNC\\") {
        return None;
    }
    let mut components = Path::new(normal_path).components();
    if !matches!(
        components.next(),
        Some(std::path::Component::Prefix(prefix))
            if matches!(prefix.kind(), std::path::Prefix::Disk(_))
    ) || !matches!(components.next(), Some(std::path::Component::RootDir))
    {
        return None;
    }
    if components.any(|component| match component {
        std::path::Component::Normal(name) => !is_dos_safe_path_component(name),
        _ => true,
    }) {
        return None;
    }
    Some(normal_path.to_string())
}

/// Resolve a recognized npm shim to `node <script.js>` without invoking a
/// shell. Unknown `.cmd` contents fail closed.
#[cfg(windows)]
fn resolve_npm_cmd_shim(
    resolved_path: &Path,
    cwd: &Path,
    environment: &[(String, String)],
) -> Option<(String, String)> {
    if resolved_path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("cmd"))
        != Some(true)
    {
        return None;
    }
    let text = std::fs::read_to_string(resolved_path).ok()?;
    let shim_directory = resolved_path.parent()?;
    let allow_parent_script = shim_directory
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(".bin"));
    let relative_script = parse_npm_cmd_shim(&text, allow_parent_script)?;
    let script_path = shim_directory.join(relative_script);
    if !script_path.is_file() {
        return None;
    }
    let allowed_directory = if relative_script.starts_with("..\\") {
        shim_directory.parent()?
    } else {
        shim_directory
    };
    let canonical_directory = allowed_directory.canonicalize().ok()?;
    let canonical_script = script_path.canonicalize().ok()?;
    if !canonical_script.starts_with(canonical_directory) {
        return None;
    }
    let script_argument = canonical_windows_command_argument(&canonical_script)?;
    let node_path = resolve_executable_path("node", cwd, environment)?;
    if is_windows_batch_file(&node_path) {
        return None;
    }
    Some((node_path.to_string_lossy().into_owned(), script_argument))
}

#[cfg(windows)]
fn resolve_execution_command(
    command: &str,
    cwd: &Path,
    environment: &[(String, String)],
) -> Option<(String, Vec<String>)> {
    let resolved = resolve_executable_path(command, cwd, environment)?;
    let extension = resolved
        .extension()
        .and_then(|extension| extension.to_str());
    let is_cmd = extension.is_some_and(|extension| extension.eq_ignore_ascii_case("cmd"));
    let is_bat = extension.is_some_and(|extension| extension.eq_ignore_ascii_case("bat"));
    if is_cmd {
        let (node_path, script_path) = resolve_npm_cmd_shim(&resolved, cwd, environment)?;
        return Some((node_path, vec![script_path]));
    }
    if is_bat {
        return None;
    }
    Some((resolved.to_string_lossy().into_owned(), vec![]))
}

/// Probe a configured provider executable by invoking it with
/// `probe_arguments` (e.g. `--help`) and building a
/// `provider_contract::ProviderProbeResult`. Never claims network isolation
/// the platform cannot actually guarantee (RUN-013): cross-platform Atlas
/// core reports `not_available` rather than `enforced`/`best_effort` unless
/// a real sandbox layer is wired in (none is, in V1.1 — `network_isolation_policy`
/// remains a workspace-level policy check, not an enforcement mechanism).
pub fn probe_provider(
    provider_name: &str,
    command: &str,
    probe_arguments: &[String],
    cwd: &Path,
    environment: Vec<(String, String)>,
    timeout: Duration,
) -> crate::provider_contract::ProviderProbeResult {
    use crate::provider_contract::{
        DiagnosticSeverity, NetworkIsolationState, ProbeDiagnostic, ProbeStatus,
        ProviderProbeResult, PROTOCOL_VERSION,
    };

    #[cfg(windows)]
    let resolved = resolve_executable_path(command, cwd, &environment);
    #[cfg(not(windows))]
    let resolved = resolve_executable_path(command);
    let executable_hash = resolved
        .as_deref()
        .and_then(|path| crate::hashing::content_hash_of_file(path).ok());
    let observed_at = crate::migrations::iso8601_now();

    if resolved.is_none() {
        return ProviderProbeResult {
            schema_version: PROTOCOL_VERSION.to_string(),
            provider_name: provider_name.to_string(),
            status: ProbeStatus::Unavailable,
            observed_version: None,
            executable_path: None,
            executable_hash: None,
            supported_output_formats: vec![],
            supported_arguments: vec![],
            network_isolation_state: NetworkIsolationState::NotApplicable,
            observed_at,
            diagnostics: vec![ProbeDiagnostic {
                severity: DiagnosticSeverity::Error,
                code: "executable_not_found".to_string(),
                message: format!("{command:?} was not found on the provider PATH"),
            }],
        };
    }

    let plan = SpawnPlan {
        command: command.to_string(),
        arguments: probe_arguments.to_vec(),
        cwd: cwd.to_path_buf(),
        environment,
        timeout,
        graceful_cancel: Duration::from_millis(500),
        max_stdout_bytes: 65_536,
        max_stderr_bytes: 65_536,
    };
    let outcome = spawn_and_wait(&plan, &AtomicBool::new(false));

    let isolation_diagnostic = ProbeDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: "network_isolation_not_enforced".to_string(),
        message: "No platform network sandbox is configured; credentials remain withheld, \
                   but this is not a guarantee of no network access."
            .to_string(),
    };

    match outcome {
        Ok(SpawnOutcome::Unavailable { reason }) => ProviderProbeResult {
            schema_version: PROTOCOL_VERSION.to_string(),
            provider_name: provider_name.to_string(),
            status: ProbeStatus::Unavailable,
            observed_version: None,
            executable_path: resolved.map(|p| p.to_string_lossy().into_owned()),
            executable_hash,
            supported_output_formats: vec![],
            supported_arguments: vec![],
            network_isolation_state: NetworkIsolationState::NotApplicable,
            observed_at,
            diagnostics: vec![ProbeDiagnostic {
                severity: DiagnosticSeverity::Error,
                code: "spawn_failed".to_string(),
                message: redact_diagnostic(&reason, 512),
            }],
        },
        Ok(SpawnOutcome::Exited {
            exit_code: Some(0), ..
        }) => ProviderProbeResult {
            schema_version: PROTOCOL_VERSION.to_string(),
            provider_name: provider_name.to_string(),
            status: ProbeStatus::Available,
            observed_version: None,
            executable_path: resolved.map(|p| p.to_string_lossy().into_owned()),
            executable_hash,
            supported_output_formats: vec![],
            supported_arguments: vec![],
            network_isolation_state: NetworkIsolationState::NotAvailable,
            observed_at,
            diagnostics: vec![isolation_diagnostic],
        },
        Ok(SpawnOutcome::TimedOut { .. }) => ProviderProbeResult {
            schema_version: PROTOCOL_VERSION.to_string(),
            provider_name: provider_name.to_string(),
            status: ProbeStatus::Misconfigured,
            observed_version: None,
            executable_path: resolved.map(|p| p.to_string_lossy().into_owned()),
            executable_hash,
            supported_output_formats: vec![],
            supported_arguments: vec![],
            network_isolation_state: NetworkIsolationState::NotApplicable,
            observed_at,
            diagnostics: vec![ProbeDiagnostic {
                severity: DiagnosticSeverity::Error,
                code: "probe_timed_out".to_string(),
                message: "probe invocation exceeded the configured timeout".to_string(),
            }],
        },
        _ => {
            let (exit_code, stderr): (Option<i32>, Vec<u8>) = match outcome {
                Ok(SpawnOutcome::Exited {
                    exit_code, stderr, ..
                }) => (exit_code, stderr),
                Ok(SpawnOutcome::Cancelled { stderr, .. }) => (None, stderr),
                _ => (None, Vec::new()),
            };
            ProviderProbeResult {
                schema_version: PROTOCOL_VERSION.to_string(),
                provider_name: provider_name.to_string(),
                status: ProbeStatus::Misconfigured,
                observed_version: None,
                executable_path: resolved.map(|p| p.to_string_lossy().into_owned()),
                executable_hash,
                supported_output_formats: vec![],
                supported_arguments: vec![],
                network_isolation_state: NetworkIsolationState::NotApplicable,
                observed_at,
                diagnostics: vec![ProbeDiagnostic {
                    severity: DiagnosticSeverity::Error,
                    code: "probe_nonzero_exit".to_string(),
                    message: format!(
                        "probe exited {:?}: {}",
                        exit_code,
                        redact_diagnostic(&String::from_utf8_lossy(&stderr), 512)
                    ),
                }],
            }
        }
    }
}
// ---------------------------------------------------------------------------
// Diagnostic redaction (SEC-001)
// ---------------------------------------------------------------------------

/// Bound and redact raw stdout, stderr, or diagnostics before persistence.
/// Raw source bodies, environment values, process environments, and
/// credential-bearing arguments are never logged.
pub fn redact_diagnostic(raw: &str, max_len: usize) -> String {
    const PATTERNS: &[&str] = &[
        "token=",
        "secret=",
        "password=",
        "apikey=",
        "api_key=",
        "authorization:",
    ];
    let lower = raw.to_ascii_lowercase();
    let mut out = String::new();
    let mut cursor = 0usize;
    for pattern in PATTERNS {
        if let Some(pos) = lower[cursor..].find(pattern) {
            let abs = cursor + pos + pattern.len();
            out.push_str(&raw[cursor..abs]);
            // Redact the token-shaped run of non-whitespace/bracket
            // characters following the marker.
            let rest = &raw[abs..];
            let value_len = rest
                .find(|c: char| c.is_whitespace() || c == ']' || c == '"')
                .unwrap_or(rest.len());
            out.push_str("[REDACTED]");
            cursor = abs + value_len;
        }
    }
    out.push_str(&raw[cursor.min(raw.len())..]);
    if out.len() > max_len {
        out.truncate(max_len);
        out.push_str("...[truncated]");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[cfg(windows)]
    const WINDOWS_CONTAINMENT_PROBE_ENV: &str = "ATLAS_WINDOWS_CONTAINMENT_PROBE";

    #[cfg(windows)]
    fn process_has_exited(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };

        unsafe {
            let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if process.is_null() {
                return true;
            }
            let exited = WaitForSingleObject(process, 0) == WAIT_OBJECT_0;
            CloseHandle(process);
            exited
        }
    }

    #[cfg(windows)]
    fn terminate_exact_process_tree(child: &mut std::process::Child) {
        let mut killer = Command::new("taskkill.exe")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("taskkill must start for watchdog cleanup");
        let deadline = Instant::now() + Duration::from_secs(3);
        while killer.try_wait().expect("poll taskkill").is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = killer.kill();
        let _ = reap_child(&mut killer, Duration::from_secs(1));
        let _ = child.kill();
        reap_child(child, Duration::from_secs(3))
            .expect("watchdog must boundedly terminate and reap its exact probe PID");
    }

    fn python_command() -> String {
        "python".to_string()
    }

    fn mock_provider_path() -> PathBuf {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        PathBuf::from(manifest_dir).join("tests/provider_runtime/mock_provider.py")
    }

    fn write_request(dir: &Path, execution_id: &str) -> PathBuf {
        let request_path = dir.join("request.json");
        std::fs::write(
            &request_path,
            serde_json::json!({ "execution_id": execution_id }).to_string(),
        )
        .unwrap();
        request_path
    }

    fn base_plan(dir: &Path, mode: &str, request_path: &Path, output_path: &Path) -> SpawnPlan {
        SpawnPlan {
            command: python_command(),
            arguments: vec![
                mock_provider_path().to_string_lossy().into_owned(),
                "--request".to_string(),
                request_path.to_string_lossy().into_owned(),
                "--output".to_string(),
                output_path.to_string_lossy().into_owned(),
                "--mode".to_string(),
                mode.to_string(),
            ],
            cwd: dir.to_path_buf(),
            environment: build_environment(
                &[
                    "PATH".to_string(),
                    "PATHEXT".to_string(),
                    "SYSTEMROOT".to_string(),
                    "TEMP".to_string(),
                    "TMP".to_string(),
                    "HOME".to_string(),
                    "USERPROFILE".to_string(),
                    "APPDATA".to_string(),
                    "LOCALAPPDATA".to_string(),
                ],
                false,
            ),
            timeout: Duration::from_secs(20),
            graceful_cancel: Duration::from_millis(200),
            max_stdout_bytes: 4096,
            max_stderr_bytes: 4096,
        }
    }

    #[test]
    fn mock_provider_success_produces_zero_exit_and_output_file() {
        let dir = tempfile::tempdir().unwrap();
        let request = write_request(dir.path(), "pex_1");
        let output = dir.path().join("out.json");
        let plan = base_plan(dir.path(), "success", &request, &output);
        let cancel = AtomicBool::new(false);
        let outcome = spawn_and_wait(&plan, &cancel).unwrap();
        match outcome {
            SpawnOutcome::Exited { exit_code, .. } => assert_eq!(exit_code, Some(0)),
            other => panic!("expected Exited, got {other:?}"),
        }
        assert!(output.exists());
        let artifact =
            validate_output_artifact(dir.path(), Path::new("out.json"), 1_000_000).unwrap();
        assert_eq!(artifact.bytes, std::fs::metadata(&output).unwrap().len());
        assert_eq!(artifact.sha256.len(), 64);
    }

    #[test]
    fn mock_provider_partial_reports_zero_exit_with_partial_status_in_payload() {
        let dir = tempfile::tempdir().unwrap();
        let request = write_request(dir.path(), "pex_2");
        let output = dir.path().join("out.json");
        let plan = base_plan(dir.path(), "partial", &request, &output);
        let outcome = spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap();
        let SpawnOutcome::Exited { exit_code, .. } = outcome else {
            panic!("expected Exited")
        };
        assert_eq!(exit_code, Some(0));
        let text = std::fs::read_to_string(&output).unwrap();
        assert!(text.contains("\"partial\""));
    }

    #[test]
    fn nonexistent_executable_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let plan = SpawnPlan {
            command: "this-executable-definitely-does-not-exist-12345".to_string(),
            arguments: vec![],
            cwd: dir.path().to_path_buf(),
            environment: vec![],
            timeout: Duration::from_secs(5),
            graceful_cancel: Duration::from_millis(100),
            max_stdout_bytes: 1024,
            max_stderr_bytes: 1024,
        };
        let outcome = spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap();
        assert!(matches!(outcome, SpawnOutcome::Unavailable { .. }));
    }

    #[test]
    fn timeout_terminates_the_process_tree() {
        let dir = tempfile::tempdir().unwrap();
        let request = write_request(dir.path(), "pex_3");
        let output = dir.path().join("out.json");
        let mut plan = base_plan(dir.path(), "timeout", &request, &output);
        plan.arguments
            .extend(["--sleep-seconds".to_string(), "30".to_string()]);
        plan.timeout = Duration::from_millis(500);
        plan.graceful_cancel = Duration::from_millis(100);
        let started = Instant::now();
        let outcome = spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap();
        let elapsed = started.elapsed();
        assert!(matches!(outcome, SpawnOutcome::TimedOut { .. }));
        assert!(
            elapsed < Duration::from_secs(5),
            "process tree must be killed promptly, took {elapsed:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_launcher_descendant_is_contained_under_outer_watchdog() {
        let mut probe = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "provider_runtime::tests::windows_launcher_descendant_probe",
                "--nocapture",
            ])
            .env(WINDOWS_CONTAINMENT_PROBE_ENV, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let watchdog = Duration::from_secs(15);
        let deadline = Instant::now() + watchdog;
        let status = loop {
            match probe.try_wait().unwrap() {
                Some(status) => break status,
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                None => {
                    terminate_exact_process_tree(&mut probe);
                    panic!("Windows containment probe exceeded outer watchdog {watchdog:?}");
                }
            }
        };
        assert!(
            status.success(),
            "Windows containment probe failed: {status}"
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "subprocess probe invoked by windows_launcher_descendant_is_contained_under_outer_watchdog"]
    fn windows_launcher_descendant_probe() {
        if std::env::var_os(WINDOWS_CONTAINMENT_PROBE_ENV).is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let request = write_request(dir.path(), "pex_windows_containment");
        let output = dir.path().join("out.json");
        let pid_file = dir.path().join("contained-pids.json");
        let mut plan = base_plan(dir.path(), "descendant-launcher", &request, &output);
        plan.arguments.extend([
            "--sleep-seconds".to_string(),
            "30".to_string(),
            "--pid-file".to_string(),
            pid_file.to_string_lossy().into_owned(),
        ]);
        plan.timeout = Duration::from_secs(2);
        plan.graceful_cancel = Duration::from_millis(100);

        let started = Instant::now();
        let outcome = spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap();
        assert!(matches!(outcome, SpawnOutcome::TimedOut { .. }));
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "containment and capture cleanup must remain bounded"
        );

        let marker: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&pid_file).unwrap()).unwrap();
        assert_eq!(marker["descendant_in_job"], true);
        let launcher_pid = marker["launcher_pid"].as_u64().unwrap() as u32;
        let descendant_pid = marker["descendant_pid"].as_u64().unwrap() as u32;
        let cleanup_deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < cleanup_deadline
            && (!process_has_exited(launcher_pid) || !process_has_exited(descendant_pid))
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            process_has_exited(launcher_pid),
            "launcher PID {launcher_pid} survived"
        );
        assert!(
            process_has_exited(descendant_pid),
            "descendant PID {descendant_pid} survived"
        );
    }

    #[test]
    fn cancellation_flag_terminates_the_process_tree() {
        let dir = tempfile::tempdir().unwrap();
        let request = write_request(dir.path(), "pex_4");
        let output = dir.path().join("out.json");
        let mut plan = base_plan(dir.path(), "timeout", &request, &output);
        plan.arguments
            .extend(["--sleep-seconds".to_string(), "30".to_string()]);
        plan.timeout = Duration::from_secs(30);
        plan.graceful_cancel = Duration::from_millis(100);
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_clone = cancel.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel_clone.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let outcome = spawn_and_wait(&plan, &cancel).unwrap();
        handle.join().unwrap();
        assert!(matches!(outcome, SpawnOutcome::Cancelled { .. }));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn nonzero_exit_is_captured_with_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let request = write_request(dir.path(), "pex_5");
        let output = dir.path().join("out.json");
        let plan = base_plan(dir.path(), "failure", &request, &output);
        let outcome = spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap();
        let SpawnOutcome::Exited {
            exit_code, stderr, ..
        } = outcome
        else {
            panic!("expected Exited")
        };
        assert_eq!(exit_code, Some(7));
        assert!(String::from_utf8_lossy(&stderr).contains("mock provider failed"));
    }

    #[test]
    fn invalid_output_bytes_are_still_captured_for_the_decoder_to_reject() {
        let dir = tempfile::tempdir().unwrap();
        let request = write_request(dir.path(), "pex_6");
        let output = dir.path().join("out.bin");
        let plan = base_plan(dir.path(), "invalid", &request, &output);
        let outcome = spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap();
        assert!(matches!(
            outcome,
            SpawnOutcome::Exited {
                exit_code: Some(0),
                ..
            }
        ));
        let bytes = std::fs::read(&output).unwrap();
        assert_eq!(bytes, b"not-json-or-scip");
    }

    #[test]
    fn oversized_output_is_rejected_before_decode() {
        let dir = tempfile::tempdir().unwrap();
        let request = write_request(dir.path(), "pex_7");
        let output = dir.path().join("out.bin");
        let plan = base_plan(dir.path(), "oversized", &request, &output);
        let outcome = spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap();
        assert!(matches!(
            outcome,
            SpawnOutcome::Exited {
                exit_code: Some(0),
                ..
            }
        ));
        let err = validate_output_artifact(dir.path(), Path::new("out.bin"), 1024).unwrap_err();
        assert!(format!("{err}").contains("exceeds max_output_bytes"));
    }

    #[test]
    fn output_path_outside_temp_root_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("escaped.json"), b"{}").unwrap();
        let escape_attempt = Path::new("../../escaped.json");
        let err = validate_output_artifact(dir.path(), escape_attempt, 1_000_000).unwrap_err();
        assert!(matches!(err, AtlasError::PathEscape { .. }));
    }

    #[test]
    fn stdout_stderr_truncation_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let request = write_request(dir.path(), "pex_8");
        let output = dir.path().join("out.json");
        let mut plan = base_plan(dir.path(), "success", &request, &output);
        plan.max_stdout_bytes = 4; // the mock's stdout line is longer than 4 bytes
        let outcome = spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap();
        let SpawnOutcome::Exited {
            stdout,
            stdout_truncated,
            ..
        } = outcome
        else {
            panic!("expected Exited")
        };
        assert!(stdout_truncated);
        assert_eq!(stdout.len(), 4);
    }

    #[test]
    fn environment_allowlist_excludes_unlisted_variables() {
        std::env::set_var("ATLAS_TEST_CANARY_SECRET", "should-not-be-visible");
        let env = build_environment(&["PATH".to_string()], false);
        assert!(env.iter().all(|(k, _)| k != "ATLAS_TEST_CANARY_SECRET"));
        std::env::remove_var("ATLAS_TEST_CANARY_SECRET");
    }

    #[test]
    fn environment_allowlist_drops_credential_shaped_names_even_if_listed() {
        std::env::set_var("MY_API_TOKEN", "super-secret-value");
        let env = build_environment(&["MY_API_TOKEN".to_string()], false);
        assert!(env.iter().all(|(k, _)| k != "MY_API_TOKEN"));
        std::env::remove_var("MY_API_TOKEN");
    }

    #[test]
    fn redact_diagnostic_masks_known_secret_patterns() {
        let raw = "bounded diagnostic: token=super-secret-abc123 trailing text";
        let redacted = redact_diagnostic(raw, 4096);
        assert!(!redacted.contains("super-secret-abc123"));
        assert!(redacted.contains("token=[REDACTED]"));
        assert!(redacted.contains("trailing text"));
    }

    #[test]
    fn redact_diagnostic_bounds_length() {
        let raw = "x".repeat(10_000);
        let redacted = redact_diagnostic(&raw, 100);
        assert!(redacted.len() <= 120);
    }

    #[test]
    fn mock_provider_stderr_diagnostic_is_captured_raw_and_redactable() {
        let dir = tempfile::tempdir().unwrap();
        let request = write_request(dir.path(), "pex_9");
        let output = dir.path().join("out.json");
        let plan = base_plan(dir.path(), "stderr", &request, &output);
        let outcome = spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap();
        let SpawnOutcome::Exited { stderr, .. } = outcome else {
            panic!("expected Exited")
        };
        let raw = String::from_utf8_lossy(&stderr);
        assert!(raw.contains("token=[REDACT_ME]"));
        let redacted = redact_diagnostic(&raw, 4096);
        assert!(
            !redacted.contains("[REDACT_ME]"),
            "the redaction layer must mask it before persistence"
        );
    }
    #[cfg(unix)]
    #[test]
    fn non_windows_spawn_retains_native_relative_command_semantics() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("native-provider");
        std::fs::write(&executable, "#!/bin/sh\nprintf native-command\n").unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();
        let plan = SpawnPlan {
            command: "./native-provider".to_string(),
            arguments: vec![],
            cwd: dir.path().to_path_buf(),
            environment: vec![],
            timeout: Duration::from_secs(5),
            graceful_cancel: Duration::from_millis(100),
            max_stdout_bytes: 1024,
            max_stderr_bytes: 1024,
        };

        let SpawnOutcome::Exited {
            exit_code, stdout, ..
        } = spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap()
        else {
            panic!("native relative command must execute")
        };
        assert_eq!(exit_code, Some(0));
        assert_eq!(stdout, b"native-command");
    }

    #[cfg(windows)]
    fn write_dummy_executable(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"not an alias stub").unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_resolution_uses_only_controlled_child_path() {
        let dir = tempfile::tempdir().unwrap();
        let parent_bin = dir.path().join("parent-bin");
        let child_bin = dir.path().join("child-bin");
        write_dummy_executable(&parent_bin.join("provider.EXE"));
        write_dummy_executable(&child_bin.join("provider.EXE"));
        let parent_environment = vec![
            (
                "PATH".to_string(),
                parent_bin.to_string_lossy().into_owned(),
            ),
            ("PATHEXT".to_string(), ".EXE".to_string()),
        ];
        let child_environment = vec![
            ("Path".to_string(), child_bin.to_string_lossy().into_owned()),
            ("PATHEXT".to_string(), ".EXE".to_string()),
        ];

        assert_eq!(
            resolve_executable_path("provider", dir.path(), &parent_environment),
            Some(parent_bin.join("provider.EXE"))
        );
        assert_eq!(
            resolve_executable_path("provider", dir.path(), &child_environment),
            Some(child_bin.join("provider.EXE"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_relative_child_path_resolves_from_plan_cwd() {
        let dir = tempfile::tempdir().unwrap();
        write_dummy_executable(&dir.path().join("tools/provider.EXE"));
        let environment = vec![
            ("PATH".to_string(), "tools".to_string()),
            ("PATHEXT".to_string(), ".EXE".to_string()),
        ];

        assert_eq!(
            resolve_executable_path("provider", dir.path(), &environment),
            Some(dir.path().join("tools/provider.EXE"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_explicit_relative_and_absolute_paths_share_candidate_rules() {
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("tools/provider.EXE");
        write_dummy_executable(&executable);
        let environment = vec![("PATHEXT".to_string(), ".EXE".to_string())];

        assert_eq!(
            resolve_executable_path("tools/provider", dir.path(), &environment),
            Some(executable.clone())
        );
        assert_eq!(
            resolve_executable_path(executable.to_str().unwrap(), dir.path(), &environment),
            Some(executable)
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_alias_metadata_is_rejected() {
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

        assert!(!candidate_metadata_is_real(
            true,
            FILE_ATTRIBUTE_REPARSE_POINT,
            0
        ));
        assert!(candidate_metadata_is_real(
            true,
            FILE_ATTRIBUTE_REPARSE_POINT,
            1
        ));
        assert!(!candidate_metadata_is_real(false, 0, 1));
    }

    #[cfg(windows)]
    #[test]
    fn windows_unrecognized_cmd_fails_closed_without_running_code() {
        let dir = tempfile::tempdir().unwrap();
        let provider_marker = dir.path().join("provider-ran.txt");
        let descendant_marker = dir.path().join("descendant-ran.txt");
        let shim = dir.path().join("unsafe.cmd");
        std::fs::write(
            &shim,
            format!(
                "@echo provider>{}\r\nstart \"\" /b cmd /c \"echo descendant>{}\"\r\n",
                provider_marker.display(),
                descendant_marker.display()
            ),
        )
        .unwrap();
        let plan = SpawnPlan {
            command: "unsafe".to_string(),
            arguments: vec![],
            cwd: dir.path().to_path_buf(),
            environment: vec![
                (
                    "PATH".to_string(),
                    dir.path().to_string_lossy().into_owned(),
                ),
                ("PATHEXT".to_string(), ".CMD".to_string()),
            ],
            timeout: Duration::from_secs(5),
            graceful_cancel: Duration::from_millis(100),
            max_stdout_bytes: 1024,
            max_stderr_bytes: 1024,
        };

        assert!(matches!(
            spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap(),
            SpawnOutcome::Unavailable { .. }
        ));
        assert!(
            !provider_marker.exists(),
            "rejected provider command must never run"
        );
        assert!(
            !descendant_marker.exists(),
            "rejected provider must not launch a descendant"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_comment_js_reference_fails_closed_without_running_payload() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("payload-ran.txt");
        let shim = dir.path().join("unknown.cmd");
        let payload = dir.path().join("payload.js");
        std::fs::write(
            &shim,
            "@ECHO off\r\nREM This is not an npm shim: \"%dp0%\\payload.js\"\r\nECHO arbitrary batch body\r\n",
        )
        .unwrap();
        std::fs::write(
            &payload,
            "require('fs').writeFileSync(process.argv[2], 'executed');",
        )
        .unwrap();
        let child_path = format!(
            "{};{}",
            dir.path().display(),
            std::env::var("PATH").expect("test requires the installed node PATH")
        );
        let plan = SpawnPlan {
            command: "unknown".to_string(),
            arguments: vec![marker.to_string_lossy().into_owned()],
            cwd: dir.path().to_path_buf(),
            environment: vec![
                ("PATH".to_string(), child_path),
                ("PATHEXT".to_string(), ".CMD;.EXE".to_string()),
                (
                    "SYSTEMROOT".to_string(),
                    std::env::var("SYSTEMROOT").expect("Windows requires SYSTEMROOT"),
                ),
            ],
            timeout: Duration::from_secs(5),
            graceful_cancel: Duration::from_millis(100),
            max_stdout_bytes: 1024,
            max_stderr_bytes: 1024,
        };

        let outcome = spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap();
        assert!(
            matches!(outcome, SpawnOutcome::Unavailable { .. }),
            "unknown .cmd returned {outcome:?}; marker_exists={}",
            marker.exists()
        );
        assert!(!marker.exists(), "comment-referenced payload must not run");
    }

    #[cfg(windows)]
    fn assert_rejected_shim_framing_does_not_run(shim_bytes: &[u8]) {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("payload-ran.txt");
        std::fs::write(dir.path().join("provider.cmd"), shim_bytes).unwrap();
        std::fs::write(
            dir.path().join("payload.js"),
            "require('fs').writeFileSync(process.argv[2], 'executed');",
        )
        .unwrap();
        let child_path = format!(
            "{};{}",
            dir.path().display(),
            std::env::var("PATH").expect("test requires the installed node PATH")
        );
        let plan = SpawnPlan {
            command: "provider".to_string(),
            arguments: vec![marker.to_string_lossy().into_owned()],
            cwd: dir.path().to_path_buf(),
            environment: vec![
                ("PATH".to_string(), child_path),
                ("PATHEXT".to_string(), ".CMD;.EXE".to_string()),
                (
                    "SYSTEMROOT".to_string(),
                    std::env::var("SYSTEMROOT").expect("Windows requires SYSTEMROOT"),
                ),
            ],
            timeout: Duration::from_secs(5),
            graceful_cancel: Duration::from_millis(100),
            max_stdout_bytes: 1024,
            max_stderr_bytes: 1024,
        };

        let outcome = spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap();
        assert!(
            matches!(outcome, SpawnOutcome::Unavailable { .. }),
            "invalidly framed .cmd returned {outcome:?}; marker_exists={}",
            marker.exists()
        );
        assert!(!marker.exists(), "rejected shim payload must not run");
    }

    #[cfg(windows)]
    #[test]
    fn windows_lf_only_npm_shim_fails_closed_without_running_payload() {
        assert_rejected_shim_framing_does_not_run(b"@ECHO off\nnode \"%dp0%\\payload.js\" %*\n");
    }

    #[cfg(windows)]
    #[test]
    fn windows_npm_shim_without_terminal_crlf_fails_closed_without_running_payload() {
        assert_rejected_shim_framing_does_not_run(b"@ECHO off\r\nnode \"%dp0%\\payload.js\" %*");
    }

    #[cfg(windows)]
    #[test]
    fn windows_npm_cmd_shim_parser_accepts_only_complete_closed_forms() {
        const GENERATED_PREFIX: &str =
            "endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"";

        let direct = "@ECHO off\r\nnode \"%dp0%\\package\\main.js\" %*\r\n";
        assert_eq!(parse_npm_cmd_shim(direct, false), Some("package\\main.js"));
        let generated = format!(
            "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\n{GENERATED_PREFIX}  \"%dp0%\\node_modules\\@sourcegraph\\scip-typescript\\dist\\src\\main.js\" %*\r\n"
        );
        assert_eq!(
            parse_npm_cmd_shim(&generated, false),
            Some("node_modules\\@sourcegraph\\scip-typescript\\dist\\src\\main.js")
        );
        let local_generated = generated.replace(
            "%dp0%\\node_modules\\@sourcegraph",
            "%dp0%\\..\\@sourcegraph",
        );
        assert_eq!(parse_npm_cmd_shim(&local_generated, false), None);
        assert_eq!(
            parse_npm_cmd_shim(&local_generated, true),
            Some("..\\@sourcegraph\\scip-typescript\\dist\\src\\main.js")
        );
        let parent_escape = "@ECHO off\r\nnode \"%dp0%\\..\\..\\payload.js\" %*\r\n";
        assert_eq!(parse_npm_cmd_shim(parent_escape, true), None);

        for (description, malformed) in [
            ("LF-only direct template", direct.replace("\r\n", "\n")),
            (
                "direct template without terminal CRLF",
                direct.strip_suffix("\r\n").unwrap().to_string(),
            ),
            (
                "LF-only generated template",
                generated.replace("\r\n", "\n"),
            ),
            (
                "generated template without terminal CRLF",
                generated.strip_suffix("\r\n").unwrap().to_string(),
            ),
        ] {
            assert_eq!(
                parse_npm_cmd_shim(&malformed, false),
                None,
                "must reject {description}"
            );
        }

        for rejected in [
            " node \"%dp0%\\payload.js\" %*",
            "node\t\"%dp0%\\payload.js\" %*",
            "node  \"%dp0%\\payload.js\" %*",
            "node \"%dp0%\\payload.js\"  %*",
            "node \"%dp0%\\payload.js\" %* ",
            "",
            "REM node \"%dp0%\\payload.js\" %*",
            "@REM node \"%dp0%\\payload.js\" %*",
            ":: node \"%dp0%\\payload.js\" %*",
            "ECHO \"%dp0%\\payload.js\"",
            "@node \"%dp0%\\payload.js\" %*",
            "node.exe \"%dp0%\\payload.js\" %*",
            "\"%_prog%\" \"%dp0%\\payload.js\" %*",
            "node %dp0%\\payload.js %*",
            "node \"%dp0%\\payload.js\"",
            "node \"%dp0%\\payload.js\"%*",
            "node \"%dp0%\\payload.js\" \"%dp0%\\other.js\" %*",
            "node \"%dp0%\\payload.js\" %* & echo injected",
            "node \"%dp0%\\payload.js\" %* REM trailing",
            "node \"%dp0%\\..\\payload.js\" %*",
            "node \"%dp0%\\.. \\payload.js\" %*",
            "node \"%dp0%\\payload.JS\" %*",
            "node \"%dp0%\\payload!.js\" %*",
            "endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\" & node \"%dp0%\\payload.js\" %*",
        ] {
            let contents = format!("@ECHO off\r\n{rejected}\r\n");
            assert_eq!(
                parse_npm_cmd_shim(&contents, false),
                None,
                "must reject {rejected:?}"
            );
        }

        for contents in [
            "@ECHO off\r\nnode \"%dp0%\\payload.js\" %*\r\nECHO unrelated\r\n",
            "@ECHO off\r\nREM \"%dp0%\\other.js\"\r\nnode \"%dp0%\\payload.js\" %*\r\n",
            "@ECHO off\r\nnode \"%dp0%\\payload.js\" %*\r\nnode \"%dp0%\\other.js\" %*\r\n",
        ] {
            assert_eq!(
                parse_npm_cmd_shim(contents, false),
                None,
                "must reject an otherwise valid line in an unrecognized file"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_canonical_script_argument_rejects_dos_normalization_changes() {
        let drive = "C:";
        let normal = format!(r"{drive}\npm\package\main.js");
        let verbatim = format!(r"\\?\{normal}");
        assert_eq!(
            canonical_windows_command_argument(Path::new(&verbatim)),
            Some(normal.clone())
        );
        let rejected = [
            format!(r"\\?\{drive}\npm\.. \main.js"),
            format!(r"\\?\{drive}\npm\package.\main.js"),
            format!(r"\\?\{drive}\npm\AUX\main.js"),
            format!(r"\\?\{drive}\npm\COM1.js"),
            r"\\?\UNC\server\share\main.js".to_string(),
            normal,
        ];
        for rejected in &rejected {
            assert_eq!(
                canonical_windows_command_argument(Path::new(rejected)),
                None,
                "must not weaken verbatim identity for {rejected:?}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_recognized_npm_shim_resolves_each_supported_form() {
        const GENERATED_PREFIX: &str =
            "endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"";

        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("provider.cmd");
        let script = dir.path().join("package/main.js");
        let node = dir.path().join("node.EXE");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, b"console.log('provider')").unwrap();
        write_dummy_executable(&node);
        let environment = vec![
            (
                "PATH".to_string(),
                dir.path().to_string_lossy().into_owned(),
            ),
            ("PATHEXT".to_string(), ".EXE;.CMD".to_string()),
        ];

        for contents in [
            "@ECHO off\r\nnode \"%dp0%\\package\\main.js\" %*\r\n".to_string(),
            format!(
                "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\n{GENERATED_PREFIX}  \"%dp0%\\package\\main.js\" %*\r\n"
            ),
        ] {
            std::fs::write(&shim, contents).unwrap();
            let (effective, prefix) =
                resolve_execution_command("provider", dir.path(), &environment).unwrap();
            assert_eq!(PathBuf::from(effective), node);
            assert_eq!(
                std::fs::canonicalize(&prefix[0]).unwrap(),
                std::fs::canonicalize(&script).unwrap()
            );
        }

        std::fs::write(
            &shim,
            "node \"%dp0%\\package\\main.js\" %*\r\nnode \"%dp0%\\package\\main.js\" %*\r\n",
        )
        .unwrap();
        assert!(
            resolve_execution_command("provider", dir.path(), &environment).is_none(),
            "multiple supported invocations are ambiguous"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_absolute_npm_shim_preserves_argument_boundaries_without_a_shell() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("provider.CMD");
        let marker = dir.path().join("arguments.json");
        std::fs::write(&shim, "@ECHO off\r\nnode \"%dp0%\\payload.js\" %*\r\n").unwrap();
        std::fs::write(
            dir.path().join("payload.js"),
            "require('fs').writeFileSync(process.argv[2], JSON.stringify(process.argv.slice(3)));",
        )
        .unwrap();
        let child_path = std::env::var("PATH").expect("test requires the installed node PATH");
        let expected = vec![
            "first value".to_string(),
            "x&echo not-a-command".to_string(),
            "\"quoted\"".to_string(),
        ];
        let plan = SpawnPlan {
            command: shim.to_string_lossy().into_owned(),
            arguments: std::iter::once(marker.to_string_lossy().into_owned())
                .chain(expected.iter().cloned())
                .collect(),
            cwd: dir.path().to_path_buf(),
            environment: vec![
                ("PATH".to_string(), child_path),
                ("PATHEXT".to_string(), ".CMD;.EXE".to_string()),
                (
                    "SYSTEMROOT".to_string(),
                    std::env::var("SYSTEMROOT").expect("Windows requires SYSTEMROOT"),
                ),
            ],
            timeout: Duration::from_secs(5),
            graceful_cancel: Duration::from_millis(100),
            max_stdout_bytes: 1024,
            max_stderr_bytes: 1024,
        };

        let outcome = spawn_and_wait(&plan, &AtomicBool::new(false)).unwrap();
        assert!(matches!(
            outcome,
            SpawnOutcome::Exited {
                exit_code: Some(0),
                ..
            }
        ));
        let observed: Vec<String> =
            serde_json::from_slice(&std::fs::read(marker).unwrap()).unwrap();
        assert_eq!(observed, expected);
    }

    #[test]
    fn probe_provider_available_for_python_version_check() {
        let dir = tempfile::tempdir().unwrap();
        let env = build_environment(
            &[
                "PATH".to_string(),
                "SYSTEMROOT".to_string(),
                "PATHEXT".to_string(),
            ],
            false,
        );
        let result = probe_provider(
            "python-probe",
            "python",
            &["--version".to_string()],
            dir.path(),
            env,
            Duration::from_secs(10),
        );
        assert_eq!(
            result.status,
            crate::provider_contract::ProbeStatus::Available
        );
        assert!(result.executable_hash.is_some());
        assert_eq!(result.executable_hash.as_ref().unwrap().len(), 64);
        assert_eq!(
            result.network_isolation_state,
            crate::provider_contract::NetworkIsolationState::NotAvailable
        );
    }

    #[test]
    fn probe_provider_unavailable_for_missing_executable() {
        let dir = tempfile::tempdir().unwrap();
        let result = probe_provider(
            "ghost-provider",
            "this-executable-definitely-does-not-exist-98765",
            &["--help".to_string()],
            dir.path(),
            vec![],
            Duration::from_secs(5),
        );
        assert_eq!(
            result.status,
            crate::provider_contract::ProbeStatus::Unavailable
        );
        assert!(result.executable_hash.is_none());
    }
}
