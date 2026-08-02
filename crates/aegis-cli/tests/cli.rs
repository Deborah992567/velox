//! End-to-end tests that spawn the `aegis` binary.

use std::io::Write;
use std::process::{Command, Output};

fn aegis(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aegis"))
        .args(args)
        .output()
        .expect("spawn aegis")
}

fn write_config(body: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().expect("temp file");
    file.write_all(body.as_bytes()).expect("write config");
    file
}

const VALID_CONFIG: &str = "\
worker_processes auto;\n\
error_log stderr warn;\n\
events {\n    worker_connections 1024;\n}\n\
http {\n    server {\n        listen 8080;\n    }\n}\n\
";

const INVALID_CONFIG: &str = "\
worker_processes auto;\n\
listen 80;\n\
";

#[test]
fn version_flag_prints_version() {
    let output = aegis(&["-v"]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("aegis version"), "{text}");
    assert!(text.contains("0.1.0"), "{text}");
}

#[test]
fn long_version_flag_prints_build_info() {
    let output = aegis(&["-V"]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "aegis version",
        "built by",
        "configured with git",
        "target:",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in: {text}");
    }
}

#[test]
fn help_flag_prints_usage() {
    for flag in ["-h", "--help"] {
        let output = aegis(&[flag]);
        assert!(output.status.success(), "{flag}");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("Usage: aegis"), "{flag}: {text}");
    }
}

#[test]
fn empty_args_prints_usage() {
    let output = aegis(&[]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: aegis"));
}

#[test]
fn test_config_accepts_valid_file() {
    let file = write_config(VALID_CONFIG);
    let output = aegis(&["-t", file.path().to_str().unwrap()]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("test is successful"), "{text}");
}

#[test]
fn test_config_rejects_invalid_file() {
    let file = write_config(INVALID_CONFIG);
    let output = aegis(&["-t", file.path().to_str().unwrap()]);
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("not allowed here"), "{text}");
    assert!(text.contains(":2:1"), "{text}");
}

#[test]
fn test_config_rejects_missing_file() {
    let output = aegis(&["-t", "/no/such/aegis.conf"]);
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("not found"), "{text}");
}

#[test]
fn unknown_command_fails() {
    let output = aegis(&["frobnicate"]);
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("unknown argument"), "{text}");
}
