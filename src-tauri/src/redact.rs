// 作者: kejiqing
//! 只作用于发给模型的副本。对照表留在进程内存。

use std::collections::HashMap;
use std::sync::LazyLock;

use aho_corasick::{AhoCorasick, MatchKind};
use regex::Regex;
use serde_json::Value;
use serde_yaml::Value as Yaml;
use uuid::Uuid;

#[derive(Clone, Copy)]
enum Kind {
    Ip,
    Sec,
    Url,
    Host,
}

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Kind::Ip => "ip",
            Kind::Sec => "sec",
            Kind::Url => "url",
            Kind::Host => "host",
        }
    }
}

pub struct Redactor {
    to_token: HashMap<String, String>,
    to_value: HashMap<String, String>,
}

impl Redactor {
    pub fn new() -> Self {
        Self {
            to_token: HashMap::new(),
            to_value: HashMap::new(),
        }
    }

    pub fn learn_url(&mut self, url: &str) {
        self.learn_exact(Kind::Url, url);
    }

    pub fn learn_secret(&mut self, secret: &str) {
        self.learn_exact(Kind::Sec, secret);
    }

    pub fn learn_yaml(&mut self, yaml: &str) {
        let Ok(value) = serde_yaml::from_str::<Yaml>(yaml) else {
            return;
        };
        self.walk(&value, None);
    }

    pub fn mask_text(&mut self, input: &str) -> String {
        let with_urls = self.replace_query_urls(input);
        let with_known = self.replace_outside_tokens(&with_urls, Pass::Known);
        let with_v6 = self.replace_outside_tokens(&with_known, Pass::Ipv6);
        self.replace_outside_tokens(&with_v6, Pass::Ipv4)
    }

    pub fn mask_value(&mut self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.mask_text(&text)),
            Value::Array(items) => Value::Array(items.into_iter().map(|item| self.mask_value(item)).collect()),
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, item)| (key, self.mask_value(item)))
                    .collect(),
            ),
            other => other,
        }
    }

    pub fn unmask(&self, text: &str) -> String {
        let mut out = String::new();
        let mut last = 0;
        for matched in token_re().find_iter(text) {
            out.push_str(&text[last..matched.start()]);
            if let Some(value) = self.to_value.get(matched.as_str()) {
                out.push_str(value);
            } else {
                out.push_str(matched.as_str());
            }
            last = matched.end();
        }
        out.push_str(&text[last..]);
        out
    }

    pub fn restore_tool_args(&self, args: &str) -> Result<Value, String> {
        let value: Value = serde_json::from_str(args).map_err(|err| format!("参数不是 JSON: {err}"))?;
        if !matches!(value, Value::Object(_)) {
            return Err("参数必须是 JSON 对象".into());
        }
        self.restore_value(value)
    }

    fn restore_value(&self, value: Value) -> Result<Value, String> {
        match value {
            Value::String(text) => Ok(Value::String(self.restore_string(&text)?)),
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.restore_value(item)?);
                }
                Ok(Value::Array(out))
            }
            Value::Object(map) => {
                let mut out = serde_json::Map::new();
                for (key, item) in map {
                    out.insert(key, self.restore_value(item)?);
                }
                Ok(Value::Object(out))
            }
            other => Ok(other),
        }
    }

    fn restore_string(&self, text: &str) -> Result<String, String> {
        for matched in token_re().find_iter(text) {
            if !self.to_value.contains_key(matched.as_str()) {
                return Err(format!("未知代号 {}", matched.as_str()));
            }
        }
        let stripped = token_re().replace_all(text, "");
        if contains_raw_address(&stripped) {
            return Err("参数含有未登记的 IP 或订阅地址".into());
        }
        for value in self.to_token.keys() {
            if value.len() >= 8 && stripped.contains(value.as_str()) {
                return Err("参数含有未登记的密钥或地址".into());
            }
            if value.len() >= 4 && stripped.as_ref() == value {
                return Err("参数含有未登记的密钥或地址".into());
            }
        }
        Ok(self.unmask(text))
    }

    fn learn_exact(&mut self, kind: Kind, value: &str) {
        let value = value.trim();
        if !should_register(value) {
            return;
        }
        let kind = if matches!(kind, Kind::Host) && is_ip(value) {
            Kind::Ip
        } else {
            kind
        };
        let _ = self.register(kind, value);
    }

    fn walk(&mut self, value: &Yaml, key: Option<&str>) {
        match value {
            Yaml::Mapping(map) => {
                for (child_key, child) in map {
                    self.walk(child, child_key.as_str());
                }
            }
            Yaml::Sequence(items) => {
                for child in items {
                    self.walk(child, None);
                }
            }
            Yaml::String(text) => {
                if let Some(kind) = kind_for_key(key) {
                    self.learn_exact(kind, text);
                }
            }
            _ => {}
        }
    }

    fn register(&mut self, kind: Kind, value: &str) -> String {
        if let Some(token) = self.to_token.get(value) {
            return token.clone();
        }
        let token = loop {
            let candidate = format!(
                "{}_{:08x}",
                kind.prefix(),
                Uuid::new_v4().as_u128() as u32
            );
            if !self.to_value.contains_key(&candidate) {
                break candidate;
            }
        };
        self.to_token.insert(value.to_string(), token.clone());
        self.to_value.insert(token.clone(), value.to_string());
        token
    }

    fn replace_query_urls(&mut self, text: &str) -> String {
        let mut out = String::new();
        let mut last = 0;
        for matched in url_re().find_iter(text) {
            let mut end = matched.end();
            let raw = matched.as_str();
            let trimmed = raw.trim_end_matches(|ch: char| matches!(ch, '.' | ',' | ';' | ')' | ']' | '。'));
            if trimmed.len() != raw.len() {
                end = matched.start() + trimmed.len();
            }
            if !trimmed.contains('?') {
                continue;
            }
            out.push_str(&text[last..matched.start()]);
            let token = self.register(Kind::Url, trimmed);
            out.push_str(&token);
            last = end;
        }
        out.push_str(&text[last..]);
        out
    }

    fn replace_outside_tokens(&mut self, text: &str, pass: Pass) -> String {
        let mut out = String::new();
        let mut last = 0;
        for matched in token_re().find_iter(text) {
            out.push_str(&self.replace_segment(&text[last..matched.start()], pass));
            out.push_str(matched.as_str());
            last = matched.end();
        }
        out.push_str(&self.replace_segment(&text[last..], pass));
        out
    }

    fn replace_segment(&mut self, segment: &str, pass: Pass) -> String {
        match pass {
            Pass::Known => self.replace_known(segment),
            Pass::Ipv6 => self.replace_regex(segment, ipv6_re(), Kind::Ip),
            Pass::Ipv4 => self.replace_regex(segment, ipv4_re(), Kind::Ip),
        }
    }

    fn replace_known(&self, text: &str) -> String {
        let pairs: Vec<(String, String)> = self
            .to_token
            .iter()
            .filter(|(value, _)| should_register(value))
            .map(|(value, token)| (value.clone(), token.clone()))
            .collect();
        if pairs.is_empty() {
            return text.to_string();
        }
        let patterns: Vec<&str> = pairs.iter().map(|(value, _)| value.as_str()).collect();
        let ac = AhoCorasick::builder()
            .match_kind(MatchKind::LeftmostLongest)
            .build(patterns)
            .expect("脱敏模式构建失败");
        let mut out = String::new();
        let mut last = 0;
        for matched in ac.find_iter(text) {
            if !is_boundary(text, matched.start(), matched.end()) {
                continue;
            }
            out.push_str(&text[last..matched.start()]);
            out.push_str(&pairs[matched.pattern().as_usize()].1);
            last = matched.end();
        }
        out.push_str(&text[last..]);
        out
    }

    fn replace_regex(&mut self, text: &str, pattern: &Regex, kind: Kind) -> String {
        let mut out = String::new();
        let mut last = 0;
        for matched in pattern.find_iter(text) {
        if matched.as_str().contains(':') && matched.as_str().matches(':').count() < 2 {
            continue;
        }
        if !is_boundary(text, matched.start(), matched.end()) {
            continue;
        }
        out.push_str(&text[last..matched.start()]);
            let token = self.register(kind, matched.as_str());
            out.push_str(&token);
            last = matched.end();
        }
        out.push_str(&text[last..]);
        out
    }
}

impl Default for Redactor {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
enum Pass {
    Known,
    Ipv6,
    Ipv4,
}

fn kind_for_key(key: Option<&str>) -> Option<Kind> {
    match key? {
        "server" => Some(Kind::Host),
        "password" | "uuid" | "token" | "private-key" | "secret" => Some(Kind::Sec),
        _ => None,
    }
}

fn should_register(value: &str) -> bool {
    let value = value.trim();
    if value.len() < 4 || value.len() > 16 * 1024 {
        return false;
    }
    if value.chars().all(|ch| ch.is_ascii_digit()) {
        return false;
    }
    !matches!(
        value,
        "rule" | "global" | "direct" | "true" | "false" | "http" | "https" | "socks" | "enable" | "enabled"
    )
}

fn is_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    !before.is_some_and(|ch| ch.is_ascii_alphanumeric())
        && !after.is_some_and(|ch| ch.is_ascii_alphanumeric())
}

fn is_ip(value: &str) -> bool {
    ipv4_re().is_match(value) && ipv4_re().find(value).is_some_and(|item| item.as_str() == value)
        || ipv6_re().find(value).is_some_and(|item| item.as_str() == value && value.matches(':').count() >= 2)
}

fn contains_raw_address(text: &str) -> bool {
    ipv4_re().is_match(text)
        || ipv6_re().find(text).is_some_and(|item| item.as_str().matches(':').count() >= 2)
        || url_re().find(text).is_some_and(|item| item.as_str().contains('?'))
}

fn token_re() -> &'static Regex {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\b(?:ip|sec|url|host)_[0-9a-f]{8}\b").unwrap());
    &RE
}

fn url_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"https?://[^\s"'<>]+"#).unwrap());
    &RE
}

fn ipv4_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b(?:(?:25[0-5]|2[0-4]\d|[01]?\d?\d)\.){3}(?:25[0-5]|2[0-4]\d|[01]?\d?\d)\b")
            .unwrap()
    });
    &RE
}

fn ipv6_re() -> &'static Regex {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:[0-9a-f]{0,4}:){2,7}[0-9a-f]{0,4}").unwrap()
    });
    &RE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_query_url_as_one_token_and_restores() {
        let mut redactor = Redactor::new();
        let text = "导入 https://sub.example.com/path?token=abc&ip=1.2.3.4 以及 8.8.8.8";
        let masked = redactor.mask_text(text);
        assert!(!masked.contains("1.2.3.4"));
        assert!(!masked.contains("token=abc"));
        assert!(!masked.contains("8.8.8.8"));
        assert!(masked.contains("url_"));
        assert!(masked.contains("ip_"));
        assert_eq!(redactor.unmask(&masked), text);
        assert_eq!(redactor.mask_text(text), masked);
    }

    #[test]
    fn masks_ipv6_and_yaml_secrets() {
        let mut redactor = Redactor::new();
        redactor.learn_yaml(
            "proxies:\n  - name: a\n    server: 2001:db8::1\n    password: supersecret\n",
        );
        let text = "节点 2001:db8::1 密码 supersecret 端口 7890";
        let masked = redactor.mask_text(text);
        assert!(!masked.contains("2001:db8::1"));
        assert!(!masked.contains("supersecret"));
        assert!(masked.contains("7890"));
        assert_eq!(redactor.unmask(&masked), text);
    }

    #[test]
    fn restores_token_and_rejects_raw_or_unknown() {
        let mut redactor = Redactor::new();
        redactor.learn_url("https://example.com/sub?token=abc");
        let masked = redactor.mask_text("https://example.com/sub?token=abc");
        let args = format!(r#"{{"url":"{masked}"}}"#);
        let restored = redactor.restore_tool_args(&args).unwrap();
        assert_eq!(
            restored.get("url").and_then(Value::as_str),
            Some("https://example.com/sub?token=abc")
        );
        assert!(redactor
            .restore_tool_args(r#"{"url":"https://evil.example/sub?token=zzz"}"#)
            .is_err());
        assert!(redactor.restore_tool_args(r#"{"host":"1.2.3.4"}"#).is_err());
        assert!(redactor
            .restore_tool_args(r#"{"name":"ip_deadbeef"}"#)
            .is_err());
        let port = redactor.restore_tool_args(r#"{"port":7890}"#).unwrap();
        assert_eq!(port.get("port").and_then(Value::as_u64), Some(7890));
    }

    #[test]
    fn does_not_mask_mode_word() {
        let mut redactor = Redactor::new();
        redactor.learn_yaml("proxies:\n  - server: direct\n");
        let masked = redactor.mask_text("模式 direct");
        assert_eq!(masked, "模式 direct");
    }
}
