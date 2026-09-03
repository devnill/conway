//! `conway::config::metadata_path_for` -- the setup-time context-window
//! persistence's own file/scope resolution (board item: setup-time context
//! window, ASK + PERSIST). Covers exactly the three cases this function's
//! own doc names: the schema default (project-relative to `cwd`), an
//! explicit `[models].metadata_path` override honored from either the user
//! or the project layer, and an ABSOLUTE override passed through unchanged.

#[path = "support/mod.rs"]
mod support;

use conway::config::metadata_path_for;

#[test]
fn defaults_to_dot_conway_models_json_relative_to_cwd_with_no_config_at_all() {
    let cwd = support::unique_temp_dir("metadata-path-default");
    let path = metadata_path_for(&cwd, &support::isolated_env()).unwrap();
    assert_eq!(path, cwd.join(".conway").join("models.json"));
}

#[test]
fn honors_an_explicit_relative_override_from_the_user_layer() {
    let cwd = support::unique_temp_dir("metadata-path-user-relative");
    let config_home = support::unique_temp_dir("metadata-path-user-relative-home");
    std::fs::write(
        config_home.join("settings.json"),
        r#"{"models":{"metadata_path":"custom/window.json"}}"#,
    )
    .unwrap();

    let mut env = std::collections::HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        config_home.to_string_lossy().to_string(),
    );

    let path = metadata_path_for(&cwd, &env).unwrap();
    assert_eq!(path, cwd.join("custom").join("window.json"));
}

#[test]
fn honors_an_absolute_override_unchanged() {
    let cwd = support::unique_temp_dir("metadata-path-absolute");
    let config_home = support::unique_temp_dir("metadata-path-absolute-home");
    let absolute = support::unique_temp_dir("metadata-path-absolute-target").join("models.json");
    std::fs::write(
        config_home.join("settings.json"),
        format!(
            r#"{{"models":{{"metadata_path":{}}}}}"#,
            serde_json::to_string(&absolute.to_string_lossy().to_string()).unwrap()
        ),
    )
    .unwrap();

    let mut env = std::collections::HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        config_home.to_string_lossy().to_string(),
    );

    let path = metadata_path_for(&cwd, &env).unwrap();
    assert_eq!(path, absolute);
}

#[test]
fn a_project_local_override_is_never_shadowed_by_the_user_layer() {
    // The exact scenario the board item's own "file/scope decision" doc
    // names as the risk of a rejected alternative (always writing into a
    // fixed user-scope models.json and pointing `metadata_path` at it):
    // a project's own `.conway/settings.json` names its OWN metadata_path,
    // and that must win over anything the user layer says.
    let cwd = support::unique_temp_dir("metadata-path-project-wins");
    std::fs::create_dir_all(cwd.join(".conway")).unwrap();
    std::fs::write(
        cwd.join(".conway").join("settings.json"),
        r#"{"models":{"metadata_path":"project-scoped-models.json"}}"#,
    )
    .unwrap();

    let config_home = support::unique_temp_dir("metadata-path-project-wins-home");
    std::fs::write(
        config_home.join("settings.json"),
        r#"{"models":{"metadata_path":"user-scoped-models.json"}}"#,
    )
    .unwrap();

    let mut env = std::collections::HashMap::new();
    env.insert(
        "CONWAY_CONFIG_DIR".to_string(),
        config_home.to_string_lossy().to_string(),
    );

    let path = metadata_path_for(&cwd, &env).unwrap();
    assert_eq!(path, cwd.join("project-scoped-models.json"));
}
