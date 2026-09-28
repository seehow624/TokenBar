use crate::agent_usage::{AgentIdentity, UsageWindow};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use std::path::PathBuf;

// Official Token Plan quota endpoints (verified 2026-08): both the global and
// the mainland-China platforms expose `/v1/token_plan/remains`. The legacy
// `/coding_plan/remains` paths return 404.
const GLOBAL_API: &str = "https://api.minimax.io/v1/token_plan/remains";
const CHINA_API: &str = "https://api.minimaxi.com/v1/token_plan/remains";
const CONNECT_HINT: &str =
    "Set MINIMAX_API_KEY or sign in to opencode with a minimax-coding-plan credential.";

pub(crate) struct MinimaxData {
    pub identity: Option<AgentIdentity>,
    pub windows: Vec<UsageWindow>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RemainsResponse {
    #[serde(default)]
    base_resp: Option<BaseResp>,
    #[serde(default)]
    model_remains: Vec<ModelRemains>,
}

#[derive(Debug, Deserialize)]
struct BaseResp {
    #[serde(default)]
    status_code: Option<serde_json::Value>,
    #[serde(default)]
    status_msg: Option<String>,
}

/// One quota entry inside `model_remains`. The API returns several entries
/// (e.g. `general` for the coding/text window and `video` for media); the
/// authoritative remaining fraction is `current_*_remaining_percent`.
#[derive(Debug, Deserialize)]
struct ModelRemains {
    #[serde(default)]
    model_name: Option<String>,
    #[serde(default)]
    current_interval_remaining_percent: Option<f64>,
    #[serde(default)]
    current_weekly_remaining_percent: Option<f64>,
    #[serde(default)]
    current_interval_usage_count: Option<f64>,
    #[serde(default)]
    current_interval_total_count: Option<f64>,
    #[serde(default)]
    current_weekly_usage_count: Option<f64>,
    #[serde(default)]
    current_weekly_total_count: Option<f64>,
    /// Milliseconds until the 5-hour window resets.
    #[serde(default)]
    remains_time: Option<f64>,
    /// Milliseconds until the weekly window resets.
    #[serde(default)]
    weekly_remains_time: Option<f64>,
}

pub(crate) fn has_minimax() -> bool {
    resolve_api_key().is_some()
}

fn resolve_api_key() -> Option<String> {
    if let Ok(key) = std::env::var("MINIMAX_API_KEY") {
        let key = key.trim().to_string();
        if !key.is_empty() {
            return Some(key);
        }
    }
    opencode_minimax_key()
}

fn opencode_auth_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/share/opencode/auth.json"))
}

fn opencode_minimax_key() -> Option<String> {
    let raw = std::fs::read_to_string(opencode_auth_path()?).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let entry = json.get("minimax-coding-plan")?;
    entry
        .get("key")
        .and_then(|k| k.as_str())
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_string)
}

/// Pick the entry whose quota the card should show: the `general` coding
/// window when present (the API also returns e.g. a `video` entry), otherwise
/// the entry with the largest declared quota, otherwise the first entry.
fn select_model_remains(remains: &[ModelRemains]) -> Option<&ModelRemains> {
    remains
        .iter()
        .find(|entry| entry.model_name.as_deref() == Some("general"))
        .or_else(|| remains.iter().max_by(|a, b| model_total(a).cmp(&model_total(b))))
        .or_else(|| remains.first())
}

fn model_total(entry: &ModelRemains) -> i64 {
    [
        entry.current_interval_total_count,
        entry.current_weekly_total_count,
    ]
    .into_iter()
    .flatten()
    .map(|value| value as i64)
    .sum()
}

/// Used percent for one window. Prefers the API's authoritative
/// `current_*_remaining_percent`; falls back to deriving from the usage/total
/// counts (despite the name, `*_usage_count` is the REMAINING count).
fn used_percent(entry: &ModelRemains, interval: bool) -> Option<f64> {
    let remaining = if interval {
        entry.current_interval_remaining_percent
    } else {
        entry.current_weekly_remaining_percent
    };
    if let Some(remaining) = remaining {
        if remaining.is_finite() && (0.0..=100.0).contains(&remaining) {
            return Some((100.0 - remaining).clamp(0.0, 100.0));
        }
    }
    let (usage, total) = if interval {
        (
            entry.current_interval_usage_count,
            entry.current_interval_total_count,
        )
    } else {
        (
            entry.current_weekly_usage_count,
            entry.current_weekly_total_count,
        )
    };
    match (usage, total) {
        (Some(usage), Some(total)) if total > 0.0 => {
            Some(((total - usage) / total * 100.0).clamp(0.0, 100.0))
        }
        _ => None,
    }
}

fn windows_from(entry: &ModelRemains, now: DateTime<Utc>) -> Vec<UsageWindow> {
    let mut windows = Vec::with_capacity(2);
    if let Some(used) = used_percent(entry, true) {
        let resets_at = entry
            .remains_time
            .filter(|ms| *ms > 0.0)
            .map(|ms| now + Duration::milliseconds(ms as i64));
        windows.push(UsageWindow::from_used_percent(
            "5h".to_string(),
            used,
            resets_at,
            now,
            Some(300),
        ));
    }
    if let Some(used) = used_percent(entry, false) {
        let resets_at = entry
            .weekly_remains_time
            .filter(|ms| *ms > 0.0)
            .map(|ms| now + Duration::milliseconds(ms as i64));
        windows.push(UsageWindow::from_used_percent(
            "Weekly".to_string(),
            used,
            resets_at,
            now,
            Some(10080),
        ));
    }
    windows
}

fn base_resp_ok(base: &Option<BaseResp>) -> bool {
    match base {
        None => true,
        Some(base) => base.status_code.as_ref().is_none_or(|code| {
            code.as_i64() == Some(0) || code.as_str() == Some("0")
        }),
    }
}

async fn try_fetch(
    api_url: &str,
    api_key: &str,
    now: DateTime<Utc>,
) -> Result<MinimaxData, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("build minimax client: {}", e))?;

    let response = client
        .get(api_url)
        .bearer_auth(api_key)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "TokenBar")
        .send()
        .await
        .map_err(|e| format!("minimax request failed: {}", e))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("read minimax response: {}", e))?;

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err("MiniMax API key is invalid or expired.".to_string());
    }
    if !status.is_success() {
        return Err(format!("MiniMax API returned {}.", status.as_u16()));
    }

    let remains: RemainsResponse = serde_json::from_str(&body)
        .map_err(|e| format!("decode minimax response: {}", e))?;

    if !base_resp_ok(&remains.base_resp) {
        return Err(format!(
            "MiniMax API error: {}",
            remains
                .base_resp
                .as_ref()
                .and_then(|base| base.status_msg.as_deref())
                .unwrap_or("unknown")
        ));
    }

    let Some(entry) = select_model_remains(&remains.model_remains) else {
        return Err("MiniMax API returned no model_remains entries.".to_string());
    };
    let windows = windows_from(entry, now);
    if windows.is_empty() {
        return Err("MiniMax API returned no recognizable usage windows.".to_string());
    }

    Ok(MinimaxData {
        identity: Some(AgentIdentity {
            email: None,
            plan: Some("Coding Plan".to_string()),
        }),
        windows,
        error: None,
    })
}

pub(crate) async fn fetch(now: DateTime<Utc>) -> MinimaxData {
    let Some(api_key) = resolve_api_key() else {
        return MinimaxData {
            identity: None,
            windows: Vec::new(),
            error: Some("MiniMax API key not configured. Set MINIMAX_API_KEY or sign in to opencode with a minimax-coding-plan credential.".to_string()),
        };
    };

    match try_fetch(GLOBAL_API, &api_key, now).await {
        Ok(data) => data,
        Err(global_err) => match try_fetch(CHINA_API, &api_key, now).await {
            Ok(data) => data,
            Err(china_err) => MinimaxData {
                identity: None,
                windows: Vec::new(),
                error: Some(format!(
                    "MiniMax quota fetch failed. Global: {}. China: {}. {}",
                    global_err, china_err, CONNECT_HINT
                )),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shape captured from the live `/v1/token_plan/remains` response.
    const FIXTURE: &str = r#"{
        "base_resp": {"status_code": 0, "status_msg": "success"},
        "model_remains": [
            {
                "model_name": "video",
                "current_interval_remaining_percent": 50.0,
                "current_interval_total_count": 9000.0,
                "current_weekly_remaining_percent": 60.0,
                "current_weekly_total_count": 90000.0,
                "remains_time": 1800000.0,
                "weekly_remains_time": 86400000.0
            },
            {
                "model_name": "general",
                "current_interval_remaining_percent": 80.0,
                "current_interval_usage_count": 20.0,
                "current_interval_total_count": 100.0,
                "current_weekly_remaining_percent": 90.0,
                "current_weekly_usage_count": 10.0,
                "current_weekly_total_count": 100.0,
                "remains_time": 3600000.0,
                "weekly_remains_time": 172800000.0
            }
        ]
    }"#;

    #[test]
    fn selects_general_entry_over_larger_quota() {
        let remains: RemainsResponse = serde_json::from_str(FIXTURE).unwrap();
        let entry = select_model_remains(&remains.model_remains).unwrap();
        assert_eq!(entry.model_name.as_deref(), Some("general"));
    }

    #[test]
    fn maps_remaining_percent_to_used_windows() {
        let remains: RemainsResponse = serde_json::from_str(FIXTURE).unwrap();
        let entry = select_model_remains(&remains.model_remains).unwrap();
        let now = Utc::now();
        let windows = windows_from(entry, now);
        assert_eq!(windows.len(), 2);
        let five_hour = windows.iter().find(|w| w.label_for_test() == "5h").unwrap();
        assert!((five_hour.remaining_for_test() - 80.0).abs() < 0.001);
        let weekly = windows.iter().find(|w| w.label_for_test() == "Weekly").unwrap();
        assert!((weekly.remaining_for_test() - 90.0).abs() < 0.001);
        assert!(weekly.reset_text_for_test().is_some());
    }

    #[test]
    fn falls_back_to_usage_counts_when_percent_missing() {
        let raw = r#"{
            "base_resp": {"status_code": 0},
            "model_remains": [
                {"model_name": "general", "current_interval_usage_count": 25.0, "current_interval_total_count": 100.0}
            ]
        }"#;
        let remains: RemainsResponse = serde_json::from_str(raw).unwrap();
        let entry = select_model_remains(&remains.model_remains).unwrap();
        let windows = windows_from(entry, Utc::now());
        let five_hour = windows.iter().find(|w| w.label_for_test() == "5h").unwrap();
        // usage_count is the REMAINING count (25 of 100) → 75% used.
        assert!((five_hour.remaining_for_test() - 25.0).abs() < 0.001);
    }

    #[test]
    fn rejects_nonzero_base_status() {
        let raw = r#"{
            "base_resp": {"status_code": 1301, "status_msg": "invalid key"},
            "model_remains": []
        }"#;
        let remains: RemainsResponse = serde_json::from_str(raw).unwrap();
        assert!(!base_resp_ok(&remains.base_resp));
        assert!(remains.model_remains.is_empty());
    }

    #[test]
    fn accepts_missing_base_resp() {
        let raw = r#"{"model_remains": [{"model_name": "general"}]}"#;
        let remains: RemainsResponse = serde_json::from_str(raw).unwrap();
        assert!(base_resp_ok(&remains.base_resp));
        assert!(windows_from(&remains.model_remains[0], Utc::now()).is_empty());
    }
}
