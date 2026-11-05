//! 本机网卡 / 路由探测。只读，给 MCP 与 agent 判断 TUN 与其它 VPN 关系。
//! Author: kejiqing

use std::process::Command;

#[derive(Clone, Debug)]
pub struct NetOverviewInput {
    pub tun_enabled: bool,
    pub tun_device: String,
    pub auto_route: bool,
    pub system_proxy: Option<bool>,
}

pub fn overview(input: &NetOverviewInput) -> String {
    if !cfg!(target_os = "macos") {
        return "当前仅实现 macOS 网络探测".into();
    }
    let default = default_route();
    let utuns = list_utun_summary();
    let mut lines = vec![
        format!(
            "系统代理: {}",
            match input.system_proxy {
                Some(true) => "开",
                Some(false) => "关",
                None => "未知",
            }
        ),
        format!(
            "mihomo TUN: {} device={} auto-route={}",
            if input.tun_enabled { "开" } else { "关" },
            if input.tun_device.is_empty() {
                "(空)"
            } else {
                &input.tun_device
            },
            if input.auto_route { "true" } else { "false" }
        ),
    ];
    match &default {
        Ok(text) => lines.push(format!("默认路由: {text}")),
        Err(err) => lines.push(format!("默认路由: 读取失败 {err}")),
    }
    if utuns.is_empty() {
        lines.push("其它 utun: 无".into());
    } else {
        lines.push(format!("其它 utun: {}", utuns.join("; ")));
    }
    let half = half_route_on_tun(&input.tun_device);
    lines.push(conflict_hint(input, default.as_deref().ok(), half));
    lines.join("\n")
}

pub fn interfaces() -> String {
    if !cfg!(target_os = "macos") {
        return "当前仅实现 macOS 网络探测".into();
    }
    let names = match iface_names() {
        Ok(names) => names,
        Err(err) => return format!("失败: {err}"),
    };
    let mut lines = Vec::new();
    for name in names {
        if skip_iface(&name) {
            continue;
        }
        let kind = iface_kind(&name);
        let addrs = iface_ipv4(&name);
        if addrs.is_empty() && !name.starts_with("utun") && !name.starts_with("en") {
            continue;
        }
        let addr = if addrs.is_empty() {
            "-".into()
        } else {
            addrs.join(",")
        };
        lines.push(format!("{name} kind={kind} ipv4={addr}"));
    }
    if lines.is_empty() {
        "无活跃网卡摘要".into()
    } else {
        lines.join("\n")
    }
}

pub fn routes() -> String {
    if !cfg!(target_os = "macos") {
        return "当前仅实现 macOS 网络探测".into();
    }
    let output = Command::new("netstat")
        .args(["-rn", "-f", "inet"])
        .output();
    let Ok(output) = output else {
        return "失败: 无法执行 netstat".into();
    };
    if !output.status.success() {
        return format!("失败: netstat {}", String::from_utf8_lossy(&output.stderr));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = vec!["IPv4 路由摘要:".to_string()];
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("Routing") || trimmed.starts_with("Internet") {
            continue;
        }
        if trimmed.starts_with("Destination") {
            continue;
        }
        // 保留 default、非直连网段、utun
        let interesting = trimmed.starts_with("default")
            || trimmed.contains("utun")
            || trimmed.split_whitespace().nth(2).is_some_and(|flags| {
                flags.contains('G') || flags.contains('H') || flags.contains('S')
            });
        if !interesting {
            continue;
        }
        // 跳过大量局域网 host 路由噪音：Destination 是单主机且 Netif=en*
        let cols: Vec<&str> = trimmed.split_whitespace().collect();
        if cols.len() >= 4 {
            let dest = cols[0];
            let netif = cols[3];
            if netif.starts_with("en")
                && !dest.starts_with("default")
                && !dest.contains('/')
                && dest.chars().filter(|ch| *ch == '.').count() == 3
            {
                continue;
            }
        }
        lines.push(trimmed.to_string());
        if lines.len() >= 60 {
            lines.push("…已截断".into());
            break;
        }
    }
    lines.join("\n")
}

/// 状态页用的短摘要：默认路由一行 + 出口提示一行。
/// Author: kejiqing
pub fn traffic_summary(input: &NetOverviewInput) -> (String, String) {
    let default = default_route();
    let route = match &default {
        Ok(text) => text.clone(),
        Err(err) => format!("读取失败 {err}"),
    };
    let half = half_route_on_tun(&input.tun_device);
    let hint = conflict_hint(input, default.as_deref().ok(), half);
    (route, hint)
}

fn conflict_hint(input: &NetOverviewInput, default_line: Option<&str>, half_route: bool) -> String {
    let default_if = default_line
        .map(parse_default_iface)
        .unwrap_or("");

    if input.tun_enabled {
        if !input.tun_device.is_empty() && default_if == input.tun_device {
            return "提示: 默认路由已在 mihomo TUN 上，全局流量经内核规则。".into();
        }
        if default_if.starts_with("utun") && default_if != input.tun_device {
            return format!(
                "提示: 默认路由在 {default_if}，不是 mihomo device={}。可能被其它 VPN 抢走 default，先核对 routes。",
                input.tun_device
            );
        }
        if half_route {
            return format!(
                "提示: auto-route 已把公网半网段指到 {}；default 仍在物理网卡是正常的，局域网直连，公网进 TUN。",
                if input.tun_device.is_empty() {
                    "mihomo TUN"
                } else {
                    input.tun_device.as_str()
                }
            );
        }
        if default_if.starts_with("en") {
            if input.auto_route {
                return "提示: TUN 与 auto-route 已开，但未见公网半网段进 TUN。核对 routes 或权限。".into();
            }
            return "提示: TUN 已开，但默认路由仍在物理网卡，且未见半网段路由。".into();
        }
    } else if default_if.starts_with("utun") {
        return format!(
            "提示: mihomo TUN 关，但默认路由在 {default_if}（其它 VPN）。公网出口由该 VPN 决定。"
        );
    } else if input.system_proxy == Some(true) {
        return "提示: TUN 关、系统代理开。应用层流量走 mixed 口。".into();
    }
    "提示: TUN 关且未见 VPN 抢走 default。公网走物理网卡；若存在 utun 网段路由，那些网段走对应 VPN。".into()
}

/// mihomo auto-route 常见写法：`128.0/1` 与 `0/1` 指到 TUN，default 仍可留在 en*。
fn half_route_on_tun(tun_device: &str) -> bool {
    if tun_device.is_empty() {
        return false;
    }
    let Ok(output) = Command::new("netstat").args(["-rn", "-f", "inet"]).output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().any(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            return false;
        }
        let dest = cols[0];
        let netif = cols[3];
        netif == tun_device && (dest == "128.0/1" || dest == "0/1" || dest.starts_with("128."))
    })
}

fn parse_default_iface(line: &str) -> &str {
    if let Some(rest) = line.split("interface=").nth(1) {
        return rest.split_whitespace().next().unwrap_or("");
    }
    if let Some(rest) = line.split("interface:").nth(1) {
        return rest.trim().split_whitespace().next().unwrap_or("");
    }
    line.split_whitespace()
        .find(|part| part.starts_with("en") || part.starts_with("utun"))
        .unwrap_or("")
}

/// 某个 IP 在本机路由表上的下一跳。只接受 IP，避免把主机名送进 route。
/// Author: kejiqing
pub fn route_get(ip: &str) -> String {
    if ip.parse::<std::net::IpAddr>().is_err() {
        return format!("{ip} 不是 IP，跳过系统路由");
    }
    let output = Command::new("route").args(["-n", "get", ip]).output();
    let Ok(output) = output else {
        return format!("{ip} route 执行失败");
    };
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return format!("{ip} route 失败 {}", err.trim());
    }
    format_route_get(ip, &String::from_utf8_lossy(&output.stdout))
}

pub fn format_route_get(ip: &str, text: &str) -> String {
    let mut destination = String::new();
    let mut gateway = String::new();
    let mut iface = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("destination:") {
            destination = rest.trim().to_string();
        }
        if let Some(rest) = line.strip_prefix("gateway:") {
            gateway = rest.trim().to_string();
        }
        if let Some(rest) = line.strip_prefix("interface:") {
            iface = rest.trim().to_string();
        }
    }
    format!("{ip} destination={destination} gateway={gateway} interface={iface}")
}

fn default_route() -> Result<String, String> {
    let output = Command::new("route")
        .args(["-n", "get", "default"])
        .output()
        .map_err(|err| format!("route: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut gateway = String::new();
    let mut iface = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("gateway:") {
            gateway = rest.trim().to_string();
        }
        if let Some(rest) = line.strip_prefix("interface:") {
            iface = rest.trim().to_string();
        }
    }
    if iface.is_empty() && gateway.is_empty() {
        return Err("无 default".into());
    }
    Ok(format!("gateway={gateway} interface={iface}"))
}

fn list_utun_summary() -> Vec<String> {
    let Ok(names) = iface_names() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for name in names {
        if !name.starts_with("utun") {
            continue;
        }
        let addrs = iface_ipv4(&name);
        if addrs.is_empty() {
            continue;
        }
        out.push(format!("{name}={}", addrs.join(",")));
    }
    out
}

fn iface_names() -> Result<Vec<String>, String> {
    let output = Command::new("ifconfig")
        .arg("-l")
        .output()
        .map_err(|err| format!("ifconfig: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .map(str::to_string)
        .collect())
}

fn iface_ipv4(name: &str) -> Vec<String> {
    let output = Command::new("ifconfig").arg(name).output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut addrs = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        // "inet 192.168.3.180 netmask ..."
        if let Some(rest) = line.strip_prefix("inet ") {
            if let Some(ip) = rest.split_whitespace().next() {
                if ip != "127.0.0.1" {
                    addrs.push(ip.to_string());
                }
            }
        }
    }
    addrs
}

fn iface_kind(name: &str) -> &'static str {
    if name.starts_with("utun") {
        "tunnel"
    } else if name.starts_with("en") {
        "ethernet"
    } else if name.starts_with("bridge") {
        "bridge"
    } else if name.starts_with("awdl") || name.starts_with("llw") {
        "awdl"
    } else if name.starts_with("lo") {
        "loopback"
    } else {
        "other"
    }
}

fn skip_iface(name: &str) -> bool {
    name.starts_with("lo")
        || name.starts_with("gif")
        || name.starts_with("stf")
        || name.starts_with("anpi")
        || name.starts_with("ap")
        || name.starts_with("awdl")
        || name.starts_with("llw")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_when_other_vpn_owns_default() {
        let input = NetOverviewInput {
            tun_enabled: false,
            tun_device: String::new(),
            auto_route: false,
            system_proxy: Some(false),
        };
        let hint = conflict_hint(&input, Some("gateway=10.0.0.1 interface=utun8"), false);
        assert!(hint.contains("其它 VPN"), "{hint}");
    }

    #[test]
    fn hint_auto_route_keeps_default_on_en() {
        let input = NetOverviewInput {
            tun_enabled: true,
            tun_device: "utun4".into(),
            auto_route: true,
            system_proxy: Some(false),
        };
        let hint = conflict_hint(&input, Some("gateway=192.168.110.1 interface=en0"), true);
        assert!(hint.contains("半网段"), "{hint}");
        assert!(hint.contains("utun4"), "{hint}");
    }

    #[test]
    fn hint_coexist_system_proxy() {
        let input = NetOverviewInput {
            tun_enabled: false,
            tun_device: String::new(),
            auto_route: false,
            system_proxy: Some(true),
        };
        let hint = conflict_hint(&input, Some("gateway=192.168.3.1 interface=en0"), false);
        assert!(hint.contains("系统代理"), "{hint}");
    }

    #[test]
    fn route_get_parses_tun_nexthop() {
        let text = "\
   route to: 185.45.5.35
destination: 128.0.0.0
    gateway: 198.18.0.1
  interface: utun4
";
        let line = format_route_get("185.45.5.35", text);
        assert!(line.contains("destination=128.0.0.0"), "{line}");
        assert!(line.contains("gateway=198.18.0.1"), "{line}");
        assert!(line.contains("interface=utun4"), "{line}");
    }
}
