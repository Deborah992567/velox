//! Virtual-host matching: `Host` parsing and `server_name` patterns.
//!
//! A request carries a `Host` header (host name plus optional port) and, on
//! TLS connections, an SNI value. Both are matched against per-server
//! `server_name` patterns to select a [`VirtualHost`](crate::routing::router::VirtualHost).
//! Name comparison is case-insensitive, per RFC 3986.

use regex::{Regex, RegexBuilder};

/// A parsed `Host` header or SNI value: a lowercased host name plus an
/// optional port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// The lowercased host name.
    pub name: String,
    /// The port, when the value carried one.
    pub port: Option<u16>,
}

impl Host {
    /// Parse a `Host` header value.
    ///
    /// Accepts `example.com`, `example.com:8080`, a bracketed IPv6 literal
    /// `[::1]` or `[::1]:8080`, and a bare IPv6 literal `::1`. Returns `None`
    /// for empty input, a malformed bracketed literal, an empty or
    /// non-numeric port, or a port that overflows `u16`.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if raw.starts_with('[') {
            let close = raw.find(']')?;
            let name = &raw[1..close];
            if name.is_empty() {
                return None;
            }
            let rest = &raw[close + 1..];
            let port = if rest.is_empty() {
                None
            } else {
                Some(parse_port(rest.strip_prefix(':')?)?)
            };
            return Some(Self {
                name: name.to_ascii_lowercase(),
                port,
            });
        }
        // Unbracketed: `name`, `name:port`, or a bare IPv6 literal. Only a
        // single `:` with a numeric suffix is a host/port split.
        match raw.rsplit_once(':') {
            Some((name, port)) if !name.is_empty() && !name.contains(':') => Some(Self {
                name: name.to_ascii_lowercase(),
                port: Some(parse_port(port)?),
            }),
            Some(_) => {
                // A colon that is not a clean host/port split must be a
                // literal IPv6 address; anything else is malformed.
                if raw.parse::<std::net::Ipv6Addr>().is_ok() {
                    Some(Self {
                        name: raw.to_ascii_lowercase(),
                        port: None,
                    })
                } else {
                    None
                }
            }
            None => Some(Self {
                name: raw.to_ascii_lowercase(),
                port: None,
            }),
        }
    }
}

fn parse_port(s: &str) -> Option<u16> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// A `server_name` pattern.
///
/// Nginx-style server names: an exact name (optionally carrying a port), a
/// leading or trailing `*` wildcard, a `~`/`~*` regular expression, or the
/// `_` catch-all.
#[derive(Debug, Clone)]
pub enum ServerName {
    /// An exact name, e.g. `example.com` or `example.com:8080`.
    Exact(String),
    /// `*.example.com` — the fixed suffix is stored (e.g. `example.com`).
    PrefixWildcard(String),
    /// `www.*` — the fixed prefix is stored (e.g. `www.`).
    SuffixWildcard(String),
    /// A `~`/`~*` regular expression matched against the host name.
    Regex(Regex),
    /// `_` or `*` — matches any host.
    Any,
}

impl ServerName {
    /// Parse a `server_name` value.
    ///
    /// Supports `name`, `name:port`, `*.suffix`, `prefix.*`, `~regex`,
    /// `~*regex` (case-insensitive), and `_`. Returns `None` for empty input,
    /// a `*` in the middle of a name, or an invalid regular expression.
    pub fn parse(pattern: &str) -> Option<Self> {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return None;
        }
        if pattern == "_" || pattern == "*" {
            return Some(Self::Any);
        }
        if let Some(regex) = pattern.strip_prefix("~*") {
            return RegexBuilder::new(regex)
                .case_insensitive(true)
                .build()
                .ok()
                .map(Self::Regex);
        }
        if let Some(regex) = pattern.strip_prefix('~') {
            return Regex::new(regex).ok().map(Self::Regex);
        }
        if let Some(suffix) = pattern.strip_prefix('*') {
            let suffix = suffix.trim_start_matches('.').to_ascii_lowercase();
            return if suffix.is_empty() || suffix.contains('*') {
                None
            } else {
                Some(Self::PrefixWildcard(suffix))
            };
        }
        if let Some(prefix) = pattern.strip_suffix('*') {
            let prefix = prefix.to_ascii_lowercase();
            return if prefix.is_empty() || prefix.contains('*') {
                None
            } else {
                Some(Self::SuffixWildcard(prefix))
            };
        }
        if pattern.contains('*') {
            return None;
        }
        Some(Self::Exact(pattern.to_ascii_lowercase()))
    }

    /// Whether this pattern matches a parsed host.
    ///
    /// An exact pattern that carries a port only matches hosts with that
    /// port; an exact pattern without a port matches any port. Wildcards and
    /// regexes match the host name only.
    pub fn matches(&self, host: &Host) -> bool {
        match self {
            Self::Exact(pattern) => Host::parse(pattern).is_some_and(|name| {
                host.name == name.name && name.port.is_none_or(|p| host.port == Some(p))
            }),
            Self::PrefixWildcard(suffix) => {
                let name = host.name.as_bytes();
                let suffix = suffix.as_bytes();
                if name == suffix {
                    return true;
                }
                let Some(split) = name.len().checked_sub(suffix.len() + 1) else {
                    return false;
                };
                name[split] == b'.' && name.ends_with(suffix)
            }
            Self::SuffixWildcard(prefix) => host.name.starts_with(prefix.as_str()),
            Self::Regex(regex) => regex.is_match(&host.name),
            Self::Any => true,
        }
    }

    /// The fixed-part length of a wildcard pattern (for longest-match
    /// selection); `0` for other kinds.
    pub(crate) const fn fixed_len(&self) -> usize {
        match self {
            Self::PrefixWildcard(s) | Self::SuffixWildcard(s) => s.len(),
            _ => 0,
        }
    }

    /// Whether this is an exact pattern.
    pub(crate) const fn is_exact(&self) -> bool {
        matches!(self, Self::Exact(_))
    }

    /// Whether this is a wildcard pattern.
    pub(crate) const fn is_wildcard(&self) -> bool {
        matches!(self, Self::PrefixWildcard(_) | Self::SuffixWildcard(_))
    }

    /// Whether this is a regex pattern.
    pub(crate) const fn is_regex(&self) -> bool {
        matches!(self, Self::Regex(_))
    }

    /// Whether this is the `_` catch-all.
    pub(crate) const fn is_catch_all(&self) -> bool {
        matches!(self, Self::Any)
    }
}

/// Select the most specific matching `server_name`, following nginx's order:
/// exact > longest wildcard > regex (declaration order) > `_`.
pub fn match_names<'a>(names: &'a [ServerName], host: &Host) -> Option<&'a ServerName> {
    if let Some(name) = names
        .iter()
        .find(|name| name.is_exact() && name.matches(host))
    {
        return Some(name);
    }
    if let Some(name) = names
        .iter()
        .filter(|name| name.is_wildcard() && name.matches(host))
        .max_by_key(|name| name.fixed_len())
    {
        return Some(name);
    }
    if let Some(name) = names
        .iter()
        .find(|name| name.is_regex() && name.matches(host))
    {
        return Some(name);
    }
    names.iter().find(|name| matches!(name, ServerName::Any))
}

#[cfg(test)]
mod tests {
    use super::{Host, ServerName, match_names};

    #[test]
    fn host_parse_accepts_names_ports_and_ipv6() {
        let host = Host::parse("example.com").expect("plain");
        assert_eq!(host.name, "example.com");
        assert_eq!(host.port, None);

        let host = Host::parse("ExAmPlE.CoM:8080").expect("port");
        assert_eq!(host.name, "example.com");
        assert_eq!(host.port, Some(8080));

        let host = Host::parse("[::1]:8080").expect("bracketed");
        assert_eq!(host.name, "::1");
        assert_eq!(host.port, Some(8080));

        let host = Host::parse("[2001:db8::1]").expect("bracketed no port");
        assert_eq!(host.name, "2001:db8::1");
        assert_eq!(host.port, None);

        let host = Host::parse("::1").expect("bare ipv6");
        assert_eq!(host.name, "::1");
        assert_eq!(host.port, None);

        let host = Host::parse("  example.com  ").expect("trimmed");
        assert_eq!(host.name, "example.com");
    }

    #[test]
    fn host_parse_rejects_malformed_values() {
        for bad in [
            "",
            " ",
            "example.com:",
            "example.com:abc",
            "example.com:99999",
            "example.com:80:90",
            "[]",
            "[::1",
            "[::1]8080",
        ] {
            assert_eq!(Host::parse(bad), None, "must reject {bad:?}");
        }
    }

    #[test]
    fn server_name_parse_variants() {
        assert!(matches!(
            ServerName::parse("example.com"),
            Some(ServerName::Exact(_))
        ));
        assert!(matches!(
            ServerName::parse("example.com:8080"),
            Some(ServerName::Exact(_))
        ));
        assert!(matches!(
            ServerName::parse("*.example.com"),
            Some(ServerName::PrefixWildcard(_))
        ));
        assert!(matches!(
            ServerName::parse("www.*"),
            Some(ServerName::SuffixWildcard(_))
        ));
        assert!(matches!(
            ServerName::parse("~^www\\."),
            Some(ServerName::Regex(_))
        ));
        assert!(matches!(
            ServerName::parse("~*^www\\."),
            Some(ServerName::Regex(_))
        ));
        assert!(matches!(ServerName::parse("_"), Some(ServerName::Any)));
        assert!(matches!(ServerName::parse("*"), Some(ServerName::Any)));
        for bad in ["", " ", "*mid*", "a*b", "~( "] {
            assert!(ServerName::parse(bad).is_none(), "must reject {bad:?}");
        }
    }

    fn host(name: &str) -> Host {
        Host::parse(name).expect("valid host")
    }

    #[test]
    fn exact_name_matching_is_case_insensitive() {
        let name = ServerName::parse("example.com").expect("parse");
        assert!(name.matches(&host("example.com")));
        assert!(name.matches(&host("EXAMPLE.COM")));
        assert!(name.matches(&host("example.com:8080")));
        assert!(!name.matches(&host("other.com")));
        assert!(!name.matches(&host("sub.example.com")));
    }

    #[test]
    fn exact_name_with_port_matches_only_that_port() {
        let name = ServerName::parse("example.com:8080").expect("parse");
        assert!(name.matches(&host("example.com:8080")));
        assert!(!name.matches(&host("example.com")));
        assert!(!name.matches(&host("example.com:80")));
    }

    #[test]
    fn prefix_wildcard_matches_domain_and_subdomains() {
        let name = ServerName::parse("*.example.com").expect("parse");
        assert!(name.matches(&host("example.com")));
        assert!(name.matches(&host("www.example.com")));
        assert!(name.matches(&host("a.b.example.com")));
        assert!(!name.matches(&host("badexample.com")));
        assert!(!name.matches(&host("example.com.evil.net")));
    }

    #[test]
    fn suffix_wildcard_matches_prefix() {
        let name = ServerName::parse("www.*").expect("parse");
        assert!(name.matches(&host("www.example.com")));
        assert!(!name.matches(&host("xwww.example.com")));
        assert!(name.matches(&host("WWW.example.com")));
    }

    #[test]
    fn regex_and_catch_all_match() {
        let re = ServerName::parse("~^www\\.").expect("parse");
        assert!(re.matches(&host("www.example.com")));
        assert!(!re.matches(&host("xwww.example.com")));

        let ci = ServerName::parse("~*^www\\.").expect("parse");
        assert!(ci.matches(&host("WWW.example.com")));

        let any = ServerName::parse("_").expect("parse");
        assert!(any.matches(&host("whatever.example.com:1")));
    }

    #[test]
    fn match_names_prefers_exact_then_longest_wildcard() {
        let names = [
            ServerName::parse("*.example.com").expect("wild"),
            ServerName::parse("example.com").expect("exact"),
        ];
        let best = match_names(&names, &host("example.com")).expect("match");
        assert!(matches!(best, ServerName::Exact(_)));

        let names = [
            ServerName::parse("*.example.com").expect("short wild"),
            ServerName::parse("*.api.example.com").expect("long wild"),
        ];
        let best = match_names(&names, &host("x.api.example.com")).expect("match");
        assert!(matches!(best, ServerName::PrefixWildcard(s) if s.as_str() == "api.example.com"));
    }

    #[test]
    fn match_names_tries_regex_then_catch_all() {
        let names = [
            ServerName::parse("_").expect("any"),
            ServerName::parse("~^api\\.").expect("regex"),
        ];
        let best = match_names(&names, &host("api.example.com")).expect("match");
        assert!(matches!(best, ServerName::Regex(_)));

        let best = match_names(&names, &host("other.com")).expect("match");
        assert!(matches!(best, ServerName::Any));

        assert!(match_names(&[], &host("x.com")).is_none());
    }
}
