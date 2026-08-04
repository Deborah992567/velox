//! Location matching: nginx-style `location` blocks and route precedence.
//!
//! A location is an exact string (`=pattern`), a prefix string, or a regular
//! expression (`~`/`~*`). Matching a path follows the documented nginx-style
//! precedence (architecture §10):
//!
//! 1. an `=` exact match wins outright;
//! 2. otherwise the **longest matching prefix** is remembered; if that prefix
//!    was declared with the `^~` modifier, the regex pass is skipped;
//! 3. otherwise regular expressions are tried in **declaration order** and the
//!    first match wins — a regex overrides the remembered prefix;
//! 4. otherwise the longest prefix from step 2 is used.
//!
//! Prefix matching is a plain substring test on the request path, so
//! `location /foo` matches `/foobar`; regexes are tested against the path as
//! received (no percent-decoding), and a path that is not valid UTF-8 can
//! never match a regex location.

use regex::{Regex, RegexBuilder};

/// How a location matched a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    /// An `=` exact match.
    Exact,
    /// A longest-prefix match.
    Prefix,
    /// A `~`/`~*` regular-expression match.
    Regex,
}

/// The match condition of a [`Location`].
#[derive(Debug, Clone)]
pub enum LocationMatcher {
    /// `=pattern` — exact path equality.
    Exact(String),
    /// A prefix match; `stop_regex` mirrors nginx's `^~` modifier.
    Prefix {
        /// The prefix string.
        prefix: String,
        /// Whether a longest-prefix match here skips the regex pass.
        stop_regex: bool,
    },
    /// A `~`/`~*` regular expression, already compiled with its case
    /// sensitivity.
    Regex(Regex),
}

impl LocationMatcher {
    /// An exact-match location (`=pattern`).
    #[must_use]
    pub const fn exact(pattern: String) -> Self {
        Self::Exact(pattern)
    }

    /// A plain prefix-match location.
    #[must_use]
    pub const fn prefix(prefix: String) -> Self {
        Self::Prefix {
            prefix,
            stop_regex: false,
        }
    }

    /// A `^~` prefix-match location: regexes are skipped once it wins.
    #[must_use]
    pub const fn prefix_stopping_regex(prefix: String) -> Self {
        Self::Prefix {
            prefix,
            stop_regex: true,
        }
    }

    /// A case-sensitive `~` regex location.
    ///
    /// Returns the underlying [`regex::Error`] for an invalid pattern.
    pub fn regex(pattern: &str) -> Result<Self, regex::Error> {
        Regex::new(pattern).map(Self::Regex)
    }

    /// A case-insensitive `~*` regex location.
    ///
    /// Returns the underlying [`regex::Error`] for an invalid pattern.
    pub fn regex_case_insensitive(pattern: &str) -> Result<Self, regex::Error> {
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map(Self::Regex)
    }

    fn exact_matches(&self, path: &[u8]) -> bool {
        match self {
            Self::Exact(pattern) => path == pattern.as_bytes(),
            _ => false,
        }
    }

    fn prefix_len(&self, path: &[u8]) -> Option<usize> {
        match self {
            Self::Prefix { prefix, .. } => {
                path.strip_prefix(prefix.as_bytes()).map(|_| prefix.len())
            }
            _ => None,
        }
    }

    const fn stops_regex(&self) -> bool {
        matches!(
            self,
            Self::Prefix {
                stop_regex: true,
                ..
            }
        )
    }

    fn captures(&self, path: &[u8]) -> Option<Vec<Option<String>>> {
        let Self::Regex(regex) = self else {
            return None;
        };
        let path = std::str::from_utf8(path).ok()?;
        let captures = regex.captures(path)?;
        Some(
            (1..captures.len())
                .map(|index| captures.get(index).map(|m| m.as_str().to_string()))
                .collect(),
        )
    }
}

/// A location block: a match condition plus handler configuration.
///
/// `T` is the handler configuration attached to the location (a static-file
/// root, a proxy target, …); routing only inspects [`Location::matcher`].
#[derive(Debug, Clone)]
pub struct Location<T> {
    /// The match condition.
    pub matcher: LocationMatcher,
    /// The named-location label (e.g. `@error`), for internal redirects.
    pub label: Option<String>,
    /// The handler configuration for this location.
    pub config: T,
}

impl<T> Location<T> {
    /// Build a location from a matcher and handler configuration.
    #[must_use]
    pub const fn new(matcher: LocationMatcher, config: T) -> Self {
        Self {
            matcher,
            label: None,
            config,
        }
    }

    /// Attach a named-location label (without the leading `@`) for internal
    /// redirects.
    #[must_use]
    pub fn named(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

/// The outcome of matching a path against a location table.
#[derive(Debug)]
pub struct RouteMatch<'a, T> {
    /// The matched location.
    pub location: &'a Location<T>,
    /// How it matched.
    pub kind: MatchKind,
    /// The regex capture groups (1..), for regex matches; `None` otherwise.
    pub captures: Option<Vec<Option<String>>>,
}

/// Match a path against a location table using the documented nginx-style
/// precedence (exact > longest prefix > first regex in declaration order).
///
/// Returns `None` when no location matches.
#[must_use]
pub fn match_location<'a, T>(
    locations: &'a [Location<T>],
    path: &[u8],
) -> Option<RouteMatch<'a, T>> {
    if let Some(location) = locations.iter().find(|l| l.matcher.exact_matches(path)) {
        return Some(RouteMatch {
            location,
            kind: MatchKind::Exact,
            captures: None,
        });
    }

    let longest_prefix = locations
        .iter()
        .filter_map(|l| l.matcher.prefix_len(path).map(|len| (l, len)))
        .max_by_key(|(_, len)| *len);

    if longest_prefix.is_none_or(|(l, _)| !l.matcher.stops_regex()) {
        for location in locations {
            if let Some(captures) = location.matcher.captures(path) {
                return Some(RouteMatch {
                    location,
                    kind: MatchKind::Regex,
                    captures: Some(captures),
                });
            }
        }
    }

    longest_prefix.map(|(location, _)| RouteMatch {
        location,
        kind: MatchKind::Prefix,
        captures: None,
    })
}

#[cfg(test)]
mod tests {
    use super::{Location, LocationMatcher, MatchKind, match_location};

    fn loc<T>(matcher: LocationMatcher, config: T) -> Location<T> {
        Location::new(matcher, config)
    }

    #[test]
    fn exact_match_wins_over_prefix() {
        let locations = [
            loc(LocationMatcher::prefix("/api/".into()), "prefix"),
            loc(LocationMatcher::exact("/api/".into()), "exact"),
        ];
        let matched = match_location(&locations, b"/api/").expect("match");
        assert_eq!(matched.location.config, "exact");
        assert_eq!(matched.kind, MatchKind::Exact);
    }

    #[test]
    fn longest_prefix_wins() {
        let locations = [
            loc(LocationMatcher::prefix("/".into()), "root"),
            loc(LocationMatcher::prefix("/a".into()), "a"),
            loc(LocationMatcher::prefix("/a/b".into()), "a/b"),
        ];
        let matched = match_location(&locations, b"/a/b/c").expect("match");
        assert_eq!(matched.location.config, "a/b");
        assert_eq!(matched.kind, MatchKind::Prefix);

        let matched = match_location(&locations, b"/x").expect("match");
        assert_eq!(matched.location.config, "root");
    }

    #[test]
    fn prefix_is_a_substring_test() {
        let locations = [loc(LocationMatcher::prefix("/foo".into()), "foo")];
        let matched = match_location(&locations, b"/foobar").expect("match");
        assert_eq!(matched.location.config, "foo");
    }

    #[test]
    fn regex_overrides_plain_prefix() {
        let locations = [
            loc(LocationMatcher::prefix("/download/".into()), "prefix"),
            loc(
                LocationMatcher::regex(r"\.tar\.gz$").expect("regex"),
                "regex",
            ),
        ];
        let matched = match_location(&locations, b"/download/x.tar.gz").expect("match");
        assert_eq!(matched.location.config, "regex");
        assert_eq!(matched.kind, MatchKind::Regex);
    }

    #[test]
    fn stopping_prefix_skips_regex_pass() {
        let locations = [
            loc(
                LocationMatcher::prefix_stopping_regex("/download/".into()),
                "stopping",
            ),
            loc(
                LocationMatcher::regex(r"\.tar\.gz$").expect("regex"),
                "regex",
            ),
        ];
        let matched = match_location(&locations, b"/download/x.tar.gz").expect("match");
        assert_eq!(matched.location.config, "stopping");
        assert_eq!(matched.kind, MatchKind::Prefix);
    }

    #[test]
    fn first_matching_regex_in_declaration_order_wins() {
        let locations = [
            loc(LocationMatcher::regex(r"/a/").expect("re1"), "first"),
            loc(LocationMatcher::regex(r"/a/").expect("re2"), "second"),
        ];
        let matched = match_location(&locations, b"/a/x").expect("match");
        assert_eq!(matched.location.config, "first");
    }

    #[test]
    fn regex_captures_are_exposed() {
        let locations = [loc(
            LocationMatcher::regex("^/file/([0-9]+)(?:/([a-z]+))?").expect("regex"),
            "file",
        )];
        let matched = match_location(&locations, b"/file/42/readme").expect("match");
        assert_eq!(matched.kind, MatchKind::Regex);
        let captures = matched.captures.expect("captures");
        assert_eq!(captures[0], Some("42".to_string()));
        assert_eq!(captures[1], Some("readme".to_string()));

        let matched = match_location(&locations, b"/file/7").expect("match");
        let captures = matched.captures.expect("captures");
        assert_eq!(captures[0], Some("7".to_string()));
        assert_eq!(captures[1], None);
    }

    #[test]
    fn case_insensitive_regex_matches() {
        let locations = [loc(
            LocationMatcher::regex_case_insensitive(r"\.html$").expect("regex"),
            "html",
        )];
        let matched = match_location(&locations, b"/INDEX.HTML").expect("match");
        assert_eq!(matched.kind, MatchKind::Regex);
    }

    #[test]
    fn non_utf8_path_cannot_match_regex() {
        let locations = [
            loc(LocationMatcher::prefix("/a".into()), "prefix"),
            loc(LocationMatcher::regex("b$").expect("regex"), "regex"),
        ];
        let matched = match_location(&locations, b"/a\xffb").expect("match");
        assert_eq!(matched.location.config, "prefix");
        assert_eq!(matched.kind, MatchKind::Prefix);
    }

    #[test]
    fn no_match_returns_none() {
        let locations = [
            loc(LocationMatcher::exact("/x".into()), "x"),
            loc(LocationMatcher::regex("^/y$").expect("regex"), "y"),
        ];
        assert!(match_location(&locations, b"/z").is_none());
        assert!(match_location::<&str>(&[], b"/z").is_none());
    }

    #[test]
    fn named_location_label_is_retained() {
        let locations = [loc(LocationMatcher::exact("/error".into()), "e").named("@error")];
        assert_eq!(locations[0].label.as_deref(), Some("@error"));
    }
}
