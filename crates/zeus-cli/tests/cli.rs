//! Black-box tests of the `zeus` binary. Every test points the state directory at a temporary folder, so nothing
//! touches the real installation, and no test needs a running daemon.

use assert_cmd::Command;
use predicates::prelude::*;

fn zeus(state: &tempfile::TempDir) -> Command {
    let mut cmd = Command::cargo_bin("zeus").unwrap();
    cmd.env("ZEUS_STATE_DIR", state.path().join("state"))
        .env("ZEUS_CONFIG_DIR", state.path().join("config"))
        .env("ZEUS_RUNTIME_DIR", state.path().join("run"))
        .env_remove("ZEUS_PROFILE")
        .env("NO_COLOR", "1");
    cmd
}

#[test]
fn version_and_help_work() {
    let dir = tempfile::tempdir().unwrap();
    zeus(&dir)
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("zeus"));
    zeus(&dir)
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("approvals").and(predicate::str::contains("native-host")));
}

#[test]
fn no_arguments_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    zeus(&dir).assert().code(2);
    zeus(&dir).arg("get").assert().code(2);
}

#[test]
fn completions_print_a_script() {
    let dir = tempfile::tempdir().unwrap();
    zeus(&dir)
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("zeus"));
}

#[test]
fn config_path_honours_the_environment_and_json_mode() {
    let dir = tempfile::tempdir().unwrap();
    let output = zeus(&dir)
        .args(["--json", "config", "path"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert!(value["state_dir"].as_str().unwrap().contains("state"));
    assert!(
        value["config_file"]
            .as_str()
            .unwrap()
            .ends_with("config.toml")
    );
}

#[test]
fn commands_that_need_the_daemon_fail_with_status_3_and_say_how_to_fix_it() {
    let dir = tempfile::tempdir().unwrap();
    zeus(&dir)
        .arg("ls")
        .assert()
        .code(3)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("zeus daemon install"));
}

#[test]
fn approvals_cannot_be_approved_by_yes_even_when_nothing_is_running() {
    let dir = tempfile::tempdir().unwrap();
    zeus(&dir)
        .args([
            "--yes",
            "approvals",
            "approve",
            "ap_01M45NHJ3XS0QXG9T42HHN2YR8",
        ])
        .assert()
        .code(3);
}

#[test]
fn native_host_install_requires_an_extension_id() {
    let dir = tempfile::tempdir().unwrap();
    zeus(&dir)
        .args(["native-host", "install"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("extension-id"));
}
