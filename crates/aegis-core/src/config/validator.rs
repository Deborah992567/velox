//! Configuration validation.
//!
//! The validator walks the AST and checks structural rules that apply to the
//! Phase 1 directive set:
//!
//! * directives are recognized,
//! * directives appear in an allowed context (`worker_connections` only
//!   inside `events`, `listen` only inside `server`, ...),
//! * argument counts are within the allowed range,
//! * scalar arguments parse to the expected type (integer, size, boolean).
//!
//! The directive registry is intentionally curated and grows as subsystems
//! land; unknown directives are always rejected so typos cannot silently
//! change behavior. Errors carry `file:line:column`.

use std::collections::HashSet;

use crate::config::ast::{ConfigNode, ConfigRoot};
use crate::core::{Error, Result};

/// The result of validating a single directive.
#[derive(Debug)]
struct Rule {
    /// Directive name.
    name: &'static str,
    /// Allowed parent contexts (`None` = top level). Empty means the
    /// directive is only legal at the top level.
    parents: &'static [&'static str],
    /// (minimum, maximum) argument count; `usize::MAX` means unbounded.
    args: (usize, usize),
    /// Optional argument type check.
    arg_check: Option<ArgCheck>,
}

#[derive(Debug, Clone, Copy)]
enum ArgCheck {
    PositiveInt,
    PositiveIntOrAuto,
    Size,
    OnOff,
}

impl Rule {
    const fn new(
        name: &'static str,
        parents: &'static [&'static str],
        args: (usize, usize),
    ) -> Self {
        Self {
            name,
            parents,
            args,
            arg_check: None,
        }
    }

    const fn checked(
        name: &'static str,
        parents: &'static [&'static str],
        args: (usize, usize),
        arg_check: ArgCheck,
    ) -> Self {
        Self {
            name,
            parents,
            args,
            arg_check: Some(arg_check),
        }
    }
}

/// The Phase 1 directive registry.
const RULES: &[Rule] = &[
    Rule::checked("worker_processes", &[], (1, 1), ArgCheck::PositiveIntOrAuto),
    Rule::new("error_log", &[], (1, 2)),
    Rule::new("events", &[], (0, 0)),
    Rule::checked(
        "worker_connections",
        &["events"],
        (1, 1),
        ArgCheck::PositiveInt,
    ),
    Rule::new("http", &[], (0, 0)),
    Rule::new("server", &["http"], (0, 0)),
    Rule::new("listen", &["server"], (1, 4)),
    Rule::new("server_name", &["server"], (1, usize::MAX)),
    Rule::new("location", &["server"], (1, 2)),
    Rule::new("root", &["http", "server", "location"], (1, 1)),
    Rule::new("index", &["server", "location"], (1, usize::MAX)),
    Rule::new("proxy_pass", &["server", "location"], (1, 1)),
    Rule::new("access_log", &["http", "server", "location"], (1, 2)),
    Rule::checked(
        "client_max_body_size",
        &["http", "server", "location"],
        (1, 1),
        ArgCheck::Size,
    ),
    Rule::checked(
        "keepalive_timeout",
        &["http", "server", "location"],
        (1, 2),
        ArgCheck::PositiveInt,
    ),
    Rule::checked(
        "sendfile",
        &["http", "server", "location"],
        (1, 1),
        ArgCheck::OnOff,
    ),
    Rule::new("default_type", &["http", "server", "location"], (1, 1)),
];

/// Validate a parsed configuration against the directive registry.
pub fn validate(root: &ConfigRoot, file: &str) -> Result<()> {
    for node in &root.nodes {
        validate_node(node, &[], file)?;
    }
    Ok(())
}

fn validate_node(node: &ConfigNode, parents: &[&str], file: &str) -> Result<()> {
    let rule = RULES
        .iter()
        .find(|rule| rule.name == node.name)
        .ok_or_else(|| {
            Error::config_at(
                node.source_pos(file),
                format!("unknown directive \"{}\"", node.name),
            )
        })?;

    if !rule.parents.is_empty() && !rule.parents.iter().any(|p| parents.last() == Some(p)) {
        return Err(Error::config_at(
            node.source_pos(file),
            format!(
                "\"{}\" directive is not allowed here (allowed in: {})",
                node.name,
                rule.parents.join(", ")
            ),
        ));
    }

    let (min, max) = rule.args;
    let count = node.args.len();
    if count < min || count > max {
        let expected = if min == max {
            format!("{min}")
        } else {
            format!("{min}..{max}")
        };
        return Err(Error::config_at(
            node.source_pos(file),
            format!(
                "invalid number of arguments in \"{}\" directive, expected {expected}, got {count}",
                node.name
            ),
        ));
    }

    if let Some(check) = rule.arg_check {
        check_args(node, check, file)?;
    }

    let mut parents = parents.to_vec();
    if node.is_block() {
        parents.push(node.name.as_str());
    }
    for child in &node.children {
        validate_node(child, &parents, file)?;
    }
    Ok(())
}

fn check_args(node: &ConfigNode, check: ArgCheck, file: &str) -> Result<()> {
    let first = &node.args[0];
    let ok = match check {
        ArgCheck::PositiveInt => parse_positive_int(first).is_some(),
        ArgCheck::PositiveIntOrAuto => first == "auto" || parse_positive_int(first).is_some(),
        ArgCheck::Size => parse_size(first).is_some(),
        ArgCheck::OnOff => matches!(first.as_str(), "on" | "off"),
    };
    if ok {
        Ok(())
    } else {
        Err(Error::config_at(
            node.source_pos(file),
            format!(
                "invalid value \"{first}\" for the \"{}\" directive",
                node.name
            ),
        ))
    }
}

/// Parse a strictly positive decimal integer.
pub fn parse_positive_int(value: &str) -> Option<u64> {
    if value.is_empty() {
        return None;
    }
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let parsed: u64 = value.parse().ok()?;
    if parsed == 0 {
        return None;
    }
    Some(parsed)
}

/// Parse a byte size with an optional unit suffix: bare bytes, or `k`/`m`/`g`
/// (1024-multiples), case-insensitive. Returns the size in bytes.
pub fn parse_size(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let split = trimmed
        .char_indices()
        .find_map(|(index, c)| (!c.is_ascii_digit()).then_some(index))
        .unwrap_or(trimmed.len());
    let (number, unit) = trimmed.split_at(split);
    let amount: u64 = number.parse().ok()?;
    let multiplier: u64 = match unit.to_ascii_lowercase().as_str() {
        "" => 1,
        "k" | "kb" => 1024,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        _ => return None,
    };
    amount.checked_mul(multiplier)
}

/// A validator that also supports scanning the whole tree for duplicated
/// top-level singleton directives (e.g. two `events {}` blocks).
#[derive(Debug)]
pub struct ConfigValidator {
    seen_top_level: HashSet<&'static str>,
}

impl Default for ConfigValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigValidator {
    /// Create a validator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seen_top_level: HashSet::new(),
        }
    }

    /// Validate a parsed configuration. `file` is used in diagnostics.
    pub fn validate(&mut self, root: &ConfigRoot, file: &str) -> Result<()> {
        self.seen_top_level.clear();
        for node in &root.nodes {
            if let Some(rule) = RULES.iter().find(|rule| rule.name == node.name)
                && rule.parents.is_empty()
                && !self.seen_top_level.insert(rule.name)
            {
                return Err(Error::config_at(
                    node.source_pos(file),
                    format!("\"{}\" directive is duplicate", node.name),
                ));
            }
            validate_node(node, &[], file)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_positive_int, parse_size, validate};
    use crate::config::parse;

    fn validate_text(text: &str) -> Result<(), crate::core::Error> {
        let root = parse(text)?;
        validate(&root, "aegis.conf")
    }

    #[test]
    fn valid_config_validates() {
        let config = "\
worker_processes 4;
error_log logs/error.log warn;
events {
    worker_connections 10000;
}
http {
    server {
        listen 80;
        server_name example.com;
        location / {
            root /var/www/html;
            index index.html;
        }
        location /api {
            proxy_pass http://backend;
        }
        client_max_body_size 4m;
        sendfile on;
    }
}
";
        validate_text(config).unwrap();
    }

    #[test]
    fn unknown_directive_is_rejected() {
        let error = validate_text("worker_processses 4;\n").unwrap_err();
        assert!(error.to_string().contains("unknown directive"));
        assert!(error.to_string().contains("aegis.conf:1:1"));
    }

    #[test]
    fn wrong_context_is_rejected() {
        let error = validate_text("worker_connections 100;\n").unwrap_err();
        let text = error.to_string();
        assert!(text.contains("not allowed here"), "{text}");
        assert!(text.contains("aegis.conf:1:1"), "{text}");
    }

    #[test]
    fn bad_integer_is_rejected() {
        for bad in ["0", "-4", "four", "4.5"] {
            let error = validate_text(&format!("worker_processes {bad};\n")).unwrap_err();
            assert!(
                error.to_string().contains("invalid value"),
                "for {bad}: {error}"
            );
        }
    }

    #[test]
    fn worker_processes_accepts_auto() {
        validate_text("worker_processes auto;\n").unwrap();
    }

    #[test]
    fn bad_size_is_rejected() {
        let error = validate_text("http { client_max_body_size banana; }\n").unwrap_err();
        assert!(error.to_string().contains("invalid value"));
        validate_text("http { client_max_body_size 10m; }\n").unwrap();
    }

    #[test]
    fn bad_arg_count_is_rejected() {
        let error = validate_text("http { root; }\n").unwrap_err();
        assert!(error.to_string().contains("invalid number of arguments"));
    }

    #[test]
    fn nested_contexts_are_checked() {
        // `listen` is fine inside http>server but not at top level.
        let error = validate_text("listen 80;\n").unwrap_err();
        assert!(error.to_string().contains("not allowed here"));
    }

    #[test]
    fn size_parser_handles_units() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("1k").unwrap(), 1024);
        assert_eq!(parse_size("1K").unwrap(), 1024);
        assert_eq!(parse_size("2m").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_size("1g").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("1mb").unwrap(), 1024 * 1024);
        assert!(parse_size("").is_none());
        assert!(parse_size("xx").is_none());
        assert!(parse_size("1x").is_none());
    }

    #[test]
    fn positive_int_parser_is_strict() {
        assert_eq!(parse_positive_int("4").unwrap(), 4);
        assert!(parse_positive_int("0").is_none());
        assert!(parse_positive_int("-4").is_none());
        assert!(parse_positive_int("4.0").is_none());
        assert!(parse_positive_int("").is_none());
    }

    #[test]
    fn duplicate_top_level_singletons_are_rejected() {
        let config = "\
events { worker_connections 100; }
events { worker_connections 200; }
";
        let root = parse(config).unwrap();
        let mut validator = crate::config::ConfigValidator::new();
        let error = validator.validate(&root, "aegis.conf").unwrap_err();
        assert!(error.to_string().contains("duplicate"));
    }
}
