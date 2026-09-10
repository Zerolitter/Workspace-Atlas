use workspace_atlas::catalogue::init_catalogue;
use workspace_atlas::config::Config;
use workspace_atlas::discovery::{classify, reconcile, Classification};
use workspace_atlas::workspace::register_workspace;

fn minimal_config(extra: &str) -> String {
    format!("schema_version = \"1.1.0\"\n[workspace]\ndisplay_name = \"privacy\"\n{extra}")
}

#[test]
fn invalid_policy_regex_fails_closed_with_field_and_index() {
    let error = Config::parse(&minimal_config(
        "[policy.exclude]\npatterns = [\"valid$\", \"[\"]\n",
    ))
    .unwrap_err()
    .to_string();

    assert!(error.contains("policy.exclude.patterns[1]"), "{error}");
    assert!(error.contains("invalid regular expression"), "{error}");
}

#[test]
fn default_secret_policy_excludes_key_files() {
    let config = Config::parse(&minimal_config("")).unwrap();

    assert!(matches!(
        classify("credentials/signing.key", &config),
        Classification::SecretExcluded { .. }
    ));
}

#[test]
fn shipped_example_uses_valid_regular_expressions() {
    let text = std::fs::read_to_string("config/workspace-atlas-v1.1.config.example.toml").unwrap();
    let config = Config::parse(&text).unwrap();

    assert!(matches!(
        classify("nested/private.pem", &config),
        Classification::SecretExcluded { .. }
    ));
    assert!(matches!(
        classify("nested/node_modules/package/index.js", &config),
        Classification::Excluded { .. }
    ));
}

#[test]
fn reconcile_prunes_excluded_directory_trees() {
    let database_directory = tempfile::tempdir().unwrap();
    let workspace_directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(workspace_directory.path().join("src")).unwrap();
    std::fs::write(
        workspace_directory.path().join("src/lib.rs"),
        "pub fn live() {}\n",
    )
    .unwrap();
    std::fs::create_dir_all(workspace_directory.path().join("vendor/dependency/src")).unwrap();
    std::fs::write(
        workspace_directory
            .path()
            .join("vendor/dependency/src/lib.rs"),
        "pub fn excluded() {}\n",
    )
    .unwrap();

    let config = Config::parse(&minimal_config("")).unwrap();
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

    reconcile(&workspace, &connection, &config).unwrap();

    let excluded_descendants: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM file_identity WHERE last_known_path LIKE 'vendor/%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(excluded_descendants, 0);
}
