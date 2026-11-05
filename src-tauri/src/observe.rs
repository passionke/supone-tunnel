//! 路由观测：域名规则、DNS、GEOIP。给 agent 用，不写配置。
//! Author: kejiqing

use std::net::IpAddr;
use std::path::Path;
use std::str::FromStr;

use ipnet::IpNet;
use maxminddb::geoip2;
use serde::Serialize;

use crate::runtime::RuleRow;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedIp {
    pub ip: String,
    pub country: Option<String>,
}

#[derive(Clone, Debug)]
pub struct RouteVerdict {
    pub lines: Vec<String>,
}

pub fn normalize_host(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("域名不能为空".into());
    }
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let host_port = without_scheme.split('/').next().unwrap_or(without_scheme);
    let host = host_port
        .rsplit_once('@')
        .map(|(_, host)| host)
        .unwrap_or(host_port);
    let host = if host.starts_with('[') {
        host.trim_matches(|ch| ch == '[' || ch == ']')
            .to_ascii_lowercase()
    } else {
        host.split(':').next().unwrap_or(host).to_ascii_lowercase()
    };
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err("域名无效".into());
    }
    Ok(host)
}

pub fn match_domain_rule<'a>(rules: &'a [RuleRow], host: &str) -> Option<&'a RuleRow> {
    for rule in rules {
        let kind = normalize_kind(&rule.kind);
        let payload = rule.payload.to_ascii_lowercase();
        let hit = match kind.as_str() {
            "domain" => host == payload,
            "domainsuffix" => host == payload || host.ends_with(&format!(".{payload}")),
            "domainkeyword" => !payload.is_empty() && host.contains(&payload),
            _ => false,
        };
        if hit {
            return Some(rule);
        }
    }
    None
}

pub fn match_ip_cidr_rule<'a>(rules: &'a [RuleRow], ip: IpAddr) -> Option<&'a RuleRow> {
    for rule in rules {
        let kind = normalize_kind(&rule.kind);
        if kind != "ipcidr" && kind != "ipcidr6" {
            continue;
        }
        let Ok(net) = IpNet::from_str(rule.payload.trim()) else {
            continue;
        };
        if net.contains(&ip) {
            return Some(rule);
        }
    }
    None
}

pub fn match_geoip_rule<'a>(rules: &'a [RuleRow], country: &str) -> Option<&'a RuleRow> {
    let country = country.to_ascii_lowercase();
    for rule in rules {
        if normalize_kind(&rule.kind) != "geoip" {
            continue;
        }
        if rule.payload.to_ascii_lowercase() == country {
            return Some(rule);
        }
    }
    None
}

pub fn final_match_rule(rules: &[RuleRow]) -> Option<&RuleRow> {
    rules
        .iter()
        .find(|rule| normalize_kind(&rule.kind) == "match")
}

pub fn lookup_country(mmdb: &Path, ip: IpAddr) -> Option<String> {
    let reader = maxminddb::Reader::open_readfile(mmdb).ok()?;
    let country: geoip2::Country = reader.lookup(ip).ok()?;
    country
        .country
        .and_then(|c| c.iso_code)
        .map(|code: &str| code.to_ascii_uppercase())
}

pub fn explain_with_dns(
    mode: &str,
    host: &str,
    rules: &[RuleRow],
    resolved: &[ResolvedIp],
) -> RouteVerdict {
    if mode == "direct" {
        return RouteVerdict {
            lines: vec![format!("{host} -> DIRECT（模式 direct）")],
        };
    }
    if mode == "global" {
        return RouteVerdict {
            lines: vec![format!("{host} -> GLOBAL（模式 global）")],
        };
    }

    let mut lines = Vec::new();
    if let Some(hit) = match_domain_rule(rules, host) {
        lines.push(format!(
            "{host} -> {}（第 {} 条 {} {}）",
            hit.proxy, hit.index, hit.kind, hit.payload
        ));
        return RouteVerdict { lines };
    }
    lines.push(format!(
        "{host} 未命中 DOMAIN / DOMAIN-SUFFIX / DOMAIN-KEYWORD"
    ));

    if resolved.is_empty() {
        lines.insert(0, format!("{host} -> 未知（无 DNS）"));
        lines.push("DNS 无结果，无法继续按 IP/GEOIP 判定".into());
        if let Some(rule) = final_match_rule(rules) {
            lines.push(format!(
                "若有地址且 GEOIP 不中，会落到第 {} 条 MATCH -> {}",
                rule.index, rule.proxy
            ));
        }
        return RouteVerdict { lines };
    }

    let mut outcomes: Vec<(String, String)> = Vec::new();
    for item in resolved {
        let Ok(ip) = IpAddr::from_str(&item.ip) else {
            lines.push(format!("{} 地址无效", item.ip));
            continue;
        };
        if let Some(hit) = match_ip_cidr_rule(rules, ip) {
            let text = format!(
                "{} country={} -> {}（第 {} 条 IP-CIDR {}）",
                item.ip,
                item.country.as_deref().unwrap_or("?"),
                hit.proxy,
                hit.index,
                hit.payload
            );
            lines.push(text);
            outcomes.push((item.ip.clone(), hit.proxy.clone()));
            continue;
        }
        if let Some(country) = item.country.as_deref() {
            if let Some(hit) = match_geoip_rule(rules, country) {
                let text = format!(
                    "{} country={} -> {}（第 {} 条 GeoIP,{}）",
                    item.ip, country, hit.proxy, hit.index, country
                );
                lines.push(text);
                outcomes.push((item.ip.clone(), hit.proxy.clone()));
                continue;
            }
        } else {
            lines.push(format!("{} 无 GEOIP 库结果", item.ip));
        }
        if let Some(hit) = final_match_rule(rules) {
            let text = format!(
                "{} country={} -> {}（第 {} 条 MATCH）",
                item.ip,
                item.country.as_deref().unwrap_or("?"),
                hit.proxy,
                hit.index
            );
            lines.push(text);
            outcomes.push((item.ip.clone(), hit.proxy.clone()));
        } else {
            lines.push(format!("{} 无 MATCH 兜底", item.ip));
            outcomes.push((item.ip.clone(), "未知".into()));
        }
    }

    let proxies: Vec<String> = {
        let mut uniq = Vec::new();
        for (_, proxy) in &outcomes {
            if !uniq.iter().any(|item| item == proxy) {
                uniq.push(proxy.clone());
            }
        }
        uniq
    };
    let summary = if proxies.len() == 1 {
        format!(
            "{host} -> {}（按 DNS/GEOIP，{} 个地址一致）",
            proxies[0],
            outcomes.len()
        )
    } else if proxies.is_empty() {
        format!("{host} -> 未知")
    } else {
        format!(
            "{host} -> 分流不一致：{}（多 A 记录出口不同）",
            proxies.join(" / ")
        )
    };
    lines.insert(0, summary);
    RouteVerdict { lines }
}

pub fn format_rules(rules: &[RuleRow], limit: usize) -> String {
    if rules.is_empty() {
        return "规则为空".into();
    }
    let mut lines = vec![format!("共 {} 条规则", rules.len())];
    for rule in rules.iter().take(limit) {
        lines.push(format!(
            "{}. {},{},{}",
            rule.index, rule.kind, rule.payload, rule.proxy
        ));
    }
    if rules.len() > limit {
        lines.push(format!("仅列出前 {limit} 条"));
    }
    lines.join("\n")
}

fn normalize_kind(kind: &str) -> String {
    kind.to_ascii_lowercase().replace(['-', '_'], "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RuleRow;

    fn rule(index: usize, kind: &str, payload: &str, proxy: &str) -> RuleRow {
        RuleRow {
            index,
            kind: kind.into(),
            payload: payload.into(),
            proxy: proxy.into(),
        }
    }

    #[test]
    fn domain_suffix_beats_geoip() {
        let rules = vec![
            rule(0, "DomainSuffix", "github.com", "ARTIFACT"),
            rule(1, "GeoIP", "cn", "DIRECT"),
            rule(2, "Match", "", "PROXY"),
        ];
        let hit = match_domain_rule(&rules, "www.github.com").unwrap();
        assert_eq!(hit.proxy, "ARTIFACT");
    }

    #[test]
    fn jd_without_domain_uses_geoip_per_ip() {
        let rules = vec![
            rule(80, "GeoIP", "cn", "DIRECT"),
            rule(81, "GeoIP", "hk", "DIRECT"),
            rule(82, "Match", "", "PROXY"),
        ];
        let resolved = vec![
            ResolvedIp {
                ip: "106.39.171.134".into(),
                country: Some("CN".into()),
            },
            ResolvedIp {
                ip: "138.113.62.19".into(),
                country: Some("US".into()),
            },
        ];
        let verdict = explain_with_dns("rule", "www.jd.com", &rules, &resolved);
        let head = verdict.lines.first().cloned().unwrap_or_default();
        assert!(head.contains("分流不一致"), "{head}");
        assert!(verdict.lines.iter().any(|line| line.contains("DIRECT")));
        assert!(verdict.lines.iter().any(|line| line.contains("PROXY")));
    }

    #[test]
    fn normalize_strips_url() {
        assert_eq!(
            normalize_host("https://www.jd.com/home").unwrap(),
            "www.jd.com"
        );
    }
}
