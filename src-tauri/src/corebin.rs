// 作者: kejiqing
//! 只认 MetaCubeX 官方 mihomo。Clash Verge 自带内核一律拒绝。

use std::fs::{self, File};
use std::io::{copy, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use flate2::read::GzDecoder;
use serde::Deserialize;

use crate::settings::reject_verge;

const RELEASE_API: &str = "https://api.github.com/repos/MetaCubeX/mihomo/releases/latest";

pub fn bundled_path(root: &Path) -> PathBuf {
    let name = if cfg!(windows) { "mihomo.exe" } else { "mihomo" };
    root.join("bin").join(name)
}

pub fn resolve(root: &Path, configured: &str) -> Result<PathBuf, String> {
    if !configured.trim().is_empty() {
        let path = PathBuf::from(configured.trim());
        reject_verge(&path)?;
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!("内核路径不存在: {}", path.display()));
    }
    let bundled = bundled_path(root);
    if bundled.is_file() {
        reject_verge(&bundled)?;
        return Ok(bundled);
    }
    if let Ok(explicit) = std::env::var("MIHOMO_BIN") {
        let path = PathBuf::from(explicit);
        reject_verge(&path)?;
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!("MIHOMO_BIN 不存在: {}", path.display()));
    }
    Err("未找到官方 mihomo。请在设置里下载官方内核，或填写内核路径。".into())
}

pub async fn download_official(root: &Path) -> Result<PathBuf, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent("supone-tunnel")
        .build()
        .map_err(|err| err.to_string())?;
    let api = mirror_url(RELEASE_API);
    let release: Release = client
        .get(&api)
        .send()
        .await
        .map_err(|err| format!("读取 mihomo 发行版失败: {err}"))?
        .error_for_status()
        .map_err(|err| format!("读取 mihomo 发行版失败: {err}"))?
        .json()
        .await
        .map_err(|err| format!("解析 mihomo 发行版失败: {err}"))?;
    let want = asset_prefix();
    let asset = release
        .assets
        .iter()
        .find(|item| {
            item.name.starts_with(want)
                && item.name.ends_with(".gz")
                && !item.name.contains("compatible")
                && !item.name.contains("-go1")
                && !item.name.contains("-v1-")
                && !item.name.contains("-v2-")
                && !item.name.contains("-v3-")
        })
        .or_else(|| {
            release.assets.iter().find(|item| {
                item.name.starts_with(want) && item.name.ends_with(".gz") && !item.name.contains("compatible")
            })
        })
        .ok_or_else(|| format!("发行版 {} 没有匹配的官方包", release.tag_name))?;
    let url = mirror_url(&asset.browser_download_url);
    let bytes = client
        .get(&url)
        .send()
        .await
        .map_err(|err| format!("下载 mihomo 失败: {err}"))?
        .error_for_status()
        .map_err(|err| format!("下载 mihomo 失败: {err}"))?
        .bytes()
        .await
        .map_err(|err| format!("下载 mihomo 失败: {err}"))?;
    let target = bundled_path(root);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let tmp = target.with_extension("tmp");
    {
        let mut decoder = GzDecoder::new(bytes.as_ref());
        let mut file = File::create(&tmp).map_err(|err| err.to_string())?;
        copy(&mut decoder, &mut file).map_err(|err| format!("解压 mihomo 失败: {err}"))?;
        file.flush().map_err(|err| err.to_string())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).map_err(|err| err.to_string())?;
    }
    fs::rename(&tmp, &target).map_err(|err| err.to_string())?;
    reject_verge(&target)?;
    Ok(target)
}

fn asset_prefix() -> &'static str {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "mihomo-darwin-arm64-"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "mihomo-darwin-amd64-"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "mihomo-linux-arm64-"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "mihomo-linux-amd64-"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "mihomo-windows-amd64-"
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        "mihomo-windows-arm64-"
    } else {
        "mihomo-unsupported-"
    }
}

fn mirror_url(url: &str) -> String {
    if std::env::var("REGION").ok().as_deref() == Some("china") {
        format!("https://ghfast.top/{url}")
    } else {
        url.to_string()
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_verge_paths() {
        assert!(reject_verge(Path::new("/Applications/Clash Verge.app/Contents/MacOS/verge-mihomo")).is_err());
        assert!(reject_verge(Path::new("/data/bin/mihomo")).is_ok());
    }
}
