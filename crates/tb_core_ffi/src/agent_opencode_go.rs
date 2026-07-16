//! OpenCode Go quota. Official numbers come from the opencode.ai workspace
//! Go page (SSR HTML, session cookie in Keychain, parsed by quota_html);
//! falls back to a local estimate from opencode's message db against the
//! documented dollar limits ($12 / 5h, $30 / week, $60 / month).

use crate::agent_usage::{AgentIdentity, UsageWindow};
use crate::quota_html::{self, CookieFetchError, ParsedUsageWindow};
use chrono::{DateTime, Duration, Utc};
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

/// Documented Go limits: (label, lookback minutes, dollar limit, window_minutes).
const GO_LIMITS: &[(&str, i64, f64, Option<i64>)] = &[
    ("5h", 5 * 60, 12.0, Some(300)),
    ("Weekly", 7 * 24 * 60, 30.0, Some(10080)),
    ("Monthly", 30 * 24 * 60, 60.0, None),
];

fn opencode_db_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/share/opencode/opencode.db"))
}

fn local_estimate_windows(db: &rusqlite::Connection, now: DateTime<Utc>) -> Vec<UsageWindow> {
    GO_LIMITS
        .iter()
        .map(|(label, lookback_minutes, dollar_limit, window_minutes)| {
            let since_ms = (now - Duration::minutes(*lookback_minutes)).timestamp_millis();
            let spent: f64 = db
                .query_row(
                    "SELECT COALESCE(SUM(CAST(json_extract(data,'$.cost') AS REAL)), 0)
                     FROM message
                     WHERE time_created >= ?1
                       AND json_extract(data,'$.providerID') = 'opencode-go'
                       AND json_extract(data,'$.role') = 'assistant'",
                    [since_ms],
                    |row| row.get(0),
                )
                .unwrap_or(0.0);
            UsageWindow::from_used_percent(
                format!("{label} (est.)"),
                spent / dollar_limit * 100.0,
                None,
                now,
                *window_minutes,
            )
        })
        .collect()
}

/// Fallback: rolling-window sums of this machine's opencode-go spend. Only an
/// approximation (single machine, rolling not session-anchored) — the card
/// carries "(est.)" labels and `source: "local estimate"` to say so.
fn fetch_local_estimate(now: DateTime<Utc>, official_failure: String) -> OpenCodeGoData {
    let Some(path) = opencode_db_path().filter(|p| p.exists()) else {
        return OpenCodeGoData {
            identity: None,
            windows: Vec::new(),
            source: "local estimate".to_string(),
            error: Some(official_failure),
        };
    };
    match rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(db) => OpenCodeGoData {
            identity: Some(AgentIdentity {
                email: None,
                plan: Some("Go".to_string()),
            }),
            windows: local_estimate_windows(&db, now),
            source: "local estimate".to_string(),
            // Windows render normally; the switch to estimates is visible via
            // labels/source rather than an error row.
            error: None,
        },
        Err(e) => OpenCodeGoData {
            identity: None,
            windows: Vec::new(),
            source: "local estimate".to_string(),
            error: Some(format!("{official_failure}; local estimate unavailable: {e}")),
        },
    }
}

pub(crate) async fn fetch(now: DateTime<Utc>) -> OpenCodeGoData {
    match fetch_official(now).await {
        Ok(data) => data,
        Err(reason) => fetch_local_estimate(now, reason),
    }
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

    fn seed_db(rows: &[(i64, &str)]) -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE message (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                data TEXT NOT NULL);",
        )
        .unwrap();
        for (i, (time_created, data)) in rows.iter().enumerate() {
            db.execute(
                "INSERT INTO message VALUES (?1, 's', ?2, ?2, ?3)",
                rusqlite::params![format!("m{i}"), time_created, data],
            )
            .unwrap();
        }
        db
    }

    #[test]
    fn local_estimate_sums_go_cost_per_window() {
        let now = Utc::now();
        let ms = |min_ago: i64| (now - Duration::minutes(min_ago)).timestamp_millis();
        let db = seed_db(&[
            // $6 within 5h → 5h window 50% of $12
            (ms(60), r#"{"role":"assistant","providerID":"opencode-go","cost":6.0}"#),
            // $9 more at 3 days ago → weekly (6+9)/30 = 50%
            (ms(3 * 24 * 60), r#"{"role":"assistant","providerID":"opencode-go","cost":9.0}"#),
            // $15 more at 20 days ago → monthly (6+9+15)/60 = 50%
            (ms(20 * 24 * 60), r#"{"role":"assistant","providerID":"opencode-go","cost":15.0}"#),
            // other providers and non-assistant roles must not count
            (ms(30), r#"{"role":"assistant","providerID":"minimax","cost":99.0}"#),
            (ms(30), r#"{"role":"user","providerID":"opencode-go"}"#),
        ]);
        let windows = local_estimate_windows(&db, now);
        assert_eq!(windows.len(), 3);
        for w in &windows {
            assert!((w.remaining_for_test() - 50.0).abs() < 0.5, "{}", w.label_for_test());
        }
        assert_eq!(windows[0].label_for_test(), "5h (est.)");
        assert_eq!(windows[1].label_for_test(), "Weekly (est.)");
        assert_eq!(windows[2].label_for_test(), "Monthly (est.)");
    }
}
