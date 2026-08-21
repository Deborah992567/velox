//! Error page templates for common HTTP error codes.
//!
//! Generates HTML error pages with optional custom branding.

/// Configuration for error page rendering.
#[derive(Debug, Clone)]
pub struct ErrorPageConfig {
    pub server_name: String,
    pub show_server_version: bool,
    pub custom_css: Option<String>,
}

impl Default for ErrorPageConfig {
    fn default() -> Self {
        Self {
            server_name: "Velox".into(),
            show_server_version: true,
            custom_css: None,
        }
    }
}

/// Generate an HTML error page.
pub fn render_error_page(config: &ErrorPageConfig, status: u16, detail: Option<&str>) -> String {
    let reason = reason_phrase(status);
    let detail_section = detail
        .map(|d| format!("<p class=\"detail\">{d}</p>"))
        .unwrap_or_default();
    let version_line = if config.show_server_version {
        format!("<p class=\"version\">{}</p>", env!("CARGO_PKG_VERSION"))
    } else {
        String::new()
    };
    let style = config.custom_css.as_deref().unwrap_or(DEFAULT_CSS);

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{status} {reason}</title>
<style>{style}</style>
</head>
<body>
<div class="container">
<h1>{status}</h1>
<h2>{reason}</h2>
{detail_section}
{version_line}
</div>
</body>
</html>"#,
    )
}

const fn reason_phrase(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Content Too Large",
        414 => "URI Too Long",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

const DEFAULT_CSS: &str = "
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: -apple-system, sans-serif; display: flex; justify-content: center; align-items: center; min-height: 100vh; background: #f5f5f5; color: #333; }
.container { text-align: center; padding: 2rem; }
h1 { font-size: 4rem; font-weight: 700; color: #e74c3c; }
h2 { font-size: 1.5rem; margin: 0.5rem 0; color: #666; }
.detail { margin-top: 1rem; color: #888; }
.version { margin-top: 2rem; font-size: 0.8rem; color: #aaa; }
";

/// Well-known error templates.
pub fn error_400(config: &ErrorPageConfig) -> String {
    render_error_page(config, 400, Some("The request was malformed or invalid."))
}

pub fn error_401(config: &ErrorPageConfig) -> String {
    render_error_page(
        config,
        401,
        Some("Authentication is required to access this resource."),
    )
}

pub fn error_403(config: &ErrorPageConfig) -> String {
    render_error_page(
        config,
        403,
        Some("You do not have permission to access this resource."),
    )
}

pub fn error_404(config: &ErrorPageConfig) -> String {
    render_error_page(config, 404, None)
}

pub fn error_500(config: &ErrorPageConfig) -> String {
    render_error_page(config, 500, None)
}

pub fn error_502(config: &ErrorPageConfig) -> String {
    render_error_page(config, 502, None)
}

pub fn error_503(config: &ErrorPageConfig) -> String {
    render_error_page(config, 503, Some("The service is temporarily unavailable."))
}

pub fn error_504(config: &ErrorPageConfig) -> String {
    render_error_page(config, 504, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_page_contains_status() {
        let config = ErrorPageConfig::default();
        let page = render_error_page(&config, 404, None);
        assert!(page.contains("<h1>404</h1>"));
        assert!(page.contains("Not Found"));
    }

    #[test]
    fn error_page_with_detail() {
        let config = ErrorPageConfig::default();
        let page = render_error_page(&config, 400, Some("bad stuff"));
        assert!(page.contains("bad stuff"));
    }

    #[test]
    fn error_page_is_valid_html() {
        let config = ErrorPageConfig::default();
        let page = render_error_page(&config, 500, None);
        assert!(page.contains("<!DOCTYPE html>"));
        assert!(page.contains("</html>"));
    }

    #[test]
    fn version_can_be_hidden() {
        let config = ErrorPageConfig {
            show_server_version: false,
            ..Default::default()
        };
        let page = render_error_page(&config, 404, None);
        assert!(!page.contains("class=\"version\""));
    }

    #[test]
    fn custom_css() {
        let config = ErrorPageConfig {
            custom_css: Some("body { background: red; }".into()),
            ..Default::default()
        };
        let page = render_error_page(&config, 404, None);
        assert!(page.contains("background: red"));
        assert!(!page.contains("#f5f5f5"));
    }

    #[test]
    fn convenience_functions() {
        let config = ErrorPageConfig::default();
        assert!(error_400(&config).contains("400"));
        assert!(error_401(&config).contains("401"));
        assert!(error_403(&config).contains("403"));
        assert!(error_404(&config).contains("404"));
        assert!(error_500(&config).contains("500"));
        assert!(error_502(&config).contains("502"));
        assert!(error_503(&config).contains("503"));
        assert!(error_504(&config).contains("504"));
    }

    #[test]
    fn reason_phrases_known_codes() {
        assert_eq!(reason_phrase(400), "Bad Request");
        assert_eq!(reason_phrase(404), "Not Found");
        assert_eq!(reason_phrase(500), "Internal Server Error");
        assert_eq!(reason_phrase(999), "Error");
    }
}
