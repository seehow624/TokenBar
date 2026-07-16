//! Shared helpers for quota providers that read a cookie-authenticated web
//! page (OpenCode Go workspace page, Ollama settings): Keychain cookie
//! lookup, page fetch, and the tiny text/HTML parsers they share.

use chrono::{DateTime, Duration, Utc};

/// Parse "Resets in 3 hours 4 minutes" / "23 days 3 hours" / "1 minute"
/// into an absolute reset instant relative to `now`.
pub(crate) fn parse_resets_in(text: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut total = Duration::zero();
    let mut matched = false;
    let mut i = 0;
    while i + 1 < tokens.len() {
        if let Ok(n) = tokens[i].parse::<i64>() {
            let unit = tokens[i + 1]
                .trim_end_matches(['.', ','])
                .trim_end_matches('s');
            // Checked constructors: out-of-range magnitudes degrade to
            // "token doesn't match" instead of panicking — this text comes
            // from a live third-party page and we sit behind an FFI boundary.
            let step = match unit {
                "day" => Duration::try_days(n),
                "hour" => Duration::try_hours(n),
                "minute" => Duration::try_minutes(n),
                "second" => Duration::try_seconds(n),
                _ => None,
            };
            if let Some(step) = step {
                let Some(next) = total.checked_add(&step) else {
                    i += 2;
                    continue;
                };
                total = next;
                matched = true;
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    matched.then(|| now.checked_add_signed(total)).flatten()
}

#[derive(Debug, PartialEq)]
pub(crate) struct ParsedUsageWindow {
    pub label: String,
    pub used_percent: f64,
    pub resets_text: Option<String>,
}

/// Parse the SolidStart SSR `data-slot="usage-item"` blocks of the OpenCode
/// workspace Go page. SSR interleaves `<!--$-->` hydration markers inside the
/// text nodes, so strip those before scanning.
pub(crate) fn parse_usage_items(html: &str) -> Vec<ParsedUsageWindow> {
    const ITEM: &str = "data-slot=\"usage-item\"";
    let clean = html
        .replace("<!--$-->", "")
        .replace("<!--/-->", "")
        .replace("<!--$!-->", "");
    let starts: Vec<usize> = clean.match_indices(ITEM).map(|(i, _)| i).collect();
    let mut out = Vec::new();
    for (n, &start) in starts.iter().enumerate() {
        let end = starts.get(n + 1).copied().unwrap_or(clean.len());
        let block = &clean[start..end];
        let Some(label) = extract_between(block, "data-slot=\"usage-label\">", "<") else {
            continue;
        };
        let Some(value) = extract_between(block, "data-slot=\"usage-value\">", "<") else {
            continue;
        };
        let Ok(used_percent) = value.trim().trim_end_matches('%').trim().parse::<f64>() else {
            continue;
        };
        let resets_text = extract_between(block, "data-slot=\"reset-time\">", "<")
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "));
        out.push(ParsedUsageWindow {
            label: label.trim().to_string(),
            used_percent,
            resets_text,
        });
    }
    out
}

fn extract_between<'a>(haystack: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = haystack.find(start)? + start.len();
    let rest = &haystack[from..];
    Some(&rest[..rest.find(end)?])
}

/// A fetched page plus the URL the request finally landed on (after
/// redirects) — the opencode workspace id is discovered from that URL.
pub(crate) struct FetchedPage {
    pub body: String,
    pub final_url: String,
}

#[derive(Debug)]
pub(crate) enum CookieFetchError {
    /// No Keychain item — the user hasn't connected this provider.
    MissingCookie,
    /// Cookie present but rejected (or bounced to a login page).
    Unauthorized,
    Http(String),
}

impl CookieFetchError {
    pub(crate) fn message(&self, connect_hint: &str) -> String {
        match self {
            Self::MissingCookie => format!("Not connected. {connect_hint}"),
            Self::Unauthorized => format!("Cookie expired. {connect_hint}"),
            Self::Http(detail) => detail.clone(),
        }
    }
}

/// Generic-password lookup (same pattern as the Claude raw-token item).
#[cfg(target_os = "macos")]
pub(crate) fn keychain_secret(service: &str) -> Option<String> {
    let output = std::process::Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", service, "-w"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let raw = raw.trim();
    (!raw.is_empty()).then(|| raw.to_string())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn keychain_secret(_service: &str) -> Option<String> {
    None
}

pub(crate) async fn fetch_with_cookie(
    url: &str,
    keychain_service: &str,
) -> Result<FetchedPage, CookieFetchError> {
    let cookie = keychain_secret(keychain_service).ok_or(CookieFetchError::MissingCookie)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| CookieFetchError::Http(format!("build client: {e}")))?;
    let response = client
        .get(url)
        .header(reqwest::header::COOKIE, cookie)
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) TokenBar",
        )
        .send()
        .await
        .map_err(|e| CookieFetchError::Http(format!("request failed: {e}")))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(CookieFetchError::Unauthorized);
    }
    if !status.is_success() {
        return Err(CookieFetchError::Http(format!("HTTP {}", status.as_u16())));
    }
    let final_url = response.url().to_string();
    let body = response
        .text()
        .await
        .map_err(|e| CookieFetchError::Http(format!("read body: {e}")))?;
    Ok(FetchedPage { body, final_url })
}

/// Strip tags to visible text (for pages without stable attribute anchors,
/// i.e. the Ollama settings page).
pub(crate) fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reset_durations() {
        let now = Utc::now();
        let cases = [
            (
                "Resets in 3 hours 4 minutes",
                Duration::hours(3) + Duration::minutes(4),
            ),
            (
                "Resets in 3 days 15 hours",
                Duration::days(3) + Duration::hours(15),
            ),
            (
                "Resets in 23 days 3 hours",
                Duration::days(23) + Duration::hours(3),
            ),
            ("Resets in 1 minute", Duration::minutes(1)),
            ("Resets in 2 hours.", Duration::hours(2)), // ollama copy ends with a period
        ];
        for (text, expected) in cases {
            assert_eq!(parse_resets_in(text, now), Some(now + expected), "{text}");
        }
        assert_eq!(parse_resets_in("no numbers here", now), None);
    }

    #[test]
    fn reset_parse_survives_out_of_range_magnitudes() {
        let now = Utc::now();
        // Absurd magnitudes must degrade (skip/None), never panic.
        assert_eq!(
            parse_resets_in("Resets in 9223372036854775807 days", now),
            None
        );
        let mixed = parse_resets_in("Resets in 9223372036854775807 days 2 hours", now);
        assert_eq!(mixed, Some(now + Duration::hours(2)));
    }

    #[test]
    fn parses_opencode_usage_items_from_fixture() {
        let html = include_str!(
            "../../../docs/superpowers/specs/fixtures/opencode-go-usage-fragment.html"
        );
        let items = parse_usage_items(html);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].label, "Rolling Usage");
        assert_eq!(items[0].used_percent, 4.0);
        assert_eq!(
            items[0].resets_text.as_deref(),
            Some("Resets in 3 hours 0 minutes")
        );
        assert_eq!(items[1].label, "Weekly Usage");
        assert_eq!(items[1].used_percent, 29.0);
        assert_eq!(items[2].label, "Monthly Usage");
        assert_eq!(items[2].used_percent, 25.0);
        assert_eq!(
            items[2].resets_text.as_deref(),
            Some("Resets in 23 days 3 hours")
        );
    }

    #[test]
    fn parse_usage_items_empty_on_login_page() {
        assert!(parse_usage_items("<html><body>Sign in</body></html>").is_empty());
    }

    #[test]
    fn parse_usage_items_accepts_decimal_percent() {
        let html = r#"
            <div data-slot="usage-item">
              <span data-slot="usage-label">Rolling Usage</span>
              <span data-slot="usage-value">4.5%</span>
            </div>
        "#;
        let items = parse_usage_items(html);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].used_percent, 4.5);
        assert_eq!(items[0].resets_text, None);
    }

    #[test]
    fn strip_tags_extracts_visible_text() {
        assert_eq!(
            strip_tags("<div><span>Session usage</span><b>12% used</b></div>")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            "Session usage 12% used"
        );
    }
}
