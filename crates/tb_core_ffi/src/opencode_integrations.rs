//! Detect which subscription-type providers opencode is authenticated against.
//!
//! opencode can sign in to providers via OAuth (a shared subscription, e.g.
//! "Sign in with ChatGPT" = the Codex/ChatGPT plan) or via API keys (metered).
//! Its `~/.local/share/opencode/auth.json` records each provider with a `type`.
//! We surface the `type: "oauth"` providers, plus an allowlist of `type: "api"`
//! providers that are actually flat-rate subscription plans rather than metered
//! keys, so the user can see which agent subscriptions opencode also draws on
//! (its usage counts against those plans).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct AuthEntry {
    #[serde(default)]
    r#type: Option<String>,
}

/// Subscription-style API-key providers (flat-rate plans, not metered keys).
/// Metered keys (e.g. openrouter) are deliberately excluded.
const API_SUBSCRIPTION_PROVIDERS: &[&str] = &["opencode-go", "minimax-coding-plan", "ollama-cloud"];

/// Friendly subscription labels for opencode's OAuth providers (plus the
/// allowlisted api-type subscription providers), in a stable order.
pub fn detect_subscriptions() -> Vec<String> {
    let Some(path) = auth_path() else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    detect_subscriptions_from_raw(&raw)
}

fn detect_subscriptions_from_raw(raw: &str) -> Vec<String> {
    let Ok(entries) = serde_json::from_str::<BTreeMap<String, AuthEntry>>(raw) else {
        return Vec::new();
    };
    let mut labels: Vec<String> = entries
        .into_iter()
        .filter(|(provider, entry)| {
            let kind = entry.r#type.as_deref().unwrap_or("");
            kind.eq_ignore_ascii_case("oauth")
                || (kind.eq_ignore_ascii_case("api")
                    && API_SUBSCRIPTION_PROVIDERS.contains(&provider.as_str()))
        })
        .map(|(provider, _)| subscription_label(&provider))
        .collect();
    labels.sort();
    labels.dedup();
    labels
}

fn subscription_label(provider: &str) -> String {
    match provider.to_lowercase().as_str() {
        "openai" => "Codex".to_string(),
        "anthropic" => "Claude".to_string(),
        "github-copilot" | "copilot" => "Copilot".to_string(),
        "google" | "gemini" => "Gemini".to_string(),
        "opencode-go" => "opencode Go".to_string(),
        "minimax-coding-plan" => "MiniMax Coding Plan".to_string(),
        "ollama-cloud" => "Ollama Cloud".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => provider.to_string(),
            }
        }
    }
}

fn auth_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local/share/opencode/auth.json"))
}

/// The durable GitHub OAuth token opencode stored for its github-copilot login
/// (its `refresh` field), used to query Copilot quota. `None` if opencode isn't
/// authed against Copilot.
pub fn github_copilot_token() -> Option<String> {
    let raw = std::fs::read_to_string(auth_path()?).ok()?;
    let json = serde_json::from_str::<serde_json::Value>(&raw).ok()?;
    let entry = json.get("github-copilot")?;
    if entry.get("type").and_then(|t| t.as_str()) != Some("oauth") {
        return None;
    }
    entry
        .get("refresh")
        .or_else(|| entry.get("access"))
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// True when opencode has an `opencode-go` credential — gate for the Go quota card.
pub fn has_opencode_go() -> bool {
    let Some(path) = auth_path() else {
        return false;
    };
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .map(|v| has_opencode_go_from(&v))
        .unwrap_or(false)
}

/// Pure core of [`has_opencode_go`]: a `null` or non-object entry counts as absent.
fn has_opencode_go_from(v: &serde_json::Value) -> bool {
    v.get("opencode-go").map(|entry| entry.is_object()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_oauth_providers_only() {
        assert_eq!(subscription_label("openai"), "Codex");
        assert_eq!(subscription_label("github-copilot"), "Copilot");
        assert_eq!(subscription_label("anthropic"), "Claude");
        assert_eq!(subscription_label("minimax-coding-plan"), "MiniMax Coding Plan");
    }

    #[test]
    fn detects_oauth_and_allowlisted_api_subscriptions() {
        let raw = r#"{
            "github-copilot": {"type": "oauth", "refresh": "x"},
            "opencode-go": {"type": "api", "key": "k"},
            "minimax-coding-plan": {"type": "api", "key": "k"},
            "ollama-cloud": {"type": "api", "key": "k"},
            "openrouter": {"type": "api", "key": "k"}
        }"#;
        // openrouter is a plain metered API key, not a subscription → must not appear
        assert_eq!(
            detect_subscriptions_from_raw(raw),
            vec![
                "Copilot",
                "MiniMax Coding Plan",
                "Ollama Cloud",
                "opencode Go",
            ]
        );
    }

    #[test]
    fn has_opencode_go_detects_entry() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"opencode-go": {"type": "api", "key": "k"}}"#).unwrap();
        assert!(has_opencode_go_from(&v));
        let v: serde_json::Value = serde_json::from_str(r#"{"opencode-go": null}"#).unwrap();
        assert!(!has_opencode_go_from(&v));
        let v: serde_json::Value = serde_json::from_str(r#"{"other": {"type": "api"}}"#).unwrap();
        assert!(!has_opencode_go_from(&v));
    }
}
