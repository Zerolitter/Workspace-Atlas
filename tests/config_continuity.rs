use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;
use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::discovery::{classify, Classification};
use workspace_atlas::workspace::{
    load_registered_config, load_workspace_by_root, register_workspace, registered_config_path,
};

fn atlas() -> Command {
    Command::cargo_bin("atlas").expect("atlas binary")
}

fn success_json(command: &mut Command) -> Value {
    let output = command.assert().success().get_output().stdout.clone();
    serde_json::from_slice(&output).expect("command emits JSON")
}

fn custom_config(path: &Path, display_name: &str) -> Config {
    let text = format!(
        "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = {display_name:?}\n[policy.exclude]\npatterns = ['^private(?:/|$)']\n"
    );
    std::fs::write(path, &text).unwrap();
    Config::parse(&text).unwrap()
}

const HISTORICAL_CONFIG_HASH: &str =
    "1c9c12cedc368263b3b813905cf637860af20ab7d677ccc591f762f84c38e3b3";
const HISTORICAL_CONFIG_JSON: &str =
    include_str!("fixtures/config/historical-registered-config-1c9c12.json");

fn registered_config_fixture(
    config_json: &str,
    registered_hash: &str,
    created_at: &str,
) -> (TempDir, TempDir, PathBuf) {
    let database_directory = TempDir::new().unwrap();
    let workspace_directory = TempDir::new().unwrap();
    let catalogue = database_directory.path().join("atlas.sqlite");
    let registered: Config = serde_json::from_str(config_json).unwrap();

    let mut initialization_config = registered.clone();
    initialization_config.policy.exclude.patterns = vec![
        r"(^|/)\.env$".to_string(),
        r"(^|/)[^/]*\.pem$".to_string(),
        r"(^|/)[^/]*\.key$".to_string(),
    ];
    initialization_config.validate().unwrap();
    let connection = init_catalogue(&catalogue, &initialization_config).unwrap();
    let workspace = register_workspace(
        &connection,
        &workspace_directory.path().canonicalize().unwrap(),
        &initialization_config,
        &catalogue,
        "1.2.0",
    )
    .unwrap();
    connection
        .execute(
            "UPDATE workspace
             SET configuration_hash = ?1, created_at = ?2
             WHERE workspace_id = ?3",
            rusqlite::params![registered_hash, created_at, workspace.workspace_id],
        )
        .unwrap();
    drop(connection);

    let registered_path = registered_config_path(&catalogue).unwrap();
    std::fs::create_dir_all(registered_path.parent().unwrap()).unwrap();
    std::fs::write(registered_path, config_json.as_bytes()).unwrap();
    (database_directory, workspace_directory, catalogue)
}

fn historical_config_fixture() -> (TempDir, TempDir, PathBuf) {
    let historical: Config = serde_json::from_str(HISTORICAL_CONFIG_JSON).unwrap();
    assert_eq!(historical.configuration_hash(), HISTORICAL_CONFIG_HASH);
    assert!(
        historical.validate().is_err(),
        "the current regex contract must reject the creation-era glob values"
    );
    registered_config_fixture(
        HISTORICAL_CONFIG_JSON,
        HISTORICAL_CONFIG_HASH,
        "2026-08-29T22:57:02.732Z",
    )
}

fn fixture() -> (TempDir, TempDir, PathBuf, PathBuf, Config) {
    let database_directory = TempDir::new().unwrap();
    let workspace_directory = TempDir::new().unwrap();
    std::fs::write(
        workspace_directory.path().join("visible.rs"),
        "pub fn visible() {}\n",
    )
    .unwrap();
    std::fs::create_dir(workspace_directory.path().join("private")).unwrap();
    std::fs::write(
        workspace_directory.path().join("private/secret.rs"),
        "pub fn secret() {}\n",
    )
    .unwrap();
    let catalogue = database_directory.path().join("atlas.sqlite");
    let config_path = database_directory.path().join("custom.toml");
    let config = custom_config(&config_path, "continuity");
    (
        database_directory,
        workspace_directory,
        catalogue,
        config_path,
        config,
    )
}

#[test]
fn exact_pre_regex_registered_config_is_effective_for_status_and_doctor() {
    let (_database_directory, workspace_directory, catalogue) = historical_config_fixture();

    for command in ["status", "doctor"] {
        let output = success_json(
            atlas()
                .arg(command)
                .arg(workspace_directory.path())
                .args(["--catalogue", catalogue.to_str().unwrap()]),
        );
        assert_eq!(output["registered_config_hash"], HISTORICAL_CONFIG_HASH);
        assert_eq!(output["effective_config_hash"], HISTORICAL_CONFIG_HASH);
    }

    let connection = rusqlite::Connection::open(&catalogue).unwrap();
    let workspace = load_workspace_by_root(&connection, workspace_directory.path())
        .unwrap()
        .unwrap();
    let config = load_registered_config(&catalogue, &workspace).unwrap();
    for secret in [".env", "nested/private.pem", "nested/private.key"] {
        assert!(matches!(
            classify(secret, &config),
            Classification::SecretExcluded { .. }
        ));
    }
}

#[test]
fn historical_compatibility_rejects_wrong_hash_and_provenance() {
    let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";
    let (_database_directory, workspace_directory, catalogue) = registered_config_fixture(
        HISTORICAL_CONFIG_JSON,
        wrong_hash,
        "2026-08-29T22:57:02.732Z",
    );
    for command in ["status", "doctor"] {
        let output = success_json(
            atlas()
                .arg(command)
                .arg(workspace_directory.path())
                .args(["--catalogue", catalogue.to_str().unwrap()]),
        );
        assert_eq!(output["registered_config_hash"], wrong_hash);
        assert_eq!(output["effective_config_hash"], Value::Null);
    }

    let (_database_directory, workspace_directory, catalogue) = historical_config_fixture();
    let connection = rusqlite::Connection::open(&catalogue).unwrap();
    connection
        .execute("UPDATE workspace SET root_fingerprint = 'drifted'", [])
        .unwrap();
    drop(connection);
    for command in ["status", "doctor"] {
        let output = success_json(
            atlas()
                .arg(command)
                .arg(workspace_directory.path())
                .args(["--catalogue", catalogue.to_str().unwrap()]),
        );
        assert_eq!(output["registered_config_hash"], HISTORICAL_CONFIG_HASH);
        assert_eq!(output["effective_config_hash"], Value::Null);
    }
}

#[test]
fn arbitrary_invalid_regex_never_activates_historical_compatibility() {
    let invalid_toml =
        "schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"privacy\"\n[policy.exclude]\npatterns = [\"[\"]\n";
    assert!(Config::parse(invalid_toml).is_err());

    let mut invalid: Value = serde_json::from_str(HISTORICAL_CONFIG_JSON).unwrap();
    invalid["policy"]["exclude"]["patterns"] = serde_json::json!(["["]);
    let invalid_json = serde_json::to_string(&invalid).unwrap();
    let invalid_config: Config = serde_json::from_str(&invalid_json).unwrap();
    let invalid_hash = invalid_config.configuration_hash();
    assert_ne!(invalid_hash, HISTORICAL_CONFIG_HASH);
    let (_database_directory, workspace_directory, catalogue) =
        registered_config_fixture(&invalid_json, &invalid_hash, "2026-08-29T22:57:02.732Z");

    for command in ["status", "doctor"] {
        let output = success_json(
            atlas()
                .arg(command)
                .arg(workspace_directory.path())
                .args(["--catalogue", catalogue.to_str().unwrap()]),
        );
        assert_eq!(output["registered_config_hash"], invalid_hash);
        assert_eq!(output["effective_config_hash"], Value::Null);
    }
}

#[test]
fn reconcile_without_config_reuses_the_registered_custom_policy() {
    let (_database_directory, workspace_directory, catalogue, config_path, _) = fixture();

    success_json(
        atlas()
            .arg("init")
            .arg(workspace_directory.path())
            .args(["--config", config_path.to_str().unwrap()])
            .args(["--catalogue", catalogue.to_str().unwrap()]),
    );
    let reconcile = success_json(
        atlas()
            .arg("reconcile")
            .arg(workspace_directory.path())
            .args(["--catalogue", catalogue.to_str().unwrap()]),
    );

    assert_eq!(reconcile["indexed_file_count"], 1);
    assert_eq!(reconcile["excluded_file_count"], 0);
    let connection = rusqlite::Connection::open(&catalogue).unwrap();
    let private_rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM file_identity WHERE last_known_path LIKE 'private/%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        private_rows, 0,
        "registered custom policy must remain effective"
    );
}

#[test]
fn supplied_mismatched_config_fails_typed_and_hash_surfaces_remain_secret_free() {
    let (database_directory, workspace_directory, catalogue, config_path, _) = fixture();
    let init = success_json(
        atlas()
            .arg("init")
            .arg(workspace_directory.path())
            .args(["--config", config_path.to_str().unwrap()])
            .args(["--catalogue", catalogue.to_str().unwrap()]),
    );
    let mismatch_path = database_directory.path().join("mismatch.toml");
    custom_config(&mismatch_path, "different-name");

    let output = atlas()
        .arg("reconcile")
        .arg(workspace_directory.path())
        .args(["--config", mismatch_path.to_str().unwrap()])
        .args(["--catalogue", catalogue.to_str().unwrap()])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let error: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(error["kind"], "invalid_config");
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("configuration_mismatch"));

    success_json(
        atlas()
            .arg("reconcile")
            .arg(workspace_directory.path())
            .args(["--catalogue", catalogue.to_str().unwrap()]),
    );
    for command in ["status", "doctor"] {
        let output = success_json(
            atlas()
                .arg(command)
                .arg(workspace_directory.path())
                .args(["--catalogue", catalogue.to_str().unwrap()]),
        );
        assert_eq!(output["registered_config_hash"], init["config_hash"]);
        assert_eq!(output["effective_config_hash"], init["config_hash"]);
        let rendered = serde_json::to_string(&output).unwrap();
        assert!(!rendered.contains(r"^private(?:/|$)"));
        assert!(!rendered.contains("different-name"));
    }
}

#[test]
fn old_catalogue_requires_config_once_then_uses_application_owned_copy() {
    let (_database_directory, workspace_directory, catalogue, config_path, config) = fixture();
    let connection = init_catalogue(&catalogue, &config).unwrap();
    register_workspace(
        &connection,
        &workspace_directory.path().canonicalize().unwrap(),
        &config,
        &catalogue,
        "1.0.0",
    )
    .unwrap();
    drop(connection);

    let missing = atlas()
        .arg("reconcile")
        .arg(workspace_directory.path())
        .args(["--catalogue", catalogue.to_str().unwrap()])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let missing: Value = serde_json::from_slice(&missing).unwrap();
    assert_eq!(missing["kind"], "invalid_config");
    assert!(missing["error"]
        .as_str()
        .unwrap()
        .contains("configuration_required"));

    success_json(
        atlas()
            .arg("reconcile")
            .arg(workspace_directory.path())
            .args(["--config", config_path.to_str().unwrap()])
            .args(["--catalogue", catalogue.to_str().unwrap()]),
    );
    let second = success_json(
        atlas()
            .arg("reconcile")
            .arg(workspace_directory.path())
            .args(["--catalogue", catalogue.to_str().unwrap()]),
    );
    assert_eq!(second["indexed_file_count"], 1);
}
