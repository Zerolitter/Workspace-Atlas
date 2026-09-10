use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::ids::workspace_id;
use workspace_atlas::workspace::register_workspace;

fn atlas(app_data: &Path) -> Command {
    let mut command = Command::cargo_bin("atlas").expect("atlas binary");
    #[cfg(target_os = "windows")]
    command.env("LOCALAPPDATA", app_data);
    #[cfg(target_os = "macos")]
    command.env("HOME", app_data);
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    command.env("XDG_DATA_HOME", app_data);
    command
}
fn atlas_process(app_data: &Path) -> ProcessCommand {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_atlas"));
    #[cfg(target_os = "windows")]
    command.env("LOCALAPPDATA", app_data);
    #[cfg(target_os = "macos")]
    command.env("HOME", app_data);
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    command.env("XDG_DATA_HOME", app_data);
    command
}

fn success_json(command: &mut Command) -> Value {
    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("command emits JSON")
}

fn failure_stderr(command: &mut Command) -> String {
    let output = command.assert().failure().get_output().stderr.clone();
    String::from_utf8(output).expect("error output is UTF-8")
}

fn catalogue_dir(app_data: &Path) -> PathBuf {
    if cfg!(target_os = "windows") {
        app_data.join("WorkspaceAtlas").join("catalogues")
    } else if cfg!(target_os = "macos") {
        app_data
            .join("Library/Application Support")
            .join("WorkspaceAtlas")
            .join("catalogues")
    } else {
        app_data.join("workspace-atlas").join("catalogues")
    }
}

fn config(display_name: &str) -> Config {
    Config::parse(&format!(
        "schema_version = \"1.0.0\"\n[workspace]\ndisplay_name = {display_name:?}\n"
    ))
    .expect("test config")
}

fn init_default(app_data: &Path, root: &Path, display_name: &str) -> Value {
    let mut command = atlas(app_data);
    command
        .arg("init")
        .arg(root)
        .arg("--display-name")
        .arg(display_name);
    success_json(&mut command)
}

#[derive(Debug, PartialEq, Eq)]
struct FileSnapshot {
    identity: String,
    len: u64,
    modified: SystemTime,
    hash: String,
}

fn file_identity(_path: &Path, _metadata: &fs::Metadata) -> String {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;

        #[repr(C)]
        struct FileTime {
            low: u32,
            high: u32,
        }
        #[repr(C)]
        struct ByHandleFileInformation {
            attributes: u32,
            creation_time: FileTime,
            last_access_time: FileTime,
            last_write_time: FileTime,
            volume_serial_number: u32,
            file_size_high: u32,
            file_size_low: u32,
            number_of_links: u32,
            file_index_high: u32,
            file_index_low: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(
                file: *mut std::ffi::c_void,
                information: *mut ByHandleFileInformation,
            ) -> i32;
        }

        let file = fs::File::open(_path).unwrap();
        let mut information = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
        // SAFETY: the handle remains valid for the call and the output points
        // to writable storage of the exact Windows structure layout.
        let succeeded = unsafe {
            GetFileInformationByHandle(file.as_raw_handle().cast(), information.as_mut_ptr())
        };
        assert_ne!(
            succeeded,
            0,
            "cannot obtain file identity for {}",
            _path.display()
        );
        // SAFETY: a successful GetFileInformationByHandle initializes every field.
        let information = unsafe { information.assume_init() };
        format!(
            "{}:{}",
            information.volume_serial_number,
            (u64::from(information.file_index_high) << 32) | u64::from(information.file_index_low)
        )
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", _metadata.dev(), _metadata.ino())
    }
}

fn catalogue_snapshot(directory: &Path) -> BTreeMap<String, FileSnapshot> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(
                metadata.file_type().is_file(),
                "{name} is not a regular file"
            );
            let mut reader = BufReader::new(File::open(&path).unwrap());
            let mut hasher = blake3::Hasher::new();
            std::io::copy(&mut reader, &mut hasher).unwrap();
            (
                name,
                FileSnapshot {
                    identity: file_identity(&path, &metadata),
                    len: metadata.len(),
                    modified: metadata.modified().unwrap(),
                    hash: hasher.finalize().to_hex().to_string(),
                },
            )
        })
        .collect()
}

fn assert_failed_scan_preserved_catalogue(
    child: Child,
    catalogue_root: &Path,
    expected: &BTreeMap<String, FileSnapshot>,
) -> String {
    let output = child.wait_with_output().unwrap();
    assert!(
        !output.status.success(),
        "scan unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        &catalogue_snapshot(catalogue_root),
        expected,
        "failed scan changed main/WAL/SHM/journal inventory or contents"
    );
    String::from_utf8(output.stderr).unwrap()
}

fn spawn_paused_scan(
    app_data: &Path,
    root: &Path,
    stage: &str,
    rendezvous: &Path,
) -> (Child, PathBuf, PathBuf) {
    let ready = rendezvous.join("ready");
    let proceed = rendezvous.join("proceed");
    let mut command = atlas_process(app_data);
    command
        .arg("status")
        .arg(root)
        .env("ATLAS_TEST_LEGACY_SCAN_PAUSE_STAGE", stage)
        .env("ATLAS_TEST_LEGACY_SCAN_READY", &ready)
        .env("ATLAS_TEST_LEGACY_SCAN_CONTINUE", &proceed)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "scanner did not reach {stage}");
        thread::sleep(Duration::from_millis(10));
    }
    (child, ready, proceed)
}

fn create_legacy_catalogue(path: &Path, root: &Path, display_name: &str, wal: bool) {
    let legacy_config = config(display_name);
    let connection = init_catalogue(path, &legacy_config).unwrap();
    register_workspace(&connection, root, &legacy_config, path, "1.0.0").unwrap();
    if wal {
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
    }
    drop(connection);
}

const LEGACY_DISCOVERY_MAX_BYTES: u64 = 268_435_456;
const SQLITE_PAGE_BYTES: u64 = 4096;

#[cfg(windows)]
fn mark_sparse(file: &File) {
    use std::os::windows::io::AsRawHandle;

    const FSCTL_SET_SPARSE: u32 = 0x0009_00c4;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn DeviceIoControl(
            device: *mut std::ffi::c_void,
            control_code: u32,
            input: *mut std::ffi::c_void,
            input_len: u32,
            output: *mut std::ffi::c_void,
            output_len: u32,
            bytes_returned: *mut u32,
            overlapped: *mut std::ffi::c_void,
        ) -> i32;
    }

    let mut bytes_returned = 0_u32;
    // SAFETY: the file handle is live, the operation has no input/output
    // buffers, and `bytes_returned` remains writable for the call.
    let succeeded = unsafe {
        DeviceIoControl(
            file.as_raw_handle().cast(),
            FSCTL_SET_SPARSE,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            &mut bytes_returned,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(
        succeeded,
        0,
        "cannot mark {} sparse: {}",
        file.metadata().unwrap().len(),
        std::io::Error::last_os_error()
    );
}

#[cfg(unix)]
fn mark_sparse(_file: &File) {}

#[cfg(windows)]
fn allocated_file_bytes(path: &Path) -> u64 {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCompressedFileSizeW(file_name: *const u16, high: *mut u32) -> u32;
    }

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut high = 0_u32;
    // SAFETY: `wide` is a live NUL-terminated path and `high` is writable.
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
    (u64::from(high) << 32) | u64::from(low)
}

#[cfg(unix)]
fn allocated_file_bytes(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;

    path.metadata().unwrap().blocks() * 512
}

fn extend_page_aligned(path: &Path, len: u64) {
    assert_eq!(len % SQLITE_PAGE_BYTES, 0);
    let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
    mark_sparse(&file);
    file.set_len(len).unwrap();
    file.flush().unwrap();
    file.sync_all().unwrap();
    assert!(
        allocated_file_bytes(path) < len,
        "test candidate must be sparse rather than physically allocating {len} bytes"
    );
}

#[test]
fn default_catalogues_are_workspace_specific_and_all_commands_reuse_the_route() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let alpha = workspaces.path().join("alpha workspace");
    let beta = workspaces.path().join("beta workspace");
    fs::create_dir_all(&alpha).unwrap();
    fs::create_dir_all(&beta).unwrap();
    fs::write(alpha.join("alpha.ts"), "export const alpha = 1;\n").unwrap();
    fs::write(beta.join("beta.ts"), "export const beta = 2;\n").unwrap();

    let alpha_init = init_default(app_data.path(), &alpha, "Alpha Display");
    let beta_init = init_default(app_data.path(), &beta, "Beta Display");
    assert_ne!(alpha_init["workspace_id"], beta_init["workspace_id"]);
    assert_ne!(alpha_init["catalogue_path"], beta_init["catalogue_path"]);

    let alpha_catalogue = PathBuf::from(alpha_init["catalogue_path"].as_str().unwrap());
    let beta_catalogue = PathBuf::from(beta_init["catalogue_path"].as_str().unwrap());
    assert_eq!(
        alpha_catalogue.file_name(),
        Some(OsStr::new(&format!(
            "{}.sqlite",
            alpha_init["workspace_id"].as_str().unwrap()
        )))
    );
    assert_eq!(
        beta_catalogue.file_name(),
        Some(OsStr::new(&format!(
            "{}.sqlite",
            beta_init["workspace_id"].as_str().unwrap()
        )))
    );
    #[cfg(target_os = "windows")]
    {
        let expected = app_data.path().join("WorkspaceAtlas").join("catalogues");
        assert!(alpha_catalogue.starts_with(&expected));
        assert!(beta_catalogue.starts_with(&expected));
    }

    let mut reconcile = atlas(app_data.path());
    reconcile.arg("reconcile").arg(&alpha);
    let reconcile = success_json(&mut reconcile);
    assert_eq!(reconcile["workspace_id"], alpha_init["workspace_id"]);

    let mut status = atlas(app_data.path());
    status.arg("status").arg(&alpha);
    let status = success_json(&mut status);
    assert_eq!(status["workspace_id"], alpha_init["workspace_id"]);
    assert_eq!(
        status["active_generation_id"],
        reconcile["candidate_generation_id"]
    );

    let mut find = atlas(app_data.path());
    find.arg("find").arg(&alpha).arg("alpha");
    let find = success_json(&mut find);
    assert_eq!(find["generation_id"], reconcile["candidate_generation_id"]);

    let mut doctor = atlas(app_data.path());
    doctor.arg("doctor").arg(&alpha);
    let doctor = success_json(&mut doctor);
    assert_eq!(doctor["workspace_id"], alpha_init["workspace_id"]);
    assert_eq!(doctor["catalogue_path"], alpha_init["catalogue_path"]);
    assert_eq!(doctor["ok"], true);

    let mut beta_status = atlas(app_data.path());
    beta_status.arg("status").arg(&beta);
    let beta_status = success_json(&mut beta_status);
    assert_eq!(beta_status["workspace_id"], beta_init["workspace_id"]);
}

#[test]
fn init_without_display_name_uses_workspace_directory_name() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let root = workspaces.path().join("named workspace");
    fs::create_dir_all(&root).unwrap();

    let mut init = atlas(app_data.path());
    init.arg("init").arg(&root);
    let init = success_json(&mut init);
    let canonical_root = fs::canonicalize(&root).unwrap();
    let expected_id = workspace_id(&canonical_root.to_string_lossy(), "named workspace");
    assert_eq!(init["display_name"], "named workspace");
    assert_eq!(init["workspace_id"], expected_id);
}

#[test]
fn explicit_catalogue_override_is_authoritative() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let default_root = workspaces.path().join("default");
    let override_root = workspaces.path().join("override");
    fs::create_dir_all(&default_root).unwrap();
    fs::create_dir_all(&override_root).unwrap();

    let default_init = init_default(app_data.path(), &default_root, "Default");
    let override_path = workspaces.path().join("explicit catalogue.sqlite");
    let mut init = atlas(app_data.path());
    init.arg("init")
        .arg(&override_root)
        .arg("--display-name")
        .arg("Override")
        .arg("--catalogue")
        .arg(&override_path);
    let override_init = success_json(&mut init);

    let mut status = atlas(app_data.path());
    status
        .arg("status")
        .arg(&override_root)
        .arg("--catalogue")
        .arg(&override_path);
    let status = success_json(&mut status);
    assert_eq!(status["workspace_id"], override_init["workspace_id"]);

    let mut without_override = atlas(app_data.path());
    without_override.arg("status").arg(&override_root);
    let error = failure_stderr(&mut without_override);
    assert!(error.contains("no default catalogue registered"), "{error}");

    let mut wrong_override = atlas(app_data.path());
    wrong_override
        .arg("status")
        .arg(&default_root)
        .arg("--catalogue")
        .arg(&override_path);
    let error = failure_stderr(&mut wrong_override);
    assert!(error.contains("workspace not found"), "{error}");
    assert_ne!(default_init["workspace_id"], override_init["workspace_id"]);

    let missing_override = workspaces.path().join("missing override.sqlite");
    let mut missing_override_status = atlas(app_data.path());
    missing_override_status
        .arg("status")
        .arg(&default_root)
        .arg("--catalogue")
        .arg(&missing_override);
    let error = failure_stderr(&mut missing_override_status);
    assert!(error.contains("does not exist or is not a file"), "{error}");
    assert!(!missing_override.exists());
}

#[test]
fn corrupt_escaping_or_mismatched_locators_fail_closed() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let alpha = workspaces.path().join("alpha");
    let beta = workspaces.path().join("beta");
    fs::create_dir_all(&alpha).unwrap();
    fs::create_dir_all(&beta).unwrap();
    let alpha_init = init_default(app_data.path(), &alpha, "Alpha");
    let beta_init = init_default(app_data.path(), &beta, "Beta");

    let alpha_root = fs::canonicalize(&alpha)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let locator_path = fs::read_dir(catalogue_dir(app_data.path()).join("locators"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            value["canonical_root"] == alpha_root
        })
        .expect("alpha locator");
    let original = fs::read(&locator_path).unwrap();

    fs::write(&locator_path, b"not json").unwrap();
    let mut corrupt_status = atlas(app_data.path());
    corrupt_status.arg("status").arg(&alpha);
    let error = failure_stderr(&mut corrupt_status);
    assert!(error.contains("is corrupt"), "{error}");

    let mut escaping: Value = serde_json::from_slice(&original).unwrap();
    let outside = workspaces.path().join("outside.sqlite");
    fs::write(&outside, b"not a catalogue").unwrap();
    escaping["catalogue_path"] = Value::String(outside.to_string_lossy().into_owned());
    fs::write(&locator_path, serde_json::to_vec_pretty(&escaping).unwrap()).unwrap();
    let mut escaping_status = atlas(app_data.path());
    escaping_status.arg("status").arg(&alpha);
    let error = failure_stderr(&mut escaping_status);
    assert!(error.contains("escapes or mismatches"), "{error}");

    let mut mismatched: Value = serde_json::from_slice(&original).unwrap();
    mismatched["workspace_id"] = beta_init["workspace_id"].clone();
    mismatched["catalogue_path"] = beta_init["catalogue_path"].clone();
    fs::write(
        &locator_path,
        serde_json::to_vec_pretty(&mismatched).unwrap(),
    )
    .unwrap();
    let mut mismatched_status = atlas(app_data.path());
    mismatched_status.arg("status").arg(&alpha);
    let error = failure_stderr(&mut mismatched_status);
    assert!(error.contains("without that workspace"), "{error}");
    assert_ne!(alpha_init["workspace_id"], beta_init["workspace_id"]);
}

#[test]
fn missing_or_unregistered_roots_fail_without_creating_a_catalogue() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let registered = workspaces.path().join("registered");
    let unregistered = workspaces.path().join("unregistered");
    let missing = workspaces.path().join("missing");
    fs::create_dir_all(&registered).unwrap();
    fs::create_dir_all(&unregistered).unwrap();
    init_default(app_data.path(), &registered, "Registered");

    let catalogue_root = catalogue_dir(app_data.path());
    let before: Vec<_> = fs::read_dir(&catalogue_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| {
            let name = name.to_string_lossy();
            !name.ends_with(".sqlite-wal") && !name.ends_with(".sqlite-shm")
        })
        .collect();

    let mut missing_status = atlas(app_data.path());
    missing_status.arg("status").arg(&missing);
    let error = failure_stderr(&mut missing_status);
    assert!(error.contains("workspace root"), "{error}");
    assert!(error.contains("does not exist"), "{error}");
    assert!(!missing.exists());

    let mut unregistered_status = atlas(app_data.path());
    unregistered_status.arg("status").arg(&unregistered);
    let error = failure_stderr(&mut unregistered_status);
    assert!(error.contains("no default catalogue registered"), "{error}");

    let after: Vec<_> = fs::read_dir(&catalogue_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| {
            let name = name.to_string_lossy();
            !name.ends_with(".sqlite-wal") && !name.ends_with(".sqlite-shm")
        })
        .collect();
    assert_eq!(before, after);
}

#[test]
fn one_legacy_catalogue_is_migrated_but_ambiguous_matches_fail_closed() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let unique_root = workspaces.path().join("legacy unique");
    let ambiguous_root = workspaces.path().join("legacy ambiguous");
    fs::create_dir_all(&unique_root).unwrap();
    fs::create_dir_all(&ambiguous_root).unwrap();
    let unique_root = fs::canonicalize(unique_root).unwrap();
    let ambiguous_root = fs::canonicalize(ambiguous_root).unwrap();
    let catalogue_root = catalogue_dir(app_data.path());
    fs::create_dir_all(&catalogue_root).unwrap();

    let unique_config = config("Legacy Unique");
    let unique_id = workspace_id(
        &unique_root.to_string_lossy(),
        &unique_config.workspace.display_name,
    );
    let unique_catalogue = catalogue_root.join(format!("{unique_id}.sqlite"));
    let connection = init_catalogue(&unique_catalogue, &unique_config).unwrap();
    register_workspace(
        &connection,
        &unique_root,
        &unique_config,
        &unique_catalogue,
        "1.0.0",
    )
    .unwrap();
    drop(connection);

    let mut unique_status = atlas(app_data.path());
    unique_status.arg("status").arg(&unique_root);
    let unique_status = success_json(&mut unique_status);
    assert_eq!(unique_status["workspace_id"], unique_id);
    assert_eq!(
        fs::read_dir(catalogue_root.join("locators"))
            .unwrap()
            .count(),
        1
    );

    for display_name in ["Legacy A", "Legacy B"] {
        let config = config(display_name);
        let id = workspace_id(&ambiguous_root.to_string_lossy(), display_name);
        let path = catalogue_root.join(format!("{id}.sqlite"));
        let connection = init_catalogue(&path, &config).unwrap();
        register_workspace(&connection, &ambiguous_root, &config, &path, "1.0.0").unwrap();
    }

    let mut ambiguous_status = atlas(app_data.path());
    ambiguous_status.arg("status").arg(&ambiguous_root);
    let error = failure_stderr(&mut ambiguous_status);
    assert!(error.contains("multiple legacy catalogues"), "{error}");
    assert_eq!(
        fs::read_dir(catalogue_root.join("locators"))
            .unwrap()
            .count(),
        1,
        "ambiguous migration must not create a locator"
    );
}

#[test]
fn legacy_relative_root_is_rejected_without_writing_a_locator() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let root_a = workspaces.path().join("root-a");
    let root_b = workspaces.path().join("root-b");
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();
    let catalogue_root = catalogue_dir(app_data.path());
    fs::create_dir_all(&catalogue_root).unwrap();

    let legacy_config = config("Legacy Relative");
    let legacy_id = workspace_id(".", &legacy_config.workspace.display_name);
    let legacy_catalogue = catalogue_root.join(format!("{legacy_id}.sqlite"));
    let connection = init_catalogue(&legacy_catalogue, &legacy_config).unwrap();
    register_workspace(
        &connection,
        Path::new("."),
        &legacy_config,
        &legacy_catalogue,
        "1.0.0",
    )
    .unwrap();
    drop(connection);

    for unrelated_root in [&root_a, &root_b] {
        let mut status = atlas(app_data.path());
        status
            .current_dir(unrelated_root)
            .arg("status")
            .arg(unrelated_root);
        let error = failure_stderr(&mut status);
        assert!(error.contains("stored workspace root"), "{error}");
        assert!(error.contains("atlas init"), "{error}");
        assert!(error.contains("--catalogue"), "{error}");
    }
    assert!(
        !catalogue_root.join("locators").exists(),
        "rejected legacy roots must not create a locator"
    );
}

#[test]
fn legacy_scan_of_quiescent_wal_catalogues_creates_no_sidecars_and_fails_closed() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let requested_root = workspaces.path().join("requested");
    fs::create_dir_all(&requested_root).unwrap();
    let catalogue_root = catalogue_dir(app_data.path());
    fs::create_dir_all(&catalogue_root).unwrap();

    for (file_name, stored_root, display_name) in [
        (
            "ws_01_unrelated.sqlite",
            workspaces.path().join("unrelated-a"),
            "Unrelated A",
        ),
        (
            "ws_02_unrelated.sqlite",
            workspaces.path().join("unrelated-b"),
            "Unrelated B",
        ),
        (
            "ws_03_relative.sqlite",
            PathBuf::from("."),
            "Legacy Relative",
        ),
    ] {
        if stored_root.is_absolute() {
            fs::create_dir_all(&stored_root).unwrap();
        }
        let catalogue = catalogue_root.join(file_name);
        let connection = init_catalogue(&catalogue, &config(display_name)).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        register_workspace(
            &connection,
            &stored_root,
            &config(display_name),
            &catalogue,
            "1.0.0",
        )
        .unwrap();
        connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        drop(connection);
    }

    let before = fs::read_dir(&catalogue_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert!(
        before
            .iter()
            .all(|name| !name.to_string_lossy().contains(".sqlite-")),
        "test setup must be quiescent: {before:#?}"
    );

    let mut status = atlas(app_data.path());
    status.arg("status").arg(&requested_root);
    let error = failure_stderr(&mut status);
    assert!(error.contains("stored workspace root"), "{error}");
    assert!(error.contains("atlas init"), "{error}");
    assert!(error.contains("--catalogue"), "{error}");

    let after = fs::read_dir(&catalogue_root)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(before, after, "legacy scan must not create SQLite sidecars");
    assert!(
        !catalogue_root.join("locators").exists(),
        "rejected legacy roots must not create a locator"
    );
}

#[test]
fn malformed_legacy_headers_fail_before_sqlite_without_filesystem_changes() {
    for (label, bytes, expected) in [
        ("invalid-magic-20", vec![b'x'; 20], "header magic"),
        ("invalid-magic-100", vec![b'x'; 100], "header magic"),
        (
            "mismatched-write-one-read-two",
            {
                let mut header = vec![0_u8; 100];
                header[..16].copy_from_slice(b"SQLite format 3\0");
                header[16..18].copy_from_slice(&4096_u16.to_be_bytes());
                header[18..24].copy_from_slice(&[1, 2, 0, 64, 32, 32]);
                header
            },
            "format versions",
        ),
        (
            "mismatched-write-two-read-one",
            {
                let mut header = vec![0_u8; 100];
                header[..16].copy_from_slice(b"SQLite format 3\0");
                header[16..18].copy_from_slice(&4096_u16.to_be_bytes());
                header[18..24].copy_from_slice(&[2, 1, 0, 64, 32, 32]);
                header
            },
            "format versions",
        ),
        (
            "unsupported-read-version",
            {
                let mut header = vec![0_u8; 100];
                header[..16].copy_from_slice(b"SQLite format 3\0");
                header[16..18].copy_from_slice(&4096_u16.to_be_bytes());
                header[18..24].copy_from_slice(&[3, 3, 0, 64, 32, 32]);
                header
            },
            "format versions",
        ),
    ] {
        let app_data = TempDir::new().unwrap();
        let workspaces = TempDir::new().unwrap();
        let root = workspaces.path().join(label);
        fs::create_dir_all(&root).unwrap();
        let catalogue_root = catalogue_dir(app_data.path());
        fs::create_dir_all(&catalogue_root).unwrap();
        fs::write(catalogue_root.join(format!("ws_{label}.sqlite")), bytes).unwrap();
        let before = catalogue_snapshot(&catalogue_root);

        let mut command = atlas_process(app_data.path());
        command
            .arg("status")
            .arg(&root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let error = assert_failed_scan_preserved_catalogue(
            command.spawn().unwrap(),
            &catalogue_root,
            &before,
        );
        assert!(error.contains("cannot inspect legacy catalogue"), "{error}");
        assert!(error.contains(expected), "{error}");
        assert!(error.contains("atlas init"), "{error}");
        assert!(error.contains("--catalogue"), "{error}");
    }
}

#[test]
fn legacy_discovery_rejects_oversized_candidate_before_sqlite_but_explicit_path_works() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let root = fs::canonicalize(workspaces.path()).unwrap();
    let catalogue_root = catalogue_dir(app_data.path());
    fs::create_dir_all(&catalogue_root).unwrap();
    let display_name = "Oversized";
    let expected_workspace_id = workspace_id(
        root.to_str().expect("temporary path is UTF-8"),
        display_name,
    );
    let candidate = catalogue_root.join(format!("{expected_workspace_id}.sqlite"));
    create_legacy_catalogue(&candidate, &root, display_name, false);
    extend_page_aligned(&candidate, LEGACY_DISCOVERY_MAX_BYTES + SQLITE_PAGE_BYTES);
    let before = catalogue_snapshot(&catalogue_root);

    let mut discovery = atlas_process(app_data.path());
    discovery
        .arg("status")
        .arg(&root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let error = assert_failed_scan_preserved_catalogue(
        discovery.spawn().unwrap(),
        &catalogue_root,
        &before,
    );
    assert!(error.contains("cannot inspect legacy catalogue"), "{error}");
    assert!(error.contains("268435456"), "{error}");
    assert!(error.contains("atlas init"), "{error}");
    assert!(error.contains("--catalogue"), "{error}");
    assert!(
        !catalogue_root.join("locators").exists(),
        "oversized discovery must not create routing state"
    );

    let mut explicit = atlas(app_data.path());
    explicit
        .arg("status")
        .arg(&root)
        .arg("--catalogue")
        .arg(&candidate);
    let status = success_json(&mut explicit);
    assert_eq!(status["workspace_id"], expected_workspace_id);
    assert_eq!(catalogue_snapshot(&catalogue_root), before);
}

#[test]
fn under_ceiling_legacy_candidate_follows_normal_discovery_validation() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let root = fs::canonicalize(workspaces.path()).unwrap();
    let catalogue_root = catalogue_dir(app_data.path());
    fs::create_dir_all(&catalogue_root).unwrap();
    let display_name = "Under ceiling";
    let expected_workspace_id = workspace_id(
        root.to_str().expect("temporary path is UTF-8"),
        display_name,
    );
    let candidate = catalogue_root.join(format!("{expected_workspace_id}.sqlite"));
    create_legacy_catalogue(&candidate, &root, display_name, false);
    assert!(candidate.metadata().unwrap().len() < LEGACY_DISCOVERY_MAX_BYTES);

    let mut status = atlas(app_data.path());
    status.arg("status").arg(&root);
    let status = success_json(&mut status);
    assert_eq!(status["workspace_id"], expected_workspace_id);
    assert!(catalogue_root.join("locators").is_dir());
}

#[test]
fn rollback_header_replaced_by_wal_database_fails_without_scanner_effects() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let root = fs::canonicalize(workspaces.path()).unwrap();
    let catalogue_root = catalogue_dir(app_data.path());
    fs::create_dir_all(&catalogue_root).unwrap();
    let candidate = catalogue_root.join("ws_replaced.sqlite");
    create_legacy_catalogue(&candidate, &root, "Original rollback", false);
    let replacement = workspaces.path().join("replacement.sqlite");
    create_legacy_catalogue(&replacement, &root, "Replacement WAL", true);
    let rendezvous = workspaces.path().join("rendezvous");
    fs::create_dir_all(&rendezvous).unwrap();

    let (child, _, proceed) =
        spawn_paused_scan(app_data.path(), &root, "after_main_open", &rendezvous);
    fs::rename(&candidate, rendezvous.join("original.sqlite")).unwrap();
    fs::rename(&replacement, &candidate).unwrap();
    let replaced = catalogue_snapshot(&catalogue_root);
    fs::write(proceed, b"go").unwrap();

    let error = assert_failed_scan_preserved_catalogue(child, &catalogue_root, &replaced);
    assert!(error.contains("changed during inspection"), "{error}");
    assert!(error.contains("atlas init"), "{error}");
    assert!(error.contains("--catalogue"), "{error}");
}

#[test]
fn wal_appearance_and_replacement_between_checks_fail_without_scanner_effects() {
    for replace_existing in [false, true] {
        let app_data = TempDir::new().unwrap();
        let workspaces = TempDir::new().unwrap();
        let root = fs::canonicalize(workspaces.path()).unwrap();
        let catalogue_root = catalogue_dir(app_data.path());
        fs::create_dir_all(&catalogue_root).unwrap();
        let candidate = catalogue_root.join("ws_wal_race.sqlite");
        create_legacy_catalogue(&candidate, &root, "WAL race", true);
        let wal = PathBuf::from(format!("{}-wal", candidate.display()));
        if replace_existing {
            fs::write(&wal, []).unwrap();
        }
        let rendezvous = workspaces.path().join("rendezvous");
        fs::create_dir_all(&rendezvous).unwrap();
        let (child, _, proceed) =
            spawn_paused_scan(app_data.path(), &root, "after_quiescence", &rendezvous);

        if replace_existing {
            fs::rename(&wal, rendezvous.join("original-wal")).unwrap();
        }
        fs::write(&wal, []).unwrap();
        let raced = catalogue_snapshot(&catalogue_root);
        fs::write(proceed, b"go").unwrap();

        let error = assert_failed_scan_preserved_catalogue(child, &catalogue_root, &raced);
        assert!(
            error.contains("sidecar state changed during inspection"),
            "{error}"
        );
        assert!(error.contains("atlas init"), "{error}");
        assert!(error.contains("--catalogue"), "{error}");
    }
}

#[test]
fn main_replacement_after_quiescence_check_fails_without_scanner_effects() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let root = fs::canonicalize(workspaces.path()).unwrap();
    let catalogue_root = catalogue_dir(app_data.path());
    fs::create_dir_all(&catalogue_root).unwrap();
    let candidate = catalogue_root.join("ws_main_race.sqlite");
    create_legacy_catalogue(&candidate, &root, "Original", false);
    let replacement = workspaces.path().join("replacement.sqlite");
    create_legacy_catalogue(&replacement, &root, "Replacement", false);
    let rendezvous = workspaces.path().join("rendezvous");
    fs::create_dir_all(&rendezvous).unwrap();
    let (child, _, proceed) =
        spawn_paused_scan(app_data.path(), &root, "after_quiescence", &rendezvous);

    fs::rename(&candidate, rendezvous.join("original.sqlite")).unwrap();
    fs::rename(&replacement, &candidate).unwrap();
    let replaced = catalogue_snapshot(&catalogue_root);
    fs::write(proceed, b"go").unwrap();

    let error = assert_failed_scan_preserved_catalogue(child, &catalogue_root, &replaced);
    assert!(error.contains("changed during inspection"), "{error}");
    assert!(error.contains("atlas init"), "{error}");
    assert!(error.contains("--catalogue"), "{error}");
}

#[cfg(unix)]
#[test]
fn init_rejects_non_unicode_canonical_workspace_root() {
    use std::os::unix::ffi::OsStringExt;

    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let root = workspaces
        .path()
        .join(std::ffi::OsString::from_vec(b"root-\xff".to_vec()));
    fs::create_dir_all(&root).unwrap();

    let mut init = atlas(app_data.path());
    init.arg("init").arg(&root).arg("--display-name").arg("Bad");
    let error = failure_stderr(&mut init);
    assert!(error.contains("valid UTF-8"), "{error}");
    assert!(error.contains("ADR-015"), "{error}");
    assert!(
        !catalogue_dir(app_data.path()).exists(),
        "rejected roots must not create routing state"
    );
}

#[test]
fn legacy_discovery_fails_closed_on_uncheckpointed_wal_without_sidecar_changes() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let root = workspaces.path().join("live wal");
    fs::create_dir_all(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let catalogue_root = catalogue_dir(app_data.path());
    fs::create_dir_all(&catalogue_root).unwrap();

    let wal_config = config("Live WAL");
    let wal_id = workspace_id(
        root.to_str().expect("temporary path is UTF-8"),
        &wal_config.workspace.display_name,
    );
    let wal_catalogue = catalogue_root.join(format!("{wal_id}.sqlite"));
    let connection = init_catalogue(&wal_catalogue, &wal_config).unwrap();
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
        .unwrap();
    register_workspace(&connection, &root, &wal_config, &wal_catalogue, "1.0.0").unwrap();
    let wal_path = PathBuf::from(format!("{}-wal", wal_catalogue.display()));
    assert!(
        wal_path.metadata().unwrap().len() > 0,
        "workspace row must remain in the live WAL"
    );
    let before = fs::read_dir(&catalogue_root)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            (
                entry.file_name(),
                metadata.len(),
                metadata.modified().unwrap(),
            )
        })
        .collect::<Vec<_>>();

    let mut status = atlas(app_data.path());
    status.arg("status").arg(&root);
    let error = failure_stderr(&mut status);
    assert!(error.contains("cannot inspect legacy catalogue"), "{error}");
    assert!(error.contains("non-empty WAL"), "{error}");
    assert!(error.contains("--catalogue"), "{error}");
    let after = fs::read_dir(&catalogue_root)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            (
                entry.file_name(),
                metadata.len(),
                metadata.modified().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(before, after);
    assert!(!catalogue_root.join("locators").exists());
    drop(connection);
}

#[cfg(unix)]
#[test]
fn locator_target_symlink_escape_is_rejected() {
    use std::os::unix::fs::symlink;

    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let root = workspaces.path().join("symlink root");
    fs::create_dir_all(&root).unwrap();
    let init = init_default(app_data.path(), &root, "Symlink");
    let catalogue = PathBuf::from(init["catalogue_path"].as_str().unwrap());
    let outside = workspaces.path().join("outside.sqlite");
    fs::rename(&catalogue, &outside).unwrap();
    symlink(&outside, &catalogue).unwrap();

    let mut status = atlas(app_data.path());
    status.arg("status").arg(&root);
    let error = failure_stderr(&mut status);
    assert!(error.contains("physically escapes"), "{error}");
}

#[test]
fn concurrent_legacy_locator_creation_is_atomic_and_ignores_crash_tempfiles() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let root = workspaces.path().join("concurrent");
    fs::create_dir_all(&root).unwrap();
    let init = init_default(app_data.path(), &root, "Concurrent");
    let expected_workspace_id = init["workspace_id"].clone();
    let locator_dir = catalogue_dir(app_data.path()).join("locators");
    let locator = fs::read_dir(&locator_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::remove_file(&locator).unwrap();
    fs::write(locator_dir.join(".locator-crash.tmp"), b"incomplete").unwrap();

    let mut first = atlas_process(app_data.path());
    first
        .arg("status")
        .arg(&root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut second = atlas_process(app_data.path());
    second
        .arg("status")
        .arg(&root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let first = first.spawn().unwrap();
    let second = second.spawn().unwrap();
    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );

    let locator_value: Value = serde_json::from_slice(&fs::read(&locator).unwrap()).unwrap();
    assert_eq!(locator_value["workspace_id"], expected_workspace_id);
    assert!(locator_dir.join(".locator-crash.tmp").is_file());
}

#[test]
fn competing_default_inits_retain_only_the_winning_catalogue() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let root = workspaces.path().join("competing init");
    fs::create_dir_all(&root).unwrap();

    let mut first = atlas_process(app_data.path());
    first
        .arg("init")
        .arg(&root)
        .arg("--display-name")
        .arg("First")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut second = atlas_process(app_data.path());
    second
        .arg("init")
        .arg(&root)
        .arg("--display-name")
        .arg("Second")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let first = first.spawn().unwrap();
    let second = second.spawn().unwrap();
    let outputs = [
        first.wait_with_output().unwrap(),
        second.wait_with_output().unwrap(),
    ];
    let successes = outputs
        .iter()
        .filter(|output| output.status.success())
        .collect::<Vec<_>>();
    let failures = outputs
        .iter()
        .filter(|output| !output.status.success())
        .collect::<Vec<_>>();
    assert_eq!(successes.len(), 1, "{outputs:#?}");
    assert_eq!(failures.len(), 1, "{outputs:#?}");
    let failure = String::from_utf8_lossy(&failures[0].stderr);
    assert!(
        failure.contains("already routed") || failure.contains("concurrently changed"),
        "{failure}"
    );

    let winner: Value = serde_json::from_slice(&successes[0].stdout).unwrap();
    let expected_catalogue = PathBuf::from(winner["catalogue_path"].as_str().unwrap());
    let catalogue_root = catalogue_dir(app_data.path());
    let retained_catalogues = fs::read_dir(&catalogue_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert_eq!(retained_catalogues, vec![expected_catalogue.clone()]);

    let locator_dir = catalogue_root.join("locators");
    let locators = fs::read_dir(&locator_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(locators.len(), 1, "{locators:#?}");
    assert_eq!(locators[0].extension(), Some(OsStr::new("json")));
    let locator: Value = serde_json::from_slice(&fs::read(&locators[0]).unwrap()).unwrap();
    assert_eq!(locator["workspace_id"], winner["workspace_id"]);
    assert_eq!(
        Path::new(locator["catalogue_path"].as_str().unwrap()),
        expected_catalogue
    );

    let mut status = atlas(app_data.path());
    status.arg("status").arg(&root);
    let status = success_json(&mut status);
    assert_eq!(status["workspace_id"], winner["workspace_id"]);
}

#[test]
fn corrupt_legacy_catalogue_fails_closed_instead_of_appearing_absent() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let root = workspaces.path().join("corrupt scan");
    fs::create_dir_all(&root).unwrap();
    let catalogue_root = catalogue_dir(app_data.path());
    fs::create_dir_all(&catalogue_root).unwrap();
    fs::write(catalogue_root.join("ws_corrupt.sqlite"), b"not sqlite").unwrap();

    let mut status = atlas(app_data.path());
    status.arg("status").arg(&root);
    let error = failure_stderr(&mut status);
    assert!(error.contains("cannot inspect legacy catalogue"), "{error}");
    assert!(
        !error.contains("no default catalogue registered"),
        "{error}"
    );
}

#[test]
fn locked_legacy_catalogue_fails_closed_instead_of_appearing_absent() {
    let app_data = TempDir::new().unwrap();
    let workspaces = TempDir::new().unwrap();
    let root = workspaces.path().join("locked scan");
    fs::create_dir_all(&root).unwrap();
    let catalogue_root = catalogue_dir(app_data.path());
    fs::create_dir_all(&catalogue_root).unwrap();
    let locked_path = catalogue_root.join("ws_locked.sqlite");
    let locked = init_catalogue(&locked_path, &config("Locked")).unwrap();
    locked
        .pragma_update(None, "journal_mode", "DELETE")
        .unwrap();
    locked.execute_batch("BEGIN EXCLUSIVE").unwrap();

    let mut status = atlas(app_data.path());
    status.arg("status").arg(&root);
    let error = failure_stderr(&mut status);
    assert!(error.contains("cannot inspect legacy catalogue"), "{error}");
    assert!(error.contains("locked"), "{error}");
    assert!(
        !error.contains("no default catalogue registered"),
        "{error}"
    );
    locked.execute_batch("ROLLBACK").unwrap();
}
