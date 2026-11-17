// 作者: kejiqing
//! 界面和 agent 共用的壳内服务。配置只经订阅合并后交给 mihomo。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_yaml::Value;

use crate::corebin;
use crate::profile::ProfileStore;
use crate::netenv::{self, NetOverviewInput};
use crate::observe::{self, ResolvedIp};
use crate::runtime::{
    bandwidth_test_url, format_conn_evidence, ConnEvidence, DelaySample, ProxyGroup, Runtime,
    TrafficSample, BANDWIDTH_DEFAULT_TIMEOUT_MS, BANDWIDTH_MAX_BYTES, BANDWIDTH_MAX_TIMEOUT_MS,
    DELAY_TEST_URL, DELAY_TIMEOUT_MS, STABILITY_DEFAULT_INTERVAL_MS, STABILITY_DEFAULT_SAMPLES,
    STABILITY_MAX_INTERVAL_MS, STABILITY_MAX_SAMPLES,
};
use crate::settings::{AppSettings, SettingsStore, SettingsView};
use crate::sysproxy;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub running: bool,
    pub version: String,
    pub mode: String,
    pub mixed_port: u16,
    pub active_profile_id: Option<String>,
    pub active_profile_name: Option<String>,
    pub system_proxy: Option<bool>,
    pub system_proxy_error: Option<String>,
    pub controller_port: u16,
    pub mihomo_bin: Option<String>,
    pub agent_ready: bool,
    /// 运行时 TUN（以 Clash API 为准，不是 yaml 意图）。
    pub tun_enabled: bool,
    pub tun_device: String,
    pub default_route: String,
    pub traffic_hint: String,
}

pub struct RedactSources {
    pub secret: String,
    pub urls: Vec<String>,
    pub yaml_bodies: Vec<String>,
}

pub struct Shell {
    profiles: ProfileStore,
    pub(crate) settings: SettingsStore,
    runtime: Runtime,
}

impl Shell {
    pub fn open(root: std::path::PathBuf) -> Result<Self, String> {
        let profiles = ProfileStore::open(root.clone())?;
        let settings = SettingsStore::open(&root)?;
        let runtime = Runtime::new(profiles.clone(), settings.clone())?;
        Ok(Self {
            profiles,
            settings,
            runtime,
        })
    }

    pub fn data_root(&self) -> std::path::PathBuf {
        self.profiles.root()
    }

    pub fn settings_view(&self) -> SettingsView {
        let data = self.settings.get();
        SettingsView {
            agent_ready: self.agent_ready(),
            resolved_mihomo_bin: self
                .runtime
                .resolve_bin()
                .ok()
                .map(|path| path.display().to_string()),
            agent_base_url: data.agent_base_url,
            agent_api_key: data.agent_api_key,
            agent_call_expires_at: data.agent_call_expires_at,
            agent_subscription_token: data.agent_subscription_token,
            agent_hub_user_id: data.agent_hub_user_id,
            agent_model: data.agent_model,
            mihomo_bin: data.mihomo_bin,
            mcp_enabled: data.mcp_enabled,
            mcp_bind: data.mcp_bind,
            mcp_token: data.mcp_token,
        }
    }

    pub fn save_settings(&self, mut next: AppSettings) -> Result<SettingsView, String> {
        // 对话凭证只由订阅换取写入；设置页保存不得覆盖。Author: kejiqing
        let current = self.settings.get();
        next.agent_base_url = current.agent_base_url;
        next.agent_api_key = current.agent_api_key;
        next.agent_call_expires_at = current.agent_call_expires_at;
        next.agent_subscription_token = current.agent_subscription_token;
        next.agent_hub_user_id = current.agent_hub_user_id;
        self.settings.save(next)?;
        Ok(self.settings_view())
    }

    /// 当前激活订阅是否为授权 phub clash URL。Author: kejiqing
    pub fn agent_ready(&self) -> bool {
        self.profiles
            .active_url()
            .and_then(|url| crate::call_token::parse_subscription_token(&url).ok())
            .is_some()
    }

    pub async fn download_mihomo(&self) -> Result<SettingsView, String> {
        let path = corebin::download_official(&self.profiles.root()).await?;
        let mut data = self.settings.get();
        // 下载后清空自定义路径，固定走应用目录官方内核。
        data.mihomo_bin.clear();
        self.settings.save(data)?;
        let _ = path;
        Ok(self.settings_view())
    }

    /// 用当前订阅 token 换取 / 复用 NeuroGate 调用钥。Author: kejiqing
    pub async fn agent_settings(&self) -> Result<crate::agent::AgentSettings, String> {
        let url = self
            .profiles
            .active_url()
            .ok_or_else(crate::call_token::missing_subscription_hint)?;
        let sub_token = crate::call_token::parse_subscription_token(&url)?;
        let data = self.settings.get();
        if data.agent_subscription_token == sub_token
            && data.agent_api_key.starts_with("ngmk_")
            && !data.agent_base_url.is_empty()
            && data.agent_hub_user_id > 0
            && crate::call_token::call_token_fresh(&data.agent_call_expires_at)
        {
            return Ok(crate::agent::AgentSettings {
                base: data.agent_base_url,
                key: data.agent_api_key,
                model: data.agent_model,
                hub_user_id: data.agent_hub_user_id,
            });
        }
        let issued = crate::call_token::exchange_call_token(&sub_token).await?;
        if issued.user_id <= 0 {
            return Err("Hub 未返回有效 user_id，请升级 Hub 后重试".into());
        }
        let saved = self.settings.save_call_credentials(
            &sub_token,
            &issued.base_url,
            &issued.call_token,
            &issued.expires_at,
            issued.user_id,
        )?;
        Ok(crate::agent::AgentSettings {
            base: saved.agent_base_url,
            key: saved.agent_api_key,
            model: saved.agent_model,
            hub_user_id: saved.agent_hub_user_id,
        })
    }

    pub fn list_profiles(&self) -> Result<Vec<crate::profile::ProfileMeta>, String> {
        self.profiles.list()
    }

    pub fn list_text(&self) -> Result<String, String> {
        self.profiles.list_text()
    }

    pub async fn import_subscription(
        &self,
        url: &str,
        name: Option<String>,
    ) -> Result<String, String> {
        let meta = self.profiles.import_url(url, name).await?;
        if self.runtime.running() && meta.active {
            self.runtime.reload().await?;
        }
        Ok(format!("已导入订阅 {} id={}", meta.name, meta.id))
    }

    #[cfg(test)]
    fn import_parsed_for_test(&self, url: &str, name: Option<String>, yaml: &str) -> Result<(), String> {
        self.profiles.import_parsed(url, name, yaml).map(|_| ())
    }

    pub async fn refresh_subscription(&self, id: &str) -> Result<String, String> {
        let meta = self.profiles.refresh(id).await?;
        if self.runtime.running() && meta.active {
            self.runtime.reload().await?;
        }
        Ok(format!("已刷新订阅 {}", meta.name))
    }

    pub async fn activate_profile(&self, id: &str) -> Result<String, String> {
        let meta = self.profiles.activate(id)?;
        if self.runtime.running() {
            self.runtime.reload().await?;
        }
        Ok(format!("已切换到订阅 {}", meta.name))
    }

    pub fn read_override(&self) -> Result<String, String> {
        self.profiles.read_override()
    }

    pub async fn write_override(&self, yaml: &str) -> Result<(), String> {
        self.profiles.write_override(yaml)?;
        self.runtime.reload().await
    }

    /// 清空本地覆盖 → 仅订阅。内核在跑且目标为当前激活订阅时重载。
    /// Author: kejiqing
    pub async fn reset_override(&self, id: Option<&str>) -> Result<String, String> {
        let meta = self.profiles.reset_override(id)?;
        let active = self.profiles.active_id();
        if self.runtime.running() && active.as_deref() == Some(meta.id.as_str()) {
            self.runtime.reload().await?;
        }
        Ok(format!(
            "已清空订阅「{}」的本地覆盖，回到仅订阅原文",
            meta.name
        ))
    }

    pub async fn start(&self) -> Result<Status, String> {
        self.runtime.start().await?;
        self.status().await
    }

    pub fn stop_blocking(&self) {
        self.runtime.stop_blocking();
    }

    /// 退出必走：关系统代理 + 停本壳内核（含 TUN 孤儿）。Author: kejiqing
    pub fn shutdown_blocking(&self) {
        let port = self.profiles.configured_mixed_port();
        let _ = sysproxy::set_enabled(false, port);
        self.stop_blocking();
        let _ = sysproxy::set_enabled(false, port);
    }

    /// 关系统代理（停内核 / 冷启动清残留用）。Author: kejiqing
    pub fn force_system_proxy_off(&self) {
        let port = self.profiles.configured_mixed_port();
        let _ = sysproxy::set_enabled(false, port);
    }

    /// 冷启动：清掉指向已死 mixed 口的残留系统代理。
    pub fn heal_stale_proxy_blocking(&self) {
        self.force_system_proxy_off();
    }

    pub async fn stop(&self) -> Result<(), String> {
        let result = self.runtime.stop().await;
        let port = self.profiles.configured_mixed_port();
        let _ = sysproxy::set_enabled(false, port);
        result
    }

    /// 开关系统代理，指向当前 mixed 口。开时要求内核已运行。Author: kejiqing
    pub async fn set_system_proxy(&self, enable: bool) -> Result<String, String> {
        let port = if self.runtime.running() {
            self.runtime
                .configs()
                .await
                .ok()
                .and_then(|c| c.mixed_port)
                .unwrap_or_else(|| self.profiles.configured_mixed_port())
        } else {
            self.profiles.configured_mixed_port()
        };
        if enable && !self.runtime.running() {
            return Err("请先启动内核，再开系统代理".into());
        }
        sysproxy::set_enabled(enable, port)?;
        let now = sysproxy::is_enabled().unwrap_or(enable);
        if now != enable {
            return Err(format!(
                "系统代理目标={}，读取={}",
                if enable { "开" } else { "关" },
                if now { "开" } else { "关" }
            ));
        }
        Ok(format!(
            "系统代理已{}（127.0.0.1:{port}）",
            if enable { "开启" } else { "关闭" }
        ))
    }

    pub async fn status(&self) -> Result<Status, String> {
        let running = self.runtime.running();
        let (system_proxy, system_proxy_error) = match sysproxy::is_enabled() {
            Ok(value) => (Some(value), None),
            Err(err) => (None, Some(err)),
        };
        let (mode, mixed_port, version) = if running {
            let general = self.runtime.configs().await?;
            let version = self.runtime.version().await.unwrap_or_default();
            (
                general.mode.unwrap_or_else(|| self.profiles.configured_mode()),
                general
                    .mixed_port
                    .unwrap_or_else(|| self.profiles.configured_mixed_port()),
                version,
            )
        } else {
            (
                self.profiles.configured_mode(),
                self.profiles.configured_mixed_port(),
                String::new(),
            )
        };
        let tun = self.tun_view().await.unwrap_or_else(|_| crate::runtime::TunView::off());
        let tun_enabled = tun.enable;
        let tun_device = tun.device.clone();
        let (default_route, traffic_hint) = netenv::traffic_summary(&NetOverviewInput {
            tun_enabled,
            tun_device: tun_device.clone(),
            auto_route: tun.auto_route,
            system_proxy,
        });
        Ok(Status {
            running,
            version,
            mode,
            mixed_port,
            active_profile_id: self.profiles.active_id(),
            active_profile_name: self.profiles.active_name(),
            system_proxy,
            system_proxy_error,
            controller_port: self.profiles.controller_port(),
            mihomo_bin: self
                .runtime
                .resolve_bin()
                .ok()
                .map(|path| path.display().to_string()),
            agent_ready: self.agent_ready(),
            tun_enabled,
            tun_device,
            default_route,
            traffic_hint,
        })
    }

    pub async fn model_status(&self) -> Result<String, String> {
        let status = self.status().await?;
        let mut lines = vec![
            format!("内核: {}", if status.running { "运行中" } else { "已停止" }),
            format!(
                "订阅: {}",
                status.active_profile_name.unwrap_or_else(|| "无".into())
            ),
            format!("模式: {}", status.mode),
            format!("TUN: {}", self.tun_summary().await),
            format!("mixed端口: {}", status.mixed_port),
            format!(
                "系统代理: {}",
                match status.system_proxy {
                    Some(true) => "开",
                    Some(false) => "关",
                    None => "未知",
                }
            ),
            format!("默认路由: {}", status.default_route),
            status.traffic_hint.clone(),
        ];
        if status.running {
            let groups = self.runtime.groups().await?;
            lines.push("代理组:".into());
            for group in groups {
                let shown = group
                    .all
                    .iter()
                    .take(40)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!("- {} 当前 {} 可选 {}", group.name, group.now, shown));
                if group.all.len() > 40 {
                    lines.push(format!("  共 {} 个，仅列出前 40 个", group.all.len()));
                }
            }
        }
        Ok(lines.join("\n"))
    }

    /// 覆盖里写入的模式；托盘勾选与未跑内核时的状态用这个。Author: kejiqing
    pub fn configured_mode(&self) -> String {
        self.profiles.configured_mode()
    }

    pub async fn set_mode(&self, mode: &str) -> Result<String, String> {
        if !matches!(mode, "rule" | "global" | "direct") {
            return Err("模式只能是 rule、global 或 direct".into());
        }
        self.profiles
            .patch_override(string_patch("mode", mode))?;
        self.runtime.reload().await?;
        let running = self.runtime.running();
        if running {
            Ok(format!("已切换为 {mode}"))
        } else {
            Ok(format!("已写入模式 {mode}，内核未运行"))
        }
    }

    pub async fn prepend_rule(&self, rule: &str) -> Result<String, String> {
        let list = self.profiles.prepend_rule(rule)?;
        self.runtime.reload().await?;
        Ok(format!(
            "已前置规则 {}。当前前置 {} 条，刷新订阅不会冲掉。",
            list.first().cloned().unwrap_or_default(),
            list.len()
        ))
    }

    pub async fn remove_prepend_rule(&self, rule: &str) -> Result<String, String> {
        let list = self.profiles.remove_prepend_rule(rule)?;
        self.runtime.reload().await?;
        Ok(format!("已删除前置规则。剩余 {} 条。", list.len()))
    }

    pub fn prepend_rules_text(&self) -> String {
        let list = self.profiles.prepend_rules();
        if list.is_empty() {
            "前置规则: 无".into()
        } else {
            format!("前置规则:\n{}", list.join("\n"))
        }
    }

    /// TUN auto-route 排除：改的是进不进 TUN，不是进了之后的 rules。
    /// Author: kejiqing
    pub async fn add_tun_route_exclude(&self, cidr: &str) -> Result<String, String> {
        let list = self.profiles.add_tun_route_exclude(cidr)?;
        self.apply_tun_route_change().await?;
        Ok(format!(
            "已加入 TUN 排除 {}。当前 {} 条（route-exclude-address，决定是否进 TUN）。",
            list.last().cloned().unwrap_or_default(),
            list.len()
        ))
    }

    pub async fn remove_tun_route_exclude(&self, cidr: &str) -> Result<String, String> {
        let list = self.profiles.remove_tun_route_exclude(cidr)?;
        self.apply_tun_route_change().await?;
        Ok(format!("已删除 TUN 排除。剩余 {} 条。", list.len()))
    }

    pub fn tun_route_excludes_text(&self) -> String {
        let list = self.profiles.tun_route_excludes();
        if list.is_empty() {
            "TUN route-exclude-address: 无".into()
        } else {
            format!(
                "TUN route-exclude-address（不进 TUN，不是 rules）:\n{}",
                list.join("\n")
            )
        }
    }

    async fn apply_tun_route_change(&self) -> Result<(), String> {
        // 改的是系统路由表，热重载不够稳，跟 set_tun 一样停再启。
        if self.runtime.running() {
            self.runtime.restart().await?;
        }
        Ok(())
    }

    pub fn dns_config_text(&self) -> String {
        self.profiles.dns_summary()
    }

    pub async fn set_dns(&self, patch: Value) -> Result<String, String> {
        let dns = normalize_dns_patch(patch)?;
        self.profiles.patch_dns(dns)?;
        self.runtime.reload().await?;
        Ok(format!("已更新 DNS 并重载。\n{}", self.profiles.dns_summary()))
    }

    pub async fn select_proxy(&self, group: &str, name: &str) -> Result<String, String> {
        if !self.runtime.running() {
            return Err("内核未运行".into());
        }
        let groups = self.runtime.groups().await?;
        let Some(found) = groups.iter().find(|item| item.name == group) else {
            return Err("代理组不存在".into());
        };
        if !found.all.iter().any(|item| item == name) {
            return Err("节点不在该组列表中".into());
        }
        self.runtime.select(group, name).await?;
        Ok(format!("已选择 {group} -> {name}"))
    }

    /// 新建或按名编辑代理组（本地覆盖，刷新订阅不冲掉）。
    /// Author: kejiqing
    pub async fn upsert_proxy_group(
        &self,
        name: &str,
        group_type: &str,
        proxies: &[String],
        url: Option<&str>,
        interval: Option<u64>,
    ) -> Result<String, String> {
        let (created, summary) =
            self.profiles
                .upsert_proxy_group(name, group_type, proxies, url, interval)?;
        self.runtime.reload().await?;
        let verb = if created { "已新建" } else { "已更新" };
        Ok(format!(
            "{verb}代理组 {summary}。刷新订阅不会冲掉；清空用 reset_override。\n{}",
            self.profiles.proxy_group_upserts_text()
        ))
    }

    /// 测单个节点延迟。只读，不切换当前节点。
    /// Author: kejiqing
    pub async fn test_delay_text(&self, name: &str) -> Result<String, String> {
        if !self.runtime.running() {
            return Err("内核未运行".into());
        }
        let name = name.trim();
        if name.is_empty() {
            return Err("节点名不能为空".into());
        }
        let sample = self.runtime.proxy_delay(name).await?;
        Ok(format_delay("节点", &[sample]))
    }

    /// 测全部叶子节点延迟。只读，不切换当前节点。
    /// Author: kejiqing
    pub async fn test_all_delays_text(&self) -> Result<String, String> {
        if !self.runtime.running() {
            return Err("内核未运行".into());
        }
        let samples = self.runtime.all_leaf_delays().await?;
        Ok(format_delay("全部节点", &samples))
    }

    /// 经 mixed 测当前出口吞吐（Mbps）。不切换节点，不是延迟。
    /// Author: kejiqing
    pub async fn test_bandwidth_text(&self, timeout_ms: Option<u64>) -> Result<String, String> {
        if !self.runtime.running() {
            return Err("内核未运行".into());
        }
        let mixed = self
            .runtime
            .configs()
            .await
            .ok()
            .and_then(|cfg| cfg.mixed_port)
            .unwrap_or_else(|| self.profiles.configured_mixed_port());
        let timeout_ms = timeout_ms
            .unwrap_or(BANDWIDTH_DEFAULT_TIMEOUT_MS)
            .clamp(2_000, BANDWIDTH_MAX_TIMEOUT_MS);
        let url = bandwidth_test_url();
        let proxy = reqwest::Proxy::all(format!("http://127.0.0.1:{mixed}"))
            .map_err(|err| format!("mixed 代理无效: {err}"))?;
        let client = reqwest::Client::builder()
            .proxy(proxy)
            .timeout(Duration::from_millis(timeout_ms))
            .connect_timeout(Duration::from_millis(timeout_ms.min(8_000)))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|err| format!("带宽测速客户端失败: {err}"))?;

        let started = Instant::now();
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|err| format!("带宽测速请求失败: {err}"))?;
        let status = response.status().as_u16();
        if !(200..400).contains(&status) {
            return Err(format!("带宽测速 HTTP {status}"));
        }
        let mut stream = response.bytes_stream();
        let mut bytes: u64 = 0;
        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| format!("带宽测速读流失败: {err}"))?;
            bytes = bytes.saturating_add(chunk.len() as u64);
            if bytes >= BANDWIDTH_MAX_BYTES || started.elapsed().as_millis() as u64 >= timeout_ms {
                break;
            }
        }
        let elapsed = started.elapsed();
        let elapsed_ms = elapsed.as_millis().max(1) as f64;
        let mbps = (bytes as f64 * 8.0) / (elapsed_ms * 1000.0);
        Ok(serde_json::json!({
            "kind": "bandwidth",
            "url": url,
            "path": format!("mixed:{mixed}"),
            "region": std::env::var("REGION").unwrap_or_else(|_| "default".into()),
            "httpStatus": status,
            "bytes": bytes,
            "elapsedMs": elapsed_ms.round() as u64,
            "mbps": (mbps * 100.0).round() / 100.0,
            "timeoutMs": timeout_ms,
            "maxBytes": BANDWIDTH_MAX_BYTES,
            "note": "当前生效出口吞吐；不切换节点"
        })
        .to_string())
    }

    /// 对指定节点（或当前出口）重复延迟采样，回报抖动/失败率。不是带宽。
    /// Author: kejiqing
    pub async fn test_stability_text(
        &self,
        name: Option<&str>,
        samples: Option<u32>,
        interval_ms: Option<u64>,
    ) -> Result<String, String> {
        if !self.runtime.running() {
            return Err("内核未运行".into());
        }
        let count = samples
            .unwrap_or(STABILITY_DEFAULT_SAMPLES)
            .clamp(2, STABILITY_MAX_SAMPLES);
        let interval_ms = interval_ms
            .unwrap_or(STABILITY_DEFAULT_INTERVAL_MS)
            .clamp(0, STABILITY_MAX_INTERVAL_MS);
        let name = name.map(str::trim).filter(|s| !s.is_empty());

        let mut delays: Vec<Option<u32>> = Vec::with_capacity(count as usize);
        for i in 0..count {
            if i > 0 && interval_ms > 0 {
                tokio::time::sleep(Duration::from_millis(interval_ms)).await;
            }
            let sample = match name {
                Some(node) => self.runtime.proxy_delay(node).await?,
                None => {
                    let ms = self.probe_latency_via_mixed().await;
                    DelaySample {
                        name: "current-exit".into(),
                        delay_ms: ms,
                    }
                }
            };
            delays.push(sample.delay_ms);
        }

        let ok: Vec<u32> = delays.iter().copied().flatten().collect();
        let fail = delays.len() - ok.len();
        let (min, max, avg, jitter) = if ok.is_empty() {
            (None, None, None, None)
        } else {
            let min = *ok.iter().min().unwrap();
            let max = *ok.iter().max().unwrap();
            let sum: u64 = ok.iter().map(|v| *v as u64).sum();
            let avg = (sum as f64) / (ok.len() as f64);
            let mut diffs = Vec::new();
            for window in ok.windows(2) {
                diffs.push((window[0] as i64 - window[1] as i64).unsigned_abs() as u32);
            }
            let jitter = if diffs.is_empty() {
                0.0
            } else {
                diffs.iter().map(|v| *v as f64).sum::<f64>() / diffs.len() as f64
            };
            (Some(min), Some(max), Some(avg), Some(jitter))
        };

        Ok(serde_json::json!({
            "kind": "stability",
            "target": name.unwrap_or("current-exit"),
            "samples": count,
            "intervalMs": interval_ms,
            "delaysMs": delays,
            "ok": ok.len(),
            "fail": fail,
            "failRate": ((fail as f64) / (delays.len() as f64) * 1000.0).round() / 10.0,
            "minMs": min,
            "maxMs": max,
            "avgMs": avg.map(|v| (v * 10.0).round() / 10.0),
            "jitterMs": jitter.map(|v| (v * 10.0).round() / 10.0),
            "note": "延迟采样统计，不是带宽；指定 name 时测该节点且不切换"
        })
        .to_string())
    }

    /// 经 mixed 对延迟探测 URL 做一次 RTT 采样（当前出口）。
    /// Author: kejiqing
    async fn probe_latency_via_mixed(&self) -> Option<u32> {
        let mixed = self
            .runtime
            .configs()
            .await
            .ok()
            .and_then(|cfg| cfg.mixed_port)
            .unwrap_or_else(|| self.profiles.configured_mixed_port());
        let proxy = reqwest::Proxy::all(format!("http://127.0.0.1:{mixed}")).ok()?;
        let client = reqwest::Client::builder()
            .proxy(proxy)
            .timeout(Duration::from_millis(DELAY_TIMEOUT_MS))
            .connect_timeout(Duration::from_millis(DELAY_TIMEOUT_MS))
            .build()
            .ok()?;
        let started = Instant::now();
        match client.get(DELAY_TEST_URL).send().await {
            Ok(response) => {
                drop(response);
                Some(started.elapsed().as_millis().min(u32::MAX as u128) as u32)
            }
            Err(_) => None,
        }
    }

    pub async fn set_mixed_port(&self, port: u16) -> Result<String, String> {
        if port == 0 {
            return Err("端口无效".into());
        }
        self.profiles
            .patch_override(number_patch("mixed-port", port))?;
        self.runtime.reload().await?;
        Ok(format!("已设置 mixed 端口 {port}"))
    }

    pub async fn set_tun(&self, enable: bool) -> Result<String, String> {
        if enable && cfg!(windows) {
            return Err("Windows 本期不支持 TUN，请用系统代理（set_system_proxy）".into());
        }
        let tun = mapping(vec![("enable".into(), Value::Bool(enable))]);
        self.profiles
            .patch_override(mapping(vec![("tun".into(), tun)]))?;
        // 关 TUN 且内核没跑：只改配置，绝不能为了「对齐」把内核拉起来。
        if !enable && !self.runtime.running() {
            return Ok("TUN 已写入关闭（内核未运行）".into());
        }
        // macOS TUN 要 root 建 utun；只靠 PUT /configs 会留下「配置开、运行关」。
        // Author: kejiqing
        self.runtime.restart().await?;
        let (now, device) = self.tun_state().await;
        if now != enable {
            return Err(format!(
                "TUN 目标={}，运行时={} device={}。{}",
                if enable { "开" } else { "关" },
                if now { "开" } else { "关" },
                if device.is_empty() { "(空)" } else { &device },
                if enable {
                    "请在弹出的管理员密码框中允许，或到「安全性与隐私」确认。"
                } else {
                    "请刷新状态页。"
                }
            ));
        }
        Ok(format!(
            "TUN 已{}{}",
            if enable { "开启" } else { "关闭" },
            if device.is_empty() {
                String::new()
            } else {
                format!("（{device}）")
            }
        ))
    }

    pub async fn set_allow_lan(&self, enable: bool) -> Result<String, String> {
        self.profiles
            .patch_override(bool_patch("allow-lan", enable))?;
        self.runtime.reload().await?;
        Ok(format!(
            "已设置 allow-lan {}",
            if enable { "开" } else { "关" }
        ))
    }

    pub async fn groups(&self) -> Result<Vec<ProxyGroup>, String> {
        if !self.runtime.running() {
            return Ok(Vec::new());
        }
        self.runtime.groups().await
    }

    /// 瞬时上下行；内核未跑时为 0 + running=false。Author: kejiqing
    pub async fn traffic_sample(&self) -> Result<TrafficSample, String> {
        self.runtime.traffic_sample().await
    }

    pub async fn list_rules_text(&self) -> Result<String, String> {
        self.list_rules_text_limited(120).await
    }

    pub async fn list_rules_text_limited(&self, limit: usize) -> Result<String, String> {
        let rules = self.load_rules().await?;
        Ok(observe::format_rules(&rules, limit.max(1).min(500)))
    }

    pub async fn groups_text(&self) -> Result<String, String> {
        if !self.runtime.running() {
            return Ok("内核未运行".into());
        }
        let groups = self.runtime.groups().await?;
        if groups.is_empty() {
            return Ok("无代理组".into());
        }
        let mut lines = Vec::new();
        for group in groups {
            let shown = group
                .all
                .iter()
                .take(40)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "- {} 当前 {} 可选 {}",
                group.name, group.now, shown
            ));
        }
        Ok(lines.join("\n"))
    }

    /// 叶子节点清单（不含代理组与内置策略）。
    /// Author: kejiqing
    pub async fn nodes_text(&self) -> Result<String, String> {
        if !self.runtime.running() {
            return Ok("内核未运行".into());
        }
        let names = self.runtime.leaf_proxy_names().await?;
        if names.is_empty() {
            return Ok("无节点".into());
        }
        let mut lines = vec![format!("节点 {} 个", names.len())];
        for name in names {
            lines.push(format!("- {name}"));
        }
        Ok(lines.join("\n"))
    }

    pub async fn net_overview(&self) -> Result<String, String> {
        let system_proxy = sysproxy::is_enabled().ok();
        let tun = self.tun_view().await.unwrap_or_else(|_| crate::runtime::TunView::off());
        Ok(netenv::overview(&NetOverviewInput {
            tun_enabled: tun.enable,
            tun_device: tun.device,
            auto_route: tun.auto_route,
            system_proxy,
        }))
    }

    /// 本机现场诊断。NeuroGate 沙箱看不见 Mac；只信这份 MCP 回报。
    /// Author: kejiqing
    pub async fn diagnose(&self) -> Result<String, String> {
        let mut lines = vec![
            "来源: 本机 Supone Tunnel MCP（不是 NeuroGate 沙箱）".into(),
            format!("时间: {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S")),
        ];
        lines.push(self.model_status().await?);
        let tun_intent = self.profiles.tun_enabled_effective();
        lines.push(format!(
            "TUN 配置意图: {}；运行时: {}",
            if tun_intent { "开" } else { "关" },
            self.tun_summary().await
        ));
        let controller = self.profiles.controller_port();
        let mixed = self.profiles.configured_mixed_port();
        let mcp = self.settings.mcp_config();
        let mcp_port = mcp
            .bind
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse::<u16>().ok());
        lines.push(format!(
            "端口: controller={controller} {}；mixed={mixed} {}；mcp={} {}",
            if tcp_open(controller) { "开" } else { "关" },
            if tcp_open(mixed) { "开" } else { "关" },
            mcp.bind,
            match mcp_port {
                Some(port) if tcp_open(port) => "开",
                Some(_) => "关",
                None => "绑定无效",
            }
        ));
        lines.push(format!("DNS 生效:\n{}", self.profiles.dns_summary().trim()));
        lines.push(self.net_overview().await?);
        lines.push(format!("系统 DNS:\n{}", os_dns_summary()));
        lines.push(format!("内核近期告警:\n{}", core_warn_tail(&self.profiles.root())));
        Ok(lines.join("\n\n"))
    }

    /// 救场：境外 DNS 作 nameserver 时改回国内；若同时还开着 TUN 则关掉（auto-route 会劫持整机）。
    /// 不启动内核。Author: kejiqing
    pub fn recover_safe_egress(&self) -> Result<String, String> {
        let dns_text = self.profiles.dns_summary();
        let foreign_dns = dns_text.contains("dns.google")
            || dns_text.contains("1.1.1.1/dns-query")
            || dns_text.contains("cloudflare-dns.com");
        if !foreign_dns {
            return Ok("无需救场：DNS 未见境外 DoH 当 nameserver".into());
        }
        let mut actions: Vec<String> = Vec::new();
        if self.profiles.tun_enabled_effective() {
            let tun = mapping(vec![("enable".into(), Value::Bool(false))]);
            self.profiles
                .patch_override(mapping(vec![("tun".into(), tun)]))?;
            actions.push("TUN+境外 DNS 组合会搞挂整机：已关闭配置中的 TUN".into());
        }
        let nameserver = Value::Sequence(vec![
            Value::String("223.5.5.5".into()),
            Value::String("119.29.29.29".into()),
        ]);
        let fallback = Value::Sequence(vec![
            Value::String("https://dns.alidns.com/dns-query".into()),
            Value::String("https://doh.pub/dns-query".into()),
        ]);
        self.profiles.patch_dns(mapping(vec![
            ("nameserver".into(), nameserver),
            ("fallback".into(), fallback),
            ("respect-rules".into(), Value::Bool(false)),
            ("enhanced-mode".into(), Value::String("redir-host".into())),
        ]))?;
        actions.push(
            "已把 nameserver 改回国内 UDP，fallback 用国内 DoH；nameserver-policy 未动".into(),
        );
        Ok(actions.join("\n"))
    }

    pub fn net_interfaces(&self) -> String {
        netenv::interfaces()
    }

    pub fn net_routes(&self) -> String {
        netenv::routes()
    }

    async fn tun_state(&self) -> (bool, String) {
        match self.tun_view().await {
            Ok(view) => (view.enable, view.device),
            Err(_) => (false, String::new()),
        }
    }

    async fn tun_view(&self) -> Result<crate::runtime::TunView, String> {
        self.runtime.tun_snapshot().await
    }

    async fn tun_summary(&self) -> String {
        match self.tun_view().await {
            Ok(view) => view.summary(),
            Err(err) => format!("读取失败 {err}"),
        }
    }

    pub async fn dns_lookup_text(&self, host: &str) -> Result<String, String> {
        let host = observe::normalize_host(host)?;
        let ips = self.runtime.dns_lookup(&host).await?;
        let mmdb = self.profiles.root().join("geoip.metadb");
        let mut lines = vec![format!("DNS {host} -> {} 个地址", ips.len())];
        for ip in ips {
            let country = ip
                .parse()
                .ok()
                .and_then(|addr| observe::lookup_country(&mmdb, addr))
                .unwrap_or_else(|| "?".into());
            lines.push(format!("{ip} country={country}"));
            lines.push(netenv::route_get(&ip));
        }
        Ok(lines.join("\n"))
    }

    pub async fn list_connections_text(&self, host: Option<&str>) -> Result<String, String> {
        let filter = match host {
            Some(raw) if !raw.trim().is_empty() => Some(observe::normalize_host(raw)?),
            _ => None,
        };
        self.runtime
            .connections_text(filter.as_deref())
            .await
    }

    /// 经 mixed 口探测目标 URL（走当前生效配置），并用 `/connections` 连接日志给出链路。
    /// 链路只来自内核连接记录，不做规则推理。Author: kejiqing
    pub async fn probe_url_text(
        &self,
        url: &str,
        timeout_ms: Option<u64>,
    ) -> Result<String, String> {
        if !self.runtime.running() {
            return Err("内核未运行".into());
        }
        let (url, host) = normalize_probe_url(url)?;
        let mixed = self
            .runtime
            .configs()
            .await
            .ok()
            .and_then(|cfg| cfg.mixed_port)
            .unwrap_or_else(|| self.profiles.configured_mixed_port());
        let timeout_ms = timeout_ms.unwrap_or(8_000).clamp(1_000, 30_000);
        let timeout = Duration::from_millis(timeout_ms);
        let proxy = reqwest::Proxy::all(format!("http://127.0.0.1:{mixed}"))
            .map_err(|err| format!("mixed 代理无效: {err}"))?;
        let client = reqwest::Client::builder()
            .proxy(proxy)
            .timeout(timeout)
            .connect_timeout(timeout)
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|err| format!("探测客户端失败: {err}"))?;

        let best: Arc<Mutex<Option<ConnEvidence>>> = Arc::new(Mutex::new(None));
        let started = Instant::now();
        let probe_url = url.clone();
        // JoinHandle 可取消等待而不中止请求；直接 select send() 会被 sleep 分支掐掉。
        let mut probe = tokio::spawn(async move {
            match client.get(probe_url).send().await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let final_url = response.url().to_string();
                    // 只认响应头；丢弃 body，避免大包干扰观测。
                    drop(response);
                    Ok((status, final_url))
                }
                Err(err) => Err(summarize_probe_error(&err)),
            }
        });
        let probe_result = loop {
            tokio::select! {
                biased;
                joined = &mut probe => {
                    break joined.map_err(|err| format!("探测任务失败: {err}"))?;
                }
                _ = tokio::time::sleep(Duration::from_millis(40)) => {
                    if let Ok(rows) = self.runtime.connection_evidence(Some(&host)).await {
                        merge_newest_conn(&best, rows);
                    }
                }
            }
        };
        // 请求结束后再采一轮，短连接常在收包瞬间才出现在 /connections。
        for _ in 0..3 {
            if let Ok(rows) = self.runtime.connection_evidence(Some(&host)).await {
                merge_newest_conn(&best, rows);
            }
            if best
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        let elapsed_ms = started.elapsed().as_millis();
        let evidence = best
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone();

        let mut lines = vec![
            format!("探测: {url}"),
            format!("路径: 经 mixed:{mixed}（当前生效配置）"),
            format!("超时: {timeout_ms} ms"),
        ];
        match probe_result {
            Ok((status, final_url)) => {
                lines.push(format!(
                    "结果: ok status={status} 耗时={elapsed_ms}ms 最终URL={final_url}"
                ));
            }
            Err(err) => {
                lines.push(format!("结果: fail 耗时={elapsed_ms}ms 错误={err}"));
            }
        }
        lines.push("连接日志（mihomo /connections，非规则推理）:".into());
        match evidence {
            Some(row) => lines.push(format!("- {}", format_conn_evidence(&row))),
            None => lines.push(format!(
                "- 探测期间未捕获到匹配 host={host} 的连接记录（连接可能已关闭；可再调 list_connections）"
            )),
        }
        Ok(lines.join("\n"))
    }

    /// 域名规则 + DNS + 该 IP 的系统路由 + 组的当前节点 + 已有连接。
    /// Author: kejiqing
    pub async fn explain_route(&self, host: &str) -> Result<String, String> {
        let host = observe::normalize_host(host)?;
        let mode = self.profiles.configured_mode();
        let rules = self.load_rules().await?;
        let domain_hit = observe::match_domain_rule(&rules, &host).is_some();
        let ips = self.runtime.dns_lookup(&host).await.unwrap_or_default();
        let mmdb = self.profiles.root().join("geoip.metadb");
        let resolved = if mode == "rule" && domain_hit {
            Vec::new()
        } else {
            ips.iter()
                .map(|ip| {
                    let country = ip
                        .parse()
                        .ok()
                        .and_then(|addr| observe::lookup_country(&mmdb, addr));
                    ResolvedIp {
                        ip: ip.clone(),
                        country,
                    }
                })
                .collect::<Vec<_>>()
        };
        let verdict = observe::explain_with_dns(&mode, &host, &rules, &resolved);
        let mut lines = verdict.lines;
        if ips.is_empty() {
            lines.push("DNS 无结果，无法给出系统路由".into());
        } else {
            let source = if self.runtime.running() {
                "mihomo DNS"
            } else {
                "系统 DNS"
            };
            lines.push(format!("{source}: {}", ips.join(", ")));
            for ip in &ips {
                lines.push(netenv::route_get(ip));
            }
        }
        let policies = policies_in_lines(&lines);
        if self.runtime.running() {
            match self.runtime.groups().await {
                Ok(groups) => {
                    for policy in &policies {
                        if let Some(group) = groups.iter().find(|group| group.name == *policy) {
                            lines.push(format!("组 {} 当前节点: {}", group.name, group.now));
                        } else {
                            lines.push(format!("策略 {policy}：不是代理组，或就是节点名本身"));
                        }
                    }
                }
                Err(err) => lines.push(format!("读取代理组失败: {err}")),
            }
            match self.list_connections_text(Some(&host)).await {
                Ok(text) => lines.push(format!("已有连接:\n{text}")),
                Err(err) => lines.push(format!("已有连接: {err}")),
            }
        } else if !policies.is_empty() {
            lines.push("内核未运行，没有组的当前节点".into());
        }
        Ok(lines.join("\n"))
    }

    async fn load_rules(&self) -> Result<Vec<crate::runtime::RuleRow>, String> {
        if self.runtime.running() {
            if let Ok(rules) = self.runtime.rules().await {
                return Ok(rules);
            }
        }
        self.profiles.effective_rules()
    }

    pub fn redact_sources(&self) -> RedactSources {
        RedactSources {
            secret: self.profiles.secret(),
            urls: self.profiles.urls(),
            yaml_bodies: self.profiles.yaml_bodies(),
        }
    }
}

fn policies_in_lines(lines: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines {
        let Some(rest) = line.split(" -> ").nth(1) else {
            continue;
        };
        let name = rest.split('（').next().unwrap_or("").trim();
        if name.is_empty() || out.iter().any(|item| item == name) {
            continue;
        }
        out.push(name.to_string());
    }
    out
}

fn tcp_open(port: u16) -> bool {
    let Ok(addr) = format!("127.0.0.1:{port}").parse::<std::net::SocketAddr>() else {
        return false;
    };
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(200)).is_ok()
}

fn os_dns_summary() -> String {
    if cfg!(windows) {
        return "Windows 暂不提供系统 DNS 摘要".into();
    }
    let output = std::process::Command::new("scutil")
        .arg("--dns")
        .output();
    let Ok(output) = output else {
        return "scutil --dns 失败".into();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = Vec::new();
    let mut in_resolver1 = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("resolver #1") {
            in_resolver1 = true;
            lines.push(trimmed.to_string());
            continue;
        }
        if in_resolver1 {
            if trimmed.starts_with("resolver #") {
                break;
            }
            if trimmed.starts_with("nameserver[")
                || trimmed.starts_with("search domain")
                || trimmed.starts_with("flags")
                || trimmed.starts_with("reach")
            {
                lines.push(trimmed.to_string());
            }
        }
    }
    if lines.is_empty() {
        "无 resolver #1".into()
    } else {
        lines.join("\n")
    }
}

fn core_warn_tail(root: &std::path::Path) -> String {
    let path = root.join("mihomo.stderr.log");
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let hits: Vec<&str> = text
        .lines()
        .rev()
        .filter(|line| {
            line.contains("dns resolve failed")
                || line.contains("Start TUN")
                || line.contains("operation not permitted")
                || line.contains("level=error")
                || line.contains("level=warning")
        })
        .take(25)
        .collect();
    if hits.is_empty() {
        return "无近期 warning/error".into();
    }
    hits.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// Author: kejiqing
fn normalize_probe_url(raw: &str) -> Result<(String, String), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("url 不能为空".into());
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    let parsed = reqwest::Url::parse(&with_scheme).map_err(|err| format!("url 无效: {err}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("只支持 http/https，收到 {other}")),
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "url 缺少 host".to_string())?
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err("url 缺少 host".into());
    }
    Ok((parsed.to_string(), host))
}

fn merge_newest_conn(slot: &Mutex<Option<ConnEvidence>>, rows: Vec<ConnEvidence>) {
    let Ok(mut guard) = slot.lock() else {
        return;
    };
    for row in rows {
        let replace = match guard.as_ref() {
            None => true,
            Some(old) => row.start > old.start || (row.start == old.start && row.id > old.id),
        };
        if replace {
            *guard = Some(row);
        }
    }
}

fn summarize_probe_error(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        return "超时".into();
    }
    if err.is_connect() {
        return format!("连接失败: {err}");
    }
    err.to_string()
}

fn string_patch(key: &str, value: &str) -> Value {
    mapping(vec![(key.to_string(), Value::String(value.to_string()))])
}

fn bool_patch(key: &str, value: bool) -> Value {
    mapping(vec![(key.to_string(), Value::Bool(value))])
}

fn number_patch(key: &str, value: u16) -> Value {
    mapping(vec![(
        key.to_string(),
        serde_yaml::to_value(value).unwrap_or(Value::Null),
    )])
}

fn mapping(pairs: Vec<(String, Value)>) -> Value {
    let mut map = serde_yaml::Mapping::new();
    for (key, value) in pairs {
        map.insert(Value::String(key), value);
    }
    Value::Mapping(map)
}

/// 只接受 dns 调整字段，避免整份 YAML 灌进来。Author: kejiqing
fn normalize_dns_patch(patch: Value) -> Result<Value, String> {
    let Value::Mapping(input) = patch else {
        return Err("dns 补丁必须是对象".into());
    };
    let mut dns = serde_yaml::Mapping::new();
    for (key, value) in input {
        let Some(name) = key.as_str() else {
            continue;
        };
        match name {
            "enable" | "respect-rules" | "ipv6" => {
                let flag = value
                    .as_bool()
                    .ok_or_else(|| format!("{name} 必须是布尔"))?;
                dns.insert(Value::String(name.into()), Value::Bool(flag));
            }
            "enhanced-mode" => {
                let mode = value
                    .as_str()
                    .ok_or_else(|| "enhanced-mode 必须是字符串".to_string())?;
                if !matches!(mode, "fake-ip" | "redir-host") {
                    return Err("enhanced-mode 只能是 fake-ip 或 redir-host".into());
                }
                dns.insert(Value::String(name.into()), Value::String(mode.into()));
            }
            "nameserver" | "fallback" | "default-nameserver" | "fake-ip-filter" => {
                dns.insert(Value::String(name.into()), string_list(name, &value)?);
            }
            "nameserver-policy" => {
                let Value::Mapping(policy) = value else {
                    return Err("nameserver-policy 必须是域名到 DNS 列表的映射".into());
                };
                let mut out = serde_yaml::Mapping::new();
                for (domain, servers) in policy {
                    let Some(domain) = domain.as_str() else {
                        return Err("nameserver-policy 的域名无效".into());
                    };
                    if domain.is_empty() || domain.contains(' ') || domain.chars().count() > 120 {
                        return Err(format!("域名无效: {domain}"));
                    }
                    out.insert(Value::String(domain.into()), string_list(domain, &servers)?);
                }
                dns.insert(Value::String(name.into()), Value::Mapping(out));
            }
            other => return Err(format!("不允许的 dns 字段 {other}")),
        }
    }
    if dns.is_empty() {
        return Err("没有可写入的 dns 字段".into());
    }
    Ok(Value::Mapping(dns))
}

fn format_delay(scope: &str, samples: &[DelaySample]) -> String {
    const LIMIT: usize = 40;
    if samples.is_empty() {
        return format!("{scope} 没有测速结果");
    }
    let mut rows = samples.to_vec();
    rows.sort_by(|a, b| match (a.delay_ms, b.delay_ms) {
        (Some(left), Some(right)) => left.cmp(&right).then_with(|| a.name.cmp(&b.name)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.name.cmp(&b.name),
    });
    let ok = rows.iter().filter(|row| row.delay_ms.is_some()).count();
    let timeout = rows.len() - ok;
    let mut lines = vec![format!(
        "{scope} 延迟（{DELAY_TEST_URL}，超时 {DELAY_TIMEOUT_MS}ms）"
    )];
    for row in rows.iter().take(LIMIT) {
        let delay = match row.delay_ms {
            Some(ms) => format!("{ms} ms"),
            None => "超时".into(),
        };
        lines.push(format!("- {} {delay}", row.name));
    }
    if rows.len() > LIMIT {
        let rest = rows.len() - LIMIT;
        let rest_timeout = rows
            .iter()
            .skip(LIMIT)
            .filter(|row| row.delay_ms.is_none())
            .count();
        lines.push(format!(
            "仅列出最快 {LIMIT} 个，其余 {rest} 个未列出（其中超时 {rest_timeout}）"
        ));
    }
    lines.push(format!("共 {}，成功 {ok}，超时 {timeout}", rows.len()));
    lines.join("\n")
}

fn string_list(name: &str, value: &Value) -> Result<Value, String> {
    let Some(items) = value.as_sequence() else {
        return Err(format!("{name} 必须是字符串数组"));
    };
    if items.is_empty() || items.len() > 16 {
        return Err(format!("{name} 需要 1 到 16 项"));
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str() else {
            return Err(format!("{name} 的每一项必须是字符串"));
        };
        let text = text.trim();
        if text.is_empty() || text.contains(' ') || text.chars().count() > 80 {
            return Err(format!("{name} 项无效: {text}"));
        }
        out.push(Value::String(text.to_string()));
    }
    Ok(Value::Sequence(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use uuid::Uuid;

    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("supone-shell-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn set_mode_rejects_unknown_and_persists_rule() {
        let tmp = Tmp::new();
        let shell = Shell::open(tmp.0.clone()).unwrap();
        let err = shell.set_mode("weird").await.unwrap_err();
        assert!(err.contains("模式"));
        shell
            .import_parsed_for_test(
                "https://example.com/sub?token=abc",
                Some("示例".into()),
                "proxies: []\n",
            )
            .unwrap();
        let text = shell.set_mode("direct").await.unwrap();
        assert!(text.contains("direct"));
        assert_eq!(shell.profiles.configured_mode(), "direct");
    }

    #[tokio::test]
    async fn explain_route_hits_domain_without_dns() {
        let tmp = Tmp::new();
        let shell = Shell::open(tmp.0.clone()).unwrap();
        shell
            .import_parsed_for_test(
                "https://example.com/sub",
                Some("示例".into()),
                "mode: rule\nrules:\n  - DOMAIN-SUFFIX,github.com,ARTIFACT\n  - GEOIP,CN,DIRECT\n  - MATCH,PROXY\n",
            )
            .unwrap();
        let github = shell.explain_route("https://www.github.com/foo").await.unwrap();
        assert!(github.contains("ARTIFACT"), "{github}");
        assert!(github.contains("github.com"), "{github}");
        let listed = shell.list_rules_text().await.unwrap();
        assert!(listed.contains("GEOIP") || listed.contains("GeoIP"), "{listed}");
    }

    #[test]
    fn normalize_probe_url_defaults_https_and_rejects_other() {
        let (url, host) = normalize_probe_url("www.github.com/foo").unwrap();
        assert_eq!(host, "www.github.com");
        assert!(url.starts_with("https://www.github.com/foo"), "{url}");
        assert!(normalize_probe_url("ftp://x").unwrap_err().contains("http"));
        assert!(normalize_probe_url("").is_err());
    }

    #[test]
    fn format_delay_sorts_fastest_first_and_caps() {
        let mut samples = vec![
            DelaySample {
                name: "慢".into(),
                delay_ms: Some(900),
            },
            DelaySample {
                name: "超时甲".into(),
                delay_ms: None,
            },
            DelaySample {
                name: "快".into(),
                delay_ms: Some(40),
            },
        ];
        for index in 0..40 {
            samples.push(DelaySample {
                name: format!("n{index:02}"),
                delay_ms: Some(100 + index),
            });
        }
        let text = format_delay("全部节点", &samples);
        assert!(text.contains("- 快 40 ms"), "{text}");
        assert!(text.contains("仅列出最快 40 个"), "{text}");
        assert!(text.contains("成功 42，超时 1"), "{text}");
        assert!(!text.contains("- 慢 "), "{text}");
        assert!(!text.contains("超时甲"), "{text}");
        assert!(format_delay("全部节点", &[]).contains("没有测速结果"));
    }
}
