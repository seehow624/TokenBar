//! Ollama Cloud card. Identity + plan come from the local ollama server
//! (`POST /api/me`, which signs the upstream ollama.com call with the CLI
//! key). Usage windows are cookie-optional: with an ollama.com session
//! cookie in Keychain we parse the settings page ("Session usage … N% used …
//! Resets in …"); without one the card stays identity-only.

use crate::agent_usage::{clean_plan, AgentIdentity, UsageWindow};
use crate::quota_html;
use chrono::{DateTime, Utc};
use serde::Deserialize;

pub(crate) const COOKIE_KEYCHAIN_SERVICE: &str = "tokenbar-ollama-cookie";
const ME_URL: &str = "http://localhost:11434/api/me";
const SETTINGS_URL: &str = "https://ollama.com/settings";

pub(crate) struct OllamaData {
    pub identity: Option<AgentIdentity>,
    pub windows: Vec<UsageWindow>,
    pub source: String,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaMe {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    plan: Option<String>,
}

/// Gate: only show the card for machines that use ollama at all.
pub(crate) fn has_ollama() -> bool {
    std::env::var_os("HOME")
        .map(|home| std::path::PathBuf::from(home).join(".ollama").is_dir())
        .unwrap_or(false)
}

fn parse_settings_usage(html: &str, now: DateTime<Utc>) -> Vec<UsageWindow> {
    let text = quota_html::strip_tags(html);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let markers: [(&str, &str, Option<i64>); 2] = [
        ("Session usage", "5h", Some(300)), // Ollama session window is a rolling 5h
        ("Weekly usage", "Weekly", Some(10080)),
    ];
    let mut windows = Vec::new();
    for (marker, label, window_minutes) in markers {
        let Some(at) = text.find(marker) else { continue };
        let after = &text[at + marker.len()..];
        // Bound the segment at the next "usage" heading so one window's
        // numbers can't bleed into the next.
        let segment = match after.find(" usage") {
            Some(cut) => &after[..cut],
            None => after,
        };
        let Some(percent_at) = segment.find("% used") else { continue };
        let Some(used) = segment[..percent_at]
            .rsplit(' ')
            .next()
            .and_then(|n| n.trim().parse::<f64>().ok())
        else {
            continue;
        };
        let resets_at = segment.find("Resets in").and_then(|r| {
            let tail = &segment[r..];
            let sentence = tail.split('.').next().unwrap_or(tail);
            quota_html::parse_resets_in(sentence, now)
        });
        windows.push(UsageWindow::from_used_percent(
            label.to_string(),
            used,
            resets_at,
            now,
            window_minutes,
        ));
    }
    windows
}

async fn fetch_identity() -> Result<AgentIdentity, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build ollama client: {e}"))?;
    let response = client
        .post(ME_URL)
        .send()
        .await
        .map_err(|e| format!("Ollama not reachable at localhost:11434 ({e}). Start the Ollama app to see account info."))?;
    if !response.status().is_success() {
        return Err(format!("ollama /api/me returned {}. Run `ollama signin`.", response.status().as_u16()));
    }
    let me: OllamaMe = response
        .json()
        .await
        .map_err(|e| format!("decode ollama /api/me: {e}"))?;
    Ok(AgentIdentity {
        email: me.email.filter(|s| !s.trim().is_empty()),
        plan: me
            .plan
            .filter(|s| !s.trim().is_empty())
            .map(clean_plan)
            .or_else(|| me.name.map(clean_plan)),
    })
}

pub(crate) async fn fetch(now: DateTime<Utc>) -> OllamaData {
    let (identity, cookie_page) = tokio::join!(
        fetch_identity(),
        quota_html::fetch_with_cookie(SETTINGS_URL, COOKIE_KEYCHAIN_SERVICE)
    );
    let (windows, source) = match cookie_page {
        Ok(page) => (parse_settings_usage(&page.body, now), "ollama.com".to_string()),
        // Missing cookie (never connected) and failed cookie (expired / network)
        // both degrade to the identity-only card; distinguishing them in the UI
        // is deferred to the E2E pass (Task 11).
        Err(_) => (Vec::new(), "local".to_string()),
    };
    match identity {
        Ok(identity) => OllamaData {
            identity: Some(identity),
            windows,
            source,
            error: None,
        },
        Err(error) => OllamaData {
            identity: None,
            windows,
            source,
            error: Some(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_from_settings_text() {
        let now = Utc::now();
        let html = r#"<h2>Cloud usage</h2><span>Free</span>
            <div><span>Session usage</span><span>12% used</span></div>
            <div>bar</div><span>Resets in 2 hours.</span>
            <div><span>Weekly usage</span><span>3% used</span></div>
            <div>bar</div><span>Resets in 3 days.</span>
            <label>Notify me when I'm close to hitting my usage limits</label>"#;
        let windows = parse_settings_usage(html, now);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label_for_test(), "5h");
        assert!((windows[0].remaining_for_test() - 88.0).abs() < 0.01);
        assert_eq!(windows[1].label_for_test(), "Weekly");
        assert!((windows[1].remaining_for_test() - 97.0).abs() < 0.01);
    }

    #[test]
    fn settings_parse_empty_when_signed_out() {
        assert!(parse_settings_usage("<html>Sign in</html>", Utc::now()).is_empty());
    }

    /// Real captured ollama.com/settings markup (de-identified, free-tier,
    /// both windows at 0%). Guards against a markup change silently breaking
    /// the text-anchor parser — the synthetic fixture above can't catch that.
    #[test]
    fn parses_usage_from_real_settings_fixture() {
        let now = Utc::now();
        let html = include_str!(
            "../../../docs/superpowers/specs/fixtures/ollama-settings-usage-fragment.html"
        );
        let windows = parse_settings_usage(html, now);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label_for_test(), "5h");
        assert!((windows[0].remaining_for_test() - 100.0).abs() < 0.01);
        assert!(windows[0].reset_text_for_test().is_some());
        assert_eq!(windows[1].label_for_test(), "Weekly");
        assert!((windows[1].remaining_for_test() - 100.0).abs() < 0.01);
        assert!(windows[1].reset_text_for_test().is_some());
    }
}
