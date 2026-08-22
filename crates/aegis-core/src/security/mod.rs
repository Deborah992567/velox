//! Security response headers.
//!
//! Phase 22: Automatic injection of security headers into responses.

pub mod nonce;
pub mod slowloris;
pub mod smuggling;

use crate::http::{HeaderName, Headers};

/// Configuration for security headers.
#[derive(Debug, Clone)]
pub struct SecurityHeaders {
    pub content_security_policy: Option<String>,
    pub strict_transport_security: Option<String>,
    pub x_content_type_options: Option<String>,
    pub x_frame_options: Option<String>,
    pub x_xss_protection: Option<String>,
    pub referrer_policy: Option<String>,
    pub permissions_policy: Option<String>,
    pub x_permitted_cross_domain: Option<String>,
}

impl SecurityHeaders {
    /// Production-recommended defaults.
    pub fn defaults() -> Self {
        Self {
            content_security_policy: Some("default-src 'self'".to_string()),
            strict_transport_security: Some("max-age=31536000; includeSubDomains".to_string()),
            x_content_type_options: Some("nosniff".to_string()),
            x_frame_options: Some("DENY".to_string()),
            x_xss_protection: Some("1; mode=block".to_string()),
            referrer_policy: Some("strict-origin-when-cross-origin".to_string()),
            permissions_policy: Some("camera=(), microphone=(), geolocation=()".to_string()),
            x_permitted_cross_domain: Some("none".to_string()),
        }
    }

    /// Minimal headers (no CSP, no HSTS).
    pub fn minimal() -> Self {
        Self {
            content_security_policy: None,
            strict_transport_security: None,
            x_content_type_options: Some("nosniff".to_string()),
            x_frame_options: Some("DENY".to_string()),
            x_xss_protection: None,
            referrer_policy: None,
            permissions_policy: None,
            x_permitted_cross_domain: None,
        }
    }

    /// Inject configured headers into a response header set.
    pub fn inject(&self, headers: &mut Headers) {
        if let Some(ref csp) = self.content_security_policy {
            headers.push_value(HeaderName::ContentSecurityPolicy, csp.as_str());
        }
        if let Some(ref hsts) = self.strict_transport_security {
            headers.push_value(HeaderName::StrictTransportSecurity, hsts.as_str());
        }
        if let Some(ref v) = self.x_content_type_options {
            headers.push_value(
                HeaderName::Custom("x-content-type-options".into()),
                v.as_str(),
            );
        }
        if let Some(ref v) = self.x_frame_options {
            headers.push_value(HeaderName::Custom("x-frame-options".into()), v.as_str());
        }
        if let Some(ref v) = self.x_xss_protection {
            headers.push_value(HeaderName::Custom("x-xss-protection".into()), v.as_str());
        }
        if let Some(ref v) = self.referrer_policy {
            headers.push_value(HeaderName::Custom("referrer-policy".into()), v.as_str());
        }
        if let Some(ref v) = self.permissions_policy {
            headers.push_value(HeaderName::Custom("permissions-policy".into()), v.as_str());
        }
        if let Some(ref v) = self.x_permitted_cross_domain {
            headers.push_value(
                HeaderName::Custom("x-permitted-cross-domain-policies".into()),
                v.as_str(),
            );
        }
    }

    /// Count of enabled headers.
    pub const fn enabled_count(&self) -> usize {
        let mut n = 0;
        if self.content_security_policy.is_some() {
            n += 1;
        }
        if self.strict_transport_security.is_some() {
            n += 1;
        }
        if self.x_content_type_options.is_some() {
            n += 1;
        }
        if self.x_frame_options.is_some() {
            n += 1;
        }
        if self.x_xss_protection.is_some() {
            n += 1;
        }
        if self.referrer_policy.is_some() {
            n += 1;
        }
        if self.permissions_policy.is_some() {
            n += 1;
        }
        if self.x_permitted_cross_domain.is_some() {
            n += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_has_8_headers() {
        let h = SecurityHeaders::defaults();
        assert_eq!(h.enabled_count(), 8);
    }

    #[test]
    fn minimal_has_2_headers() {
        let h = SecurityHeaders::minimal();
        assert_eq!(h.enabled_count(), 2);
    }

    #[test]
    fn inject_defaults() {
        let h = SecurityHeaders::defaults();
        let mut headers = Headers::new();
        h.inject(&mut headers);
        assert!(headers.contains(&HeaderName::ContentSecurityPolicy));
        assert!(headers.contains(&HeaderName::StrictTransportSecurity));
        assert!(
            headers
                .get(&HeaderName::Custom("x-content-type-options".into()))
                .is_some()
        );
        assert!(
            headers
                .get(&HeaderName::Custom("x-frame-options".into()))
                .is_some()
        );
    }

    #[test]
    fn inject_minimal_only_two() {
        let h = SecurityHeaders::minimal();
        let mut headers = Headers::new();
        h.inject(&mut headers);
        assert!(!headers.contains(&HeaderName::ContentSecurityPolicy));
        assert!(!headers.contains(&HeaderName::StrictTransportSecurity));
        assert!(
            headers
                .get(&HeaderName::Custom("x-content-type-options".into()))
                .is_some()
        );
        assert!(
            headers
                .get(&HeaderName::Custom("x-frame-options".into()))
                .is_some()
        );
    }

    #[test]
    fn inject_with_none_values() {
        let h = SecurityHeaders {
            content_security_policy: None,
            strict_transport_security: None,
            x_content_type_options: Some("nosniff".to_string()),
            x_frame_options: None,
            x_xss_protection: None,
            referrer_policy: None,
            permissions_policy: None,
            x_permitted_cross_domain: None,
        };
        let mut headers = Headers::new();
        h.inject(&mut headers);
        assert_eq!(headers.len(), 1);
        assert_eq!(h.enabled_count(), 1);
    }
}
