//! OpenCode Go quota. Official numbers come from the opencode.ai workspace
//! Go page (SSR HTML, session cookie in Keychain, parsed by quota_html);
//! falls back to a local estimate from opencode's message db against the
//! documented dollar limits ($12 / 5h, $30 / week, $60 / month).

use crate::agent_usage::{AgentIdentity, UsageWindow};
use crate::quota_html::{self, CookieFetchError, ParsedUsageWindow};
use chrono::{DateTime, Utc};
use std::path::PathBuf;

pub(crate) const COOKIE_KEYCHAIN_SERVICE: &str = "tokenbar-opencode-cookie";
const CONNECT_HINT: &str = "Sign in at opencode.ai, then store the browser Cookie header in Keychain item `tokenbar-opencode-cookie`.";
const AUTH_URL: &str = "https://opencode.ai/auth";

pub(crate) struct OpenCodeGoData {
    pub identity: Option<AgentIdentity>,
    pub windows: Vec<UsageWindow>,
    /// "opencode.ai" (official) or "local estimate" (fallback).
    pub source: String,
    pub error: Option<String>,
}

/// Map one parsed page item onto the UsageWindow shape the limits card renders.
fn window_from_parsed(item: &ParsedUsageWindow, now: DateTime<Utc>) -> UsageWindow {
    let (label, window_minutes) = match item.label.as_str() {
        "Rolling Usage" => ("5h".to_string(), Some(300)),
        "Weekly Usage" => ("Weekly".to_string(), Some(10080)),
        "Monthly Usage" => ("Monthly".to_string(), None), // billing-cycle length varies
        other => (other.to_string(), None),
    };
    let resets_at = item
        .resets_text
        .as_deref()
        .and_then(|t| quota_html::parse_resets_in(t, now));
    UsageWindow::from_used_percent(label, item.used_percent, resets_at, now, window_minutes)
}

/// Workspace id cache (survives restarts; discovery needs a network round-trip).
fn workspace_cache_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("TokenBar").join("opencode-workspace"))
}

async fn discover_workspace_id() -> Result<String, CookieFetchError> {
    if let Some(path) = workspace_cache_path() {
        if let Ok(cached) = std::fs::read_to_string(&path) {
            let cached = cached.trim().to_string();
            if !cached.is_empty() {
                return Ok(cached);
            }
        }
    }
    // Signed-in /auth redirects to /workspace/<id>; a login page means the
    // cookie is missing/expired.
    let page = quota_html::fetch_with_cookie(AUTH_URL, COOKIE_KEYCHAIN_SERVICE).await?;
    let id = page
        .final_url
        .split("/workspace/")
        .nth(1)
        .map(|rest| rest.split(['/', '?']).next().unwrap_or("").to_string())
        .filter(|id| !id.is_empty())
        .ok_or(CookieFetchError::Unauthorized)?;
    if let Some(path) = workspace_cache_path() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, &id);
    }
    Ok(id)
}

async fn fetch_official(now: DateTime<Utc>) -> Result<OpenCodeGoData, String> {
    let workspace = discover_workspace_id()
        .await
        .map_err(|e| e.message(CONNECT_HINT))?;
    let url = format!("https://opencode.ai/workspace/{workspace}/go");
    let page = quota_html::fetch_with_cookie(&url, COOKIE_KEYCHAIN_SERVICE)
        .await
        .map_err(|e| e.message(CONNECT_HINT))?;
    let items = quota_html::parse_usage_items(&page.body);
    if items.is_empty() {
        // Bounced to the login page, or the page markup changed.
        return Err(format!("No usage found on the Go page. {CONNECT_HINT}"));
    }
    Ok(OpenCodeGoData {
        identity: Some(AgentIdentity {
            email: None,
            plan: Some("Go".to_string()),
        }),
        windows: items.iter().map(|i| window_from_parsed(i, now)).collect(),
        source: "opencode.ai".to_string(),
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_page_items_to_windows() {
        let now = Utc::now();
        let item = ParsedUsageWindow {
            label: "Rolling Usage".to_string(),
            used_percent: 4.0,
            resets_text: Some("Resets in 3 hours 0 minutes".to_string()),
        };
        let window = window_from_parsed(&item, now);
        assert_eq!(window.label_for_test(), "5h");
        assert!((window.remaining_for_test() - 96.0).abs() < 0.01);
    }
}
