//! The binary refuses to run for an unknown or disallowed launcher and never writes to stdout.

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn no_arguments_exit_with_usage_error_and_empty_stdout() {
    Command::cargo_bin("zeus-native-host")
        .unwrap()
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("zeus-native-host"));
}

#[test]
fn an_extension_not_in_the_allow_list_is_refused() {
    Command::cargo_bin("zeus-native-host")
        .unwrap()
        .arg("chrome-extension://abcdefghijklmnopabcdefghijklmnop/")
        .assert()
        .code(3)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("not allowed"));
}

#[test]
fn a_malformed_origin_is_refused() {
    Command::cargo_bin("zeus-native-host")
        .unwrap()
        .arg("https://evil.example/")
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty());
}
