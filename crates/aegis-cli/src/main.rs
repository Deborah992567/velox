//! `aegis` — command-line entry point for the Velox web server.
//!
//! Phase 1 implements version reporting (`-v`, `-V`) and configuration
//! validation (`-t`). The daemon lifecycle commands (`start`, `stop`,
//! `reload`, `restart`, `status`) arrive with the master-process work in
//! later phases.

use std::path::PathBuf;
use std::process::ExitCode;

use aegis_core::config::{ConfigValidator, parse_named};
use aegis_core::{BINARY_NAME, PROJECT_NAME, VERSION};

/// Configuration files probed by `-t` when no explicit path is given.
const DEFAULT_CONFIG_PATHS: &[&str] = &[
    "./aegis.conf",
    "./conf/aegis.conf",
    "/usr/local/etc/aegis/aegis.conf",
    "/etc/aegis/aegis.conf",
];

const USAGE: &str = "\
Aegis (Velox) — an Nginx-class web server, reverse proxy, and load balancer.

Usage: aegis [OPTIONS]
       aegis <COMMAND>

Options:
  -v, --version   Print the version and exit.
  -V              Print the version and build information.
  -t [FILE]       Test the configuration file for syntax errors and exit.
  -h, --help      Print this help and exit.

Commands (implemented in later phases):
  start     Start the server (daemonize).
  stop      Gracefully stop the server.
  reload    Gracefully reload the configuration.
  restart   Restart the server.
  status    Report server status.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    run(&args)
}

/// Dispatch the command line. Kept separate from `main` for unit testing.
fn run(args: &[String]) -> ExitCode {
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-v" | "--version") => {
            println!("{BINARY_NAME} version: {VERSION}");
            ExitCode::SUCCESS
        }
        Some("-V") => {
            print_verbose_version();
            ExitCode::SUCCESS
        }
        Some("-t") => test_config(args.get(1).map(String::as_str)),
        Some(other) => {
            eprintln!("aegis: unknown argument or unimplemented command: '{other}'");
            eprintln!();
            eprint!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn print_verbose_version() {
    println!("{BINARY_NAME} version: {VERSION}");
    println!(
        "built by {PROJECT_NAME} ({rustc})",
        rustc = aegis_build_rustc()
    );
    println!(
        "configured with git: {branch} @ {describe}",
        branch = aegis_build_git_branch(),
        describe = aegis_build_git_describe(),
    );
    println!("target: {target}", target = aegis_build_target());
}

fn aegis_build_rustc() -> String {
    option_env!("AEGIS_BUILD_RUSTC")
        .unwrap_or("unknown")
        .to_owned()
}

fn aegis_build_git_describe() -> String {
    option_env!("AEGIS_BUILD_GIT_DESCRIBE")
        .unwrap_or("unknown")
        .to_owned()
}

fn aegis_build_git_branch() -> String {
    option_env!("AEGIS_BUILD_GIT_BRANCH")
        .unwrap_or("unknown")
        .to_owned()
}

fn aegis_build_target() -> String {
    option_env!("AEGIS_BUILD_TARGET")
        .unwrap_or("unknown")
        .to_owned()
}

/// Resolve the configuration file for `-t`: an explicit path wins, otherwise
/// probe the standard locations in order.
fn resolve_config_path(explicit: Option<&str>) -> Option<PathBuf> {
    explicit.map_or_else(
        || {
            DEFAULT_CONFIG_PATHS
                .iter()
                .map(PathBuf::from)
                .find(|path| path.is_file())
        },
        |path| {
            let path = PathBuf::from(path);
            path.is_file().then_some(path)
        },
    )
}

/// `-t`: parse and validate the configuration, reporting diagnostics to the
/// terminal in nginx style.
fn test_config(explicit: Option<&str>) -> ExitCode {
    let Some(path) = resolve_config_path(explicit) else {
        let shown = explicit.map_or_else(|| DEFAULT_CONFIG_PATHS.join(", "), ToOwned::to_owned);
        eprintln!("aegis: [emerg] configuration file not found, searched: {shown}");
        return ExitCode::FAILURE;
    };

    let display = path.display().to_string();
    let input = match std::fs::read_to_string(&path) {
        Ok(input) => input,
        Err(error) => {
            eprintln!("aegis: [emerg] open() \"{display}\" failed: {error}");
            return ExitCode::FAILURE;
        }
    };

    let result = parse_named(&input, &display)
        .and_then(|root| ConfigValidator::new().validate(&root, &display));

    match result {
        Ok(()) => {
            println!("aegis: the configuration file {display} test is successful");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("aegis: [emerg] {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::resolve_config_path;

    fn write_temp_config(body: &str) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        file.write_all(body.as_bytes()).expect("write config");
        file
    }

    #[test]
    fn resolves_explicit_path() {
        let file = write_temp_config("worker_processes 1;\n");
        let path = resolve_config_path(Some(file.path().to_str().unwrap()));
        assert!(path.is_some(), "existing explicit path resolves");
    }

    #[test]
    fn rejects_missing_explicit_path() {
        assert!(resolve_config_path(Some("/no/such/aegis.conf")).is_none());
    }

    #[test]
    fn returns_none_when_nothing_found() {
        assert!(resolve_config_path(None).is_none());
    }

    #[test]
    fn test_config_accepts_valid_file() {
        let file = write_temp_config(
            "worker_processes 1;\n\
             error_log stderr info;\n\
             events {\n    worker_connections 1000;\n}\n\
             http {\n\
                 server {\n    listen 8080;\n}\n\
             }\n",
        );
        let code = super::test_config(Some(file.path().to_str().unwrap()));
        assert_eq!(code, std::process::ExitCode::SUCCESS);
    }

    #[test]
    fn test_config_rejects_invalid_file() {
        let file = write_temp_config("worker_processes 4;\nlisten 80;\n");
        let code = super::test_config(Some(file.path().to_str().unwrap()));
        assert_ne!(code, std::process::ExitCode::SUCCESS);
    }

    #[test]
    fn test_config_rejects_missing_file() {
        let code = super::test_config(Some("/no/such/aegis.conf"));
        assert_ne!(code, std::process::ExitCode::SUCCESS);
    }

    #[test]
    fn read_config_into_string() {
        let file = write_temp_config("worker_processes 1;\n");
        let body = fs::read_to_string(file.path()).expect("read back");
        assert!(body.contains("worker_processes"));
    }
}
