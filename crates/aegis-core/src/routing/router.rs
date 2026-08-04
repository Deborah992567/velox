//! The router: virtual-host selection combined with location dispatch.

use super::host::{Host, ServerName, match_names};
use super::location::{Location, RouteMatch, match_location};

/// One virtual server: a set of `server_name` patterns and a location table.
#[derive(Debug, Clone)]
pub struct VirtualHost<T> {
    /// The `server_name` patterns, in declaration order.
    pub names: Vec<ServerName>,
    /// The location table, in declaration order.
    pub locations: Vec<Location<T>>,
}

impl<T> VirtualHost<T> {
    /// Build a virtual host from its names and location table.
    #[must_use]
    pub const fn new(names: Vec<ServerName>, locations: Vec<Location<T>>) -> Self {
        Self { names, locations }
    }

    /// The most specific `server_name` matching a host, if any.
    #[must_use]
    pub fn matches(&self, host: &Host) -> Option<&ServerName> {
        match_names(&self.names, host)
    }

    /// Match a path against this host's location table.
    #[must_use]
    pub fn route_location<'a>(&'a self, path: &[u8]) -> Option<RouteMatch<'a, T>> {
        match_location(&self.locations, path)
    }

    /// Look up a named location by its label (without the leading `@`).
    #[must_use]
    pub fn find_named(&self, label: &str) -> Option<&Location<T>> {
        self.locations
            .iter()
            .find(|location| location.label.as_deref() == Some(label))
    }
}

/// The outcome of routing a request: the selected host plus its matched
/// location.
#[derive(Debug)]
pub struct Route<'a, T> {
    /// The selected virtual host.
    pub host: &'a VirtualHost<T>,
    /// The `server_name` that matched, when selection was by name; `None`
    /// when the default host was chosen.
    pub matched_name: Option<&'a ServerName>,
    /// The matched location, or `None` when no location matched the path.
    pub location: Option<RouteMatch<'a, T>>,
}

/// A virtual-host routing table.
#[derive(Debug, Clone)]
pub struct Router<T> {
    hosts: Vec<VirtualHost<T>>,
    default: usize,
}

impl<T> Router<T> {
    /// Build a router; the first host becomes the default (last-resort)
    /// server.
    ///
    /// # Panics
    ///
    /// Panics if `hosts` is empty — a router needs at least one server.
    #[must_use]
    pub fn new(hosts: Vec<VirtualHost<T>>) -> Self {
        assert!(
            !hosts.is_empty(),
            "a router requires at least one virtual host"
        );
        Self { hosts, default: 0 }
    }

    /// The default (last-resort) host.
    #[must_use]
    pub fn default_host(&self) -> &VirtualHost<T> {
        &self.hosts[self.default]
    }

    /// All virtual hosts, in declaration order.
    #[must_use]
    pub fn hosts(&self) -> &[VirtualHost<T>] {
        &self.hosts
    }

    /// Select the host for a request.
    ///
    /// The `Host` header (when present) is matched first, then the SNI value
    /// (when present). Within a pass, host selection follows nginx's order
    /// across *all* servers: exact `server_name` first, then the longest
    /// wildcard, then regexes in declaration order, then a `_` catch-all.
    /// When nothing matches, the default host is returned.
    #[must_use]
    pub fn select_host<'a>(
        &'a self,
        host: Option<&Host>,
        sni: Option<&str>,
    ) -> (&'a VirtualHost<T>, Option<&'a ServerName>) {
        let sni = sni.map(|name| Host {
            name: name.to_ascii_lowercase(),
            port: None,
        });
        for candidate in [host, sni.as_ref()].into_iter().flatten() {
            if let Some((virtual_host, name)) = self.match_exact(candidate) {
                return (virtual_host, Some(name));
            }
            if let Some((virtual_host, name)) = self.match_longest_wildcard(candidate) {
                return (virtual_host, Some(name));
            }
            if let Some((virtual_host, name)) = self.match_regex(candidate) {
                return (virtual_host, Some(name));
            }
            if let Some((virtual_host, name)) = self.match_catch_all(candidate) {
                return (virtual_host, Some(name));
            }
        }
        (self.default_host(), None)
    }

    fn match_exact<'a>(&'a self, host: &Host) -> Option<(&'a VirtualHost<T>, &'a ServerName)> {
        self.hosts.iter().find_map(|virtual_host| {
            virtual_host
                .names
                .iter()
                .find(|name| name.is_exact() && name.matches(host))
                .map(|name| (virtual_host, name))
        })
    }

    fn match_longest_wildcard<'a>(
        &'a self,
        host: &Host,
    ) -> Option<(&'a VirtualHost<T>, &'a ServerName)> {
        let mut best: Option<(&VirtualHost<T>, &ServerName)> = None;
        for virtual_host in &self.hosts {
            for name in &virtual_host.names {
                let matched = name.is_wildcard() && name.matches(host);
                if matched && best.is_none_or(|(_, best)| name.fixed_len() > best.fixed_len()) {
                    best = Some((virtual_host, name));
                }
            }
        }
        best
    }

    fn match_regex<'a>(&'a self, host: &Host) -> Option<(&'a VirtualHost<T>, &'a ServerName)> {
        self.hosts.iter().find_map(|virtual_host| {
            virtual_host
                .names
                .iter()
                .find(|name| name.is_regex() && name.matches(host))
                .map(|name| (virtual_host, name))
        })
    }

    fn match_catch_all<'a>(&'a self, host: &Host) -> Option<(&'a VirtualHost<T>, &'a ServerName)> {
        self.hosts.iter().find_map(|virtual_host| {
            virtual_host
                .names
                .iter()
                .find(|name| name.is_catch_all() && name.matches(host))
                .map(|name| (virtual_host, name))
        })
    }

    /// Route a request path to a host and location.
    #[must_use]
    pub fn route<'a>(
        &'a self,
        host: Option<&Host>,
        sni: Option<&str>,
        path: &[u8],
    ) -> Route<'a, T> {
        let (virtual_host, matched_name) = self.select_host(host, sni);
        Route {
            host: virtual_host,
            matched_name,
            location: virtual_host.route_location(path),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Router, VirtualHost};
    use crate::routing::host::{Host, ServerName};
    use crate::routing::location::{Location, LocationMatcher};

    fn host(value: &str) -> Host {
        Host::parse(value).expect("valid host")
    }

    fn names(values: &[&str]) -> Vec<ServerName> {
        values
            .iter()
            .map(|v| ServerName::parse(v).expect("valid name"))
            .collect()
    }

    fn locations<'a>(configs: &[&'a str]) -> Vec<Location<&'a str>> {
        configs
            .iter()
            .map(|c| Location::new(LocationMatcher::prefix("/".into()), *c))
            .collect()
    }

    fn router() -> Router<&'static str> {
        let main = VirtualHost::new(
            names(&["example.com", "*.example.com"]),
            locations(&["main"]),
        );
        let api = VirtualHost::new(names(&["api.example.com"]), locations(&["api"]));
        let edge = VirtualHost::new(names(&["edge.example.com:8443"]), locations(&["edge"]));
        let catch_all = VirtualHost::new(names(&["_"]), locations(&["catch-all"]));
        Router::new(vec![main, api, edge, catch_all])
    }

    #[test]
    fn select_host_by_exact_name() {
        let router = router();
        let (selected, name) = router.select_host(Some(&host("example.com")), None);
        assert_eq!(selected.names.len(), 2);
        assert!(matches!(name, Some(ServerName::Exact(_))));

        let (selected, _) = router.select_host(Some(&host("www.example.com")), None);
        assert_eq!(selected.names.len(), 2);
    }

    #[test]
    fn select_host_matches_port_scoped_server() {
        let router = router();
        // The exact `api.example.com` beats the `*.example.com` wildcard.
        let (selected, _) = router.select_host(Some(&host("api.example.com:8443")), None);
        assert_eq!(selected.locations[0].config, "api");
        let (selected, _) = router.select_host(Some(&host("api.example.com:80")), None);
        assert_eq!(selected.locations[0].config, "api");

        // A name carrying a port only matches that port.
        let (selected, _) = router.select_host(Some(&host("edge.example.com:8443")), None);
        assert_eq!(selected.locations[0].config, "edge");
        let (selected, name) = router.select_host(Some(&host("edge.example.com:80")), None);
        assert_eq!(selected.locations[0].config, "main");
        assert!(matches!(name, Some(ServerName::PrefixWildcard(_))));
    }

    #[test]
    fn select_host_falls_back_to_sni_then_default() {
        let router = router();
        let (selected, _) = router.select_host(None, Some("example.com"));
        assert_eq!(selected.locations[0].config, "main");

        let (selected, name) = router.select_host(None, None);
        assert_eq!(selected.locations[0].config, "main");
        assert!(name.is_none());
    }

    #[test]
    fn route_combines_host_and_location() {
        let router = router();
        let route = router.route(Some(&host("example.com")), None, b"/index.html");
        assert_eq!(route.host.locations[0].config, "main");
        assert!(route.matched_name.is_some());
        assert!(route.location.is_some());
    }

    #[test]
    fn route_reports_no_location_for_unmatched_path() {
        let main = VirtualHost::new(
            names(&["example.com"]),
            vec![Location::new(
                LocationMatcher::exact("/index.html".into()),
                "main",
            )],
        );
        let router = Router::new(vec![main]);
        let route = router.route(Some(&host("example.com")), None, b"/no-such-path");
        assert!(route.location.is_none());
    }

    #[test]
    fn named_location_lookup() {
        let main = VirtualHost::new(
            names(&["example.com"]),
            vec![
                Location::new(LocationMatcher::exact("/error".into()), "error-page").named("error"),
            ],
        );
        let found = main.find_named("error").expect("named");
        assert_eq!(found.config, "error-page");
        assert!(main.find_named("missing").is_none());
    }

    #[test]
    fn router_requires_at_least_one_host() {
        assert!(std::panic::catch_unwind(|| Router::<()>::new(Vec::new())).is_err());
    }

    #[test]
    fn default_host_is_exposed() {
        let router = router();
        assert_eq!(router.default_host().locations[0].config, "main");
        assert_eq!(router.hosts().len(), 4);
    }
}
