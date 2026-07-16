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
            let step = match unit {
                "day" => Some(Duration::days(n)),
                "hour" => Some(Duration::hours(n)),
                "minute" => Some(Duration::minutes(n)),
                "second" => Some(Duration::seconds(n)),
                _ => None,
            };
            if let Some(step) = step {
                total = total + step;
                matched = true;
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    matched.then(|| now + total)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reset_durations() {
        let now = Utc::now();
        let cases = [
            ("Resets in 3 hours 4 minutes", Duration::hours(3) + Duration::minutes(4)),
            ("Resets in 3 days 15 hours", Duration::days(3) + Duration::hours(15)),
            ("Resets in 23 days 3 hours", Duration::days(23) + Duration::hours(3)),
            ("Resets in 1 minute", Duration::minutes(1)),
            ("Resets in 2 hours.", Duration::hours(2)), // ollama copy ends with a period
        ];
        for (text, expected) in cases {
            assert_eq!(parse_resets_in(text, now), Some(now + expected), "{text}");
        }
        assert_eq!(parse_resets_in("no numbers here", now), None);
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
        assert_eq!(items[0].resets_text.as_deref(), Some("Resets in 3 hours 0 minutes"));
        assert_eq!(items[1].label, "Weekly Usage");
        assert_eq!(items[1].used_percent, 29.0);
        assert_eq!(items[2].label, "Monthly Usage");
        assert_eq!(items[2].used_percent, 25.0);
        assert_eq!(items[2].resets_text.as_deref(), Some("Resets in 23 days 3 hours"));
    }

    #[test]
    fn parse_usage_items_empty_on_login_page() {
        assert!(parse_usage_items("<html><body>Sign in</body></html>").is_empty());
    }
}
