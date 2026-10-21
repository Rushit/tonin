//! `--env` is required: commands that render or record per-environment state
//! must fail when no environment is named, instead of silently defaulting.

use std::process::{Command, Output};

fn tonin(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tonin"))
        .args(args)
        .env_remove("TONIN_ENV")
        .env_remove("TONIN_SERVICE_DIR")
        .output()
        .expect("spawn tonin")
}

fn assert_requires_env(args: &[&str]) {
    let out = tonin(args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{args:?} succeeded without --env");
    assert!(
        stderr.contains("--env"),
        "{args:?}: error should name --env, got:\n{stderr}"
    );
}

#[test]
fn helm_generate_requires_env() {
    assert_requires_env(&["helm", "generate", "--path", "."]);
}

#[test]
fn helm_check_requires_env() {
    assert_requires_env(&["helm", "check", "--path", "."]);
}

#[test]
fn affected_requires_env() {
    assert_requires_env(&["affected", "--base", "HEAD"]);
}

#[test]
fn release_requires_env() {
    assert_requires_env(&[
        "release",
        "--service",
        "svc",
        "--version",
        "1.0.0",
        "--digest",
        "sha256:abc",
    ]);
}

#[test]
fn helm_proxy_commands_require_env() {
    for cmd in [
        "upgrade",
        "template",
        "diff",
        "status",
        "rollback",
        "history",
        "uninstall",
    ] {
        assert_requires_env(&["helm", cmd, "--path", "."]);
    }
}

#[test]
fn deploy_requires_env() {
    assert_requires_env(&["deploy", "--dry-run"]);
}
