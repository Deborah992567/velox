//! Route parameter extraction utilities.
//!
//! Parses named parameters from URL paths based on route patterns.

use std::collections::HashMap;

/// A matched route with extracted parameters.
#[derive(Debug, Clone)]
pub struct RouteMatch {
    pub params: HashMap<String, String>,
    pub path: String,
}

impl RouteMatch {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            params: HashMap::new(),
            path: path.into(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.params.get(name).map(String::as_str)
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.params.insert(key.into(), value.into());
    }

    pub fn param_count(&self) -> usize {
        self.params.len()
    }

    pub fn has_param(&self, name: &str) -> bool {
        self.params.contains_key(name)
    }
}

/// Match a request path against a route pattern and extract params.
///
/// Pattern syntax: `:name` for named params, `*` for wildcard.
///
/// # Examples
/// - Pattern `/users/:id` matches `/users/42` → `{"id": "42"}`
/// - Pattern `/files/*` matches `/files/a/b/c` → wildcard captures remainder
/// - Pattern `/api/:version/users/:id` matches `/api/v2/users/1` → `{"version": "v2", "id": "1"}`
pub fn match_route(pattern: &str, path: &str) -> Option<RouteMatch> {
    let pattern_parts: Vec<&str> = pattern.trim_matches('/').split('/').collect();
    let path_parts: Vec<&str> = path.trim_matches('/').split('/').collect();

    let mut result = RouteMatch::new(path);

    let mut pi = 0;
    let mut pa = 0;

    while pi < pattern_parts.len() {
        let pp = pattern_parts[pi];

        if pp == "*" {
            let remainder: Vec<&str> = path_parts[pa..].to_vec();
            result.insert("*", remainder.join("/"));
            return Some(result);
        }

        if pa >= path_parts.len() {
            return None;
        }

        if let Some(param_name) = pp.strip_prefix(':') {
            result.insert(param_name.to_string(), path_parts[pa].to_string());
        } else if pp != path_parts[pa] {
            return None;
        }

        pi += 1;
        pa += 1;
    }

    if pa == path_parts.len() {
        Some(result)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_match() {
        let result = match_route("/users/:id", "/users/42").unwrap();
        assert_eq!(result.get("id"), Some("42"));
    }

    #[test]
    fn multiple_params() {
        let result = match_route("/api/:version/users/:id", "/api/v2/users/1").unwrap();
        assert_eq!(result.get("version"), Some("v2"));
        assert_eq!(result.get("id"), Some("1"));
        assert_eq!(result.param_count(), 2);
    }

    #[test]
    fn no_match() {
        assert!(match_route("/users/:id", "/posts/42").is_none());
    }

    #[test]
    fn extra_segments_no_match() {
        assert!(match_route("/users/:id", "/users/42/extra").is_none());
    }

    #[test]
    fn wildcard_match() {
        let result = match_route("/files/*", "/files/a/b/c").unwrap();
        assert_eq!(result.get("*"), Some("a/b/c"));
    }

    #[test]
    fn exact_match() {
        assert!(match_route("/health", "/health").is_some());
    }

    #[test]
    fn root_pattern() {
        let result = match_route("/", "/").unwrap();
        assert_eq!(result.param_count(), 0);
    }

    #[test]
    fn has_param_check() {
        let result = match_route("/users/:id", "/users/42").unwrap();
        assert!(result.has_param("id"));
        assert!(!result.has_param("name"));
    }
}
