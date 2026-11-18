// 作者: kejiqing
//! 用用户订阅 token 换取 NeuroGate 临时调用钥（ngmk_）。

use std::time::Duration;

use serde::Deserialize;

/// 公网 Hub（Tunnel）基址。Author: kejiqing
const HUB_BASE: &str = "https://phub.supone.vip";

const SUB_TOKEN_HINT: &str =
    "请先拿到一个授权的订阅 token（形如 https://phub.supone.vip/clash/<token>），并在「订阅」页导入";

#[derive(Debug, Deserialize)]
pub struct CallTokenResponse {
    pub call_token: String,
    pub expires_at: String,
    pub base_url: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub proj_id: i64,
    /// Hub users.id；对话请求编进 NeuroGate `user`。Author: kejiqing
    #[serde(default)]
    pub user_id: i64,
}

/// NeuroGate Chat Completions 的 `user`：Hub 用户 + 本机会话键。Author: kejiqing
pub fn neurogate_user_key(hub_user_id: i64, conversation_user: &str) -> String {
    format!("phub-u{hub_user_id}:{conversation_user}")
}

/// 从订阅 URL 解析 Hub 订阅 token。仅接受 phub.supone.vip/clash/<token>。
/// Author: kejiqing
pub fn parse_subscription_token(url: &str) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    let rest = url
        .strip_prefix("https://phub.supone.vip/clash/")
        .or_else(|| url.strip_prefix("http://phub.supone.vip/clash/"))
        .ok_or_else(|| SUB_TOKEN_HINT.to_string())?;
    if rest.is_empty()
        || rest.contains('/')
        || rest.contains('?')
        || rest.contains('#')
        || rest.chars().any(char::is_whitespace)
    {
        return Err(SUB_TOKEN_HINT.into());
    }
    Ok(rest.to_string())
}

pub fn missing_subscription_hint() -> String {
    SUB_TOKEN_HINT.into()
}

/// 距失效不足 5 分钟视为需重签。Author: kejiqing
pub fn call_token_fresh(expires_at: &str) -> bool {
    let Ok(exp) = chrono::DateTime::parse_from_rfc3339(expires_at.trim()) else {
        return false;
    };
    let skew = chrono::Duration::minutes(5);
    exp.with_timezone(&chrono::Utc) > chrono::Utc::now() + skew
}

pub async fn exchange_call_token(subscription_token: &str) -> Result<CallTokenResponse, String> {
    let token = subscription_token.trim();
    if token.is_empty() {
        return Err(SUB_TOKEN_HINT.into());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|err| err.to_string())?;
    let response = client
        .post(format!("{HUB_BASE}/api/neurogate/call-token"))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({ "token": token }))
        .send()
        .await
        .map_err(|err| format!("换取调用 token 失败: {err}"))?;
    let status = response.status();
    let raw = response
        .text()
        .await
        .map_err(|err| format!("换取调用 token 失败: {err}"))?;
    if status.as_u16() == 404 {
        return Err("订阅 token 无效、已停用或已过期，请先拿到一个授权的订阅 token".into());
    }
    if status.as_u16() == 422 {
        return Err("订阅 token 格式非法".into());
    }
    if status.as_u16() == 429 {
        return Err("换取调用 token 过于频繁，请稍后再试".into());
    }
    if status.as_u16() == 502 {
        return Err("Hub 签发 NeuroGate 调用 token 失败，请稍后再试".into());
    }
    if !status.is_success() {
        return Err(format!(
            "换取调用 token 失败: HTTP {status} {}",
            short(&raw)
        ));
    }
    let parsed: CallTokenResponse = serde_json::from_str(&raw)
        .map_err(|err| format!("换取调用 token 响应无效: {err}"))?;
    if !(parsed.call_token.starts_with("phllm_") || parsed.call_token.starts_with("ngmk_")) {
        return Err("换取调用 token 响应无效: 非 phllm_/ngmk_ 钥".into());
    }
    if parsed.base_url.trim().is_empty() {
        return Err("换取调用 token 响应无效: 缺少 base_url".into());
    }
    if parsed.user_id <= 0 {
        return Err("换取调用 token 响应无效: 缺少 user_id（请升级 Hub）".into());
    }
    Ok(parsed)
}

fn short(text: &str) -> String {
    const LIMIT: usize = 180;
    let compact: String = text.chars().take(LIMIT).collect();
    if text.chars().count() > LIMIT {
        format!("{compact}…")
    } else {
        compact
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_phub_clash_url() {
        let token = parse_subscription_token(
            "https://phub.supone.vip/clash/example_token",
        )
        .unwrap();
        assert_eq!(token, "example_token");
        let with_slash = parse_subscription_token(
            "https://phub.supone.vip/clash/example_token/",
        )
        .unwrap();
        assert_eq!(with_slash, "example_token");
    }

    #[test]
    fn rejects_non_phub_or_bad_path() {
        assert!(parse_subscription_token("https://example.com/clash/abc").is_err());
        assert!(parse_subscription_token("https://phub.supone.vip/clash/").is_err());
        assert!(parse_subscription_token("https://phub.supone.vip/clash/a/b").is_err());
        assert!(parse_subscription_token("example_token").is_err());
    }

    #[test]
    fn neurogate_user_key_embeds_hub_user() {
        assert_eq!(
            neurogate_user_key(42, "conv-abc"),
            "phub-u42:conv-abc"
        );
    }
}
