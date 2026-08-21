//! IP-based access control lists.

use std::net::IpAddr;

/// An access control rule.
#[derive(Debug, Clone)]
pub enum AclRule {
    Allow(IpCidr),
    Deny(IpCidr),
}

/// A CIDR range or single IP address.
#[derive(Debug, Clone)]
pub struct IpCidr {
    addr: IpAddr,
    prefix_len: u8,
}

impl IpCidr {
    /// A single IP address (equivalent to /32 or /128).
    pub const fn host(addr: IpAddr) -> Self {
        let prefix_len = match addr {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        Self { addr, prefix_len }
    }

    /// Parse a CIDR notation string (e.g., "192.168.1.0/24" or "10.0.0.1").
    pub fn parse(s: &str) -> Option<Self> {
        if let Some((addr_str, prefix_str)) = s.rsplit_once('/') {
            let addr: IpAddr = addr_str.parse().ok()?;
            let prefix_len: u8 = prefix_str.parse().ok()?;
            let max_prefix = match addr {
                IpAddr::V4(_) => 32u8,
                IpAddr::V6(_) => 128u8,
            };
            if prefix_len <= max_prefix {
                Some(Self { addr, prefix_len })
            } else {
                None
            }
        } else {
            let addr: IpAddr = s.parse().ok()?;
            Some(Self::host(addr))
        }
    }

    /// Check if an address matches this CIDR.
    #[allow(clippy::missing_const_for_fn)]
    pub fn contains(&self, addr: IpAddr) -> bool {
        match (self.addr, addr) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = if self.prefix_len == 0 {
                    0u32
                } else {
                    u32::MAX << (32 - self.prefix_len)
                };
                let n = u32::from_be_bytes(net.octets());
                let a = u32::from_be_bytes(ip.octets());
                (n & mask) == (a & mask)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = if self.prefix_len == 0 {
                    0u128
                } else {
                    u128::MAX << (128 - self.prefix_len)
                };
                let n = u128::from_be_bytes(net.octets());
                let a = u128::from_be_bytes(ip.octets());
                (n & mask) == (a & mask)
            }
            _ => false,
        }
    }
}

/// An access control list.
#[derive(Debug)]
pub struct Acl {
    rules: Vec<AclRule>,
}

impl Acl {
    /// Create an empty ACL (denies all by default).
    pub const fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Add a rule.
    pub fn push(&mut self, rule: AclRule) {
        self.rules.push(rule);
    }

    /// Evaluate an address. First matching rule wins; default deny.
    pub fn evaluate(&self, addr: IpAddr) -> bool {
        for rule in &self.rules {
            match rule {
                AclRule::Allow(cidr) if cidr.contains(addr) => return true,
                AclRule::Deny(cidr) if cidr.contains(addr) => return false,
                _ => {}
            }
        }
        false
    }
}

impl Default for Acl {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_ip_match() {
        let cidr = IpCidr::host("127.0.0.1".parse().unwrap());
        assert!(cidr.contains("127.0.0.1".parse().unwrap()));
        assert!(!cidr.contains("127.0.0.2".parse().unwrap()));
    }

    #[test]
    fn cidr_24_match() {
        let cidr = IpCidr::parse("192.168.1.0/24").unwrap();
        assert!(cidr.contains("192.168.1.0".parse().unwrap()));
        assert!(cidr.contains("192.168.1.255".parse().unwrap()));
        assert!(!cidr.contains("192.168.2.0".parse().unwrap()));
    }

    #[test]
    fn cidr_parse() {
        let cidr = IpCidr::parse("10.0.0.1").unwrap();
        assert!(cidr.contains("10.0.0.1".parse().unwrap()));
        assert!(!cidr.contains("10.0.0.2".parse().unwrap()));
    }

    #[test]
    fn ipv6_cidr() {
        let cidr = IpCidr::parse("::1/128").unwrap();
        assert!(cidr.contains("::1".parse().unwrap()));
        assert!(!cidr.contains("::2".parse().unwrap()));
    }

    #[test]
    fn acl_allow_deny() {
        let mut acl = Acl::new();
        acl.push(AclRule::Deny(IpCidr::parse("192.168.1.128/25").unwrap()));
        acl.push(AclRule::Allow(IpCidr::parse("192.168.1.0/24").unwrap()));
        assert!(!acl.evaluate("192.168.1.200".parse().unwrap()));
        assert!(acl.evaluate("192.168.1.1".parse().unwrap()));
        assert!(!acl.evaluate("10.0.0.1".parse().unwrap()));
    }

    #[test]
    fn acl_default_deny() {
        let acl = Acl::new();
        assert!(!acl.evaluate("127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn acl_first_match_wins() {
        let mut acl = Acl::new();
        acl.push(AclRule::Deny(IpCidr::parse("10.0.0.0/8").unwrap()));
        acl.push(AclRule::Allow(IpCidr::parse("10.0.0.1/32").unwrap()));
        // Deny comes first, so 10.0.0.1 is denied
        assert!(!acl.evaluate("10.0.0.1".parse().unwrap()));
    }
}
