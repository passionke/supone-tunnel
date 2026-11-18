// 作者: kejiqing
//! 订阅存储与合并。有效配置只从这里写出。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use chrono::{FixedOffset, SecondsFormat};
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use uuid::Uuid;

const CONTROLLER_PORT: u16 = 9090;
/// Nora 上的 DB-IP Lite，避免 mihomo 去拉 GitHub 的 geoip.metadb / ASN。
/// Author: kejiqing
const NORA_MMDB: &str = "https://nora.home.passionke.top/raw/geoip/dbip-country-lite.mmdb";
const NORA_ASN: &str = "https://nora.home.passionke.top/raw/geoip/dbip-asn-lite.mmdb";
/// 覆盖未声明 route-exclude-address 时的默认：常见 VPN 叠网（如 home WireGuard）。
/// Author: kejiqing
const DEFAULT_TUN_ROUTE_EXCLUDE: &str = "10.8.0.0/16";

const STRIP_KEYS: &[&str] = &[
    "external-ui",
    "external-ui-url",
    "external-ui-name",
    "external-controller-unix",
    "external-controller-pipe",
    "external-controller-tls",
    "external-doh-server",
];

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileMeta {
    pub id: String,
    pub name: String,
    pub url: String,
    pub updated_at: String,
    pub active: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct MetaFile {
    id: String,
    name: String,
    url: String,
    updated_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct AppConfig {
    active_id: Option<String>,
    secret: String,
    controller_port: u16,
}

impl AppConfig {
    fn new() -> Self {
        Self {
            active_id: None,
            secret: Uuid::new_v4().simple().to_string(),
            controller_port: CONTROLLER_PORT,
        }
    }
}

struct StoreInner {
    root: PathBuf,
    state: AppConfig,
}

#[derive(Clone)]
pub struct ProfileStore {
    inner: Arc<Mutex<StoreInner>>,
}

impl ProfileStore {
    pub fn open(root: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(root.join("profiles")).map_err(|err| err.to_string())?;
        let state_path = root.join("state.json");
        let state = load_state(&state_path)?;
        if !state_path.exists() {
            write_state(&state_path, &state)?;
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(StoreInner { root, state })),
        })
    }

    pub fn root(&self) -> PathBuf {
        self.lock().root.clone()
    }

    pub fn secret(&self) -> String {
        self.lock().state.secret.clone()
    }

    pub fn controller_port(&self) -> u16 {
        self.lock().state.controller_port
    }

    pub fn urls(&self) -> Vec<String> {
        self.list()
            .unwrap_or_default()
            .into_iter()
            .map(|item| item.url)
            .collect()
    }

    pub fn yaml_bodies(&self) -> Vec<String> {
        let guard = self.lock();
        let profiles = guard.root.join("profiles");
        let Ok(entries) = fs::read_dir(&profiles) else {
            return Vec::new();
        };
        let mut bodies = Vec::new();
        for entry in entries.flatten() {
            for name in ["remote.yaml", "override.yaml", "effective.yaml"] {
                let path = entry.path().join(name);
                if let Ok(text) = fs::read_to_string(path) {
                    bodies.push(text);
                }
            }
        }
        bodies
    }

    pub fn list(&self) -> Result<Vec<ProfileMeta>, String> {
        let guard = self.lock();
        let mut items = Vec::new();
        let dir = guard.root.join("profiles");
        let entries = fs::read_dir(&dir).map_err(|err| err.to_string())?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if Uuid::parse_str(id).is_err() {
                continue;
            }
            let Ok(meta) = read_meta(&path.join("meta.json")) else {
                continue;
            };
            items.push(ProfileMeta {
                active: guard.state.active_id.as_deref() == Some(meta.id.as_str()),
                id: meta.id,
                name: meta.name,
                url: meta.url,
                updated_at: meta.updated_at,
            });
        }
        items.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        Ok(items)
    }

    pub fn list_text(&self) -> Result<String, String> {
        let list = self.list()?;
        if list.is_empty() {
            return Ok("没有订阅".into());
        }
        let mut lines = vec![format!("共 {} 个订阅", list.len())];
        for (index, item) in list.iter().take(50).enumerate() {
            let mark = if item.active { "当前" } else { "" };
            lines.push(format!(
                "{}. id={} 名称={} {}",
                index + 1,
                item.id,
                item.name,
                mark
            ));
        }
        Ok(lines.join("\n"))
    }

    pub async fn import_url(&self, url: &str, name: Option<String>) -> Result<ProfileMeta, String> {
        ensure_http_url(url)?;
        let body = fetch_subscription(url).await?;
        let yaml = parse_subscription(&body)?;
        self.import_parsed(url, name, &yaml)
    }

    pub fn import_parsed(
        &self,
        url: &str,
        name: Option<String>,
        yaml: &str,
    ) -> Result<ProfileMeta, String> {
        ensure_http_url(url)?;
        let yaml = parse_subscription(yaml)?;
        let mut guard = self.lock();
        if let Some(existing) = find_by_url(&guard, url)? {
            let id = existing.id.clone();
            drop(guard);
            self.write_remote(&id, &yaml)?;
            return self.meta_of(&id);
        }
        let id = Uuid::new_v4().to_string();
        let name = clean_name(
            &name.unwrap_or_else(|| host_of(url).unwrap_or_else(|| "订阅".into())),
        );
        let dir = guard.root.join("profiles").join(&id);
        fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
        let meta = MetaFile {
            id: id.clone(),
            name,
            url: url.to_string(),
            updated_at: now_stamp(),
        };
        write_private(&dir.join("remote.yaml"), yaml.as_bytes())?;
        write_private(&dir.join("override.yaml"), b"{}\n")?;
        write_meta(&dir.join("meta.json"), &meta)?;
        let activate = guard.state.active_id.is_none();
        if activate {
            guard.state.active_id = Some(id.clone());
            write_state(&guard.root.join("state.json"), &guard.state)?;
        }
        let port = guard.state.controller_port;
        let secret = guard.state.secret.clone();
        drop(guard);
        self.rebuild_effective(&id, port, &secret)?;
        self.meta_of(&id)
    }

    pub async fn refresh(&self, id: &str) -> Result<ProfileMeta, String> {
        ensure_id(id)?;
        let url = {
            let guard = self.lock();
            let meta = read_meta(&profile_dir(&guard.root, id).join("meta.json"))?;
            meta.url
        };
        let body = fetch_subscription(&url).await?;
        let yaml = parse_subscription(&body)?;
        self.write_remote(id, &yaml)?;
        self.meta_of(id)
    }

    pub fn activate(&self, id: &str) -> Result<ProfileMeta, String> {
        ensure_id(id)?;
        let mut guard = self.lock();
        let dir = profile_dir(&guard.root, id);
        if !dir.join("meta.json").is_file() {
            return Err("订阅不存在".into());
        }
        guard.state.active_id = Some(id.to_string());
        let port = guard.state.controller_port;
        let secret = guard.state.secret.clone();
        write_state(&guard.root.join("state.json"), &guard.state)?;
        drop(guard);
        self.rebuild_effective(id, port, &secret)?;
        self.meta_of(id)
    }

    pub fn active_id(&self) -> Option<String> {
        self.lock().state.active_id.clone()
    }

    /// 当前激活订阅的导入 URL（对话鉴权取订阅 token 用）。Author: kejiqing
    pub fn active_url(&self) -> Option<String> {
        let id = self.active_id()?;
        self.meta_of(&id).ok().map(|meta| meta.url)
    }

    pub fn read_override(&self) -> Result<String, String> {
        let id = self.require_active()?;
        let guard = self.lock();
        fs::read_to_string(profile_dir(&guard.root, &id).join("override.yaml"))
            .map_err(|err| err.to_string())
    }

    pub fn write_override(&self, yaml: &str) -> Result<(), String> {
        let id = self.require_active()?;
        let text = if yaml.trim().is_empty() {
            "{}\n".to_string()
        } else {
            let value: Value = serde_yaml::from_str(yaml).map_err(|err| format!("覆盖 YAML 无效: {err}"))?;
            if !matches!(value, Value::Mapping(_)) {
                return Err("覆盖必须是 YAML 映射".into());
            }
            yaml.to_string()
        };
        let (port, secret) = {
            let guard = self.lock();
            write_private(
                &profile_dir(&guard.root, &id).join("override.yaml"),
                text.as_bytes(),
            )?;
            (guard.state.controller_port, guard.state.secret.clone())
        };
        self.rebuild_effective(&id, port, &secret)
    }

    /// 清空本地覆盖，回到仅订阅原文（+ 壳锁字段）。不改 remote。
    /// `id` 空则用当前激活订阅。Author: kejiqing
    pub fn reset_override(&self, id: Option<&str>) -> Result<ProfileMeta, String> {
        let id = match id.map(str::trim).filter(|s| !s.is_empty()) {
            Some(id) => id.to_string(),
            None => self.require_active()?,
        };
        let (port, secret) = {
            let guard = self.lock();
            let dir = profile_dir(&guard.root, &id);
            if !dir.join("meta.json").is_file() {
                return Err(format!("订阅不存在: {id}"));
            }
            write_private(&dir.join("override.yaml"), b"{}\n")?;
            (guard.state.controller_port, guard.state.secret.clone())
        };
        self.rebuild_effective(&id, port, &secret)?;
        self.meta_of(&id)
    }

    pub fn patch_override(&self, patch: Value) -> Result<(), String> {
        let id = self.require_active()?;
        let (port, secret, mut doc) = {
            let guard = self.lock();
            let path = profile_dir(&guard.root, &id).join("override.yaml");
            let text = fs::read_to_string(&path).unwrap_or_else(|_| "{}\n".into());
            let doc = if text.trim().is_empty() {
                Value::Mapping(serde_yaml::Mapping::new())
            } else {
                serde_yaml::from_str(&text).map_err(|err| format!("覆盖 YAML 无效: {err}"))?
            };
            if !matches!(doc, Value::Mapping(_)) {
                return Err("覆盖必须是 YAML 映射".into());
            }
            (guard.state.controller_port, guard.state.secret.clone(), doc)
        };
        deep_merge(&mut doc, patch);
        let text = serde_yaml::to_string(&doc).map_err(|err| err.to_string())?;
        {
            let guard = self.lock();
            write_private(
                &profile_dir(&guard.root, &id).join("override.yaml"),
                text.as_bytes(),
            )?;
        }
        self.rebuild_effective(&id, port, &secret)
    }

    pub fn configured_mode(&self) -> String {
        self.yaml_string_field("mode").unwrap_or_else(|| "rule".into())
    }

    pub fn configured_mixed_port(&self) -> u16 {
        self.yaml_u16_field("mixed-port").unwrap_or(7890)
    }

    /// 生效配置里的 tun.enable（默认锁为开）。
    pub fn tun_enabled_effective(&self) -> bool {
        self.read_effective_value()
            .and_then(|value| mapping_get(&value, "tun").cloned())
            .and_then(|tun| mapping_get(&tun, "enable").cloned())
            .and_then(|enable| enable.as_bool())
            .unwrap_or(true)
    }

    pub fn effective_rules(&self) -> Result<Vec<crate::runtime::RuleRow>, String> {
        let value = self
            .read_effective_value()
            .ok_or_else(|| "没有生效配置".to_string())?;
        let Some(rules) = mapping_get(&value, "rules").and_then(Value::as_sequence) else {
            return Ok(Vec::new());
        };
        let mut rows = Vec::with_capacity(rules.len());
        for (index, item) in rules.iter().enumerate() {
            let Some(text) = item.as_str() else {
                continue;
            };
            let parts = text.splitn(3, ',').map(str::trim).collect::<Vec<_>>();
            if parts.is_empty() {
                continue;
            }
            let kind = parts[0].to_string();
            let (payload, proxy) = match parts.len() {
                1 => (String::new(), String::new()),
                2 => (String::new(), parts[1].to_string()),
                _ => (parts[1].to_string(), parts[2].to_string()),
            };
            rows.push(crate::runtime::RuleRow {
                index,
                kind,
                payload,
                proxy,
            });
        }
        Ok(rows)
    }

    pub fn effective_file(&self) -> Result<PathBuf, String> {
        let id = self.require_active()?;
        let port = self.controller_port();
        let secret = self.secret();
        // 每次启动都重写。已存在的 effective.yaml 不会自己带上后来锁上的 geox-url。
        self.rebuild_effective(&id, port, &secret)?;
        let path = profile_dir(&self.lock().root, &id).join("effective.yaml");
        std::path::absolute(&path).map_err(|err| err.to_string())
    }

    pub fn active_name(&self) -> Option<String> {
        let id = self.active_id()?;
        let guard = self.lock();
        read_meta(&profile_dir(&guard.root, &id).join("meta.json"))
            .ok()
            .map(|meta| meta.name)
    }

    fn write_remote(&self, id: &str, yaml: &str) -> Result<(), String> {
        let (port, secret) = {
            let guard = self.lock();
            let dir = profile_dir(&guard.root, id);
            write_private(&dir.join("remote.yaml"), yaml.as_bytes())?;
            let mut meta = read_meta(&dir.join("meta.json"))?;
            meta.updated_at = now_stamp();
            write_meta(&dir.join("meta.json"), &meta)?;
            (guard.state.controller_port, guard.state.secret.clone())
        };
        self.rebuild_effective(id, port, &secret)
    }

    fn rebuild_effective(&self, id: &str, port: u16, secret: &str) -> Result<(), String> {
        let guard = self.lock();
        let dir = profile_dir(&guard.root, id);
        let remote_text = fs::read_to_string(dir.join("remote.yaml")).map_err(|err| err.to_string())?;
        let override_text =
            fs::read_to_string(dir.join("override.yaml")).unwrap_or_else(|_| "{}\n".into());
        let mut doc: Value =
            serde_yaml::from_str(&remote_text).map_err(|err| format!("订阅 YAML 无效: {err}"))?;
        if !override_text.trim().is_empty() {
            let overlay: Value = serde_yaml::from_str(&override_text)
                .map_err(|err| format!("覆盖 YAML 无效: {err}"))?;
            deep_merge(&mut doc, overlay);
        }
        lock_controller(&mut doc, port, secret)?;
        lock_tun(&mut doc, &override_text);
        // 订阅节点公网 IP 进 route-exclude（只写 effective，不写 override）。Author: kejiqing
        ensure_proxy_server_route_excludes(&mut doc);
        // rule-prepend 只存在于覆盖里，拼到订阅规则前面，再从生效配置里拿掉。
        apply_rule_prepend(&mut doc);
        // proxy-group-upsert 按 name 合并进 proxy-groups，再删掉该键（mihomo 不认）。
        apply_proxy_group_upsert(&mut doc);
        let text = serde_yaml::to_string(&doc).map_err(|err| err.to_string())?;
        write_private(&dir.join("effective.yaml"), text.as_bytes())
    }

    /// 新建或按名覆盖一个本地代理组（写入 override，刷新订阅不冲掉）。
    /// 返回 `(created, 组摘要)`：created=true 表示此前生效配置里没有同名组。
    /// Author: kejiqing
    pub fn upsert_proxy_group(
        &self,
        name: &str,
        group_type: &str,
        proxies: &[String],
        url: Option<&str>,
        interval: Option<u64>,
    ) -> Result<(bool, String), String> {
        let name = normalize_group_name(name)?;
        let group_type = normalize_group_type(group_type)?;
        let proxies = normalize_group_proxies(proxies)?;
        let url = url.map(str::trim).filter(|v| !v.is_empty());
        if matches!(group_type.as_str(), "url-test" | "fallback" | "load-balance") {
            if let Some(raw) = url {
                normalize_http_url_field(raw)?;
            }
        } else if url.is_some() {
            return Err("只有 url-test / fallback / load-balance 才需要 url".into());
        }
        if let Some(sec) = interval {
            if !(1..=86_400).contains(&sec) {
                return Err("interval 须在 1..=86400 秒".into());
            }
            if group_type == "select" {
                return Err("select 组不要传 interval".into());
            }
        }

        let known = self.known_proxy_names()?;
        for member in &proxies {
            if member == &name {
                return Err("组成员不能指向自己".into());
            }
            if !known.contains(member) {
                return Err(format!(
                    "未知成员 {member}；须是叶子节点、已有代理组或 DIRECT/REJECT 等内置策略"
                ));
            }
        }

        let created = !self
            .effective_proxy_group_names()
            .iter()
            .any(|item| item == &name);

        let mut entry = serde_yaml::Mapping::new();
        entry.insert(
            Value::String("name".into()),
            Value::String(name.clone()),
        );
        entry.insert(
            Value::String("type".into()),
            Value::String(group_type.clone()),
        );
        entry.insert(
            Value::String("proxies".into()),
            Value::Sequence(proxies.iter().cloned().map(Value::String).collect()),
        );
        if matches!(group_type.as_str(), "url-test" | "fallback" | "load-balance") {
            let test_url = url
                .unwrap_or("http://www.gstatic.com/generate_204")
                .to_string();
            entry.insert(Value::String("url".into()), Value::String(test_url));
            let sec = interval.unwrap_or(300);
            entry.insert(
                Value::String("interval".into()),
                Value::Number(sec.into()),
            );
        }

        let mut list = self.proxy_group_upserts();
        list.retain(|item| group_entry_name(item).as_deref() != Some(name.as_str()));
        list.push(Value::Mapping(entry));
        self.write_proxy_group_upserts(&list)?;

        let summary = format!(
            "{name} type={group_type} proxies={}",
            proxies.join(", ")
        );
        Ok((created, summary))
    }

    pub fn proxy_group_upserts_text(&self) -> String {
        let list = self.proxy_group_upserts();
        if list.is_empty() {
            return "本地代理组覆盖: 无".into();
        }
        let mut lines = vec![format!("本地代理组覆盖 {} 个:", list.len())];
        for item in list {
            let name = group_entry_name(&item).unwrap_or_else(|| "?".into());
            let kind = mapping_get(&item, "type")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let proxies = mapping_get(&item, "proxies")
                .and_then(Value::as_sequence)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            lines.push(format!("- {name} type={kind} proxies={proxies}"));
        }
        lines.join("\n")
    }

    fn proxy_group_upserts(&self) -> Vec<Value> {
        self.read_override_value()
            .and_then(|doc| mapping_get(&doc, "proxy-group-upsert").cloned())
            .and_then(|value| value.as_sequence().cloned())
            .unwrap_or_default()
    }

    fn write_proxy_group_upserts(&self, list: &[Value]) -> Result<(), String> {
        self.patch_override(mapping_insert(
            "proxy-group-upsert",
            Value::Sequence(list.to_vec()),
        ))
    }

    fn effective_proxy_group_names(&self) -> Vec<String> {
        self.read_effective_value()
            .and_then(|doc| mapping_get(&doc, "proxy-groups").cloned())
            .and_then(|value| value.as_sequence().cloned())
            .map(|items| {
                items
                    .iter()
                    .filter_map(group_entry_name)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 叶子节点 + 生效代理组 + 内置策略，供组成员校验。
    fn known_proxy_names(&self) -> Result<std::collections::BTreeSet<String>, String> {
        let doc = self
            .read_effective_value()
            .ok_or_else(|| "没有生效配置".to_string())?;
        let mut names = std::collections::BTreeSet::new();
        for builtin in ["DIRECT", "REJECT", "REJECT-DROP", "PASS", "COMPATIBLE"] {
            names.insert(builtin.into());
        }
        if let Some(Value::Sequence(proxies)) = mapping_get(&doc, "proxies") {
            for item in proxies {
                if let Some(name) = mapping_get(item, "name").and_then(Value::as_str) {
                    names.insert(name.to_string());
                }
            }
        }
        if let Some(Value::Sequence(groups)) = mapping_get(&doc, "proxy-groups") {
            for item in groups {
                if let Some(name) = group_entry_name(item) {
                    names.insert(name);
                }
            }
        }
        // 尚未 rebuild 进 effective 的本地 upsert 也算已知组，便于互相引用。
        for item in self.proxy_group_upserts() {
            if let Some(name) = group_entry_name(&item) {
                names.insert(name);
            }
        }
        Ok(names)
    }

    /// 在订阅规则最前插入一条（刷新订阅不会冲掉）。Author: kejiqing
    pub fn prepend_rule(&self, raw: &str) -> Result<Vec<String>, String> {
        let rule = normalize_rule(raw)?;
        let mut list = self.prepend_rules();
        list.retain(|item| item != &rule);
        list.insert(0, rule);
        self.write_prepend_rules(&list)?;
        Ok(list)
    }

    pub fn remove_prepend_rule(&self, raw: &str) -> Result<Vec<String>, String> {
        let rule = normalize_rule(raw)?;
        let mut list = self.prepend_rules();
        let before = list.len();
        list.retain(|item| item != &rule);
        if list.len() == before {
            return Err(format!("前置规则里没有 {rule}"));
        }
        self.write_prepend_rules(&list)?;
        Ok(list)
    }

    pub fn prepend_rules(&self) -> Vec<String> {
        self.read_override_value()
            .and_then(|doc| mapping_get(&doc, "rule-prepend").cloned())
            .and_then(|value| value.as_sequence().cloned())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// TUN auto-route 排除网段（进 TUN 之前，不是 rules）。Author: kejiqing
    pub fn add_tun_route_exclude(&self, raw: &str) -> Result<Vec<String>, String> {
        let cidr = normalize_cidr(raw)?;
        let mut list = self.tun_route_excludes();
        list.retain(|item| item != &cidr);
        list.push(cidr);
        self.write_tun_route_excludes(&list)?;
        Ok(list)
    }

    pub fn remove_tun_route_exclude(&self, raw: &str) -> Result<Vec<String>, String> {
        let cidr = normalize_cidr(raw)?;
        let mut list = self.tun_route_excludes();
        let before = list.len();
        list.retain(|item| item != &cidr);
        if list.len() == before {
            return Err(format!("TUN 排除里没有 {cidr}"));
        }
        self.write_tun_route_excludes(&list)?;
        Ok(list)
    }

    pub fn tun_route_excludes(&self) -> Vec<String> {
        let Some(doc) = self.read_override_value() else {
            return default_tun_route_excludes();
        };
        let Some(tun) = mapping_get(&doc, "tun") else {
            return default_tun_route_excludes();
        };
        match mapping_get(tun, "route-exclude-address") {
            Some(Value::Sequence(items)) => items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
            Some(_) => Vec::new(),
            None => default_tun_route_excludes(),
        }
    }

    fn write_tun_route_excludes(&self, list: &[String]) -> Result<(), String> {
        let items = list
            .iter()
            .map(|item| Value::String(item.clone()))
            .collect();
        let tun = mapping_insert("route-exclude-address", Value::Sequence(items));
        self.patch_override(mapping_insert("tun", tun))
    }

    /// 合并进覆盖里的 dns 映射（数组字段整段替换，映射字段按 key 合并）。
    pub fn patch_dns(&self, dns: Value) -> Result<(), String> {
        if !matches!(dns, Value::Mapping(_)) {
            return Err("dns 必须是映射".into());
        }
        self.patch_override(mapping_insert("dns", dns))
    }

    pub fn dns_summary(&self) -> String {
        let Some(doc) = self.read_effective_value() else {
            return "没有生效配置".into();
        };
        let Some(dns) = mapping_get(&doc, "dns") else {
            return "dns: 未配置".into();
        };
        serde_yaml::to_string(dns).unwrap_or_else(|_| "dns: 无法序列化".into())
    }

    fn write_prepend_rules(&self, list: &[String]) -> Result<(), String> {
        let items = list
            .iter()
            .map(|item| Value::String(item.clone()))
            .collect();
        self.patch_override(mapping_insert(
            "rule-prepend",
            Value::Sequence(items),
        ))
    }

    fn read_override_value(&self) -> Option<Value> {
        let id = self.active_id()?;
        let path = profile_dir(&self.lock().root, &id).join("override.yaml");
        let text = fs::read_to_string(path).ok()?;
        serde_yaml::from_str(&text).ok()
    }

    fn meta_of(&self, id: &str) -> Result<ProfileMeta, String> {
        let guard = self.lock();
        let meta = read_meta(&profile_dir(&guard.root, id).join("meta.json"))?;
        Ok(ProfileMeta {
            active: guard.state.active_id.as_deref() == Some(id),
            id: meta.id,
            name: meta.name,
            url: meta.url,
            updated_at: meta.updated_at,
        })
    }

    fn require_active(&self) -> Result<String, String> {
        self.active_id().ok_or_else(|| "没有正在使用的订阅".into())
    }

    fn yaml_string_field(&self, key: &str) -> Option<String> {
        let value = self.read_effective_value()?;
        mapping_get(&value, key)?.as_str().map(str::to_string)
    }

    fn yaml_u16_field(&self, key: &str) -> Option<u16> {
        let value = self.read_effective_value()?;
        as_u16(mapping_get(&value, key)?)
    }

    fn read_effective_value(&self) -> Option<Value> {
        let id = self.active_id()?;
        let path = profile_dir(&self.lock().root, &id).join("effective.yaml");
        let text = fs::read_to_string(path).ok()?;
        serde_yaml::from_str(&text).ok()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StoreInner> {
        self.inner.lock().expect("订阅存储锁已损坏")
    }
}

pub fn parse_subscription(body: &str) -> Result<String, String> {
    let text = strip_bom(body).trim();
    if text.is_empty() {
        return Err("订阅为空".into());
    }
    if yaml_mapping(text).is_some() {
        return Ok(text.to_string());
    }
    if let Some(bytes) = decode_b64(text) {
        let decoded = String::from_utf8(bytes).map_err(|_| "订阅不是 UTF-8 文本".to_string())?;
        let decoded = strip_bom(&decoded).trim().to_string();
        if yaml_mapping(&decoded).is_some() {
            return Ok(decoded);
        }
    }
    Err("订阅不是 Clash/mihomo YAML".into())
}

pub fn deep_merge(base: &mut Value, overlay: Value) {
    match overlay {
        Value::Mapping(overlay_map) => {
            if !matches!(base, Value::Mapping(_)) {
                *base = Value::Mapping(serde_yaml::Mapping::new());
            }
            let Value::Mapping(base_map) = base else {
                unreachable!();
            };
            for (key, value) in overlay_map {
                if let Some(existing) = base_map.get_mut(&key) {
                    deep_merge(existing, value);
                } else {
                    base_map.insert(key, value);
                }
            }
        }
        other => *base = other,
    }
}

pub fn lock_controller(doc: &mut Value, port: u16, secret: &str) -> Result<(), String> {
    let Value::Mapping(map) = doc else {
        return Err("配置必须是 YAML 映射".into());
    };
    map.insert(
        Value::String("external-controller".into()),
        Value::String(format!("127.0.0.1:{port}")),
    );
    map.insert(
        Value::String("secret".into()),
        Value::String(secret.to_string()),
    );
    for key in STRIP_KEYS {
        map.remove(Value::String((*key).to_string()));
    }
    // GEOIP 规则走 MMDB。geodata-mode 打开时会改拉 GitHub 的 geoip.dat，本地没有这份 dat。
    map.insert(Value::String("geodata-mode".into()), Value::Bool(false));
    lock_geox(map);
    Ok(())
}

fn lock_geox(map: &mut serde_yaml::Mapping) {
    let key = Value::String("geox-url".into());
    if !matches!(map.get(&key), Some(Value::Mapping(_))) {
        map.insert(key.clone(), Value::Mapping(serde_yaml::Mapping::new()));
    }
    let Some(Value::Mapping(geox)) = map.get_mut(&key) else {
        return;
    };
    geox.insert(
        Value::String("mmdb".into()),
        Value::String(NORA_MMDB.into()),
    );
    geox.insert(Value::String("asn".into()), Value::String(NORA_ASN.into()));
}

/// 默认走 TUN 虚拟网卡（Windows/Android 默认关，走系统代理）。覆盖里写明 tun.enable: false 时才关掉。
///
/// 两层防环，职责分开：
/// - **拨号层**（本函数）：auto-detect-interface + gvisor + strict-route=false，
///   护住 mihomo 自己连节点 / 测速，不进 TUN 套娃。
/// - **路由层**（`ensure_proxy_server_route_excludes`）：把 `proxies[].server` 字面量 IP
///   并入 route-exclude-address，护住本机 SSH/直连节点 IP 不进 TUN。
/// Author: kejiqing
fn lock_tun(doc: &mut Value, override_text: &str) {
    if override_disables_tun(override_text) {
        return;
    }
    let Value::Mapping(map) = doc else {
        return;
    };
    let key = Value::String("tun".into());
    if !matches!(map.get(&key), Some(Value::Mapping(_))) {
        map.insert(key.clone(), Value::Mapping(serde_yaml::Mapping::new()));
    }
    let Some(Value::Mapping(tun)) = map.get_mut(&key) else {
        return;
    };
    // Windows/Android 本期无 VpnService/wintun，默认关 TUN；macOS/Linux 默认开。
    let enable = !cfg!(windows) && !cfg!(target_os = "android");
    tun.insert(Value::String("enable".into()), Value::Bool(enable));
    tun.insert(Value::String("auto-route".into()), Value::Bool(true));
    tun.insert(
        Value::String("auto-detect-interface".into()),
        Value::Bool(true),
    );
    tun.insert(Value::String("stack".into()), Value::String("gvisor".into()));
    tun.insert(Value::String("strict-route".into()), Value::Bool(false));
    ensure_tun_dns_hijack(tun);
    ensure_default_tun_route_exclude(tun, override_text);
}

/// 把订阅 `proxies[].server` 里可解析的字面量 IP 并入 `tun.route-exclude-address`。
/// 只改生效文档，不写 override；域名型 server 本轮跳过。Author: kejiqing
fn ensure_proxy_server_route_excludes(doc: &mut Value) {
    let servers = collect_proxy_server_cidrs(doc);
    if servers.is_empty() {
        return;
    }
    if mapping_get(doc, "tun")
        .and_then(|tun| mapping_get(tun, "enable"))
        .and_then(|v| v.as_bool())
        == Some(false)
    {
        return;
    }
    let Value::Mapping(map) = doc else {
        return;
    };
    let key = Value::String("tun".into());
    if !matches!(map.get(&key), Some(Value::Mapping(_))) {
        return;
    }
    let Some(Value::Mapping(tun)) = map.get_mut(&key) else {
        return;
    };
    let exclude_key = Value::String("route-exclude-address".into());
    match tun.get_mut(&exclude_key) {
        Some(Value::Sequence(seq)) => {
            for cidr in servers {
                if !seq.iter().any(|item| item.as_str() == Some(cidr.as_str())) {
                    seq.push(Value::String(cidr));
                }
            }
        }
        _ => {
            tun.insert(
                exclude_key,
                Value::Sequence(servers.into_iter().map(Value::String).collect()),
            );
        }
    }
}

fn collect_proxy_server_cidrs(doc: &Value) -> Vec<String> {
    let Some(proxies) = mapping_get(doc, "proxies").and_then(|v| v.as_sequence()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in proxies {
        let Some(server) = mapping_get(item, "server").and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok(cidr) = normalize_cidr(server) else {
            continue;
        };
        if !out.iter().any(|have| have == &cidr) {
            out.push(cidr);
        }
    }
    out
}

/// 未配置 dns-hijack 时补 Verge 同款 any:53，保证 TUN 下 DNS 仍进内核。author: kejiqing
fn ensure_tun_dns_hijack(tun: &mut serde_yaml::Mapping) {
    let key = Value::String("dns-hijack".into());
    match tun.get(&key) {
        Some(Value::Sequence(seq)) if !seq.is_empty() => {}
        _ => {
            tun.insert(
                key,
                Value::Sequence(vec![Value::String("any:53".into())]),
            );
        }
    }
}

fn default_tun_route_excludes() -> Vec<String> {
    vec![DEFAULT_TUN_ROUTE_EXCLUDE.into()]
}

/// 覆盖未声明 `route-exclude-address` 时，生效配置默认排除 10.8.0.0/16。
/// 一旦覆盖写过该键（含空列表），以覆盖为准，不再塞默认。Author: kejiqing
fn ensure_default_tun_route_exclude(tun: &mut serde_yaml::Mapping, override_text: &str) {
    if override_mentions_route_exclude(override_text) {
        return;
    }
    let key = Value::String("route-exclude-address".into());
    let default = Value::String(DEFAULT_TUN_ROUTE_EXCLUDE.into());
    match tun.get_mut(&key) {
        Some(Value::Sequence(seq)) => {
            if !seq
                .iter()
                .any(|item| item.as_str() == Some(DEFAULT_TUN_ROUTE_EXCLUDE))
            {
                seq.insert(0, default);
            }
        }
        _ => {
            tun.insert(key, Value::Sequence(vec![default]));
        }
    }
}

fn override_mentions_route_exclude(text: &str) -> bool {
    let Ok(value) = serde_yaml::from_str::<Value>(text) else {
        return false;
    };
    mapping_get(&value, "tun")
        .and_then(|tun| mapping_get(tun, "route-exclude-address"))
        .is_some()
}

/// 覆盖里的 `rule-prepend` 插到 rules 最前，并从文档删除该键（mihomo 不认）。
/// Author: kejiqing
fn apply_rule_prepend(doc: &mut Value) {
    let Value::Mapping(map) = doc else {
        return;
    };
    let key = Value::String("rule-prepend".into());
    let Some(raw) = map.remove(&key) else {
        return;
    };
    let Some(extra) = raw.as_sequence() else {
        return;
    };
    let mut front = Vec::new();
    for item in extra {
        if let Some(text) = item.as_str() {
            if let Ok(rule) = normalize_rule(text) {
                if !front.iter().any(|have: &String| have == &rule) {
                    front.push(rule);
                }
            }
        }
    }
    if front.is_empty() {
        return;
    }
    let rules_key = Value::String("rules".into());
    let mut merged: Vec<Value> = front.into_iter().map(Value::String).collect();
    if let Some(Value::Sequence(existing)) = map.get(&rules_key) {
        for item in existing {
            let text = item.as_str().unwrap_or("");
            if merged.iter().any(|have| have.as_str() == Some(text)) {
                continue;
            }
            merged.push(item.clone());
        }
    }
    map.insert(rules_key, Value::Sequence(merged));
}

/// 覆盖里的 `proxy-group-upsert` 按 name 合并进 proxy-groups，并从文档删除该键。
/// Author: kejiqing
fn apply_proxy_group_upsert(doc: &mut Value) {
    let Value::Mapping(map) = doc else {
        return;
    };
    let key = Value::String("proxy-group-upsert".into());
    let Some(raw) = map.remove(&key) else {
        return;
    };
    let Some(extra) = raw.as_sequence() else {
        return;
    };
    let mut upserts = Vec::new();
    for item in extra {
        if !matches!(item, Value::Mapping(_)) {
            continue;
        }
        if group_entry_name(item).is_none() {
            continue;
        }
        upserts.push(item.clone());
    }
    if upserts.is_empty() {
        return;
    }
    let groups_key = Value::String("proxy-groups".into());
    let mut merged: Vec<Value> = match map.get(&groups_key) {
        Some(Value::Sequence(existing)) => existing.clone(),
        _ => Vec::new(),
    };
    for upsert in upserts {
        let Some(name) = group_entry_name(&upsert) else {
            continue;
        };
        if let Some(idx) = merged
            .iter()
            .position(|item| group_entry_name(item).as_deref() == Some(name.as_str()))
        {
            merged[idx] = upsert;
        } else {
            merged.push(upsert);
        }
    }
    map.insert(groups_key, Value::Sequence(merged));
}

fn group_entry_name(item: &Value) -> Option<String> {
    mapping_get(item, "name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn normalize_group_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() || name.contains('\n') || name.chars().count() > 64 {
        return Err("代理组名无效".into());
    }
    if name.contains(',') {
        return Err("代理组名不能含逗号".into());
    }
    const RESERVED: &[&str] = &["DIRECT", "REJECT", "REJECT-DROP", "PASS", "COMPATIBLE"];
    if RESERVED.iter().any(|item| item.eq_ignore_ascii_case(name)) {
        return Err(format!("不能使用内置策略名 {name}"));
    }
    Ok(name.to_string())
}

fn normalize_group_type(raw: &str) -> Result<String, String> {
    let kind = raw.trim().to_ascii_lowercase();
    match kind.as_str() {
        "select" | "url-test" | "fallback" | "load-balance" => Ok(kind),
        "" => Ok("select".into()),
        other => Err(format!(
            "不支持的代理组类型 {other}；可选 select / url-test / fallback / load-balance"
        )),
    }
}

fn normalize_group_proxies(raw: &[String]) -> Result<Vec<String>, String> {
    if raw.is_empty() {
        return Err("proxies 不能为空".into());
    }
    if raw.len() > 200 {
        return Err("proxies 过多".into());
    }
    let mut out = Vec::new();
    for item in raw {
        let name = item.trim();
        if name.is_empty() || name.contains('\n') || name.chars().count() > 64 {
            return Err(format!("无效成员名: {item}"));
        }
        if out.iter().any(|have: &String| have == name) {
            continue;
        }
        out.push(name.to_string());
    }
    if out.is_empty() {
        return Err("proxies 不能为空".into());
    }
    Ok(out)
}

fn normalize_http_url_field(raw: &str) -> Result<(), String> {
    let url = raw.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("url 只支持 http 或 https".into());
    }
    if url.chars().any(|ch| ch.is_whitespace()) || url.chars().count() > 200 {
        return Err("url 无效".into());
    }
    Ok(())
}

/// 接受 IPv4/IPv6 CIDR，或单 IP（补 /32、/128）。给 TUN route-exclude 用。
/// Author: kejiqing
fn normalize_cidr(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains('\n') || raw.contains(' ') || raw.chars().count() > 64 {
        return Err("CIDR 无效".into());
    }
    let (addr, prefix) = match raw.split_once('/') {
        Some((addr, prefix)) => (addr.trim(), Some(prefix.trim())),
        None => (raw, None),
    };
    if addr.is_empty() {
        return Err("CIDR 无效".into());
    }
    if addr.contains(':') {
        let ip: std::net::Ipv6Addr = addr
            .parse()
            .map_err(|_| format!("不是合法 IPv6: {addr}"))?;
        let bits = match prefix {
            Some(text) => text
                .parse::<u8>()
                .ok()
                .filter(|n| *n <= 128)
                .ok_or_else(|| format!("IPv6 前缀无效: {text}"))?,
            None => 128,
        };
        Ok(format!("{ip}/{bits}"))
    } else {
        let ip: std::net::Ipv4Addr = addr
            .parse()
            .map_err(|_| format!("不是合法 IPv4: {addr}"))?;
        let bits = match prefix {
            Some(text) => text
                .parse::<u8>()
                .ok()
                .filter(|n| *n <= 32)
                .ok_or_else(|| format!("IPv4 前缀无效: {text}"))?,
            None => 32,
        };
        Ok(format!("{ip}/{bits}"))
    }
}

fn normalize_rule(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.contains('\n') || raw.chars().count() > 180 {
        return Err("规则无效".into());
    }
    let parts: Vec<&str> = raw.splitn(4, ',').map(str::trim).collect();
    if parts.len() < 3 {
        return Err("规则形如 TYPE,匹配,策略".into());
    }
    const KINDS: &[&str] = &[
        "DOMAIN",
        "DOMAIN-SUFFIX",
        "DOMAIN-KEYWORD",
        "DOMAIN-REGEX",
        "GEOSITE",
        "GEOIP",
        "IP-CIDR",
        "IP-CIDR6",
        "SRC-IP-CIDR",
        "DST-PORT",
        "SRC-PORT",
        "PROCESS-NAME",
        "MATCH",
    ];
    let kind = parts[0].to_ascii_uppercase();
    if !KINDS.contains(&kind.as_str()) {
        return Err(format!("不支持的规则类型 {kind}"));
    }
    if parts[1].is_empty() || parts[2].is_empty() {
        return Err("规则的匹配和策略不能为空".into());
    }
    if parts[1].contains(' ') {
        return Err("匹配段不能含空格".into());
    }
    let mut line = format!("{kind},{},{}", parts[1], parts[2]);
    if parts.len() == 4 && !parts[3].is_empty() {
        line.push(',');
        line.push_str(parts[3]);
    }
    Ok(line)
}

fn mapping_insert(key: &str, value: Value) -> Value {
    let mut map = serde_yaml::Mapping::new();
    map.insert(Value::String(key.into()), value);
    Value::Mapping(map)
}

fn override_disables_tun(text: &str) -> bool {
    let Ok(value) = serde_yaml::from_str::<Value>(text) else {
        return false;
    };
    mapping_get(&value, "tun")
        .and_then(|tun| mapping_get(tun, "enable"))
        .and_then(Value::as_bool)
        == Some(false)
}

fn yaml_mapping(text: &str) -> Option<Value> {
    let value: Value = serde_yaml::from_str(text).ok()?;
    matches!(value, Value::Mapping(_)).then_some(value)
}

fn decode_b64(raw: &str) -> Option<Vec<u8>> {
    let compact: String = raw.chars().filter(|ch| !ch.is_whitespace()).collect();
    if compact.len() < 16 {
        return None;
    }
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&compact) {
        return Some(bytes);
    }
    if let Ok(bytes) = base64::engine::general_purpose::STANDARD_NO_PAD.decode(&compact) {
        return Some(bytes);
    }
    if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE.decode(&compact) {
        return Some(bytes);
    }
    if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&compact) {
        return Some(bytes);
    }
    None
}

fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

pub fn ensure_http_url(url: &str) -> Result<(), String> {
    let url = url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("订阅地址只支持 http 或 https".into());
    }
    if url.chars().any(|ch| ch.is_whitespace()) {
        return Err("订阅地址含有空白".into());
    }
    Ok(())
}

fn ensure_id(id: &str) -> Result<(), String> {
    Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| "订阅 id 无效".to_string())
}

async fn fetch_subscription(url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|err| err.to_string())?;
    let response = client
        .get(url)
        .header("user-agent", "clash.meta")
        .send()
        .await
        .map_err(|err| format!("订阅下载失败: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("订阅下载失败: HTTP {}", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|err| format!("订阅下载失败: {err}"))?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("订阅过大".into());
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| "订阅不是 UTF-8 文本".to_string())
}

fn find_by_url(guard: &StoreInner, url: &str) -> Result<Option<MetaFile>, String> {
    let dir = guard.root.join("profiles");
    let entries = fs::read_dir(&dir).map_err(|err| err.to_string())?;
    for entry in entries.flatten() {
        let meta_path = entry.path().join("meta.json");
        if let Ok(meta) = read_meta(&meta_path) {
            if meta.url == url {
                return Ok(Some(meta));
            }
        }
    }
    Ok(None)
}

fn profile_dir(root: &Path, id: &str) -> PathBuf {
    root.join("profiles").join(id)
}

fn load_state(path: &Path) -> Result<AppConfig, String> {
    if !path.exists() {
        return Ok(AppConfig::new());
    }
    let text = fs::read_to_string(path).map_err(|err| err.to_string())?;
    let state: AppConfig = serde_json::from_str(&text).map_err(|err| format!("state.json 无效: {err}"))?;
    if state.secret.len() < 16 {
        return Err("state.json 中的 secret 无效".into());
    }
    if state.controller_port == 0 {
        return Err("state.json 中的控制器端口无效".into());
    }
    Ok(state)
}

fn write_state(path: &Path, state: &AppConfig) -> Result<(), String> {
    let text = serde_json::to_string_pretty(state).map_err(|err| err.to_string())?;
    write_private(path, text.as_bytes())
}

fn read_meta(path: &Path) -> Result<MetaFile, String> {
    let text = fs::read_to_string(path).map_err(|err| err.to_string())?;
    serde_json::from_str(&text).map_err(|err| err.to_string())
}

fn write_meta(path: &Path, meta: &MetaFile) -> Result<(), String> {
    let text = serde_json::to_string_pretty(meta).map_err(|err| err.to_string())?;
    write_private(path, text.as_bytes())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(|err| err.to_string())?;
    fs::rename(&tmp, path).map_err(|err| err.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn now_stamp() -> String {
    // 统一东八区展示，避免 UI 出现带 Z 的 UTC。Author: kejiqing
    let cst = FixedOffset::east_opt(8 * 3600).expect("+08:00");
    chrono::Utc::now()
        .with_timezone(&cst)
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn clean_name(name: &str) -> String {
    let text = name.replace(['\n', '\r'], " ");
    let text = text.trim();
    if text.is_empty() {
        "订阅".into()
    } else {
        text.chars().take(80).collect()
    }
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.split('@').next_back()?;
    let host = host.split(':').next().unwrap_or(host);
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

fn mapping_get<'a>(doc: &'a Value, key: &str) -> Option<&'a Value> {
    doc.as_mapping()?.get(Value::String(key.to_string()))
}

fn as_u16(value: &Value) -> Option<u16> {
    value.as_u64().and_then(|number| u16::try_from(number).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("supone-profile-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn parse_yaml_and_base64_and_reject_noise() {
        let yaml = "proxies: []\nrules:\n  - MATCH,DIRECT\n";
        assert_eq!(parse_subscription(yaml).unwrap(), yaml.trim());
        let encoded = base64::engine::general_purpose::STANDARD.encode(yaml);
        let decoded = parse_subscription(&encoded).unwrap();
        assert!(yaml_mapping(&decoded).is_some());
        assert!(parse_subscription("this is not yaml").is_err());
    }

    #[test]
    fn merge_replaces_arrays_and_locks_controller() {
        let tmp = Tmp::new();
        let store = ProfileStore::open(tmp.0.clone()).unwrap();
        let remote = r#"
mixed-port: 7890
rules:
  - A
tun:
  enable: false
  stack: gvisor
external-controller: 0.0.0.0:9090
external-ui: ./ui
external-controller-unix: /tmp/mihomo.sock
secret: from-sub
geodata-mode: true
geox-url:
  mmdb: https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geoip.metadb
  asn: https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/GeoLite2-ASN.mmdb
  geosite: https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geosite.dat
"#;
        let meta = store
            .import_parsed(
                "https://example.com/sub?token=abc",
                Some("示例".into()),
                remote,
            )
            .unwrap();
        assert!(meta.active);
        store
            .write_override(
                "mixed-port: 7891\nrules:\n  - B\ntun:\n  enable: true\nexternal-controller: 0.0.0.0:1\nsecret: leak\n",
            )
            .unwrap();
        let text = fs::read_to_string(
            tmp.0
                .join("profiles")
                .join(&meta.id)
                .join("effective.yaml"),
        )
        .unwrap();
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        assert_eq!(mapping_get(&doc, "mixed-port").and_then(as_u16), Some(7891));
        let rules = mapping_get(&doc, "rules").unwrap().as_sequence().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].as_str(), Some("B"));
        store.reset_override(None).unwrap();
        let override_text = fs::read_to_string(
            tmp.0
                .join("profiles")
                .join(&meta.id)
                .join("override.yaml"),
        )
        .unwrap();
        assert_eq!(override_text.trim(), "{}");
        let text = fs::read_to_string(
            tmp.0
                .join("profiles")
                .join(&meta.id)
                .join("effective.yaml"),
        )
        .unwrap();
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        let rules = mapping_get(&doc, "rules").unwrap().as_sequence().unwrap();
        assert_eq!(rules[0].as_str(), Some("A"));
        let tun = mapping_get(&doc, "tun").unwrap();
        assert_eq!(mapping_get(tun, "enable").unwrap().as_bool(), Some(true));
        assert_eq!(mapping_get(tun, "stack").unwrap().as_str(), Some("gvisor"));
        assert_eq!(
            mapping_get(tun, "auto-route").unwrap().as_bool(),
            Some(true)
        );
        assert_eq!(
            mapping_get(tun, "auto-detect-interface").unwrap().as_bool(),
            Some(true)
        );
        assert_eq!(
            mapping_get(tun, "strict-route").unwrap().as_bool(),
            Some(false)
        );
        let hijack = mapping_get(tun, "dns-hijack")
            .and_then(|v| v.as_sequence())
            .unwrap();
        assert_eq!(hijack[0].as_str(), Some("any:53"));
        assert_eq!(
            mapping_get(&doc, "external-controller").unwrap().as_str(),
            Some("127.0.0.1:9090")
        );
        assert_eq!(
            mapping_get(&doc, "secret").unwrap().as_str(),
            Some(store.secret().as_str())
        );
        assert!(mapping_get(&doc, "external-ui").is_none());
        assert!(mapping_get(&doc, "external-controller-unix").is_none());
        assert_eq!(
            mapping_get(&doc, "geodata-mode").unwrap().as_bool(),
            Some(false)
        );
        let geox = mapping_get(&doc, "geox-url").unwrap();
        assert_eq!(
            mapping_get(geox, "mmdb").unwrap().as_str(),
            Some("https://nora.home.passionke.top/raw/geoip/dbip-country-lite.mmdb")
        );
        assert_eq!(
            mapping_get(geox, "asn").unwrap().as_str(),
            Some("https://nora.home.passionke.top/raw/geoip/dbip-asn-lite.mmdb")
        );
        assert_eq!(
            mapping_get(geox, "geosite").unwrap().as_str(),
            Some("https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geosite.dat")
        );
    }

    #[test]
    fn effective_file_rewrites_stale_geoip_url() {
        let tmp = Tmp::new();
        let store = ProfileStore::open(tmp.0.clone()).unwrap();
        let meta = store
            .import_parsed(
                "https://example.com/sub",
                Some("示例".into()),
                "mixed-port: 7890\nrules:\n  - GEOIP,CN,DIRECT\n",
            )
            .unwrap();
        let path = tmp
            .0
            .join("profiles")
            .join(&meta.id)
            .join("effective.yaml");
        fs::write(
            &path,
            "mixed-port: 7890\nrules:\n  - GEOIP,CN,DIRECT\n",
        )
        .unwrap();
        let out = store.effective_file().unwrap();
        let text = fs::read_to_string(out).unwrap();
        assert!(text.contains(
            "https://nora.home.passionke.top/raw/geoip/dbip-country-lite.mmdb"
        ));
        assert!(!text.contains("github.com/MetaCubeX/meta-rules-dat"));
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        let tun = mapping_get(&doc, "tun").unwrap();
        assert_eq!(mapping_get(tun, "enable").unwrap().as_bool(), Some(true));
        assert_eq!(
            mapping_get(tun, "auto-route").unwrap().as_bool(),
            Some(true)
        );
    }

    #[test]
    fn subscription_cannot_turn_tun_off() {
        let tmp = Tmp::new();
        let store = ProfileStore::open(tmp.0.clone()).unwrap();
        let meta = store
            .import_parsed(
                "https://example.com/sub",
                Some("示例".into()),
                "tun:\n  enable: false\n  stack: gvisor\n",
            )
            .unwrap();
        let text = fs::read_to_string(
            tmp.0
                .join("profiles")
                .join(&meta.id)
                .join("effective.yaml"),
        )
        .unwrap();
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        let tun = mapping_get(&doc, "tun").unwrap();
        assert_eq!(mapping_get(tun, "enable").unwrap().as_bool(), Some(true));
        assert_eq!(mapping_get(tun, "stack").unwrap().as_str(), Some("gvisor"));
    }

    #[test]
    fn override_can_turn_tun_off() {
        let tmp = Tmp::new();
        let store = ProfileStore::open(tmp.0.clone()).unwrap();
        store
            .import_parsed(
                "https://example.com/sub",
                Some("示例".into()),
                "mixed-port: 7890\n",
            )
            .unwrap();
        store.write_override("tun:\n  enable: false\n").unwrap();
        let text = fs::read_to_string(store.effective_file().unwrap()).unwrap();
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        let tun = mapping_get(&doc, "tun").unwrap();
        assert_eq!(mapping_get(tun, "enable").unwrap().as_bool(), Some(false));
    }

    #[test]
    fn prepend_rule_stays_in_front_and_is_not_a_mihomo_key() {
        let tmp = Tmp::new();
        let store = ProfileStore::open(tmp.0.clone()).unwrap();
        store
            .import_parsed(
                "https://example.com/sub",
                Some("示例".into()),
                "rules:\n  - MATCH,PROXY\n",
            )
            .unwrap();
        store
            .prepend_rule("domain-suffix,jd.com,DIRECT")
            .unwrap();
        let text = fs::read_to_string(store.effective_file().unwrap()).unwrap();
        assert!(!text.contains("rule-prepend"));
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        let rules = mapping_get(&doc, "rules").unwrap().as_sequence().unwrap();
        assert_eq!(rules[0].as_str(), Some("DOMAIN-SUFFIX,jd.com,DIRECT"));
        assert_eq!(rules[1].as_str(), Some("MATCH,PROXY"));
    }

    #[test]
    fn default_tun_route_exclude_is_10_8_when_override_silent() {
        let tmp = Tmp::new();
        let store = ProfileStore::open(tmp.0.clone()).unwrap();
        store
            .import_parsed(
                "https://example.com/sub",
                Some("示例".into()),
                "rules:\n  - MATCH,PROXY\n",
            )
            .unwrap();
        assert_eq!(
            store.tun_route_excludes(),
            vec!["10.8.0.0/16".to_string()]
        );
        let text = fs::read_to_string(store.effective_file().unwrap()).unwrap();
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        let tun = mapping_get(&doc, "tun").unwrap();
        let excludes = mapping_get(tun, "route-exclude-address")
            .unwrap()
            .as_sequence()
            .unwrap();
        assert_eq!(excludes[0].as_str(), Some("10.8.0.0/16"));
    }

    #[test]
    fn lock_tun_sets_verge_equivalent_dialer_guards() {
        // 订阅无 tun 块时，生效配置仍须带齐 Verge 同款拨号防回环字段。author: kejiqing
        let tmp = Tmp::new();
        let store = ProfileStore::open(tmp.0.clone()).unwrap();
        store
            .import_parsed(
                "https://example.com/sub",
                Some("示例".into()),
                "proxies:\n  - name: sg\n    type: vless\n    server: 43.106.56.233\n    port: 32423\nrules:\n  - MATCH,PROXY\n",
            )
            .unwrap();
        let text = fs::read_to_string(store.effective_file().unwrap()).unwrap();
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        let tun = mapping_get(&doc, "tun").unwrap();
        assert_eq!(mapping_get(tun, "enable").unwrap().as_bool(), Some(true));
        assert_eq!(
            mapping_get(tun, "auto-detect-interface").unwrap().as_bool(),
            Some(true)
        );
        assert_eq!(mapping_get(tun, "stack").unwrap().as_str(), Some("gvisor"));
        assert_eq!(
            mapping_get(tun, "strict-route").unwrap().as_bool(),
            Some(false)
        );
        let hijack = mapping_get(tun, "dns-hijack")
            .unwrap()
            .as_sequence()
            .unwrap();
        assert_eq!(hijack[0].as_str(), Some("any:53"));
        let excludes = mapping_get(tun, "route-exclude-address")
            .unwrap()
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|item| item.as_str())
            .collect::<Vec<_>>();
        assert!(
            excludes.contains(&"10.8.0.0/16"),
            "{excludes:?}"
        );
        assert!(
            excludes.contains(&"43.106.56.233/32"),
            "{excludes:?}"
        );
    }

    #[test]
    fn proxy_server_ips_auto_join_route_exclude_even_if_override_cleared() {
        // 覆盖显式清空默认 10.8 后，订阅节点 IP 仍须出现在 effective。Author: kejiqing
        let tmp = Tmp::new();
        let store = ProfileStore::open(tmp.0.clone()).unwrap();
        store
            .import_parsed(
                "https://example.com/sub",
                Some("示例".into()),
                "proxies:\n  - name: sg\n    type: vless\n    server: 43.106.56.233\n    port: 1\n  - name: hosty\n    type: vless\n    server: node.example.com\n    port: 1\nrules:\n  - MATCH,PROXY\n",
            )
            .unwrap();
        store.remove_tun_route_exclude("10.8.0.0/16").unwrap();
        assert!(store.tun_route_excludes().is_empty());
        let text = fs::read_to_string(store.effective_file().unwrap()).unwrap();
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        let tun = mapping_get(&doc, "tun").unwrap();
        let excludes = mapping_get(tun, "route-exclude-address")
            .unwrap()
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|item| item.as_str())
            .collect::<Vec<_>>();
        assert_eq!(excludes, vec!["43.106.56.233/32"]);
    }

    #[test]
    fn tun_route_exclude_writes_mihomo_key_not_rules() {
        let tmp = Tmp::new();
        let store = ProfileStore::open(tmp.0.clone()).unwrap();
        store
            .import_parsed(
                "https://example.com/sub",
                Some("示例".into()),
                "rules:\n  - MATCH,PROXY\n",
            )
            .unwrap();
        store.add_tun_route_exclude("10.8.0.0/24").unwrap();
        store.add_tun_route_exclude("1.2.3.4").unwrap();
        let text = fs::read_to_string(store.effective_file().unwrap()).unwrap();
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        let tun = mapping_get(&doc, "tun").unwrap();
        let excludes = mapping_get(tun, "route-exclude-address")
            .unwrap()
            .as_sequence()
            .unwrap();
        assert_eq!(excludes[0].as_str(), Some("10.8.0.0/16"));
        assert_eq!(excludes[1].as_str(), Some("10.8.0.0/24"));
        assert_eq!(excludes[2].as_str(), Some("1.2.3.4/32"));
        let rules = mapping_get(&doc, "rules").unwrap().as_sequence().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].as_str(), Some("MATCH,PROXY"));
        store.remove_tun_route_exclude("10.8.0.0/16").unwrap();
        store.remove_tun_route_exclude("10.8.0.0/24").unwrap();
        assert_eq!(store.tun_route_excludes(), vec!["1.2.3.4/32".to_string()]);
    }

    #[test]
    fn clearing_tun_route_exclude_keeps_default_off() {
        let tmp = Tmp::new();
        let store = ProfileStore::open(tmp.0.clone()).unwrap();
        store
            .import_parsed(
                "https://example.com/sub",
                Some("示例".into()),
                "mixed-port: 7890\n",
            )
            .unwrap();
        store.remove_tun_route_exclude("10.8.0.0/16").unwrap();
        assert!(store.tun_route_excludes().is_empty());
        let text = fs::read_to_string(store.effective_file().unwrap()).unwrap();
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        let tun = mapping_get(&doc, "tun").unwrap();
        let excludes = mapping_get(tun, "route-exclude-address")
            .unwrap()
            .as_sequence()
            .unwrap();
        assert!(excludes.is_empty());
    }

    #[test]
    fn upsert_proxy_group_creates_and_edits_by_name() {
        // 本地代理组写入 override，按名合并进生效 proxy-groups；刷新订阅不冲掉。
        // Author: kejiqing
        let tmp = Tmp::new();
        let store = ProfileStore::open(tmp.0.clone()).unwrap();
        store
            .import_parsed(
                "https://example.com/sub",
                Some("示例".into()),
                r#"
proxies:
  - name: sg
    type: vless
    server: 1.1.1.1
    port: 1
  - name: jp
    type: vless
    server: 1.0.0.1
    port: 1
proxy-groups:
  - name: PROXY
    type: select
    proxies:
      - sg
      - jp
rules:
  - MATCH,PROXY
"#,
            )
            .unwrap();

        let (created, _) = store
            .upsert_proxy_group(
                "CURSOR",
                "select",
                &["sg".into(), "DIRECT".into()],
                None,
                None,
            )
            .unwrap();
        assert!(created);

        let text = fs::read_to_string(store.effective_file().unwrap()).unwrap();
        assert!(!text.contains("proxy-group-upsert"));
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        let groups = mapping_get(&doc, "proxy-groups")
            .unwrap()
            .as_sequence()
            .unwrap();
        assert_eq!(groups.len(), 2);
        let cursor = groups
            .iter()
            .find(|item| group_entry_name(item).as_deref() == Some("CURSOR"))
            .unwrap();
        assert_eq!(
            mapping_get(cursor, "type").and_then(Value::as_str),
            Some("select")
        );
        let proxies = mapping_get(cursor, "proxies")
            .unwrap()
            .as_sequence()
            .unwrap();
        assert_eq!(proxies[0].as_str(), Some("sg"));
        assert_eq!(proxies[1].as_str(), Some("DIRECT"));

        let (created_again, _) = store
            .upsert_proxy_group(
                "PROXY",
                "select",
                &["jp".into(), "DIRECT".into()],
                None,
                None,
            )
            .unwrap();
        assert!(!created_again);
        let text = fs::read_to_string(store.effective_file().unwrap()).unwrap();
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        let groups = mapping_get(&doc, "proxy-groups")
            .unwrap()
            .as_sequence()
            .unwrap();
        let proxy = groups
            .iter()
            .find(|item| group_entry_name(item).as_deref() == Some("PROXY"))
            .unwrap();
        let proxies = mapping_get(proxy, "proxies")
            .unwrap()
            .as_sequence()
            .unwrap();
        assert_eq!(proxies[0].as_str(), Some("jp"));
        assert_eq!(proxies[1].as_str(), Some("DIRECT"));

        assert!(store
            .upsert_proxy_group("BAD", "select", &["no-such".into()], None, None)
            .is_err());
    }
}
