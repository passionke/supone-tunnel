// 作者: kejiqing
//! 监管本程序拉起的 mihomo。
//! 控制器端口属于本壳：退出时结束本壳内核和该端口上的监听，启动前先清掉再拉起。

use std::fs::{self, File};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::corebin;
use crate::privkill::{self, is_our_mihomo, listener_pids, our_mihomo_pids, process_command};
use crate::profile::ProfileStore;
use crate::settings::SettingsStore;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyGroup {
    pub name: String,
    pub now: String,
    pub all: Vec<String>,
}

/// 单节点延迟。`None` 表示超时或探测失败。
/// Author: kejiqing
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DelaySample {
    pub name: String,
    pub delay_ms: Option<u32>,
}

/// mihomo `/traffic` 瞬时上下行（字节/秒）。
/// Author: kejiqing
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrafficSample {
    pub up: u64,
    pub down: u64,
    pub running: bool,
}

/// mihomo `/connections` 一条连接的运行时证据（不是规则推理）。
/// Author: kejiqing
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnEvidence {
    pub id: String,
    pub host: String,
    pub dest_ip: String,
    pub dest_port: String,
    pub network: String,
    pub conn_type: String,
    pub rule: String,
    pub rule_payload: String,
    /// API `chains` 原序，用 `>` 连接。
    pub chain: String,
    pub start: String,
}

/// mihomo 默认探测地址。测的是经该节点访问此 URL 的延迟，不是带宽。
/// Author: kejiqing
pub const DELAY_TEST_URL: &str = "https://www.gstatic.com/generate_204";
pub const DELAY_TIMEOUT_MS: u64 = 5000;

/// 带宽测速下载上限（字节）。到此即停，不追求下完整文件。
/// Author: kejiqing
pub const BANDWIDTH_MAX_BYTES: u64 = 12 * 1024 * 1024;
/// 带宽测速默认/最大超时（毫秒）。
pub const BANDWIDTH_DEFAULT_TIMEOUT_MS: u64 = 12_000;
pub const BANDWIDTH_MAX_TIMEOUT_MS: u64 = 30_000;

/// 稳定性采样默认次数与间隔。
pub const STABILITY_DEFAULT_SAMPLES: u32 = 5;
pub const STABILITY_MAX_SAMPLES: u32 = 20;
pub const STABILITY_DEFAULT_INTERVAL_MS: u64 = 400;
pub const STABILITY_MAX_INTERVAL_MS: u64 = 5_000;

/// 带宽测速 URL：`REGION=china` 用国内镜像大文件，否则 Cloudflare 可控字节下载。
/// Author: kejiqing
pub fn bandwidth_test_url() -> &'static str {
    if std::env::var("REGION").ok().as_deref() == Some("china") {
        "https://mirrors.tuna.tsinghua.edu.cn/ubuntu-releases/24.04/ubuntu-24.04.1-desktop-amd64.iso"
    } else {
        "https://speed.cloudflare.com/__down?bytes=25000000"
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleRow {
    pub index: usize,
    pub kind: String,
    pub payload: String,
    pub proxy: String,
}

pub struct Runtime {
    profiles: ProfileStore,
    settings: SettingsStore,
    child: Mutex<Option<Child>>,
    /// 应用重启后丢失 Child 句柄时，认领本壳孤儿 mihomo 的 pid。
    adopted_pid: Mutex<Option<u32>>,
    log_path: PathBuf,
    client: reqwest::Client,
}

impl Runtime {
    pub fn new(profiles: ProfileStore, settings: SettingsStore) -> Result<Self, String> {
        let log_path = profiles.root().join("mihomo.stderr.log");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(|err| err.to_string())?;
        Ok(Self {
            profiles,
            settings,
            child: Mutex::new(None),
            adopted_pid: Mutex::new(None),
            log_path,
            client,
        })
    }

    pub fn resolve_bin(&self) -> Result<PathBuf, String> {
        let configured = self.settings.get().mihomo_bin;
        corebin::resolve(&self.profiles.root(), &configured)
    }

    pub fn running(&self) -> bool {
        {
            let mut guard = self.child.lock().expect("内核锁已损坏");
            if reap(&mut guard) {
                return true;
            }
        }
        self.reclaim_orphan();
        let mut adopted = self.adopted_pid.lock().expect("内核锁已损坏");
        if let Some(pid) = *adopted {
            if process_alive(pid) {
                return true;
            }
            *adopted = None;
        }
        false
    }

    pub async fn start(&self) -> Result<(), String> {
        if self.running() {
            // 已在跑但配置要 TUN、运行时却没挂上 → 同一条提权重启路径（UI / AI / MCP 共用）。
            return self.reconcile_tun_intent().await;
        }
        self.boot_core().await
    }

    /// 热重载补不了 macOS TUN 权限；开关 TUN 与纠偏都走停再启。
    /// Author: kejiqing
    pub async fn restart(&self) -> Result<(), String> {
        let _ = self.stop().await;
        self.boot_core().await
    }

    /// 启动前先清端口上的进程，再拉起。清完仍绑不上就再清一次。
    /// Author: kejiqing
    async fn boot_core(&self) -> Result<(), String> {
        self.ensure_controller_free().await;
        match self.spawn_core().await {
            Ok(()) => Ok(()),
            Err(_) if port_open(self.profiles.controller_port()) => {
                self.ensure_controller_free().await;
                self.spawn_core().await
            }
            Err(err) => Err(err),
        }
    }

    async fn ensure_controller_free(&self) {
        self.force_free_controller();
        let port = self.profiles.controller_port();
        for _ in 0..40 {
            if !port_open(port) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// 配置要 TUN 但运行时未开时，自动提权重启；已对齐则不动。
    async fn reconcile_tun_intent(&self) -> Result<(), String> {
        if !self.profiles.tun_enabled_effective() {
            return Ok(());
        }
        if let Ok(view) = self.tun_snapshot().await {
            if view.enable {
                return Ok(());
            }
        }
        self.restart().await
    }

    async fn spawn_core(&self) -> Result<(), String> {
        let config = self.profiles.effective_file()?;
        let bin = self.resolve_bin()?;
        let work = self.profiles.root();
        let want_tun = self.profiles.tun_enabled_effective();
        if want_tun && (cfg!(windows) || cfg!(target_os = "android")) {
            return Err("当前平台本期不支持 TUN，请用系统代理（set_system_proxy）".into());
        }
        if want_tun {
            // macOS 创建 utun 要 root；一次性 setuid mihomo + 停内核 helper。
            privkill::ensure_tun_privilege(&bin, &work)?;
        }
        let log = File::create(&self.log_path).map_err(|err| err.to_string())?;
        let log_err = log.try_clone().map_err(|err| err.to_string())?;
        let mut command = Command::new(&bin);
        command
            .arg("-d")
            .arg(&work)
            .arg("-f")
            .arg(&config)
            .current_dir(&work)
            .env("SAFE_PATHS", work.as_os_str())
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        let child = command.spawn().map_err(|err| format!("无法启动 mihomo: {err}"))?;
        {
            let mut guard = self.child.lock().expect("内核锁已损坏");
            if guard.is_some() {
                drop(child);
                return Err("内核已在运行".into());
            }
            *guard = Some(child);
            *self.adopted_pid.lock().expect("内核锁已损坏") = None;
        }
        self.wait_healthy().await?;
        if want_tun {
            self.ensure_tun_up().await?;
        }
        Ok(())
    }

    async fn ensure_tun_up(&self) -> Result<(), String> {
        for _ in 0..20 {
            if let Ok(view) = self.tun_snapshot().await {
                if view.enable {
                    return Ok(());
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let detail = last_tun_error(&self.log_path).unwrap_or_else(|| {
            "内核未报告 TUN 错误，但运行时 tun.enable 仍为关".into()
        });
        Err(format!(
            "TUN 未能开启。{detail}。macOS 需管理员权限创建虚拟网卡；Windows 本期请关 TUN、改用系统代理。"
        ))
    }

    pub fn stop_blocking(&self) {
        self.force_free_controller();
        let port = self.profiles.controller_port();
        for _ in 0..40 {
            if !port_open(port) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// 结束本壳 mihomo，并清控制器端口。root 进程优先走 setuid helper（无密码）。
    /// Author: kejiqing
    fn force_free_controller(&self) {
        let mut pids = Vec::new();
        {
            let mut guard = self.child.lock().expect("内核锁已损坏");
            if let Some(mut child) = guard.take() {
                pids.push(child.id());
                // setuid root 时 kill 会 EPERM。不能 wait()，否则退出卡死，端口一直占着。
                let _ = child.kill();
                let _ = child.try_wait();
            }
        }
        if let Some(pid) = self.adopted_pid.lock().expect("内核锁已损坏").take() {
            pids.push(pid);
        }
        if let Ok(bin) = self.resolve_bin() {
            pids.extend(our_mihomo_pids(&bin, &self.profiles.root()));
        }
        let port = self.profiles.controller_port();
        pids.extend(listener_pids(port));
        pids.sort_unstable();
        pids.dedup();
        for pid in &pids {
            privkill::force_kill_pid(*pid);
        }
        if port_open(port) || pids.iter().any(|pid| process_alive(*pid)) {
            // 给套接字一点时间关掉，避免杀成功了还弹管理员框。
            std::thread::sleep(Duration::from_millis(200));
        }
        let still: Vec<u32> = pids.into_iter().filter(|pid| process_alive(*pid)).collect();
        if port_open(port) || !still.is_empty() {
            privkill::privileged_kill(&self.profiles.root(), port, &still);
        }
    }

    pub async fn stop(&self) -> Result<(), String> {
        let port = self.profiles.controller_port();
        let had = self.running() || port_open(port);
        self.force_free_controller();
        for _ in 0..40 {
            if !port_open(port) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !had {
            return Err("内核未运行".into());
        }
        Ok(())
    }

    /// 认领「本壳数据目录 + 官方 mihomo」孤儿，避免重启 UI 后误报端口占用。
    /// Author: kejiqing
    fn reclaim_orphan(&self) {
        {
            let adopted = self.adopted_pid.lock().expect("内核锁已损坏");
            if adopted.is_some() {
                return;
            }
        }
        {
            let child = self.child.lock().expect("内核锁已损坏");
            if child.is_some() {
                return;
            }
        }
        let port = self.profiles.controller_port();
        if !port_open(port) {
            return;
        }
        let Ok(bin) = self.resolve_bin() else {
            return;
        };
        let work = self.profiles.root();
        // lsof 经常看不到 setuid root 的套接字，进程表仍能按数据目录认领。
        let pid = listener_pids(port).into_iter().find(|pid| is_our_mihomo(*pid, &bin, &work)).or_else(|| {
            our_mihomo_pids(&bin, &work).into_iter().next()
        });
        if let Some(pid) = pid {
            *self.adopted_pid.lock().expect("内核锁已损坏") = Some(pid);
        }
    }

    pub async fn reload(&self) -> Result<(), String> {
        if !self.running() {
            return Ok(());
        }
        let config = self.profiles.effective_file()?;
        let response = self
            .request(reqwest::Method::PUT, "/configs?force=true")
            .json(&serde_json::json!({ "path": config, "payload": "" }))
            .send()
            .await
            .map_err(|err| format!("重载失败: {err}"))?;
        check_api(response).await
    }

    pub async fn configs(&self) -> Result<GeneralConfig, String> {
        let response = self
            .request(reqwest::Method::GET, "/configs")
            .send()
            .await
            .map_err(|err| format!("读取运行配置失败: {err}"))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("读取运行配置失败: HTTP {status} {text}"));
        }
        response
            .json()
            .await
            .map_err(|err| format!("读取运行配置失败: {err}"))
    }

    pub async fn tun_snapshot(&self) -> Result<TunView, String> {
        if !self.running() {
            return Ok(TunView::off());
        }
        let response = self
            .request(reqwest::Method::GET, "/configs")
            .send()
            .await
            .map_err(|err| format!("读取 TUN 失败: {err}"))?;
        if !response.status().is_success() {
            return Err(format!("读取 TUN 失败: HTTP {}", response.status()));
        }
        let body: serde_json::Value = response.json().await.map_err(|err| err.to_string())?;
        let tun = body.get("tun").cloned().unwrap_or(serde_json::Value::Null);
        Ok(TunView::from_json(&tun))
    }

    pub async fn version(&self) -> Result<String, String> {
        let response = self
            .request(reqwest::Method::GET, "/version")
            .send()
            .await
            .map_err(|err| format!("读取内核版本失败: {err}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("读取内核版本失败: HTTP {status}"));
        }
        let body: serde_json::Value = response.json().await.map_err(|err| err.to_string())?;
        Ok(body
            .get("version")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn groups(&self) -> Result<Vec<ProxyGroup>, String> {
        let response = self
            .request(reqwest::Method::GET, "/proxies")
            .send()
            .await
            .map_err(|err| format!("读取代理组失败: {err}"))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("读取代理组失败: HTTP {status} {text}"));
        }
        let body: serde_json::Value = response.json().await.map_err(|err| err.to_string())?;
        let Some(proxies) = body.get("proxies").and_then(|value| value.as_object()) else {
            return Err("代理组响应无效".into());
        };
        let mut groups = Vec::new();
        for (name, item) in proxies {
            let kind = item.get("type").and_then(|value| value.as_str()).unwrap_or("");
            if !matches!(kind, "Selector" | "URLTest" | "Fallback" | "LoadBalance") {
                continue;
            }
            let Some(all) = item.get("all").and_then(|value| value.as_array()) else {
                continue;
            };
            let now = item
                .get("now")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let names = all
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect::<Vec<_>>();
            groups.push(ProxyGroup {
                name: name.clone(),
                now,
                all: names,
            });
        }
        groups.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(groups)
    }

    pub async fn select(&self, group: &str, name: &str) -> Result<(), String> {
        let path = format!("/proxies/{}", urlencoding::encode(group));
        let response = self
            .request(reqwest::Method::PUT, &path)
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .map_err(|err| format!("选择节点失败: {err}"))?;
        check_api(response).await
    }

    /// 列出可测速的叶子节点（排除代理组与内置策略）。
    /// Author: kejiqing
    pub async fn leaf_proxy_names(&self) -> Result<Vec<String>, String> {
        let response = self
            .request(reqwest::Method::GET, "/proxies")
            .send()
            .await
            .map_err(|err| format!("读取节点失败: {err}"))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("读取节点失败: HTTP {status} {text}"));
        }
        let body: serde_json::Value = response.json().await.map_err(|err| err.to_string())?;
        let Some(proxies) = body.get("proxies").and_then(|value| value.as_object()) else {
            return Err("节点响应无效".into());
        };
        let mut names = Vec::new();
        for (name, item) in proxies {
            let kind = item.get("type").and_then(|value| value.as_str()).unwrap_or("");
            if !is_leaf_proxy(kind) {
                continue;
            }
            names.push(name.clone());
        }
        names.sort();
        Ok(names)
    }

    /// 测单个节点。504 记为超时，不把整次调用当成失败。
    /// Author: kejiqing
    pub async fn proxy_delay(&self, name: &str) -> Result<DelaySample, String> {
        let path = delay_path("/proxies", name);
        let (status, text) = self.delay_request(&path).await?;
        parse_proxy_delay_http(name, status, &text)
    }

    /// 测全部叶子节点。并发有上限，不切换当前节点。
    /// Author: kejiqing
    pub async fn all_leaf_delays(&self) -> Result<Vec<DelaySample>, String> {
        let names = self.leaf_proxy_names().await?;
        if names.is_empty() {
            return Ok(Vec::new());
        }
        const CONCURRENCY: usize = 10;
        let client = self.client.clone();
        let port = self.profiles.controller_port();
        let secret = self.profiles.secret();
        let gate = Arc::new(tokio::sync::Semaphore::new(CONCURRENCY));
        let mut joins = Vec::with_capacity(names.len());
        for name in names {
            let client = client.clone();
            let secret = secret.clone();
            let gate = Arc::clone(&gate);
            joins.push(tokio::spawn(async move {
                let _permit = gate
                    .acquire_owned()
                    .await
                    .map_err(|err| format!("测速调度失败: {err}"))?;
                // 整批测速时单节点失败记超时，不拖垮整次调用。
                match delay_one_proxy(&client, port, &secret, &name).await {
                    Ok(sample) => Ok::<DelaySample, String>(sample),
                    Err(_) => Ok(DelaySample {
                        name,
                        delay_ms: None,
                    }),
                }
            }));
        }
        let mut samples = Vec::with_capacity(joins.len());
        for join in joins {
            let sample = join
                .await
                .map_err(|err| format!("测速任务失败: {err}"))??;
            samples.push(sample);
        }
        Ok(samples)
    }

    async fn delay_request(&self, path: &str) -> Result<(reqwest::StatusCode, String), String> {
        let response = self
            .request(reqwest::Method::GET, path)
            .timeout(Duration::from_millis(DELAY_TIMEOUT_MS + 7_000))
            .send()
            .await
            .map_err(|err| format!("测速失败: {err}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Ok((status, text))
    }

    pub async fn rules(&self) -> Result<Vec<RuleRow>, String> {
        if !self.running() {
            return Err("内核未运行".into());
        }
        let response = self
            .request(reqwest::Method::GET, "/rules")
            .send()
            .await
            .map_err(|err| format!("读取规则失败: {err}"))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("读取规则失败: HTTP {status} {text}"));
        }
        let body: serde_json::Value = response.json().await.map_err(|err| err.to_string())?;
        let Some(items) = body.get("rules").and_then(|value| value.as_array()) else {
            return Err("规则响应无效".into());
        };
        let mut rows = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            rows.push(RuleRow {
                index,
                kind: item
                    .get("type")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string(),
                payload: item
                    .get("payload")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string(),
                proxy: item
                    .get("proxy")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
        Ok(rows)
    }

    /// 优先走 mihomo /dns/query；失败则系统解析。
    /// Author: kejiqing
    pub async fn dns_lookup(&self, name: &str) -> Result<Vec<String>, String> {
        if self.running() {
            if let Ok(ips) = self.dns_via_controller(name).await {
                if !ips.is_empty() {
                    return Ok(ips);
                }
            }
        }
        system_dns_lookup(name).await
    }

    async fn dns_via_controller(&self, name: &str) -> Result<Vec<String>, String> {
        let path = format!(
            "/dns/query?name={}&type=A",
            urlencoding::encode(name)
        );
        let response = self
            .request(reqwest::Method::GET, &path)
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map_err(|err| err.to_string())?;
        if !response.status().is_success() {
            return Err(format!("DNS API HTTP {}", response.status()));
        }
        let body: serde_json::Value = response.json().await.map_err(|err| err.to_string())?;
        let mut ips = Vec::new();
        if let Some(answers) = body.get("Answer").and_then(|value| value.as_array()) {
            for answer in answers {
                let data = answer
                    .get("data")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .trim();
                if data.parse::<std::net::IpAddr>().is_ok() {
                    ips.push(data.to_string());
                }
            }
        }
        Ok(ips)
    }

    pub async fn connections_text(&self, host_filter: Option<&str>) -> Result<String, String> {
        let rows = self.connection_evidence(host_filter).await?;
        if rows.is_empty() {
            return Ok(match host_filter.filter(|value| !value.is_empty()) {
                Some(filter) => format!("没有匹配 {filter} 的连接"),
                None => "当前无连接".into(),
            });
        }
        let mut lines = Vec::new();
        for row in rows.iter().take(40) {
            lines.push(format_conn_evidence(row));
        }
        Ok(format!("连接 {} 条（最多 40）\n{}", lines.len(), lines.join("\n")))
    }

    /// 采一帧 `/traffic`（流式接口取首行）。未运行时返回 0。
    /// Author: kejiqing
    pub async fn traffic_sample(&self) -> Result<TrafficSample, String> {
        if !self.running() {
            return Ok(TrafficSample {
                up: 0,
                down: 0,
                running: false,
            });
        }
        use futures_util::StreamExt;
        // mihomo `/traffic` 首帧在 ticker 约 1s 后才推；超时需盖过首帧。Author: kejiqing
        let response = self
            .request(reqwest::Method::GET, "/traffic")
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .map_err(|err| format!("读取流量失败: {err}"))?;
        if !response.status().is_success() {
            return Err(format!("读取流量失败: HTTP {}", response.status()));
        }
        let mut stream = response.bytes_stream();
        let mut buf = String::new();
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            let next = tokio::time::timeout(Duration::from_millis(2500), stream.next()).await;
            match next {
                Ok(Some(Ok(chunk))) => {
                    buf.push_str(&String::from_utf8_lossy(&chunk));
                    if let Some(line) = buf.lines().find(|item| !item.trim().is_empty()) {
                        let value: serde_json::Value = serde_json::from_str(line.trim())
                            .map_err(|err| format!("流量解析失败: {err}"))?;
                        let up = json_u64(&value, "up");
                        let down = json_u64(&value, "down");
                        return Ok(TrafficSample {
                            up,
                            down,
                            running: true,
                        });
                    }
                }
                Ok(Some(Err(err))) => return Err(format!("读取流量失败: {err}")),
                Ok(None) => break,
                Err(_) => break,
            }
        }
        Err("读取流量超时".into())
    }

    /// 读取 `/connections` 运行时证据。可按 host 子串过滤（大小写不敏感）。
    /// Author: kejiqing
    pub async fn connection_evidence(
        &self,
        host_filter: Option<&str>,
    ) -> Result<Vec<ConnEvidence>, String> {
        if !self.running() {
            return Err("内核未运行".into());
        }
        let response = self
            .request(reqwest::Method::GET, "/connections")
            .send()
            .await
            .map_err(|err| format!("读取连接失败: {err}"))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(format!("读取连接失败: HTTP {status} {text}"));
        }
        let body: serde_json::Value = response.json().await.map_err(|err| err.to_string())?;
        Ok(parse_connection_evidence(&body, host_filter))
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let port = self.profiles.controller_port();
        let secret = self.profiles.secret();
        self.client
            .request(method, format!("http://127.0.0.1:{port}{path}"))
            .header("Authorization", format!("Bearer {secret}"))
    }

    async fn wait_healthy(&self) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            {
                let mut guard = self.child.lock().expect("内核锁已损坏");
                if let Some(child) = guard.as_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            let tail = log_tail(&self.log_path);
                            *guard = None;
                            return Err(format!("mihomo 已退出，状态 {status}。\n{tail}"));
                        }
                        Ok(None) => {}
                        Err(err) => return Err(format!("无法检查 mihomo: {err}")),
                    }
                } else {
                    return Err("mihomo 未在运行".into());
                }
            }
            if self.version().await.is_ok() {
                return Ok(());
            }
            if Instant::now() > deadline {
                self.stop_blocking();
                let tail = log_tail(&self.log_path);
                return Err(format!("mihomo 未在 8 秒内就绪。\n{tail}"));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop_blocking();
    }
}

/// 运行时 TUN。诊断要看到 auto-route、劫持的 DNS 和网卡地址，不能只看开关。
/// Author: kejiqing
pub struct TunView {
    pub enable: bool,
    pub device: String,
    pub auto_route: bool,
    pub dns_hijack: String,
    pub inet4: String,
}

impl TunView {
    pub fn off() -> Self {
        Self {
            enable: false,
            device: String::new(),
            auto_route: false,
            dns_hijack: String::new(),
            inet4: String::new(),
        }
    }

    pub fn from_json(tun: &serde_json::Value) -> Self {
        let enable = tun
            .get("enable")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let device = tun
            .get("device")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let auto_route = tun
            .get("auto-route")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let dns_hijack = json_string_list(tun.get("dns-hijack"));
        let inet4 = json_string_list(tun.get("inet4-address"));
        Self {
            enable,
            device,
            auto_route,
            dns_hijack,
            inet4,
        }
    }

    pub fn summary(&self) -> String {
        let device = if self.device.is_empty() {
            "(空)"
        } else {
            self.device.as_str()
        };
        if !self.enable {
            return format!("关 device={device}");
        }
        format!(
            "开 device={device} auto-route={} dns-hijack={} inet4={}",
            if self.auto_route { "true" } else { "false" },
            if self.dns_hijack.is_empty() {
                "(无)"
            } else {
                self.dns_hijack.as_str()
            },
            if self.inet4.is_empty() {
                "(无)"
            } else {
                self.inet4.as_str()
            }
        )
    }
}

fn json_u64(value: &serde_json::Value, key: &str) -> u64 {
    value
        .get(key)
        .and_then(|item| {
            item.as_u64()
                .or_else(|| item.as_i64().and_then(|n| u64::try_from(n).ok()))
                .or_else(|| item.as_f64().map(|n| n.max(0.0) as u64))
        })
        .unwrap_or(0)
}

fn json_string_list(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

#[derive(Deserialize)]
pub struct GeneralConfig {
    pub mode: Option<String>,
    #[serde(rename = "mixed-port")]
    pub mixed_port: Option<u16>,
}

use serde::Deserialize;

fn reap(child: &mut Option<Child>) -> bool {
    if let Some(proc) = child.as_mut() {
        match proc.try_wait() {
            Ok(None) => true,
            _ => {
                *child = None;
                false
            }
        }
    } else {
        false
    }
}

fn port_open(port: u16) -> bool {
    let Ok(addr) = format!("127.0.0.1:{port}").parse::<SocketAddr>() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let Ok(output) = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
        else {
            return false;
        };
        if !output.status.success() {
            return false;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        return text.contains(&pid.to_string());
    }
    #[cfg(not(windows))]
    {
        // setuid root 的内核：普通用户 `kill -0` 常返回 EPERM，不能当成已死。
        if Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
        {
            return true;
        }
        !process_command(pid).is_empty()
    }
}

fn last_tun_error(log_path: &Path) -> Option<String> {
    let text = fs::read_to_string(log_path).ok()?;
    text.lines()
        .rev()
        .find(|line| line.contains("Start TUN listening error") || line.contains("operation not permitted"))
        .map(|line| {
            line.split("msg=")
                .nth(1)
                .unwrap_or(line)
                .trim_matches('"')
                .to_string()
        })
}

async fn check_api(response: reqwest::Response) -> Result<(), String> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let text = response.text().await.unwrap_or_default();
    Err(format!("mihomo API {status}: {text}"))
}

fn delay_path(prefix: &str, name: &str) -> String {
    format!(
        "{prefix}/{}/delay?timeout={DELAY_TIMEOUT_MS}&url={}",
        urlencoding::encode(name),
        urlencoding::encode(DELAY_TEST_URL)
    )
}

/// Author: kejiqing
pub fn format_conn_evidence(row: &ConnEvidence) -> String {
    let dest = if row.dest_port.is_empty() {
        row.dest_ip.clone()
    } else if row.dest_ip.is_empty() {
        format!(":{}", row.dest_port)
    } else {
        format!("{}:{}", row.dest_ip, row.dest_port)
    };
    format!(
        "host={} dest={} type={} net={} rule={} {} chain={} start={}",
        row.host,
        dest,
        row.conn_type,
        row.network,
        row.rule,
        row.rule_payload,
        row.chain,
        row.start
    )
}

/// Author: kejiqing
fn parse_connection_evidence(
    body: &serde_json::Value,
    host_filter: Option<&str>,
) -> Vec<ConnEvidence> {
    let Some(items) = body.get("connections").and_then(|value| value.as_array()) else {
        return Vec::new();
    };
    let filter = host_filter
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    let mut rows = Vec::new();
    for item in items {
        let Some(row) = parse_one_connection(item) else {
            continue;
        };
        if let Some(filter) = &filter {
            let blob = format!("{} {}", row.host, row.dest_ip).to_ascii_lowercase();
            if !blob.contains(filter) {
                continue;
            }
        }
        rows.push(row);
    }
    rows
}

fn parse_one_connection(item: &serde_json::Value) -> Option<ConnEvidence> {
    let meta = item.get("metadata")?;
    let host = meta
        .get("host")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let dest_ip = meta
        .get("destinationIP")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            meta.get("remoteDestination")
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("")
        .to_string();
    let dest_port = meta
        .get("destinationPort")
        .map(|value| match value {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Number(num) => num.to_string(),
            _ => String::new(),
        })
        .unwrap_or_default();
    let network = meta
        .get("network")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let conn_type = meta
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let rule = item
        .get("rule")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let rule_payload = item
        .get("rulePayload")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let chain = item
        .get("chains")
        .and_then(|value| value.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(">")
        })
        .unwrap_or_default();
    let start = item
        .get("start")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let id = item
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    Some(ConnEvidence {
        id,
        host,
        dest_ip,
        dest_port,
        network,
        conn_type,
        rule,
        rule_payload,
        chain,
        start,
    })
}

/// 叶子出站节点；组与内置策略不可当作「节点」测速。
/// Author: kejiqing
fn is_leaf_proxy(kind: &str) -> bool {
    !matches!(
        kind,
        "Selector"
            | "URLTest"
            | "Fallback"
            | "LoadBalance"
            | "Relay"
            | "Direct"
            | "Reject"
            | "RejectDrop"
            | "Compatible"
            | "Pass"
            | "Dns"
            | "Mirror"
    )
}

async fn delay_one_proxy(
    client: &reqwest::Client,
    port: u16,
    secret: &str,
    name: &str,
) -> Result<DelaySample, String> {
    let path = delay_path("/proxies", name);
    let response = client
        .get(format!("http://127.0.0.1:{port}{path}"))
        .header("Authorization", format!("Bearer {secret}"))
        .timeout(Duration::from_millis(DELAY_TIMEOUT_MS + 7_000))
        .send()
        .await
        .map_err(|err| format!("测速失败: {err}"))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    parse_proxy_delay_http(name, status, &text)
}

fn parse_proxy_delay_http(
    name: &str,
    status: reqwest::StatusCode,
    text: &str,
) -> Result<DelaySample, String> {
    if status.as_u16() == 504 || status.as_u16() == 408 {
        return Ok(DelaySample {
            name: name.to_string(),
            delay_ms: None,
        });
    }
    if status.as_u16() == 404 {
        return Err(format!("节点不存在: {name}"));
    }
    if !status.is_success() {
        return Err(format!("测速失败: HTTP {status} {}", clip(text)));
    }
    let body: serde_json::Value =
        serde_json::from_str(text).map_err(|err| format!("测速响应无效: {err}"))?;
    parse_proxy_delay(name, &body)
}

fn parse_proxy_delay(name: &str, body: &serde_json::Value) -> Result<DelaySample, String> {
    let Some(value) = body.get("delay") else {
        return Err("测速响应无效".into());
    };
    Ok(DelaySample {
        name: name.to_string(),
        delay_ms: delay_ms(value),
    })
}

fn delay_ms(value: &serde_json::Value) -> Option<u32> {
    let n = value.as_u64()?;
    if n == 0 || n > u32::MAX as u64 {
        None
    } else {
        Some(n as u32)
    }
}

fn clip(text: &str) -> String {
    text.chars().take(200).collect()
}

fn log_tail(path: &std::path::Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(30);
    lines[start..].join("\n")
}

async fn system_dns_lookup(name: &str) -> Result<Vec<String>, String> {
    let name = name.to_string();
    tokio::task::spawn_blocking(move || {
        use std::net::ToSocketAddrs;
        let addrs = (name.as_str(), 0)
            .to_socket_addrs()
            .map_err(|err| format!("DNS 解析失败: {err}"))?;
        let mut ips = Vec::new();
        for addr in addrs {
            let ip = addr.ip().to_string();
            if !ips.iter().any(|item| item == &ip) {
                ips.push(ip);
            }
        }
        if ips.is_empty() {
            Err("DNS 无结果".into())
        } else {
            Ok(ips)
        }
    })
    .await
    .map_err(|err| format!("DNS 任务失败: {err}"))?
}

#[cfg(test)]
mod tests {
    use super::{
        format_conn_evidence, is_leaf_proxy, parse_connection_evidence, parse_proxy_delay,
    };
    use serde_json::json;

    #[test]
    fn leaf_proxy_excludes_groups_and_builtins() {
        assert!(is_leaf_proxy("Vmess"));
        assert!(is_leaf_proxy("Shadowsocks"));
        assert!(is_leaf_proxy("Hysteria2"));
        assert!(!is_leaf_proxy("Selector"));
        assert!(!is_leaf_proxy("URLTest"));
        assert!(!is_leaf_proxy("Direct"));
        assert!(!is_leaf_proxy("Reject"));
    }

    #[test]
    fn proxy_delay_reads_delay_field() {
        let row = parse_proxy_delay("香港-01", &json!({"delay": 42})).unwrap();
        assert_eq!(row.name, "香港-01");
        assert_eq!(row.delay_ms, Some(42));
        assert!(parse_proxy_delay("x", &json!({"message": "no"})).is_err());
    }

    #[test]
    fn bandwidth_url_switches_on_region() {
        // 不依赖进程环境是否已设 REGION：分别断言两路常量语义。
        let china = "https://mirrors.tuna.tsinghua.edu.cn/ubuntu-releases/24.04/ubuntu-24.04.1-desktop-amd64.iso";
        let intl = "https://speed.cloudflare.com/__down?bytes=25000000";
        let url = super::bandwidth_test_url();
        assert!(url == china || url == intl, "{url}");
        if std::env::var("REGION").ok().as_deref() == Some("china") {
            assert_eq!(url, china);
        } else {
            assert_eq!(url, intl);
        }
    }

    #[test]
    fn connection_evidence_parses_chains_from_api() {
        let body = json!({
            "connections": [{
                "id": "abc",
                "start": "2026-11-05T10:00:00.000Z",
                "rule": "DomainSuffix",
                "rulePayload": "github.com",
                "chains": ["香港-01", "PROXY"],
                "metadata": {
                    "host": "www.github.com",
                    "destinationIP": "1.2.3.4",
                    "destinationPort": "443",
                    "network": "tcp",
                    "type": "HTTPS"
                }
            }]
        });
        let rows = parse_connection_evidence(&body, Some("github.com"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].chain, "香港-01>PROXY");
        assert_eq!(rows[0].rule, "DomainSuffix");
        let text = format_conn_evidence(&rows[0]);
        assert!(text.contains("chain=香港-01>PROXY"), "{text}");
        assert!(text.contains("dest=1.2.3.4:443"), "{text}");
        assert!(parse_connection_evidence(&body, Some("nomatch")).is_empty());
    }
}
