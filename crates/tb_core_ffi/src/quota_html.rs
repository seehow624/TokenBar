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
}
