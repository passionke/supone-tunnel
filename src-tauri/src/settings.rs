// 作者: kejiqing
//! 界面可改的壳设置。对话凭证由订阅 token 换取，不另开用户配置面。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    /// 由 Hub 换取响应写入；设置页不展示。Author: kejiqing
    #[serde(default = "default_agent_base_url")]
    pub agent_base_url: String,
    /// 由 Hub 换取的 ngmk_；设置页不展示。Author: kejiqing
    #[serde(default)]
    pub agent_api_key: String,
    /// Hub 约定的 call_token 失效时间（UTC RFC3339）。Author: kejiqing
    #[serde(default)]
    pub agent_call_expires_at: String,
    /// 签发时绑定的订阅 token，变更则重签。Author: kejiqing
    #[serde(default)]
    pub agent_subscription_token: String,
    /// Hub users.id；对话归因用。Author: kejiqing
    #[serde(default)]
    pub agent_hub_user_id: i64,
    #[serde(default = "default_agent_model")]
    pub agent_model: String,
    /// 可选。留空则使用应用目录里的官方 mihomo。
    #[serde(default)]
    pub mihomo_bin: String,
    /// 向 NeuroGate 暴露 MCP（HTTP + Bearer）。
    #[serde(default)]
    pub mcp_enabled: bool,
    #[serde(default = "default_mcp_bind")]
    pub mcp_bind: String,
    #[serde(default)]
    pub mcp_token: String,
}

fn default_agent_base_url() -> String {
    "https://neurogate.bitkindle.com/v1".into()
}

fn default_agent_model() -> String {
    "agent".into()
}

fn default_mcp_bind() -> String {
    "0.0.0.0:18765".into()
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub agent_base_url: String,
    pub agent_api_key: String,
    pub agent_call_expires_at: String,
    pub agent_subscription_token: String,
    pub agent_hub_user_id: i64,
    pub agent_model: String,
    pub mihomo_bin: String,
    pub resolved_mihomo_bin: Option<String>,
    pub agent_ready: bool,
    pub mcp_enabled: bool,
    pub mcp_bind: String,
    pub mcp_token: String,
}

struct Inner {
    path: PathBuf,
    data: AppSettings,
}

#[derive(Clone)]
pub struct SettingsStore {
    inner: Arc<Mutex<Inner>>,
}

impl SettingsStore {
    pub fn open(root: &Path) -> Result<Self, String> {
        let path = root.join("settings.json");
        let data = if path.exists() {
            let text = fs::read_to_string(&path).map_err(|err| err.to_string())?;
            serde_json::from_str(&text).map_err(|err| format!("settings.json 无效: {err}"))?
        } else {
            AppSettings::default()
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner { path, data })),
        })
    }

    pub fn get(&self) -> AppSettings {
        let mut data = self.inner.lock().expect("设置锁已损坏").data.clone();
        if data.agent_base_url.trim().is_empty() {
            data.agent_base_url = default_agent_base_url();
        }
        if data.agent_model.trim().is_empty() {
            data.agent_model = default_agent_model();
        }
        if data.mcp_bind.trim().is_empty() {
            data.mcp_bind = default_mcp_bind();
        }
        data
    }

    pub fn mcp_config(&self) -> crate::mcp::McpConfig {
        let data = self.get();
        crate::mcp::McpConfig {
            enabled: data.mcp_enabled,
            bind: data.mcp_bind,
            token: data.mcp_token,
        }
    }

    pub fn save(&self, mut next: AppSettings) -> Result<AppSettings, String> {
        next.agent_base_url = next.agent_base_url.trim().trim_end_matches('/').to_string();
        if next.agent_base_url.is_empty() {
            next.agent_base_url = default_agent_base_url();
        }
        next.agent_api_key = next.agent_api_key.trim().to_string();
        next.agent_call_expires_at = next.agent_call_expires_at.trim().to_string();
        next.agent_subscription_token = next.agent_subscription_token.trim().to_string();
        next.agent_model = next.agent_model.trim().to_string();
        if next.agent_model.is_empty() {
            next.agent_model = default_agent_model();
        }
        next.mihomo_bin = next.mihomo_bin.trim().to_string();
        next.mcp_bind = next.mcp_bind.trim().to_string();
        if next.mcp_bind.is_empty() {
            next.mcp_bind = default_mcp_bind();
        }
        next.mcp_token = next.mcp_token.trim().to_string();
        if !next.mihomo_bin.is_empty() {
            reject_verge(&PathBuf::from(&next.mihomo_bin))?;
            if !Path::new(&next.mihomo_bin).is_file() {
                return Err(format!("内核路径不存在: {}", next.mihomo_bin));
            }
        }
        let mut guard = self.inner.lock().expect("设置锁已损坏");
        let text = serde_json::to_string_pretty(&next).map_err(|err| err.to_string())?;
        write_private(&guard.path, text.as_bytes())?;
        guard.data = next.clone();
        Ok(next)
    }

    /// 写入 Hub 换取的对话凭证。Author: kejiqing
    pub fn save_call_credentials(
        &self,
        subscription_token: &str,
        base_url: &str,
        call_token: &str,
        expires_at: &str,
        hub_user_id: i64,
    ) -> Result<AppSettings, String> {
        if hub_user_id <= 0 {
            return Err("Hub 未返回有效 user_id".into());
        }
        let mut data = self.get();
        data.agent_subscription_token = subscription_token.trim().to_string();
        data.agent_base_url = base_url.trim().trim_end_matches('/').to_string();
        data.agent_api_key = call_token.trim().to_string();
        data.agent_call_expires_at = expires_at.trim().to_string();
        data.agent_hub_user_id = hub_user_id;
        if data.agent_model.trim().is_empty() {
            data.agent_model = default_agent_model();
        }
        self.save(data)
    }
}

pub fn reject_verge(path: &Path) -> Result<(), String> {
    let text = path.to_string_lossy().to_ascii_lowercase();
    if text.contains("verge-mihomo") || text.contains("clash verge") {
        return Err("不要使用 Clash Verge 自带内核，请用 MetaCubeX 官方 mihomo".into());
    }
    Ok(())
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
