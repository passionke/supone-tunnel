// 作者: kejiqing
//! 界面可改的壳设置。模型地址只走这一份，不另开环境变量分叉。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(default)]
    pub agent_base_url: String,
    #[serde(default)]
    pub agent_api_key: String,
    #[serde(default)]
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

fn default_mcp_bind() -> String {
    "0.0.0.0:18765".into()
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub agent_base_url: String,
    pub agent_api_key: String,
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
        next.agent_api_key = next.agent_api_key.trim().to_string();
        next.agent_model = next.agent_model.trim().to_string();
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

    pub fn agent_ready(settings: &AppSettings) -> bool {
        !settings.agent_base_url.is_empty()
            && !settings.agent_api_key.is_empty()
            && !settings.agent_model.is_empty()
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
