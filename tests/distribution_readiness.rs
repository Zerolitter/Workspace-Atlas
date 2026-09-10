use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: usize = 1_048_576;
const CHECKER_TIMEOUT: Duration = Duration::from_secs(30);
const POST_ROOT_DRAIN: Duration = Duration::from_millis(500);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

struct Capture {
    bytes: Vec<u8>,
    exceeded: bool,
}

fn read_capped(mut stream: impl Read) -> io::Result<Capture> {
    let mut output = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Ok(Capture {
                bytes: output,
                exceeded: false,
            });
        }
        let remaining = OUTPUT_LIMIT.saturating_sub(output.len());
        output.extend_from_slice(&chunk[..count.min(remaining)]);
        if count > remaining {
            return Ok(Capture {
                bytes: output,
                exceeded: true,
            });
        }
    }
}

fn capture_thread(
    stream: impl Read + Send + 'static,
) -> (Receiver<io::Result<Capture>>, JoinHandle<()>) {
    let (sender, receiver) = mpsc::sync_channel(1);
    let handle = thread::spawn(move || {
        let _ = sender.send(read_capped(stream));
    });
    (receiver, handle)
}

#[cfg(windows)]
mod process_owner {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_NO_MORE_FILES, FILETIME, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        GetThreadTimes, OpenThread, ResumeThread, CREATE_SUSPENDED,
        THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
    };

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    pub struct ProcessOwner {
        handle: OwnedHandle,
    }

    impl ProcessOwner {
        fn new() -> io::Result<Self> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let handle = OwnedHandle(handle);
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    handle.0,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                ) == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let mut observed: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                if QueryInformationJobObject(
                    handle.0,
                    JobObjectExtendedLimitInformation,
                    &mut observed as *mut _ as *mut core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                ) == 0
                    || observed.BasicLimitInformation.LimitFlags
                        & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                        == 0
                {
                    return Err(io::Error::other(
                        "Windows Job Object kill-on-close was not observable",
                    ));
                }
                Ok(Self { handle })
            }
        }

        fn assign_and_resume(&self, child: &Child) -> io::Result<()> {
            unsafe {
                if AssignProcessToJobObject(self.handle.0, child.as_raw_handle() as HANDLE) == 0 {
                    return Err(io::Error::last_os_error());
                }
                resume_primary_thread(child.id())
            }
        }

        pub fn active_process_count(&self) -> io::Result<u32> {
            unsafe {
                let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
                if QueryInformationJobObject(
                    self.handle.0,
                    JobObjectBasicAccountingInformation,
                    &mut accounting as *mut _ as *mut core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    std::ptr::null_mut(),
                ) == 0
                {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(accounting.ActiveProcesses)
                }
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

        pub fn wait_empty(&self, timeout: Duration) -> io::Result<bool> {
            let deadline = Instant::now() + timeout;
            loop {
                if self.active_process_count()? == 0 {
                    return Ok(true);
                }
                if Instant::now() >= deadline {
                    return Ok(false);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for ProcessOwner {
        fn drop(&mut self) {
            if self.active_process_count().is_ok_and(|count| count != 0) {
                let _ = self.terminate();
                let _ = self.wait_empty(Duration::from_secs(2));
            }
        }
    }

    pub fn spawn(command: &mut Command) -> io::Result<(Child, ProcessOwner)> {
        command.creation_flags(CREATE_SUSPENDED);
        let owner = ProcessOwner::new()?;
        let mut child = command.spawn()?;
        if let Err(error) = owner.assign_and_resume(&child) {
            let _ = owner.terminate();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok((child, owner))
    }

    unsafe fn resume_primary_thread(process_id: u32) -> io::Result<()> {
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

#[cfg(unix)]
mod process_owner {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    pub struct ProcessOwner {
        pgid: i32,
    }

    impl ProcessOwner {
        pub fn active_process_count(&self) -> io::Result<u32> {
            unsafe {
                if libc::kill(-self.pgid, 0) == 0 {
                    return Ok(1);
                }
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::ESRCH) => Ok(0),
                Some(libc::EPERM) => Ok(1),
                _ => Err(error),
            }
        }

        pub fn terminate(&self) -> io::Result<()> {
            unsafe {
                if libc::kill(-self.pgid, libc::SIGKILL) == 0 {
                    return Ok(());
                }
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(())
            } else {
                Err(error)
            }
        }

        pub fn wait_empty(&self, timeout: Duration) -> io::Result<bool> {
            let deadline = Instant::now() + timeout;
            loop {
                if self.active_process_count()? == 0 {
                    return Ok(true);
                }
                if Instant::now() >= deadline {
                    return Ok(false);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for ProcessOwner {
        fn drop(&mut self) {
            if self.active_process_count().is_ok_and(|count| count != 0) {
                let _ = self.terminate();
                let _ = self.wait_empty(Duration::from_secs(2));
            }
        }
    }

    pub fn spawn(command: &mut Command) -> io::Result<(Child, ProcessOwner)> {
        command.process_group(0);
        let child = command.spawn()?;
        let owner = ProcessOwner {
            pgid: child.id() as i32,
        };
        Ok((child, owner))
    }
}

fn poll_capture(receiver: &Receiver<io::Result<Capture>>) -> Result<Option<Capture>, String> {
    match receiver.try_recv() {
        Ok(result) => result.map(Some).map_err(|error| error.to_string()),
        Err(TryRecvError::Empty) => Ok(None),
        Err(TryRecvError::Disconnected) => {
            Err("checker output reader terminated unexpectedly".into())
        }
    }
}

#[cfg(windows)]
fn cancel_reader(handle: &JoinHandle<()>) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CancelSynchronousIo(thread: *mut core::ffi::c_void) -> i32;
    }

    unsafe {
        if CancelSynchronousIo(handle.as_raw_handle()) == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

fn finish_capture(
    receiver: Receiver<io::Result<Capture>>,
    handle: JoinHandle<()>,
    prior: Option<Capture>,
    deadline: Instant,
) -> Result<Capture, String> {
    let result = if let Some(capture) = prior {
        Ok(capture)
    } else {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                handle
                    .join()
                    .map_err(|_| "checker output reader panicked".to_string())?;
                return Err("checker output reader terminated unexpectedly".into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                #[cfg(windows)]
                cancel_reader(&handle)
                    .map_err(|error| format!("failed to cancel bounded output reader: {error}"))?;
                #[cfg(not(windows))]
                return Err("checker output reader exceeded cleanup deadline".into());
                #[cfg(windows)]
                receiver
                    .recv_timeout(Duration::from_millis(500))
                    .map_err(|_| {
                        "checker output reader did not stop after cancellation".to_string()
                    })?
            }
        }
    };
    handle
        .join()
        .map_err(|_| "checker output reader panicked".to_string())?;
    result.map_err(|error| error.to_string())
}

fn run_command(mut command: Command) -> Result<Output, String> {
    let (mut child, owner) = process_owner::spawn(&mut command)
        .map_err(|error| format!("failed to spawn contained checker: {error}"))?;
    let stdout = child.stdout.take().ok_or("checker stdout was not piped")?;
    let stderr = child.stderr.take().ok_or("checker stderr was not piped")?;
    let (stdout_receiver, stdout_handle) = capture_thread(stdout);
    let (stderr_receiver, stderr_handle) = capture_thread(stderr);
    let deadline = Instant::now() + CHECKER_TIMEOUT;
    let mut status: Option<ExitStatus> = None;
    let mut root_exited_at = None;
    let mut stdout_capture = None;
    let mut stderr_capture = None;
    let mut failure = None;

    loop {
        if stdout_capture.is_none() {
            match poll_capture(&stdout_receiver) {
                Ok(value) => stdout_capture = value,
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if stderr_capture.is_none() {
            match poll_capture(&stderr_receiver) {
                Ok(value) => stderr_capture = value,
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        if stdout_capture
            .as_ref()
            .is_some_and(|capture| capture.exceeded)
            || stderr_capture
                .as_ref()
                .is_some_and(|capture| capture.exceeded)
        {
            failure = Some("checker output exceeded 1048576 bytes".to_string());
            break;
        }
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(value)) => {
                    status = Some(value);
                    root_exited_at = Some(Instant::now());
                }
                Ok(None) => {}
                Err(error) => {
                    failure = Some(format!("failed to poll checker: {error}"));
                    break;
                }
            }
        }

        let active = match owner.active_process_count() {
            Ok(value) => value,
            Err(error) => {
                failure = Some(format!("failed to query process owner: {error}"));
                break;
            }
        };
        if status.is_some() && active == 0 && stdout_capture.is_some() && stderr_capture.is_some() {
            break;
        }
        if root_exited_at.is_some_and(|exited| exited.elapsed() >= POST_ROOT_DRAIN) {
            failure = Some(if active != 0 {
                format!("{active} checker descendant process(es) survived root exit")
            } else {
                "checker pipes remained open after the process owner became empty".to_string()
            });
            break;
        }
        if Instant::now() >= deadline {
            failure = Some("checker exceeded its 30 second test timeout".to_string());
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }

    if failure.is_some() {
        let _ = owner.terminate();
        let _ = child.kill();
    }
    let cleanup_deadline = Instant::now() + CLEANUP_TIMEOUT;
    while status.is_none() && Instant::now() < cleanup_deadline {
        match child.try_wait() {
            Ok(Some(value)) => status = Some(value),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                failure.get_or_insert_with(|| format!("failed to reap checker: {error}"));
                break;
            }
        }
    }
    if status.is_none() {
        failure.get_or_insert_with(|| "checker root did not terminate during cleanup".into());
    }
    match owner.wait_empty(CLEANUP_TIMEOUT) {
        Ok(true) => {}
        Ok(false) => {
            failure.get_or_insert_with(|| "checker process owner did not become empty".into());
        }
        Err(error) => {
            failure.get_or_insert_with(|| format!("failed to measure checker cleanup: {error}"));
        }
    }
    // Closing the independently retained owner is the final kill-on-close
    // boundary. Do it before awaiting readers so no inherited writer can keep
    // either pipe open after cleanup was measured.
    drop(owner);
    let capture_deadline = Instant::now() + CLEANUP_TIMEOUT;
    let stdout = finish_capture(
        stdout_receiver,
        stdout_handle,
        stdout_capture,
        capture_deadline,
    );
    let stderr = finish_capture(
        stderr_receiver,
        stderr_handle,
        stderr_capture,
        capture_deadline,
    );
    let stdout = match stdout {
        Ok(value) => value,
        Err(error) => {
            failure.get_or_insert(error);
            Capture {
                bytes: Vec::new(),
                exceeded: false,
            }
        }
    };
    let stderr = match stderr {
        Ok(value) => value,
        Err(error) => {
            failure.get_or_insert(error);
            Capture {
                bytes: Vec::new(),
                exceeded: false,
            }
        }
    };
    if stdout.exceeded || stderr.exceeded {
        failure.get_or_insert_with(|| "checker output exceeded 1048576 bytes".into());
    }
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(Output {
        status: status.ok_or("checker exited without an observable status")?,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

#[cfg(windows)]
fn python_executable() -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .filter(|directory| {
            !directory
                .to_string_lossy()
                .replace('/', "\\")
                .to_ascii_lowercase()
                .contains("\\microsoft\\windowsapps")
        })
        .map(|directory| directory.join("python.exe"))
        .find(|candidate| candidate.is_file())
        .expect("a concrete Python executable outside WindowsApps must be available")
}

#[cfg(unix)]
fn python_executable() -> PathBuf {
    PathBuf::from("python3")
}

fn git_output(root: &Path, arguments: &[&str]) -> Result<Output, String> {
    let mut command = Command::new("git");
    command
        .args(arguments)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = run_command(command)?;
    if !output.status.success() {
        return Err(format!(
            "git authority command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output)
}

fn preflight_checker_authority(root: &Path) -> Result<(), String> {
    // Bootstrap boundary: this compiled harness independently compares index and
    // worktree bytes, modes, and bounds. It cannot authenticate its own already
    // compiled instructions; fresh trusted CI invocation remains the trust root.
    let paths = [
        "scripts/check-distribution-readiness.py",
        "tests/distribution_readiness.rs",
    ];
    let output = git_output(
        root,
        &["ls-files", "--stage", "-z", "--", paths[0], paths[1]],
    )?;
    let mut entries = std::collections::BTreeMap::new();
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let text =
            std::str::from_utf8(raw).map_err(|error| format!("non-UTF-8 index entry: {error}"))?;
        let (identity, path) = text
            .split_once('\t')
            .ok_or_else(|| format!("malformed index entry: {text:?}"))?;
        let fields = identity.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3
            || fields[2] != "0"
            || !matches!(fields[0], "100644" | "100755")
            || !matches!(fields[1].len(), 40 | 64)
            || !fields[1]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || !paths.contains(&path)
            || entries.insert(path, (fields[0], fields[1])).is_some()
        {
            return Err(format!("unsupported checker authority entry: {text:?}"));
        }
    }
    if entries.len() != paths.len() {
        return Err("checker authority entries are incomplete".to_owned());
    }
    for path in paths {
        let (mode, oid) = entries[path];
        if mode != "100644" {
            return Err(format!(
                "checker authority mode is noncanonical: {path}={mode}"
            ));
        }
        let size = git_output(root, &["cat-file", "-s", oid])?;
        let size = std::str::from_utf8(&size.stdout)
            .map_err(|error| format!("non-UTF-8 blob size for {path}: {error}"))?
            .trim()
            .parse::<usize>()
            .map_err(|error| format!("invalid blob size for {path}: {error}"))?;
        if size > OUTPUT_LIMIT {
            return Err(format!("checker authority blob exceeds bound: {path}"));
        }
        let indexed = git_output(root, &["cat-file", "blob", oid])?.stdout;
        if indexed.len() != size {
            return Err(format!("checker authority blob length drifted: {path}"));
        }
        let candidate = root.join(path);
        let metadata = std::fs::symlink_metadata(&candidate)
            .map_err(|error| format!("checker authority path unavailable {path}: {error}"))?;
        if !metadata.file_type().is_file() || metadata.len() > OUTPUT_LIMIT as u64 {
            return Err(format!(
                "checker authority worktree identity unsupported: {path}"
            ));
        }
        let mut observed = Vec::new();
        std::fs::File::open(&candidate)
            .and_then(|file| {
                file.take(OUTPUT_LIMIT as u64 + 1)
                    .read_to_end(&mut observed)
            })
            .map_err(|error| format!("checker authority read failed {path}: {error}"))?;
        if observed != indexed {
            return Err(format!(
                "checker authority worktree differs from stage zero: {path}"
            ));
        }
    }
    Ok(())
}

fn invoke_checker(root: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(python_executable());
    command
        .arg(root.join("scripts/check-distribution-readiness.py"))
        .args(arguments)
        .current_dir(root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_command(command).unwrap_or_else(|error| panic!("distribution-readiness checker: {error}"))
}

fn checker(root: &Path, arguments: &[&str]) -> Output {
    preflight_checker_authority(root)
        .unwrap_or_else(|error| panic!("distribution-readiness authority preflight: {error}"));
    invoke_checker(root, arguments)
}

fn output_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

const T25_DOCUMENT_PATHS: [&str; 4] = [
    "specs/release-readiness/security.md",
    "specs/release-readiness/support.md",
    "specs/release-readiness/troubleshooting.md",
    "specs/release-readiness/capacity.md",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum T25SourceContext {
    RepositoryCheckout,
    UnpackedCargoPackage,
}

fn t25_source_context(root: &Path) -> T25SourceContext {
    let git_marker = root.join(".git");
    if git_marker.is_file() || git_marker.is_dir() {
        return T25SourceContext::RepositoryCheckout;
    }

    let vcs_marker = root.join(".cargo_vcs_info.json");
    let original_manifest = root.join("Cargo.toml.orig");
    let markers_are_bounded_files = [(&vcs_marker, 4 * 1024), (&original_manifest, 128 * 1024)]
        .into_iter()
        .all(|(path, maximum_bytes)| {
            std::fs::metadata(path)
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= maximum_bytes)
        });
    if !markers_are_bounded_files {
        return T25SourceContext::RepositoryCheckout;
    }

    let valid_vcs_marker = std::fs::read(&vcs_marker)
        .ok()
        .and_then(|contents| serde_json::from_slice::<serde_json::Value>(&contents).ok())
        .is_some_and(|marker| {
            marker
                .pointer("/git/sha1")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|sha1| {
                    sha1.len() == 40 && sha1.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                && marker
                    .get("path_in_vcs")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
        });
    let valid_original_manifest = std::fs::read_to_string(&original_manifest)
        .ok()
        .and_then(|contents| toml::from_str::<toml::Value>(&contents).ok())
        .is_some_and(|manifest| {
            let Some(package) = manifest.get("package") else {
                return false;
            };
            package.get("name").and_then(toml::Value::as_str) == Some("workspace_atlas")
                && package.get("version").and_then(toml::Value::as_str) == Some("2.0.0")
                && package.get("edition").and_then(toml::Value::as_str) == Some("2021")
        });

    if valid_vcs_marker && valid_original_manifest {
        T25SourceContext::UnpackedCargoPackage
    } else {
        T25SourceContext::RepositoryCheckout
    }
}

fn validate_t25_document_presence(root: &Path) -> Result<(), String> {
    match t25_source_context(root) {
        T25SourceContext::RepositoryCheckout => {
            for relative in T25_DOCUMENT_PATHS {
                if !root.join(relative).is_file() {
                    return Err(format!(
                        "repository checkout is missing required T25 draft: {relative}"
                    ));
                }
            }
        }
        T25SourceContext::UnpackedCargoPackage => {
            for relative in T25_DOCUMENT_PATHS {
                if root.join(relative).exists() {
                    return Err(format!(
                        "unpacked Cargo package contains excluded T25 draft: {relative}"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn read_t25_document(root: &Path, relative: &str) -> String {
    let path = root.join(relative);
    let metadata = std::fs::metadata(&path)
        .unwrap_or_else(|error| panic!("T25 document is unavailable at {relative}: {error}"));
    assert!(metadata.is_file(), "T25 path is not a file: {relative}");
    assert!(
        metadata.len() <= 131_072,
        "T25 document exceeds the 128 KiB review bound: {relative}"
    );
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("T25 document is not valid UTF-8 at {relative}: {error}"))
}

fn markdown_indented_content(line: &str) -> Option<&str> {
    let indentation = line.bytes().take_while(|byte| *byte == b' ').count();
    (indentation <= 3).then_some(&line[indentation..])
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    let heading = markdown_indented_content(line)?;
    let hashes = heading.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&hashes)
        || !heading
            .as_bytes()
            .get(hashes)
            .is_some_and(u8::is_ascii_whitespace)
    {
        return None;
    }

    let content = heading[hashes..].trim_end_matches(['\r', '\n']);
    let without_trailing_whitespace = content.trim_end_matches([' ', '\t']);
    let closing_start = without_trailing_whitespace.trim_end_matches('#').len();
    let text = if closing_start < without_trailing_whitespace.len()
        && closing_start > 0
        && matches!(
            without_trailing_whitespace.as_bytes()[closing_start - 1],
            b' ' | b'\t'
        ) {
        &without_trailing_whitespace[..closing_start]
    } else {
        content
    };
    Some((hashes, text.trim()))
}

fn markdown_headings(document: &str) -> Vec<(usize, usize, usize, &str)> {
    let mut headings = Vec::new();
    let mut fence = None;
    let mut offset = 0;

    for line in document.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let Some(content) = markdown_indented_content(line) else {
            continue;
        };
        let marker = content.as_bytes().first().copied();
        let marker_count = marker
            .map(|marker| content.bytes().take_while(|byte| *byte == marker).count())
            .unwrap_or_default();

        if let Some((fence_marker, minimum_count)) = fence {
            if marker == Some(fence_marker)
                && marker_count >= minimum_count
                && content[marker_count..].trim().is_empty()
            {
                fence = None;
            }
            continue;
        }

        if marker.is_some_and(|marker| marker == b'`' || marker == b'~')
            && marker_count >= 3
            && (marker != Some(b'`') || !content[marker_count..].contains('`'))
        {
            fence = marker.map(|marker| (marker, marker_count));
            continue;
        }

        if let Some((level, text)) = markdown_heading(line) {
            headings.push((line_start, offset, level, text));
        }
    }

    headings
}

fn markdown_heading_fragment(heading: &str) -> Option<String> {
    let mut fragment = String::new();
    let mut pending_separator = false;
    for character in heading.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '-' || character == '_' {
            if pending_separator && !fragment.is_empty() && !fragment.ends_with('-') {
                fragment.push('-');
            }
            fragment.push(character);
            pending_separator = false;
        } else if character.is_whitespace() {
            pending_separator = true;
        }
    }
    (!fragment.is_empty()).then_some(fragment)
}

fn markdown_heading_fragments(document: &str) -> std::collections::HashSet<String> {
    let mut occurrences = std::collections::HashMap::<String, usize>::new();
    let mut fragments = std::collections::HashSet::new();
    for (_, _, _, heading) in markdown_headings(document) {
        let Some(base) = markdown_heading_fragment(heading) else {
            continue;
        };
        let occurrence = occurrences.entry(base.clone()).or_default();
        let fragment = if *occurrence == 0 {
            base
        } else {
            format!("{base}-{occurrence}")
        };
        *occurrence += 1;
        fragments.insert(fragment);
    }
    fragments
}

fn markdown_section<'a>(document: &'a str, heading: &str) -> Result<&'a str, String> {
    let (expected_level, expected_text) =
        markdown_heading(heading).ok_or_else(|| format!("invalid Markdown heading: {heading}"))?;
    let headings = markdown_headings(document);
    let mut matches = headings
        .iter()
        .filter(|(_, _, level, text)| *level == expected_level && *text == expected_text);
    let Some(&(_, section_start, _, _)) = matches.next() else {
        return Err(format!("missing Markdown section: {heading}"));
    };
    if matches.next().is_some() {
        return Err(format!("duplicate Markdown section: {heading}"));
    }

    let section_end = headings
        .iter()
        .find(|(line_start, _, level, _)| *line_start >= section_start && *level <= expected_level)
        .map_or(document.len(), |(line_start, _, _, _)| *line_start);
    Ok(&document[section_start..section_end])
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn replace_normalized_in_markdown_section(
    document: &str,
    heading: &str,
    source: &str,
    replacement: &str,
) -> Result<String, String> {
    let section = markdown_section(document, heading)?;
    let section_tokens = section.split_whitespace().collect::<Vec<_>>();
    let source_tokens = source.split_whitespace().collect::<Vec<_>>();
    if source_tokens.is_empty() {
        return Err("normalized Markdown replacement source is empty".to_owned());
    }
    let Some(token_index) = section_tokens
        .windows(source_tokens.len())
        .position(|tokens| tokens == source_tokens)
    else {
        return Err(format!(
            "{heading} omits normalized Markdown replacement source"
        ));
    };

    let document_start = document.as_ptr() as usize;
    let replacement_start = section_tokens[token_index].as_ptr() as usize - document_start;
    let last_token = section_tokens[token_index + source_tokens.len() - 1];
    let replacement_end = last_token.as_ptr() as usize - document_start + last_token.len();
    let mut replaced = String::with_capacity(
        document.len() - (replacement_end - replacement_start) + replacement.len(),
    );
    replaced.push_str(&document[..replacement_start]);
    replaced.push_str(replacement);
    replaced.push_str(&document[replacement_end..]);
    Ok(replaced)
}

fn require_section_terms(section: &str, owner: &str, required: &[&str]) -> Result<(), String> {
    for term in required {
        if !section.contains(term) {
            return Err(format!("{owner} omits required closed term: {term}"));
        }
    }
    Ok(())
}

fn validate_h8_capability_document(
    document: &str,
    heading: &str,
    owner: &str,
    operative_statement: &str,
    stale_claim: &str,
) -> Result<(), String> {
    let section = markdown_section(document, heading)?;
    let normalized_section = normalize_whitespace(section);
    let normalized_statement = normalize_whitespace(operative_statement);
    if !normalized_section.contains(&normalized_statement) {
        return Err(format!(
            "{owner} omits the operative capability non-release statement"
        ));
    }

    let normalized_stale_claim = normalize_whitespace(stale_claim);
    if normalized_section.contains(&normalized_stale_claim) {
        return Err(format!(
            "{owner} authoritative availability section retains stale unavailable capability text"
        ));
    }
    Ok(())
}

fn validate_h8_implementation_tasks(document: &str) -> Result<(), String> {
    let tasks = [
        (
            "### T30A — Capacity fixture generator and immutable manifests",
            "- **Dependencies:** accepted fresh independent T29 review.",
            "- **Allowed files (4):** `tests/fixtures/pilot/generate_fixture.py`, `tests/fixtures/capacity/repo-small-v1.json` (new), `tests/fixtures/capacity/repo-medium-v1.json` (new), `tests/fixtures/capacity/repo-large-v1.json` (new).",
        ),
        (
            "### T30B — Capacity harness and manifest enforcement",
            "- **Dependencies:** accepted T30A review.",
            "- **Allowed files (3):** `bin/atlas-bench.rs`, `tests/benchmark_manifest.rs`, `tests/fixtures/context_yield/benchmark-scenario.json`.",
        ),
        (
            "### T30C — Cross-platform process-tree memory sampling",
            "- **Dependencies:** accepted T30B review.",
            "- **Allowed files (4):** `scripts/process-tree-memory.py` (new), `scripts/test_process_tree_memory.py` (new), `bin/atlas-bench.rs`, `tests/benchmark_manifest.rs`.",
        ),
        (
            "### T31 — Bounded private-CI capacity evidence logs",
            "- **Dependencies:** accepted T30C review.",
            "- **Allowed files (5):** `.github/workflows/quality.yml`, `.github/workflows/distribution-readiness.yml`, `bin/atlas-bench.rs`, `tests/benchmark_manifest.rs`, `scripts/check-distribution-readiness.py`.",
        ),
        (
            "### T32A — Response-lifetime progressive DEEP execution",
            "- **Dependencies:** accepted fresh independent T29 review; may proceed independently of T30A–T31.",
            "- **Allowed files (5):** `src/context_application.rs`, `src/context_route.rs`, `tests/context_route_decision.rs`, `tests/compiler_lifecycle.rs`, `tests/compiler_composition.rs`.",
        ),
        (
            "### T32B — CLI-only bounded source materialization",
            "- **Dependencies:** accepted T32A review.",
            "- **Allowed files (5):** `src/cli.rs`, `src/context_application.rs`, `src/task_compiler.rs`, `tests/compiler_surface_parity.rs`, `tests/exact_source_compiler.rs`.",
        ),
        (
            "### T32C — Transport discovery truth and MCP prohibition",
            "- **Dependencies:** accepted T32B review.",
            "- **Allowed files (4):** `src/context_route.rs`, `src/mcp_adapter.rs`, `tests/context_route_decision.rs`, `tests/mcp_compiler_parity.rs`.",
        ),
        (
            "### T33 — Fresh independent implementation reviews",
            "- **Dependencies:** accepted T31 and T32C implementation checkpoints.",
            "- **Allowed tracked files (0):** none. Review evidence is private and package-excluded.",
        ),
    ];

    for (heading, dependency, allowlist) in tasks {
        let section = markdown_section(document, heading)?;
        require_section_terms(section, heading, &[dependency, allowlist])?;
        let mut allowlist_lines = section.lines().filter(|line| {
            line.starts_with("- **Allowed files (")
                || line.starts_with("- **Allowed tracked files (")
        });
        let allowlist_line = allowlist_lines
            .next()
            .ok_or_else(|| format!("{heading} must contain one tracked-file allowlist"))?;
        if allowlist_lines.next().is_some() {
            return Err(format!(
                "{heading} must contain exactly one tracked-file allowlist"
            ));
        }
        let count = allowlist_line
            .split_once('(')
            .and_then(|(_, suffix)| suffix.split_once(')'))
            .and_then(|(count, _)| count.parse::<usize>().ok())
            .ok_or_else(|| format!("{heading} has an invalid tracked-file allowlist count"))?;
        if count > 5 {
            return Err(format!(
                "{heading} exceeds the five-tracked-file repair boundary: {count}"
            ));
        }
    }
    Ok(())
}

fn validate_h8_fixture_manifest(document: &str) -> Result<(), String> {
    let h8_section = markdown_section(document, "## H8-E0 approved private candidate hypotheses")?;
    let fixture_section = markdown_section(h8_section, "### Immutable private fixture strata")?;
    require_section_terms(
        fixture_section,
        "H8-E0 immutable fixture strata",
        &[
            "`repo-small-v1`",
            "`repo-medium-v1`",
            "`repo-large-v1`",
            "| `repo-small-v1` | 128 | 2 |",
            "| `repo-medium-v1` | 1,024 | 11 |",
            "| `repo-large-v1` | 8,192 | 82 |",
            "`schema_version`",
            "`generator_version`",
            "`seed`",
            "a full sorted `relative_path` plus `sha256` list",
            "`manifest_sha256`",
            "eligible, indexed, excluded, initial, changed-file, changed-byte, and total-byte counts",
            "language and artifact-class distribution",
            "excluded generated/vendor/dist/archive/secret/binary cases",
            "symbol, relationship, effect, diagnostic, coverage, and conflict counts",
            "project markers",
            "exact provider descriptors, versions, and required/optional state",
            "target identity",
            "expected accepted-outcome hash",
            "correctness/deterministic-identity expectations",
            "expected ready-versus-Truth result hash",
            "A digest or count mismatch fails before execution",
        ],
    )?;
    require_section_terms(
        h8_section,
        "H8-E0 provider evidence contract",
        &[
            "built-in deterministic provider",
            "scip-typescript` `0.4.0`",
            "rust-analyzer` `1.94.1`",
            "optional/degraded",
            "timeout, unsupported semantic output, or platform absence is retained as explicit non-success evidence",
            "actual `runner.os`, `runner.arch`, image, CPU, and resolved Rust patch version",
        ],
    )
}

fn validate_local_markdown_links(
    root: &Path,
    relative: &str,
    document: &str,
) -> Result<(), String> {
    let parent = root
        .join(relative)
        .parent()
        .ok_or_else(|| format!("T25 document has no parent: {relative}"))?
        .to_owned();

    for suffix in document.split("](").skip(1) {
        let target = suffix
            .split(')')
            .next()
            .ok_or_else(|| format!("unterminated Markdown link in {relative}"))?;
        if target.contains("://") {
            continue;
        }
        let (local, fragment) = target
            .split_once('#')
            .map_or((target, None), |(local, fragment)| (local, Some(fragment)));
        let target_path = if local.is_empty() {
            root.join(relative)
        } else {
            parent.join(local)
        };
        if !local.is_empty() && !target_path.is_file() {
            return Err(format!(
                "T25 document {relative} has a broken local link: {target}"
            ));
        }

        let Some(fragment) = fragment else {
            continue;
        };
        let target_document = if local.is_empty() {
            std::borrow::Cow::Borrowed(document)
        } else {
            std::borrow::Cow::Owned(std::fs::read_to_string(&target_path).map_err(|error| {
                format!("T25 document {relative} cannot read local link target {target}: {error}")
            })?)
        };
        if !markdown_heading_fragments(target_document.as_ref()).contains(fragment) {
            return Err(format!(
                "T25 document {relative} has a broken local fragment: {target}"
            ));
        }
    }
    Ok(())
}

#[test]
fn t25_source_context_requires_checkout_drafts_and_package_exclusion() {
    let cargo_vcs_info =
        r#"{"git":{"sha1":"08af51a481184208b785fe829e4c678b0a4f0d78"},"path_in_vcs":""}"#;
    let original_manifest = r#"[package]
name = "workspace_atlas"
version = "2.0.0"
edition = "2021"
"#;

    for git_marker_is_directory in [false, true] {
        let checkout = tempfile::tempdir().expect("create checkout fixture");
        std::fs::write(checkout.path().join(".cargo_vcs_info.json"), cargo_vcs_info)
            .expect("write checkout VCS marker");
        std::fs::write(checkout.path().join("Cargo.toml.orig"), original_manifest)
            .expect("write checkout original manifest marker");
        if git_marker_is_directory {
            std::fs::create_dir(checkout.path().join(".git"))
                .expect("create checkout Git directory");
        } else {
            std::fs::write(
                checkout.path().join(".git"),
                "gitdir: ../worktrees/checkout\n",
            )
            .expect("write checkout Git file");
        }

        assert_eq!(
            t25_source_context(checkout.path()),
            T25SourceContext::RepositoryCheckout
        );
        let error = validate_t25_document_presence(checkout.path()).unwrap_err();
        assert!(
            error.contains("repository checkout is missing required T25 draft"),
            "{error}"
        );
    }

    for (cargo_vcs_info, original_manifest) in [
        ("{}", original_manifest),
        (cargo_vcs_info, "[package]"),
        (
            r#"{"git":{"sha1":"not-a-commit"},"path_in_vcs":""}"#,
            original_manifest,
        ),
    ] {
        let source_tree = tempfile::tempdir().expect("create source-tree fixture");
        std::fs::write(
            source_tree.path().join(".cargo_vcs_info.json"),
            cargo_vcs_info,
        )
        .expect("write malformed VCS marker");
        std::fs::write(
            source_tree.path().join("Cargo.toml.orig"),
            original_manifest,
        )
        .expect("write malformed original manifest marker");

        assert_eq!(
            t25_source_context(source_tree.path()),
            T25SourceContext::RepositoryCheckout
        );
        let error = validate_t25_document_presence(source_tree.path()).unwrap_err();
        assert!(
            error.contains("repository checkout is missing required T25 draft"),
            "{error}"
        );
    }

    let package = tempfile::tempdir().expect("create package fixture");
    std::fs::write(package.path().join(".cargo_vcs_info.json"), cargo_vcs_info)
        .expect("write package VCS marker");
    std::fs::write(package.path().join("Cargo.toml.orig"), original_manifest)
        .expect("write original manifest marker");
    assert_eq!(
        t25_source_context(package.path()),
        T25SourceContext::UnpackedCargoPackage
    );
    validate_t25_document_presence(package.path())
        .expect("package-excluded drafts may be absent from an unpacked package");

    let leaked = package.path().join("specs/release-readiness/security.md");
    std::fs::create_dir_all(leaked.parent().expect("draft parent"))
        .expect("create leaked draft parent");
    std::fs::write(leaked, "# Security").expect("write leaked draft");
    let error = validate_t25_document_presence(package.path()).unwrap_err();
    assert!(
        error.contains("unpacked Cargo package contains excluded T25 draft"),
        "{error}"
    );
}

#[test]
fn markdown_heading_normalizes_only_legitimate_closing_sequences() {
    assert_eq!(
        markdown_heading("### Rendered heading ### \t\r\n"),
        Some((3, "Rendered heading"))
    );
    assert_eq!(
        markdown_heading("### Literal heading#"),
        Some((3, "Literal heading#"))
    );
    assert_eq!(
        markdown_heading("### Literal heading ### suffix"),
        Some((3, "Literal heading ### suffix"))
    );
}

#[test]
fn t25_markdown_links_reject_broken_file_and_fragment_only_anchors() {
    let fixture = tempfile::tempdir().expect("create Markdown fixture");
    let source = "docs/source.md";
    let target = fixture.path().join("docs/target.md");
    std::fs::create_dir_all(target.parent().expect("target parent"))
        .expect("create Markdown fixture directory");
    std::fs::write(&target, "# TypeScript/JavaScript pilot provider\n")
        .expect("write target Markdown");

    let valid = "# H3 — Benchmark/profile recommendation\n\
        [same document](#h3-benchmarkprofile-recommendation)\n\
        [other document](target.md#typescriptjavascript-pilot-provider)\n";
    validate_local_markdown_links(fixture.path(), source, valid)
        .expect("repository heading convention must resolve both link contexts");

    let error = validate_local_markdown_links(fixture.path(), source, "[broken](#missing-heading)")
        .unwrap_err();
    assert!(error.contains("broken local fragment"), "{error}");

    let error = validate_local_markdown_links(
        fixture.path(),
        source,
        "[broken](target.md#missing-heading)",
    )
    .unwrap_err();
    assert!(error.contains("broken local fragment"), "{error}");
}

#[test]
fn t25_readiness_guidance_is_complete_linked_and_package_excluded() {
    let root = root();
    let source_context = t25_source_context(&root);
    validate_t25_document_presence(&root).unwrap_or_else(|error| panic!("{error}"));
    let requirements = [
        (
            "specs/release-readiness/security.md",
            ["threat model", "vulnerability", "untrusted", "no contact"],
        ),
        (
            "specs/release-readiness/support.md",
            ["support", "contribution", "diagnostic", "support channel"],
        ),
        (
            "specs/release-readiness/troubleshooting.md",
            [
                "direct",
                "atlas_light",
                "atlas_deep",
                "governor capabilities",
            ],
        ),
        (
            "specs/release-readiness/capacity.md",
            ["methodology", "windows", "not executed", "public slo"],
        ),
    ];

    if source_context == T25SourceContext::RepositoryCheckout {
        for (relative, required) in requirements {
            let document = read_t25_document(&root, relative);
            let lowered = document.to_ascii_lowercase();
            for token in required {
                assert!(
                    lowered.contains(token),
                    "T25 document {relative} omits required guidance: {token}"
                );
            }
            validate_local_markdown_links(&root, relative, &document)
                .unwrap_or_else(|error| panic!("{error}"));
        }
    }

    let mut command = Command::new("cargo");
    command
        .args(["package", "--list", "--locked", "--allow-dirty"])
        .current_dir(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = run_command(command).expect("cargo package --list must complete");
    assert!(output.status.success(), "{}", output_text(&output));
    let paths = String::from_utf8(output.stdout).expect("Cargo package list must be UTF-8");
    let paths = paths.lines().collect::<Vec<_>>();
    assert_eq!(paths.len(), 120, "Cargo package membership drifted");
    assert!(
        paths
            .iter()
            .all(|path| !path.starts_with("specs/release-readiness/")),
        "repository-only T25 drafts entered the Cargo package: {paths:?}"
    );
}

#[test]
fn h8_e0_pre_authorization_is_complete_and_fail_closed() {
    let root = root();
    let contracts = [
        (
            "specs/roadmap-v15-v20/decision-checkpoint.md",
            [
                "## H8-E0 — Pre-H8 evidence-closure pre-authorization",
                "**Accepted — 2026-09-05**",
                "Final H8 remains unsatisfied",
                "bundle A–C only",
                "`durable_contract_unavailable`",
                "separate H7 gate",
                "Bundle D remains withheld",
                "response-lifetime progressive execution",
                "ATLAS_DEEP Context IR `2.0.0`",
                "CLI-only opt-in source materialization",
                "MCP materialization remains prohibited",
                "DIRECT and target-bounded LIGHT remain valid",
                "DEEP is never mandatory for every request",
                "no merge, tag, release, public artifact, publication, visibility change, protected-branch action",
                "distribution channel",
                "support/security contact identity",
                "SBOM/signing/provenance/checksum service",
                "runtime learning",
                "EXP-DPPM implementation",
                "Ponytail adoption",
            ]
            .as_slice(),
        ),
        (
            "specs/release-readiness/capacity.md",
            [
                "## H8-E0 approved private candidate hypotheses",
                "runner-default Windows, Ubuntu, and macOS",
                "Rust `1.94` and `stable`",
                "Inspector `2.5.0` and Codex CLI `0.151.0`",
                "actual `runner.os`, `runner.arch`, image, CPU",
                "dedicated/pinned Windows x64 and Linux x64",
                "macOS runner-default latency is advisory",
                "built-in deterministic provider and TypeScript provider `scip-typescript` `0.4.0` are required",
                "Rust provider `rust-analyzer` `1.94.1` is optional/degraded",
                "128 | 2",
                "1,024 | 11",
                "8,192 | 82",
                "75% TypeScript source/test, 15% Rust source/test, 5% documentation, and 5% configuration",
                "expected ready-versus-Truth result hash",
                "50 ms",
                "aggregate concurrent peak",
                "bounded redacted private Actions log",
                "no new upload-artifact dependency",
                "separate private artifact-upload decision",
                "not supported limits or public SLOs",
            ]
            .as_slice(),
        ),
        (
            "specs/roadmap-v15-v20/implementation-plan.md",
            [
                "### T30A — Capacity fixture generator and immutable manifests",
                "### T30B — Capacity harness and manifest enforcement",
                "### T30C — Cross-platform process-tree memory sampling",
                "### T31 — Bounded private-CI capacity evidence logs",
                "### T32A — Response-lifetime progressive DEEP execution",
                "### T32B — CLI-only bounded source materialization",
                "### T32C — Transport discovery truth and MCP prohibition",
                "### T33 — Fresh independent implementation reviews",
                "### T34 — Post-v2.0 private PR and hosted evidence qualification",
                "### T35 — Post-v2.0 capacity qualification",
                "### T36 — Post-v2.0 H8 qualification review",
                "No new upload-artifact dependency",
                "Bundle D remains withheld",
            ]
            .as_slice(),
        ),
        (
            "README.md",
            [
                "`progressive_execution`,",
                "`deep_context_ir`, and `materialization` available",
                "bounded, explicit, CLI-only response-lifetime capability",
                "MCP materialization remains disabled",
                "not durable V2 lifecycle storage",
                "`durable_contract_unavailable`",
            ]
            .as_slice(),
        ),
        (
            "OPERATIONS.md",
            [
                "progressive execution,",
                "DEEP Context IR, and bounded materialization available",
                "source materialization is CLI-only and response-lifetime",
                "MCP materialization remains disabled",
                "not durable V2 lifecycle storage",
                "`durable_contract_unavailable`",
            ]
            .as_slice(),
        ),
        (
            "docs/adr/023-context-compiler-contract.md",
            [
                "progressive execution, DEEP Context IR,",
                "bounded materialization available",
                "response-lifetime capability",
                "MCP materialization remains disabled",
                "durable V2 lifecycle storage",
                "`durable_contract_unavailable`",
            ]
            .as_slice(),
        ),
    ];

    for (relative, required) in contracts {
        let document = read_t25_document(&root, relative);
        for term in required {
            assert!(
                document.contains(term),
                "H8-E0 contract {relative} omits required closed term: {term}"
            );
        }
    }

    let implementation_plan =
        read_t25_document(&root, "specs/roadmap-v15-v20/implementation-plan.md");
    validate_h8_implementation_tasks(&implementation_plan)
        .expect("T30A-T36 dependencies and allowlists must remain bound to their owning sections");
    let capacity = read_t25_document(&root, "specs/release-readiness/capacity.md");
    validate_h8_fixture_manifest(&capacity)
        .expect("H8-E0 fixture manifest fields must remain complete and heading-bounded");
}

#[test]
fn h8_capability_document_mutations_fail_closed() {
    let contracts = [
        (
            "README.md",
            include_str!("../README.md"),
            "### From a task to evidence",
            "MCP materialization remains disabled. Availability describes runtime features, \
             not durable V2 lifecycle storage: bounded legacy reads reject V2 lookup as \
             `durable_contract_unavailable`.",
            "discovery reports `route_decision` and `light_payload` available, and reports \
             `progressive_execution`, `deep_context_ir`, and `materialization` unavailable.",
        ),
        (
            "OPERATIONS.md",
            include_str!("../OPERATIONS.md"),
            "## Context Governor and lifecycle commands",
            "Current discovery reports route decision, LIGHT payload, progressive execution, \
             DEEP Context IR, and bounded materialization available. Availability describes \
             runtime features, not durable V2 lifecycle storage or a runtime default.",
            "Current discovery reports route decision and LIGHT payload available, while \
             progressive execution, DEEP Context IR, and materialization are unavailable.",
        ),
        (
            "docs/adr/023-context-compiler-contract.md",
            include_str!("../docs/adr/023-context-compiler-contract.md"),
            "## Decision",
            "Capability discovery is authoritative for runtime availability. It reports route \
             decision, LIGHT payload, progressive execution, DEEP Context IR, and bounded \
             materialization available. Materialization is an explicit, CLI-only, \
             response-lifetime capability; MCP materialization remains disabled. Availability \
             does not imply durable V2 lifecycle storage or a runtime route default.",
            "reports route decision and LIGHT payload available, and progressive execution, \
             DEEP Context IR, and materialization unavailable.",
        ),
    ];

    for (relative, document, heading, operative_statement, stale_claim) in contracts {
        let rewrapped_statement = normalize_whitespace(operative_statement).replace(' ', "\n");
        let rewrapped_document = replace_normalized_in_markdown_section(
            document,
            heading,
            operative_statement,
            &rewrapped_statement,
        )
        .unwrap_or_else(|error| panic!("{relative} rewrap precondition failed: {error}"));
        validate_h8_capability_document(
            &rewrapped_document,
            heading,
            relative,
            operative_statement,
            stale_claim,
        )
        .unwrap_or_else(|error| panic!("{relative} harmless rewrap failed: {error}"));

        let stale_contradiction =
            document.replacen(heading, &format!("{heading}\n\n{stale_claim}"), 1);
        assert_ne!(
            stale_contradiction, document,
            "{relative} mutation precondition must find the authoritative availability section"
        );
        assert!(
            validate_h8_capability_document(
                &stale_contradiction,
                heading,
                relative,
                operative_statement,
                stale_claim,
            )
            .is_err(),
            "{relative} must reject a stale unavailable claim in its authoritative section"
        );
    }
}

#[test]
fn h8_e0_structural_contract_mutations_fail_closed() {
    let implementation_plan = include_str!("../specs/roadmap-v15-v20/implementation-plan.md");
    let swapped_dependencies = implementation_plan
        .replacen(
            "- **Dependencies:** accepted T30A review.",
            "__T30_DEPENDENCY_SWAP__",
            1,
        )
        .replacen(
            "- **Dependencies:** accepted T30B review.",
            "- **Dependencies:** accepted T30A review.",
            1,
        )
        .replacen(
            "__T30_DEPENDENCY_SWAP__",
            "- **Dependencies:** accepted T30B review.",
            1,
        );
    assert!(
        validate_h8_implementation_tasks(&swapped_dependencies).is_err(),
        "swapping T30B and T30C dependencies must fail closed"
    );

    let corrupted_t30a = implementation_plan.replacen(
        "- **Dependencies:** accepted fresh independent T29 review.",
        "- **Dependencies:** corrupted.",
        1,
    );
    let t30a_contract = "\
### T30A — Capacity fixture generator and immutable manifests
- **Dependencies:** accepted fresh independent T29 review.
- **Allowed files (4):** `tests/fixtures/pilot/generate_fixture.py`, `tests/fixtures/capacity/repo-small-v1.json` (new), `tests/fixtures/capacity/repo-medium-v1.json` (new), `tests/fixtures/capacity/repo-large-v1.json` (new).";
    for fenced_decoy in [
        format!("   ````markdown\n{t30a_contract}\n```\n   ````\n\n{corrupted_t30a}"),
        format!("~~~markdown\n{t30a_contract}\n~~~\n\n{corrupted_t30a}"),
    ] {
        assert!(
            validate_h8_implementation_tasks(&fenced_decoy).is_err(),
            "a complete fenced T30A decoy must not conceal a corrupted owning section"
        );
    }

    let duplicate_decoy = format!("{t30a_contract}\n\n{corrupted_t30a}");
    assert!(
        validate_h8_implementation_tasks(&duplicate_decoy).is_err(),
        "duplicate T30A headings must fail closed rather than select a decoy"
    );

    let closing_sequence_owner = corrupted_t30a.replacen(
        "### T30A — Capacity fixture generator and immutable manifests",
        "### T30A — Capacity fixture generator and immutable manifests ###",
        1,
    );
    let closing_sequence_decoy = format!("{t30a_contract}\n\n{closing_sequence_owner}");
    assert!(
        validate_h8_implementation_tasks(&closing_sequence_decoy).is_err(),
        "a complete exact T30A decoy must not conceal a corrupted owning section with closing hashes"
    );

    let capacity = include_str!("../specs/release-readiness/capacity.md");
    for field in [
        "`schema_version`;",
        "`manifest_sha256`;",
        "exact provider descriptors, versions, and required/optional state;",
        "target identity;",
    ] {
        let missing_manifest_field = capacity.replacen(field, "", 1);
        assert!(
            validate_h8_fixture_manifest(&missing_manifest_field).is_err(),
            "removing required fixture manifest field {field} must fail closed"
        );
    }
}

#[test]
fn no_publish_is_the_only_executable_mode() {
    let output = checker(&root(), &["--static-only"]);
    assert!(!output.status.success(), "{}", output_text(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--no-publish"),
        "{}",
        output_text(&output)
    );
}

#[test]
fn accepted_checkout_contracts_pass_the_portable_static_gate() {
    let output = checker(&root(), &["--no-publish", "--static-only"]);
    assert!(output.status.success(), "{}", output_text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    for check_id in [
        "DR-ALLOWLIST",
        "DR-COMPATIBILITY",
        "DR-CAPACITY",
        "DR-LIFECYCLE",
        "DR-PACKAGE",
        "DR-USABILITY",
        "DR-NO-PUBLISH",
    ] {
        assert!(stdout.contains(check_id), "missing {check_id}: {stdout}");
    }
}

#[test]
fn representative_contract_mutations_fail_closed() {
    let output = checker(&root(), &["--no-publish", "--self-test"]);
    assert!(output.status.success(), "{}", output_text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    for mutation in [
        "allowlist-extra-binary",
        "compatibility-route-version",
        "compatibility-context-ir-authority",
        "compatibility-planner-authority",
        "workflow-all-jobs-disabled",
        "workflow-required-step-disabled",
        "compatibility-projection-duplicate",
        "compatibility-estimator-duplicate",
        "compatibility-metric-duplicate",
        "compatibility-builtin-provider-duplicate",
        "compatibility-external-provider-duplicate",
        "compatibility-external-provider-fallback",
        "compatibility-external-provider-early-return",
        "compatibility-external-provider-name-literal-spacing",
        "compatibility-external-provider-version-literal-spacing",
        "compatibility-external-provider-method-duplicate",
        "compatibility-external-provider-method-decoy",
        "capacity-public-slo",
        "lifecycle-suite-removed",
        "package-private-path",
        "package-same-count-substitution",
        "package-symlink-substitution",
        "package-ancestor-link-substitution",
        "package-archive-content-substitution",
        "package-archive-dirty-attributes-filter",
        "package-archive-unauthorized-crlf",
        "package-cargo-lock-mixed-newlines",
        "package-cargo-lock-bare-cr",
        "package-cargo-lock-binary",
        "package-cargo-lock-content-substitution",
        "package-text-auto-working-tree-crlf",
        "package-archive-arbitrary-prefix",
        "package-archive-backslash-member",
        "package-stale-noop-cargo",
        "package-concurrent-replacement",
        "source-staged-worktree-divergence",
        "source-dirty-worktree-divergence",
        "usability-capability-step-removed",
        "workflow-timeout-removed",
        "workflow-extra-trigger",
        "workflow-write-permission",
        "workflow-shell-override",
        "workflow-null-shell",
        "workflow-comment-only-matrix",
        "workflow-nonexistent-needs",
        "workflow-duplicate-key",
        "workflow-anchor",
        "workflow-alias",
        "workflow-unknown-field",
        "workflow-yaml-null",
        "workflow-uses-run-conflict",
        "package-archive-noncanonical-mode",
        "package-archive-noncanonical-executable-mode",
        "package-archive-compressed-expansion-bomb",
        "package-archive-noncanonical-magic",
        "package-archive-noncanonical-version",
        "package-archive-nonzero-padding",
        "package-archive-trailing-raw-tar-zero-block",
        "package-archive-concatenated-gzip",
        "package-archive-trailing-zero",
        "package-archive-gzip-checksum",
        "package-archive-pax-size-expansion",
        "package-archive-pax-path-override",
        "package-archive-pax-global",
        "package-archive-gnu-longname",
        "package-archive-gnu-longlink",
        "package-archive-gnu-sparse",
        "package-archive-pax-sparse",
        "attributes-bare-cr",
        "attributes-mixed-newlines",
    ] {
        assert!(
            stdout.contains(&format!("MUTATION {mutation} rejected")),
            "missing mutation evidence for {mutation}: {stdout}"
        );
    }
    assert!(
        stdout.contains("CONTROL compatibility-external-provider-formatting accepted"),
        "missing formatting control evidence: {stdout}"
    );
    for control in [
        "workflow-enabled-required-steps",
        "package-archive-authorized-text-checkout",
        "source-authorized-text-checkout",
        "package-cargo-lock-authorized-checkout",
        "package-concurrent-in-place-mutation-contained",
        "package-archive-canonical-mode",
        "package-archive-canonical-executable-mode",
        "package-archive-bounded-members",
        "package-archive-canonical-cargo",
        "attributes-exact-lf",
    ] {
        assert!(
            stdout.contains(&format!("CONTROL {control} accepted")),
            "missing control evidence for {control}: {stdout}"
        );
    }
    assert!(stdout.contains("SELF-TEST passed: 81/81 mutations rejected"));
}

#[cfg(windows)]
fn process_is_running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let running = WaitForSingleObject(process, 0) == 0x0000_0102;
        CloseHandle(process);
        running
    }
}

#[cfg(unix)]
fn process_is_running(pid: u32) -> bool {
    unsafe {
        if libc::kill(pid as i32, 0) == 0 {
            true
        } else {
            io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        }
    }
}

#[test]
fn root_exit_with_pipe_holding_descendant_is_bounded_and_contained() {
    for attempt in 0..5 {
        let fixture = tempfile::tempdir().expect("temporary adversarial checker root");
        let scripts = fixture.path().join("scripts");
        std::fs::create_dir(&scripts).expect("create fixture scripts directory");
        let pid_file = fixture.path().join("descendant.pid");
        std::fs::write(
            scripts.join("check-distribution-readiness.py"),
            r#"import pathlib
import subprocess
import sys
child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
pathlib.Path(sys.argv[1]).write_text(str(child.pid), encoding="utf-8")
"#,
        )
        .expect("write adversarial checker");

        let started = Instant::now();
        let result = std::panic::catch_unwind(|| {
            invoke_checker(
                fixture.path(),
                &[pid_file.to_str().expect("UTF-8 temporary path")],
            )
        });
        assert!(
            result.is_err(),
            "a surviving descendant must reject the launch"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "descendant rejection exceeded its bounded drain"
        );

        let pid: u32 = std::fs::read_to_string(&pid_file)
            .expect("descendant PID evidence")
            .parse()
            .expect("numeric descendant PID");
        let cleanup_deadline = Instant::now() + Duration::from_secs(2);
        while process_is_running(pid) && Instant::now() < cleanup_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !process_is_running(pid),
            "descendant {pid} escaped containment on attempt {attempt}"
        );
    }
}
