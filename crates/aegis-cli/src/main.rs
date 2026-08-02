//! `aegis` — command-line entry point for the Velox web server.
//!
//! Phase 0 provides the version/usage scaffold. The full CLI
//! (`-t`, `start`, `stop`, `reload`, `restart`, `status`) is implemented in
//! Phase 1 (config validation) and Phase 17+ (daemon lifecycle).

use std::process::ExitCode;

use aegis_core::{BINARY_NAME, VERSION};

const USAGE: &str = "\
Aegis (Velox) — an Nginx-class web server, reverse proxy, and load balancer.

Usage: aegis [OPTIONS]
       aegis <COMMAND>

Options:
  -v, --version   Print the version and exit.
  -h, --help      Print this help and exit.

Commands (implemented in later phases):
  -t        Validate the configuration and exit.
  start     Start the server (daemonize).
  stop      Gracefully stop the server.
  reload    Gracefully reload the configuration.
  restart   Restart the server.
  status    Report server status.
";

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);

    match arg.as_deref() {
        None | Some("-h") | Some("--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-v") | Some("--version") => {
            println!("{BINARY_NAME} {VERSION}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("aegis: unknown argument or unimplemented command: '{other}'");
            eprintln!();
            eprint!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}
