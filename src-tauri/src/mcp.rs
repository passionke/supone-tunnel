//! 本机 MCP：HTTP JSON-RPC + Bearer，给 NeuroGate 挂载。
//! 与 550w panel `/api/neurogate/mcp` 同形，不引入第二套业务逻辑。
//! Author: kejiqing

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::shell::Shell;

pub struct McpConfig {
    pub enabled: bool,
    pub bind: String,
    pub token: String,
}

fn mcp_file_log(shell: &Shell, msg: impl AsRef<str>) {
    let path = shell.data_root().join("mcp.log");
    let line = format!(
        "{} {}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        msg.as_ref()
    );
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(line.as_bytes())
        });
}

pub fn spawn(shell: Arc<Shell>, cfg: McpConfig) -> Option<watch::Sender<bool>> {
    mcp_file_log(
        &shell,
        format!(
            "spawn enabled={} bind={} token_len={}",
            cfg.enabled,
            cfg.bind,
            cfg.token.trim().len()
        ),
    );
    if !cfg.enabled {
        return None;
    }
    if cfg.token.trim().is_empty() {
        mcp_file_log(&shell, "token 为空，不启动");
        return None;
    }
    let (tx, rx) = watch::channel(true);
    let bind = cfg.bind.clone();
    let token = cfg.token.trim().to_string();
    let shell_thread = Arc::clone(&shell);
    // setup 阶段无当前线程 runtime；独立线程 + Tokio，避免依赖 Tauri pump。
    // Author: kejiqing
    match std::thread::Builder::new()
        .name("supone-mcp".into())
        .spawn(move || {
            mcp_file_log(&shell_thread, format!("thread start bind={bind}"));
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .thread_name("supone-mcp-worker")
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    mcp_file_log(&shell_thread, format!("runtime {err}"));
                    return;
                }
            };
            let shell_serve = Arc::clone(&shell_thread);
            let bind_log = bind.clone();
            rt.block_on(async move {
                if let Err(err) = serve(shell_serve, bind, token, rx).await {
                    mcp_file_log(&shell_thread, format!("serve err {err}"));
                } else {
                    mcp_file_log(&shell_thread, format!("serve end bind={bind_log}"));
                }
            });
        }) {
        Ok(_) => mcp_file_log(&shell, "thread spawned"),
        Err(err) => {
            mcp_file_log(&shell, format!("thread spawn failed: {err}"));
            return None;
        }
    }
    Some(tx)
}

async fn serve(
    shell: Arc<Shell>,
    bind: String,
    token: String,
    mut alive: watch::Receiver<bool>,
) -> Result<(), String> {
    let addr: SocketAddr = bind
        .parse()
        .map_err(|err| format!("mcp bind 无效 {bind}: {err}"))?;
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|err| format!("mcp listen {bind}: {err}"))?;
    mcp_file_log(&shell, format!("listening on http://{bind}/mcp"));
    eprintln!("mcp: listening on http://{bind}/mcp");
    loop {
        tokio::select! {
            _ = alive.changed() => {
                if !*alive.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|err| err.to_string())?;
                let io = TokioIo::new(stream);
                let shell = Arc::clone(&shell);
                let token = token.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| {
                        let shell = Arc::clone(&shell);
                        let token = token.clone();
                        async move { handle(req, shell, &token).await }
                    });
                    let _ = http1::Builder::new().serve_connection(io, service).await;
                });
            }
        }
    }
    Ok(())
}

async fn handle(
    req: Request<Incoming>,
    shell: Arc<Shell>,
    token: &str,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if !authorized(req.headers().get(AUTHORIZATION), token) {
        return Ok(json_response(StatusCode::UNAUTHORIZED, json!({"error":"unauthorized"})));
    }
    let path = req.uri().path().to_string();
    match (req.method(), path.as_str()) {
        (&Method::GET, "/mcp") | (&Method::GET, "/health") => Ok(json_response(
            StatusCode::OK,
            json!({
                "ok": true,
                "transport": "json-rpc-http",
                "tools": tool_names(),
                "auth": "bearer"
            }),
        )),
        (&Method::POST, "/mcp") => {
            let body = match req.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(_) => {
                    return Ok(json_response(
                        StatusCode::BAD_REQUEST,
                        json!({"error":"bad_body"}),
                    ));
                }
            };
            let parsed: Value = match serde_json::from_slice(&body) {
                Ok(value) => value,
                Err(_) => {
                    return Ok(json_response(
                        StatusCode::BAD_REQUEST,
                        json!({"error":"invalid_json"}),
                    ));
                }
            };
            if let Some(items) = parsed.as_array() {
                let mut outs = Vec::new();
                for item in items {
                    if let Some(out) = dispatch_rpc(shell.as_ref(), item).await {
                        outs.push(out);
                    }
                }
                return Ok(json_response(StatusCode::OK, Value::Array(outs)));
            }
            match dispatch_rpc(shell.as_ref(), &parsed).await {
                None => Ok(Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(Full::new(Bytes::new()))
                    .unwrap()),
                Some(out) => Ok(json_response(StatusCode::OK, out)),
            }
        }
        _ => Ok(json_response(StatusCode::NOT_FOUND, json!({"error":"not_found"}))),
    }
}

fn authorized(header: Option<&hyper::header::HeaderValue>, token: &str) -> bool {
    let Some(value) = header.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    value
        .strip_prefix("Bearer ")
        .is_some_and(|got| got == token)
}

async fn dispatch_rpc(shell: &Shell, msg: &Value) -> Option<Value> {
    let method = msg.get("method")?.as_str()?;
    let id = msg.get("id").cloned();
    if method.starts_with("notifications/") {
        return None;
    }
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "supone-tunnel", "version": "0.1.0" }
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tool_schemas() }),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or(json!({}));
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(shell, name, &args).await {
                Ok(text) => json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false
                }),
                Err(err) => json!({
                    "content": [{ "type": "text", "text": format!("失败: {err}") }],
                    "isError": true
                }),
            }
        }
        other => {
            return Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {other}") }
            }));
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

async fn call_tool(shell: &Shell, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "supone_status" => shell.model_status().await,
        "supone_diagnose" => shell.diagnose().await,
        "supone_list_rules" => {
            let limit = args
                .get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(120) as usize;
            shell.list_rules_text_limited(limit).await
        }
        "supone_dns_lookup" => {
            let host = arg_str(args, "host")?;
            shell.dns_lookup_text(host).await
        }
        "supone_explain_route" => {
            let host = arg_str(args, "host")?;
            shell.explain_route(host).await
        }
        "supone_list_connections" => {
            let host = args.get("host").and_then(|v| v.as_str());
            shell.list_connections_text(host).await
        }
        "supone_probe_url" => {
            let url = arg_str(args, "url")?;
            let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_u64());
            shell.probe_url_text(url, timeout_ms).await
        }
        "supone_list_groups" => shell.groups_text().await,
        "supone_list_nodes" => shell.nodes_text().await,
        "supone_test_delay" => {
            let name = arg_str(args, "name")?;
            shell.test_delay_text(name).await
        }
        "supone_test_all_delays" => shell.test_all_delays_text().await,
        "supone_test_bandwidth" => {
            let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_u64());
            shell.test_bandwidth_text(timeout_ms).await
        }
        "supone_test_stability" => {
            let name = args.get("name").and_then(|v| v.as_str());
            let samples = args
                .get("samples")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
            let interval_ms = args.get("interval_ms").and_then(|v| v.as_u64());
            shell
                .test_stability_text(name, samples, interval_ms)
                .await
        }
        "supone_list_profiles" => shell.list_text(),
        "supone_import_subscription" => {
            let url = arg_str(args, "url")?;
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            shell.import_subscription(url, name).await
        }
        "supone_refresh_subscription" => {
            let id = arg_str(args, "id")?;
            shell.refresh_subscription(id).await
        }
        "supone_activate_profile" => {
            let id = arg_str(args, "id")?;
            shell.activate_profile(id).await
        }
        "supone_reset_override" => {
            let id = args.get("id").and_then(|v| v.as_str());
            shell.reset_override(id).await
        }
        "supone_start_core" => shell.start().await.map(|_| "内核已启动".into()),
        "supone_stop_core" => shell.stop().await.map(|_| "内核已停止".into()),
        "supone_set_mode" => {
            let mode = arg_str(args, "mode")?;
            shell.set_mode(mode).await
        }
        "supone_select_proxy" => {
            let group = arg_str(args, "group")?;
            let name = arg_str(args, "name")?;
            shell.select_proxy(group, name).await
        }
        "supone_upsert_proxy_group" => {
            let name = arg_str(args, "name")?;
            let group_type = args
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("select");
            let proxies = arg_string_list(args, "proxies")?;
            let url = args.get("url").and_then(|v| v.as_str());
            let interval = args.get("interval").and_then(|v| v.as_u64());
            shell
                .upsert_proxy_group(name, group_type, &proxies, url, interval)
                .await
        }
        "supone_set_tun" => {
            let enable = arg_bool(args, "enable")?;
            shell.set_tun(enable).await
        }
        "supone_set_system_proxy" => {
            let enable = arg_bool(args, "enable")?;
            shell.set_system_proxy(enable).await
        }
        "supone_set_allow_lan" => {
            let enable = arg_bool(args, "enable")?;
            shell.set_allow_lan(enable).await
        }
        "supone_set_mixed_port" => {
            let port = arg_u16(args, "port")?;
            shell.set_mixed_port(port).await
        }
        "supone_dns_config" => Ok(shell.dns_config_text()),
        "supone_set_dns" => {
            let patch = json_to_yaml(args.get("dns").cloned().unwrap_or(Value::Null))?;
            shell.set_dns(patch).await
        }
        "supone_list_prepend_rules" => Ok(shell.prepend_rules_text()),
        "supone_prepend_rule" => {
            let rule = arg_str(args, "rule")?;
            shell.prepend_rule(rule).await
        }
        "supone_remove_prepend_rule" => {
            let rule = arg_str(args, "rule")?;
            shell.remove_prepend_rule(rule).await
        }
        "supone_list_tun_route_excludes" => Ok(shell.tun_route_excludes_text()),
        "supone_add_tun_route_exclude" => {
            let cidr = arg_str(args, "cidr")?;
            shell.add_tun_route_exclude(cidr).await
        }
        "supone_remove_tun_route_exclude" => {
            let cidr = arg_str(args, "cidr")?;
            shell.remove_tun_route_exclude(cidr).await
        }
        "supone_net_overview" => shell.net_overview().await,
        "supone_net_interfaces" => Ok(shell.net_interfaces()),
        "supone_net_routes" => Ok(shell.net_routes()),
        other => Err(format!("未知工具 {other}")),
    }
}

fn tool_names() -> Vec<String> {
    tool_schemas()
        .iter()
        .filter_map(|tool| tool.get("name").and_then(|v| v.as_str()).map(str::to_string))
        .collect()
}

fn tool_schemas() -> Vec<Value> {
    vec![
        tool("supone_status", "内核/模式/订阅/系统代理/TUN/组摘要", json!({"type":"object","properties":{}})),
        tool(
            "supone_diagnose",
            "本机现场：状态、TUN（auto-route/dns-hijack/网卡地址）、端口、DNS、默认路由、系统 DNS、内核告警。问整机先调这个；问某个域名先 supone_explain_route。",
            json!({"type":"object","properties":{}}),
        ),
        tool("supone_list_rules", "列出生效规则", json!({"type":"object","properties":{"limit":{"type":"integer"}}})),
        tool("supone_dns_lookup", "内核 DNS + GEOIP + 每个 IP 的系统路由（route -n get）", json!({"type":"object","properties":{"host":{"type":"string"}},"required":["host"]})),
        tool("supone_explain_route", "域名怎么走：命中规则、mihomo DNS、该 IP 的系统路由网卡、代理组当前节点、已有连接。问某个站的本机流量时先调这个。", json!({"type":"object","properties":{"host":{"type":"string"}},"required":["host"]})),
        tool("supone_list_connections", "当前连接", json!({"type":"object","properties":{"host":{"type":"string"}}})),
        tool(
            "supone_probe_url",
            "经 mixed 口探测目标 URL（当前生效配置），返回 HTTP 结果；链路只来自 mihomo /connections 连接日志（rule/chains），不做规则推理。改完规则/DNS/选点后用这个验证站点是否 OK。",
            json!({
                "type":"object",
                "properties":{
                    "url":{"type":"string","description":"http(s) URL，或省略协议时默认 https"},
                    "timeout_ms":{"type":"integer","minimum":1000,"maximum":30000}
                },
                "required":["url"]
            }),
        ),
        tool("supone_list_groups", "代理组与当前选中/可选成员", json!({"type":"object","properties":{}})),
        tool(
            "supone_list_nodes",
            "列出全部叶子节点名（不含代理组与 DIRECT/REJECT 等内置策略）。测速或选节点前先看这个。",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "supone_test_delay",
            "测单个节点延迟（毫秒）。不切换节点，不是带宽。",
            json!({"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}),
        ),
        tool(
            "supone_test_all_delays",
            "测全部叶子节点延迟（毫秒），按延迟排序。不切换节点，不是带宽。节点多时较慢。",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "supone_test_bandwidth",
            "经 mixed 测当前出口吞吐（Mbps）。限时截断下载；REGION=china 用国内源，否则 Cloudflare。不切换节点。",
            json!({
                "type":"object",
                "properties":{
                    "timeout_ms":{"type":"integer","minimum":2000,"maximum":30000,"description":"默认 12000"}
                }
            }),
        ),
        tool(
            "supone_test_stability",
            "延迟采样统计（min/avg/max/jitter/失败率）。指定 name 测该节点且不切换；省略则测当前出口。不是带宽。",
            json!({
                "type":"object",
                "properties":{
                    "name":{"type":"string","description":"叶子节点名；省略=当前出口"},
                    "samples":{"type":"integer","minimum":2,"maximum":20,"description":"默认 5"},
                    "interval_ms":{"type":"integer","minimum":0,"maximum":5000,"description":"采样间隔，默认 400"}
                }
            }),
        ),
        tool("supone_list_profiles", "订阅列表", json!({"type":"object","properties":{}})),
        tool("supone_import_subscription", "导入订阅", json!({"type":"object","properties":{"url":{"type":"string"},"name":{"type":"string"}},"required":["url"]})),
        tool("supone_refresh_subscription", "刷新订阅", json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"]})),
        tool("supone_activate_profile", "切换订阅", json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"]})),
        tool(
            "supone_reset_override",
            "清空本地覆盖，回到仅订阅原文（规则/DNS/代理组/TUN 排除等一并清除）。不改 remote。id 省略则当前激活订阅。",
            json!({"type":"object","properties":{"id":{"type":"string"}}}),
        ),
        tool("supone_start_core", "启动 mihomo", json!({"type":"object","properties":{}})),
        tool("supone_stop_core", "停止 mihomo", json!({"type":"object","properties":{}})),
        tool("supone_set_mode", "设置模式", json!({"type":"object","properties":{"mode":{"type":"string","enum":["rule","global","direct"]}},"required":["mode"]})),
        tool("supone_select_proxy", "选择节点", json!({"type":"object","properties":{"group":{"type":"string"},"name":{"type":"string"}},"required":["group","name"]})),
        tool(
            "supone_upsert_proxy_group",
            "新建或按名编辑代理组（写入本地覆盖，刷新订阅不冲掉）。成员须来自 list_nodes / 已有组 / DIRECT|REJECT。新建后可用 prepend_rule 把流量指到该组。",
            json!({
                "type":"object",
                "properties":{
                    "name":{"type":"string","description":"组名；同名则覆盖（含改订阅原文里的组）"},
                    "type":{"type":"string","enum":["select","url-test","fallback","load-balance"],"description":"默认 select"},
                    "proxies":{"type":"array","items":{"type":"string"},"minItems":1,"description":"成员：叶子节点名、其它组名或内置策略"},
                    "url":{"type":"string","description":"url-test/fallback/load-balance 测活 URL；省略则用 gstatic generate_204"},
                    "interval":{"type":"integer","minimum":1,"maximum":86400,"description":"测活间隔秒；默认 300"}
                },
                "required":["name","proxies"]
            }),
        ),
        tool("supone_set_tun", "开关 TUN", json!({"type":"object","properties":{"enable":{"type":"boolean"}},"required":["enable"]})),
        tool("supone_set_system_proxy", "开关系统代理（指向 mixed 口；开时需内核已运行）", json!({"type":"object","properties":{"enable":{"type":"boolean"}},"required":["enable"]})),
        tool("supone_set_allow_lan", "开关 allow-lan（是否允许局域网访问 mixed 口）", json!({"type":"object","properties":{"enable":{"type":"boolean"}},"required":["enable"]})),
        tool("supone_set_mixed_port", "设置 mixed 端口并写入覆盖后重载", json!({"type":"object","properties":{"port":{"type":"integer","minimum":1,"maximum":65535}},"required":["port"]})),
        tool("supone_dns_config", "读取当前生效 DNS", json!({"type":"object","properties":{}})),
        tool(
            "supone_set_dns",
            "调整 DNS：enhanced-mode、nameserver、fallback、nameserver-policy、respect-rules。写入覆盖并重载，刷新订阅不会冲掉已合并字段。",
            json!({
                "type":"object",
                "properties":{
                    "dns":{
                        "type":"object",
                        "properties":{
                            "enable":{"type":"boolean"},
                            "enhanced-mode":{"type":"string","enum":["fake-ip","redir-host"]},
                            "respect-rules":{"type":"boolean"},
                            "ipv6":{"type":"boolean"},
                            "nameserver":{"type":"array","items":{"type":"string"}},
                            "fallback":{"type":"array","items":{"type":"string"}},
                            "default-nameserver":{"type":"array","items":{"type":"string"}},
                            "fake-ip-filter":{"type":"array","items":{"type":"string"}},
                            "nameserver-policy":{"type":"object","additionalProperties":{"type":"array","items":{"type":"string"}}}
                        }
                    }
                },
                "required":["dns"]
            }),
        ),
        tool("supone_list_prepend_rules", "列出插在订阅规则最前的本地规则", json!({"type":"object","properties":{}})),
        tool(
            "supone_prepend_rule",
            "在订阅规则最前插入一条，形如 DOMAIN-SUFFIX,jd.com,DIRECT。刷新订阅不会冲掉。",
            json!({"type":"object","properties":{"rule":{"type":"string"}},"required":["rule"]}),
        ),
        tool(
            "supone_remove_prepend_rule",
            "删除一条本地前置规则",
            json!({"type":"object","properties":{"rule":{"type":"string"}},"required":["rule"]}),
        ),
        tool(
            "supone_list_tun_route_excludes",
            "列出 tun.route-exclude-address：这些网段不进 TUN（系统路由层），不是进 TUN 后的 rules",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "supone_add_tun_route_exclude",
            "把 CIDR/IP 写入 tun.route-exclude-address，让该网段不走 mihomo TUN（例：其它 VPN 叠网 10.8.0.0/24，或 VPN 出口公网 IP）。不是 DOMAIN/IP-CIDR 规则。内核在跑时会停再启。",
            json!({"type":"object","properties":{"cidr":{"type":"string"}},"required":["cidr"]}),
        ),
        tool(
            "supone_remove_tun_route_exclude",
            "从 tun.route-exclude-address 删除一条",
            json!({"type":"object","properties":{"cidr":{"type":"string"}},"required":["cidr"]}),
        ),
        tool("supone_net_overview", "本机默认路由/TUN/其它 VPN 冲突摘要", json!({"type":"object","properties":{}})),
        tool("supone_net_interfaces", "活跃网卡列表", json!({"type":"object","properties":{}})),
        tool("supone_net_routes", "IPv4 路由摘要", json!({"type":"object","properties":{}})),
    ]
}

fn tool(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": parameters
    })
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| format!("缺少参数 {key}"))
}

fn arg_string_list(args: &Value, key: &str) -> Result<Vec<String>, String> {
    let items = args
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("缺少参数 {key}"))?;
    if items.is_empty() {
        return Err(format!("{key} 不能为空"));
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str().map(str::trim).filter(|v| !v.is_empty()) else {
            return Err(format!("{key} 须是非空字符串数组"));
        };
        out.push(text.to_string());
    }
    Ok(out)
}

fn arg_bool(args: &Value, key: &str) -> Result<bool, String> {
    args.get(key)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| format!("缺少参数 {key}"))
}

fn arg_u16(args: &Value, key: &str) -> Result<u16, String> {
    let number = args
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("缺少参数 {key}"))?;
    u16::try_from(number).map_err(|_| "端口无效".into())
}

fn json_to_yaml(value: Value) -> Result<serde_yaml::Value, String> {
    let text = serde_json::to_string(&value).map_err(|err| err.to_string())?;
    serde_yaml::from_str(&text).map_err(|err| format!("dns 参数无效: {err}"))
}

fn json_response(status: StatusCode, body: Value) -> Response<Full<Bytes>> {
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .body(Full::new(Bytes::from(bytes)))
        .unwrap()
}
