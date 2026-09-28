//! Ollama Cloud card. Identity + plan come from the local ollama server
//! (`POST /api/me`, which signs the upstream ollama.com call with the CLI
//! key). Usage windows are cookie-optional: with an ollama.com session
//! cookie in Keychain we parse the settings page (older "Session/Weekly usage"
//! percentages or the current "Monthly usage $X of $Y" allowance); without
//! one the card stays identity-only.

use crate::agent_usage::{clean_plan, AgentIdentity, UsageWindow};
use crate::quota_html::{self, CookieFetchError};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::sync::Mutex;

pub(crate) const COOKIE_KEYCHAIN_SERVICE: &str = "tokenbar-ollama-cookie";
const ME_URL: &str = "http://localhost:11434/api/me";
const CLOUD_ME_URL: &str = "https://ollama.com/api/me";
const SETTINGS_URL: &str = "https://ollama.com/settings";
const COOKIE_CONNECT_HINT: &str =
    "Store the ollama.com Cookie header in Keychain item `tokenbar-ollama-cookie`.";

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

/// The settings page is the authoritative source for the percentage, but a
/// transient network failure must not make a working card flash to "No quota".
/// This cache is process-local and is cleared when the cookie is absent or
/// rejected, so it cannot outlive an explicit sign-out.
static LAST_GOOD_WINDOWS: Mutex<Option<Vec<UsageWindow>>> = Mutex::new(None);

#[cfg(test)]
static CACHE_TEST_LOCK: Mutex<()> = Mutex::new(());

fn cached_windows() -> Vec<UsageWindow> {
    LAST_GOOD_WINDOWS
        .lock()
        .ok()
        .and_then(|cached| cached.clone())
        .unwrap_or_default()
}

fn remember_windows(windows: &[UsageWindow]) {
    if let Ok(mut cached) = LAST_GOOD_WINDOWS.lock() {
        *cached = Some(windows.to_vec());
    }
}

fn clear_cached_windows() {
    if let Ok(mut cached) = LAST_GOOD_WINDOWS.lock() {
        *cached = None;
    }
}

fn non_empty_env(name: &str) -> bool {
    std::env::var(name)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

fn auth_mentions_ollama(raw: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| {
            value.as_object().map(|entries| {
                entries
                    .keys()
                    .any(|key| key.to_ascii_lowercase().contains("ollama"))
            })
        })
        .unwrap_or(false)
}

/// Pure gate used by `has_ollama`; config text is intentionally only searched
/// for the provider name, never logged or surfaced to the UI.
fn has_ollama_from_sources(
    local_dir: bool,
    api_key: Option<&str>,
    auth_json: Option<&str>,
    config_texts: &[&str],
) -> bool {
    local_dir
        || api_key.map(|key| !key.trim().is_empty()).unwrap_or(false)
        || auth_json.map(auth_mentions_ollama).unwrap_or(false)
        || config_texts
            .iter()
            .any(|text| text.to_ascii_lowercase().contains("ollama"))
}

/// Show the card for a local Ollama install as well as cloud-only OpenCode /
/// environment configurations. The old directory-only gate hid cloud users
/// who never installed the local Ollama daemon.
pub(crate) fn has_ollama() -> bool {
    // An explicit cookie is also a strong opt-in signal. This covers a
    // cloud-only setup that has no local daemon, API-key environment variable,
    // or OpenCode config entry.
    let cookie_configured = quota_html::keychain_secret(COOKIE_KEYCHAIN_SERVICE).is_some();
    let Some(home) = std::env::var_os("HOME") else {
        return non_empty_env("OLLAMA_API_KEY") || cookie_configured;
    };
    let home = std::path::PathBuf::from(home);
    let auth_path = home.join(".local/share/opencode/auth.json");
    let config_paths = [
        home.join(".config/opencode/opencode.json"),
        home.join(".config/opencode/opencode.jsonc"),
    ];
    let auth = std::fs::read_to_string(auth_path).ok();
    let configs: Vec<String> = config_paths
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .collect();
    let config_refs: Vec<&str> = configs.iter().map(String::as_str).collect();
    has_ollama_from_sources(
        home.join(".ollama").is_dir(),
        std::env::var("OLLAMA_API_KEY").ok().as_deref(),
        auth.as_deref(),
        &config_refs,
    ) || cookie_configured
}

fn reset_after(text: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let reset_at = text.find("Resets in")?;
    let tail = &text[reset_at..];
    let sentence = tail.split('.').next().unwrap_or(tail);
    quota_html::parse_resets_in(sentence, now)
}

fn parse_dollars(value: &str) -> Option<f64> {
    let value = value.trim().trim_start_matches('$').replace(',', "");
    let value = value.parse::<f64>().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn parse_percent_window(
    text: &str,
    marker: &str,
    label: &str,
    now: DateTime<Utc>,
    window_minutes: Option<i64>,
) -> Option<UsageWindow> {
    let mut search_from = 0;
    while let Some(relative_at) = text[search_from..].find(marker) {
        let at = search_from + relative_at;
        let after = &text[at + marker.len()..];
        // Bound the segment at the next "usage" heading so one window's
        // numbers can't bleed into the next. Some pages mention "Free usage"
        // in explanatory copy before the actual meter, so keep searching when
        // the first occurrence has no percentage.
        let segment = match after.find(" usage") {
            Some(cut) => &after[..cut],
            None => after,
        };
        if let Some(percent_at) = segment.find("% used") {
            if let Some(used) = segment[..percent_at]
                .rsplit(' ')
                .next()
                .and_then(|n| n.trim().parse::<f64>().ok())
                .filter(|value| value.is_finite())
            {
                return Some(UsageWindow::from_used_percent(
                    label.to_string(),
                    used,
                    reset_after(segment, now),
                    now,
                    window_minutes,
                ));
            }
        }
        search_from = at + marker.len();
    }
    None
}

fn parse_settings_usage(html: &str, now: DateTime<Utc>) -> Vec<UsageWindow> {
    let text = quota_html::strip_tags(html);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let markers: [(&str, &str, Option<i64>); 3] = [
        ("Session usage", "5h", Some(300)), // Ollama session window is a rolling 5h
        ("Weekly usage", "Weekly", Some(10080)),
        ("Free usage", "Free", None),
    ];
    let mut windows = Vec::new();
    for (marker, label, window_minutes) in markers {
        if let Some(window) = parse_percent_window(&text, marker, label, now, window_minutes) {
            windows.push(window);
        }
    }

    // Ollama's current paid settings page exposes a monthly dollar allowance
    // rather than the older session/weekly percentages: "$2.85 of $60 used".
    // Keep accepting both shapes because accounts and page deployments differ.
    if let Some(at) = text.find("Monthly usage") {
        let after = &text[at + "Monthly usage".len()..];
        if let Some((spent, rest)) = after.split_once(" of ") {
            let limit = rest.split_whitespace().next().and_then(parse_dollars);
            if let (Some(spent), Some(limit)) = (parse_dollars(spent), limit) {
                if limit > 0.0 {
                    windows.push(UsageWindow::from_used_percent(
                        "Monthly".to_string(),
                        spent / limit * 100.0,
                        reset_after(after, now),
                        now,
                        None,
                    ));
                }
            }
        }
    }
    windows
}

fn settings_page_is_authenticated(final_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(final_url) else {
        return false;
    };
    matches!(url.host_str(), Some("ollama.com" | "www.ollama.com"))
        && url.path().trim_end_matches('/') == "/settings"
}

fn settings_page_is_signed_out(body: &str) -> bool {
    let text = quota_html::strip_tags(body).to_ascii_lowercase();
    (text.contains("sign in") || text.contains("log in")) && !text.contains("session usage")
}

/// Turn a cookie-page result into a displayable snapshot. Missing or rejected
/// credentials clear the old value; transport and markup failures retain it
/// with an error so the user sees stale-but-honest data instead of a false
/// zero/quota absence.
fn usage_from_cookie_page(
    page: Result<quota_html::FetchedPage, CookieFetchError>,
    now: DateTime<Utc>,
) -> (Vec<UsageWindow>, String, Option<String>) {
    match page {
        Err(CookieFetchError::MissingCookie) => {
            clear_cached_windows();
            (Vec::new(), "local".to_string(), None)
        }
        Err(CookieFetchError::Unauthorized) => {
            clear_cached_windows();
            (
                Vec::new(),
                "local".to_string(),
                Some(CookieFetchError::Unauthorized.message(COOKIE_CONNECT_HINT)),
            )
        }
        Err(CookieFetchError::Http(detail)) => {
            let cached = cached_windows();
            (
                cached,
                "ollama.com".to_string(),
                Some(format!("Ollama usage refresh failed: {detail}")),
            )
        }
        Ok(page) if !settings_page_is_authenticated(&page.final_url) => {
            clear_cached_windows();
            (
                Vec::new(),
                "local".to_string(),
                Some("Ollama settings session expired. ".to_string() + COOKIE_CONNECT_HINT),
            )
        }
        Ok(page) if settings_page_is_signed_out(&page.body) => {
            clear_cached_windows();
            (
                Vec::new(),
                "local".to_string(),
                Some("Ollama settings session expired. ".to_string() + COOKIE_CONNECT_HINT),
            )
        }
        Ok(page) => {
            let parsed = parse_settings_usage(&page.body, now);
            if parsed.is_empty() {
                let cached = cached_windows();
                let message = if cached.is_empty() {
                    "Could not parse Ollama usage; usage is unavailable."
                } else {
                    "Could not parse Ollama usage; showing the last good reading."
                };
                return (cached, "ollama.com".to_string(), Some(message.to_string()));
            }
            remember_windows(&parsed);
            (parsed, "ollama.com".to_string(), None)
        }
    }
}

fn api_key_from_auth(raw: &str) -> Option<String> {
    let root = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    for provider in ["ollama-cloud", "ollama"] {
        let Some(entry) = root.get(provider).and_then(serde_json::Value::as_object) else {
            continue;
        };
        for field in ["key", "apiKey", "api_key", "access"] {
            let Some(key) = entry.get(field).and_then(serde_json::Value::as_str) else {
                continue;
            };
            if !key.trim().is_empty() {
                return Some(key.trim().to_string());
            }
        }
    }
    None
}

fn ollama_api_key() -> Option<String> {
    if let Ok(key) = std::env::var("OLLAMA_API_KEY") {
        if !key.trim().is_empty() {
            return Some(key.trim().to_string());
        }
    }
    let home = std::env::var_os("HOME")?;
    let path = std::path::PathBuf::from(home).join(".local/share/opencode/auth.json");
    api_key_from_auth(&std::fs::read_to_string(path).ok()?)
}

async fn fetch_identity_from(
    client: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    source: &str,
) -> Result<AgentIdentity, String> {
    let mut request = client.post(url);
    if let Some(api_key) = api_key {
        request = request.bearer_auth(api_key);
    }
    let response = request.send().await.map_err(|e| {
        format!("Ollama {source} identity request failed ({e}). Start Ollama or check its API key.")
    })?;
    if !response.status().is_success() {
        let hint = if api_key.is_some() {
            "Check OLLAMA_API_KEY or the OpenCode Ollama credential."
        } else {
            "Run `ollama signin`."
        };
        return Err(format!(
            "Ollama {source} /api/me returned {}. {hint}",
            response.status().as_u16()
        ));
    }
    let me: OllamaMe = response
        .json()
        .await
        .map_err(|e| format!("decode Ollama {source} /api/me: {e}"))?;
    Ok(AgentIdentity {
        email: me.email.filter(|s| !s.trim().is_empty()),
        plan: me
            .plan
            .filter(|s| !s.trim().is_empty())
            .map(clean_plan)
            .or_else(|| me.name.map(clean_plan)),
    })
}

async fn fetch_identity() -> Result<AgentIdentity, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build ollama client: {e}"))?;

    if let Some(api_key) = ollama_api_key() {
        match fetch_identity_from(&client, CLOUD_ME_URL, Some(&api_key), "Cloud").await {
            Ok(identity) => return Ok(identity),
            Err(cloud_error) => {
                // A machine may have both a direct Cloud key and a locally
                // signed-in daemon. Prefer the daemon if the direct key is
                // stale, but preserve both actionable errors if neither works.
                match fetch_identity_from(&client, ME_URL, None, "local").await {
                    Ok(identity) => return Ok(identity),
                    Err(local_error) => return Err(format!("{cloud_error} {local_error}")),
                }
            }
        }
    }
    fetch_identity_from(&client, ME_URL, None, "local").await
}

pub(crate) async fn fetch(now: DateTime<Utc>) -> OllamaData {
    let (identity, cookie_page) = tokio::join!(
        fetch_identity(),
        quota_html::fetch_with_cookie(SETTINGS_URL, COOKIE_KEYCHAIN_SERVICE)
    );
    let (windows, source, usage_error) = usage_from_cookie_page(cookie_page, now);

    // A cloud-only setup can have valid web usage while no local daemon is
    // running. In that case the usage card is still useful; only report the
    // identity error when there is no usage signal at all.
    match identity {
        Ok(identity) => OllamaData {
            identity: Some(identity),
            windows,
            source,
            error: usage_error,
        },
        Err(identity_error) => OllamaData {
            identity: None,
            windows: windows.clone(),
            source,
            error: if windows.is_empty() {
                usage_error.or(Some(identity_error))
            } else {
                usage_error
            },
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
    fn parses_current_free_usage_after_explanatory_copy() {
        let now = Utc::now();
        let html = r#"<p>Free usage can be used with included usage.</p>
            <span>Free usage</span><span>100% used</span>
            <div>Resets in 4 weeks.</div>"#;
        let windows = parse_settings_usage(html, now);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label_for_test(), "Free");
        assert!((windows[0].remaining_for_test()).abs() < 0.01);
        assert!(windows[0].reset_text_for_test().is_some());
    }

    #[test]
    fn parses_current_monthly_dollar_usage() {
        let now = Utc::now();
        let html = r#"<h2>Included usage</h2><span>Monthly usage</span><span>$2.85 of $60 used</span><div>Resets in 4 weeks.</div>"#;
        let windows = parse_settings_usage(html, now);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label_for_test(), "Monthly");
        assert!((windows[0].remaining_for_test() - 95.25).abs() < 0.01);
        assert!(windows[0].reset_text_for_test().is_some());
    }

    #[test]
    fn parses_current_settings_fixture() {
        let now = Utc::now();
        let html = include_str!(
            "../../../docs/superpowers/specs/fixtures/ollama-settings-current-usage-fragment.html"
        );
        let windows = parse_settings_usage(html, now);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label_for_test(), "Free");
        assert!((windows[0].remaining_for_test()).abs() < 0.01);
        assert!(windows[0].reset_text_for_test().is_some());
    }

    #[test]
    fn settings_parse_empty_when_signed_out() {
        assert!(parse_settings_usage("<html>Sign in</html>", Utc::now()).is_empty());
    }

    #[test]
    fn settings_parse_rejects_non_finite_percentages() {
        let html = r#"
            <div>Session usage NaN% used</div>
            <div>Weekly usage inf% used</div>
        "#;
        assert!(parse_settings_usage(html, Utc::now()).is_empty());
    }

    #[test]
    fn cloud_api_key_reads_supported_auth_fields_without_accepting_empty_values() {
        assert_eq!(
            api_key_from_auth(r#"{"ollama-cloud":{"type":"api","key":"secret"}}"#).as_deref(),
            Some("secret")
        );
        assert_eq!(api_key_from_auth(r#"{"ollama":{"apiKey":"  "}}"#), None);
        assert_eq!(api_key_from_auth("not json"), None);
    }

    #[test]
    fn cloud_source_signals_include_api_auth_and_config() {
        assert!(has_ollama_from_sources(true, None, None, &[]));
        assert!(has_ollama_from_sources(false, Some("key"), None, &[]));
        assert!(has_ollama_from_sources(
            false,
            None,
            Some(r#"{"ollama-cloud":{"type":"api"}}"#),
            &[]
        ));
        assert!(has_ollama_from_sources(
            false,
            None,
            None,
            &[r#"{"provider":{"ollama-cloud":{}}}"#]
        ));
        assert!(!has_ollama_from_sources(false, None, None, &["openai"]));
    }

    #[test]
    fn settings_url_must_remain_on_authenticated_settings_page() {
        assert!(settings_page_is_authenticated(
            "https://ollama.com/settings"
        ));
        assert!(settings_page_is_authenticated(
            "https://www.ollama.com/settings/"
        ));
        assert!(!settings_page_is_authenticated("https://ollama.com/login"));
        assert!(!settings_page_is_authenticated(
            "https://example.com/settings"
        ));
        assert!(settings_page_is_signed_out("<html>Sign in</html>"));
        assert!(!settings_page_is_signed_out("Session usage 12% used"));
    }

    #[test]
    fn rejected_cookie_clears_last_good_windows() {
        let _test_lock = CACHE_TEST_LOCK.lock().unwrap();
        clear_cached_windows();
        let now = Utc::now();
        let good = parse_settings_usage(
            "<div>Session usage 12% used</div><div>Weekly usage 3% used</div>",
            now,
        );
        remember_windows(&good);
        let (windows, _, error) = usage_from_cookie_page(Err(CookieFetchError::Unauthorized), now);
        assert!(windows.is_empty());
        assert!(error.is_some());
        assert!(cached_windows().is_empty());
    }

    #[test]
    fn changed_page_retains_last_good_windows_with_an_error() {
        let _test_lock = CACHE_TEST_LOCK.lock().unwrap();
        clear_cached_windows();
        let now = Utc::now();
        let good = parse_settings_usage(
            "<div>Session usage 12% used</div><div>Weekly usage 3% used</div>",
            now,
        );
        remember_windows(&good);
        let (windows, _, error) = usage_from_cookie_page(
            Ok(quota_html::FetchedPage {
                body: "<html>new markup</html>".to_string(),
                final_url: "https://ollama.com/settings".to_string(),
            }),
            now,
        );
        assert_eq!(windows.len(), 2);
        assert!(error.is_some());
        clear_cached_windows();
    }

    #[test]
    fn transient_cookie_failure_retains_last_good_windows() {
        let _test_lock = CACHE_TEST_LOCK.lock().unwrap();
        clear_cached_windows();
        let now = Utc::now();
        let good = parse_settings_usage(
            "<div>Session usage 12% used</div><div>Weekly usage 3% used</div>",
            now,
        );
        remember_windows(&good);
        let (windows, source, error) =
            usage_from_cookie_page(Err(CookieFetchError::Http("timeout".to_string())), now);
        assert_eq!(windows.len(), 2);
        assert_eq!(source, "ollama.com");
        assert!(error.is_some());
        clear_cached_windows();
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
