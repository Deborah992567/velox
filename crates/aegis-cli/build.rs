//! Captures build-time environment information for `aegis -V`.
//!
//! Embeds the git revision, the rustc version, and the target triple into
//! the binary via `env!`/`option_env!`. All lookups are best-effort: builds
//! outside a git checkout or in constrained CI still succeed with empty
//! fields.

use std::process::Command;

fn command(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn main() {
    let git_describe = command("git", &["describe", "--tags", "--always", "--dirty"]);
    let git_branch = command("git", &["rev-parse", "--abbrev-ref", "HEAD"]);
    let rustc = command("rustc", &["--version"]);

    println!(
        "cargo:rustc-env=AEGIS_BUILD_GIT_DESCRIBE={}",
        git_describe.as_deref().unwrap_or("unknown")
    );
    println!(
        "cargo:rustc-env=AEGIS_BUILD_GIT_BRANCH={}",
        git_branch.as_deref().unwrap_or("unknown")
    );
    println!(
        "cargo:rustc-env=AEGIS_BUILD_RUSTC={}",
        rustc.as_deref().unwrap_or("unknown")
    );
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=AEGIS_BUILD_TARGET={target}");

    // Rebuild when the git state changes so `-V` stays accurate.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
