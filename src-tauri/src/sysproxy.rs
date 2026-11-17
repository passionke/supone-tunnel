// 作者: kejiqing
//! 系统代理是操作系统设置，不写进 mihomo 配置。

use std::process::Command;

pub fn set_enabled(enable: bool, port: u16) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return macos_set(enable, port);
    }
    #[cfg(target_os = "windows")]
    {
        return windows_set(enable, port);
    }
    #[cfg(target_os = "linux")]
    {
        return gnome_set(enable, port);
    }
    #[cfg(target_os = "android")]
    {
        return android_set(enable, port);
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "windows",
        target_os = "linux",
        target_os = "android"
    )))]
    {
        let _ = (enable, port);
        Err("当前系统不支持系统代理".into())
    }
}

pub fn is_enabled() -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        return macos_enabled();
    }
    #[cfg(target_os = "windows")]
    {
        return windows_enabled();
    }
    #[cfg(target_os = "linux")]
    {
        return gnome_enabled();
    }
    #[cfg(target_os = "android")]
    {
        return android_enabled();
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "windows",
        target_os = "linux",
        target_os = "android"
    )))]
    {
        Err("当前系统不支持系统代理".into())
    }
}

/// Android：写全局 http_proxy（需 WRITE_SECURE_SETTINGS 或 root su）。
/// 本期不做 VpnService；Chrome 可能不认该设置。Author: kejiqing
#[cfg(target_os = "android")]
fn android_set(enable: bool, port: u16) -> Result<(), String> {
    let value = if enable {
        format!("127.0.0.1:{port}")
    } else {
        ":0".into()
    };
    // 不用 su：避免启动时弹 Superuser；部署侧 pm grant WRITE_SECURE_SETTINGS。
    android_settings_put(&value).map_err(|err| {
        format!("{err}；请 adb shell pm grant <pkg> android.permission.WRITE_SECURE_SETTINGS")
    })
}

#[cfg(target_os = "android")]
fn android_enabled() -> Result<bool, String> {
    let output = Command::new("settings")
        .args(["get", "global", "http_proxy"])
        .output()
        .map_err(|_| "无法读 Android 系统代理".to_string())?;
    // 无 WRITE_SECURE_SETTINGS 时 settings 会吐 UserManager 异常；当作未开启，不刷屏。
    if !output.status.success() {
        return Ok(false);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    if text.contains("Security exception") || text.contains("Exception") {
        return Ok(false);
    }
    Ok(parse_android_http_proxy(&text))
}

#[cfg(target_os = "android")]
fn android_settings_put(value: &str) -> Result<(), String> {
    let output = Command::new("settings")
        .args(["put", "global", "http_proxy", value])
        .output()
        .map_err(|err| format!("无法写 Android 系统代理: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error(&output))
    }
}

pub(crate) fn parse_android_http_proxy(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty() && t != "null" && t != ":0" && t.contains("127.0.0.1")
}

fn macos_set(enable: bool, port: u16) -> Result<(), String> {
    let output = Command::new("networksetup")
        .arg("-listallnetworkservices")
        .output()
        .map_err(|err| format!("无法执行 networksetup: {err}"))?;
    if !output.status.success() {
        return Err(command_error(&output));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut errors = Vec::new();
    for line in text.lines().skip(1) {
        let name = line.trim();
        if name.is_empty() || name.starts_with('*') || name.starts_with("An asterisk") {
            continue;
        }
        let result = if enable {
            macos_enable_service(name, port)
        } else {
            macos_disable_service(name)
        };
        if let Err(err) = result {
            errors.push(format!("{name}: {err}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn macos_enable_service(name: &str, port: u16) -> Result<(), String> {
    let port = port.to_string();
    for (flag, extra) in [
        ("-setwebproxy", true),
        ("-setsecurewebproxy", true),
        ("-setsocksfirewallproxy", true),
    ] {
        let mut command = Command::new("networksetup");
        command.arg(flag).arg(name).arg("127.0.0.1").arg(&port);
        if extra {
            command.arg("off");
        }
        let output = command
            .output()
            .map_err(|err| format!("无法执行 networksetup: {err}"))?;
        if !output.status.success() {
            return Err(command_error(&output));
        }
    }
    for flag in [
        "-setwebproxystate",
        "-setsecurewebproxystate",
        "-setsocksfirewallproxystate",
    ] {
        let output = Command::new("networksetup")
            .args([flag, name, "on"])
            .output()
            .map_err(|err| format!("无法执行 networksetup: {err}"))?;
        if !output.status.success() {
            return Err(command_error(&output));
        }
    }
    Ok(())
}

fn macos_disable_service(name: &str) -> Result<(), String> {
    for flag in [
        "-setwebproxystate",
        "-setsecurewebproxystate",
        "-setsocksfirewallproxystate",
    ] {
        let output = Command::new("networksetup")
            .args([flag, name, "off"])
            .output()
            .map_err(|err| format!("无法执行 networksetup: {err}"))?;
        if !output.status.success() {
            return Err(command_error(&output));
        }
    }
    Ok(())
}

fn macos_enabled() -> Result<bool, String> {
    let output = Command::new("networksetup")
        .arg("-listallnetworkservices")
        .output()
        .map_err(|err| format!("无法执行 networksetup: {err}"))?;
    if !output.status.success() {
        return Err(command_error(&output));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines().skip(1) {
        let name = line.trim();
        if name.is_empty() || name.starts_with('*') || name.starts_with("An asterisk") {
            continue;
        }
        let output = Command::new("networksetup")
            .args(["-getwebproxy", name])
            .output()
            .map_err(|err| format!("无法执行 networksetup: {err}"))?;
        if !output.status.success() {
            continue;
        }
        if parse_macos_web_proxy(&String::from_utf8_lossy(&output.stdout)) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn parse_macos_web_proxy(text: &str) -> bool {
    let mut enabled = false;
    let mut server = "";
    for line in text.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("Enabled:") {
            enabled = value.trim().eq_ignore_ascii_case("Yes");
        }
        if let Some(value) = line.strip_prefix("Server:") {
            server = value.trim();
        }
    }
    enabled && server == "127.0.0.1"
}

fn windows_set(enable: bool, port: u16) -> Result<(), String> {
    let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let enable_value = if enable { "1" } else { "0" };
    reg_add(key, "ProxyEnable", "REG_DWORD", enable_value)?;
    if enable {
        reg_add(key, "ProxyServer", "REG_SZ", &format!("127.0.0.1:{port}"))?;
        reg_add(key, "ProxyOverride", "REG_SZ", "<local>")?;
    }
    let script = r#"
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class WinInet {
  [DllImport("wininet.dll", SetLastError=true)]
  public static extern bool InternetSetOption(IntPtr hInternet, int dwOption, IntPtr lpBuffer, int dwBufferLength);
}
"@
[WinInet]::InternetSetOption([IntPtr]::Zero, 39, [IntPtr]::Zero, 0) | Out-Null
[WinInet]::InternetSetOption([IntPtr]::Zero, 37, [IntPtr]::Zero, 0) | Out-Null
"#;
    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
        .map_err(|err| format!("无法刷新系统代理: {err}"))?;
    if !output.status.success() {
        return Err(command_error(&output));
    }
    Ok(())
}

fn windows_enabled() -> Result<bool, String> {
    let output = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyEnable",
        ])
        .output()
        .map_err(|err| format!("无法读取系统代理: {err}"))?;
    if !output.status.success() {
        return Err(command_error(&output));
    }
    parse_windows_proxy_enable(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| "无法识别系统代理状态".to_string())
}

fn reg_add(key: &str, name: &str, kind: &str, data: &str) -> Result<(), String> {
    let output = Command::new("reg")
        .args(["add", key, "/v", name, "/t", kind, "/d", data, "/f"])
        .output()
        .map_err(|err| format!("无法写入系统代理: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error(&output))
    }
}

pub(crate) fn parse_windows_proxy_enable(text: &str) -> Option<bool> {
    if text.contains("0x1") {
        Some(true)
    } else if text.contains("0x0") {
        Some(false)
    } else {
        None
    }
}

fn gnome_set(enable: bool, port: u16) -> Result<(), String> {
    if enable {
        gsettings(&["set", "org.gnome.system.proxy", "mode", "manual"])?;
        for schema in [
            "org.gnome.system.proxy.http",
            "org.gnome.system.proxy.https",
            "org.gnome.system.proxy.socks",
        ] {
            gsettings(&["set", schema, "host", "127.0.0.1"])?;
            gsettings(&["set", schema, "port", &port.to_string()])?;
        }
        Ok(())
    } else {
        gsettings(&["set", "org.gnome.system.proxy", "mode", "none"])
    }
}

fn gnome_enabled() -> Result<bool, String> {
    let text = gsettings_get(&["get", "org.gnome.system.proxy", "mode"])?;
    parse_gnome_mode(&text).ok_or_else(|| "当前桌面不支持系统代理（需要 GNOME gsettings）".to_string())
}

fn gsettings(args: &[&str]) -> Result<(), String> {
    let output = Command::new("gsettings")
        .args(args)
        .output()
        .map_err(|_| "当前桌面不支持系统代理（需要 GNOME gsettings）".to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err("当前桌面不支持系统代理（需要 GNOME gsettings）".into())
    }
}

fn gsettings_get(args: &[&str]) -> Result<String, String> {
    let output = Command::new("gsettings")
        .args(args)
        .output()
        .map_err(|_| "当前桌面不支持系统代理（需要 GNOME gsettings）".to_string())?;
    if !output.status.success() {
        return Err("当前桌面不支持系统代理（需要 GNOME gsettings）".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(crate) fn parse_gnome_mode(text: &str) -> Option<bool> {
    match text.trim().trim_matches('\'') {
        "manual" => Some(true),
        "none" => Some(false),
        _ => None,
    }
}

fn command_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        format!("命令退出码 {}", output.status)
    } else {
        stderr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_platform_proxy_status() {
        assert!(parse_macos_web_proxy("Enabled: Yes\nServer: 127.0.0.1\nPort: 7890\n"));
        assert!(!parse_macos_web_proxy("Enabled: Yes\nServer: 10.0.0.1\n"));
        assert!(!parse_macos_web_proxy("Enabled: No\nServer: 127.0.0.1\n"));
        assert_eq!(parse_windows_proxy_enable("ProxyEnable    REG_DWORD    0x1"), Some(true));
        assert_eq!(parse_windows_proxy_enable("ProxyEnable    REG_DWORD    0x0"), Some(false));
        assert_eq!(parse_gnome_mode("'manual'"), Some(true));
        assert_eq!(parse_gnome_mode("'none'"), Some(false));
        assert_eq!(parse_gnome_mode("'auto'"), None);
        assert!(parse_android_http_proxy("127.0.0.1:7890\n"));
        assert!(!parse_android_http_proxy(":0"));
        assert!(!parse_android_http_proxy("null"));
    }
}
