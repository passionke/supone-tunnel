// 作者: kejiqing
//! 7B 工具循环。模型只看见代号，执行前还原成真实参数。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::redact::Redactor;
use crate::shell::Shell;

const SYSTEM: &str = "\
你是本机 mihomo 壳的控制助手。只能通过工具操作，不要输出 YAML。\n\
ip_、sec_、url_、host_ 后接 8 位十六进制是代号，必须原样复制到工具参数里。\n\
不要编写、猜测或补全 IP、域名、密钥和订阅地址。\n\
节点名必须来自当前状态里的列表。模式只能是 rule、global、direct。\n\
观测工具：dns_lookup、list_rules、list_connections、explain_route、probe_url、net_overview、net_interfaces、net_routes。\n\
问某个网站走直连还是代理时，必须先调用 explain_route；要验证当前配置下站点是否可达用 probe_url（链路只信连接日志，不推理）。TUN/其它 VPN 冲突先 net_overview。\n\
出口可选 TUN 或系统代理（set_tun / set_system_proxy）；开系统代理前须内核已运行。\n\
其它 VPN 叠网或 VPN 出口 IP 不要进 TUN 时用 add_tun_route_exclude（写 route-exclude-address），不要用 prepend_rule / IP-CIDR 规则冒充。\n\
订阅 proxies[].server 字面量 IP 会在 rebuild 时自动进 route-exclude-address；SSH/管理节点不要用 prepend_rule。\n\
对话只附带最近一段记录。做完用一句中文说明结果。";

const HISTORY_LIMIT: usize = 30;
const MODEL_TEXT_LIMIT: usize = 2500;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiMessage {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Record {
    User { text: String },
    Assistant { text: String },
    Tools { preface: String, calls: Vec<StoredCall> },
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredCall {
    id: String,
    name: String,
    args: Value,
    result: String,
}

pub struct Session {
    redactor: Redactor,
    records: Vec<Record>,
    path: PathBuf,
    /// NeuroGate Chat Completions 的稳定会话键（字段名 `user`）。
    conversation_user: String,
    meta_path: PathBuf,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SessionMeta {
    #[serde(default)]
    conversation_user: String,
}

impl Session {
    pub fn open(dir: PathBuf) -> Self {
        let path = dir.join("transcript.jsonl");
        let meta_path = dir.join("conversation.json");
        let records = load_records(&path);
        let meta = load_meta(&meta_path);
        let conversation_user = if meta.conversation_user.trim().is_empty() {
            new_conversation_user()
        } else {
            meta.conversation_user
        };
        let session = Self {
            redactor: Redactor::new(),
            records,
            path,
            conversation_user,
            meta_path,
        };
        session.save_meta();
        session
    }

    pub fn conversation_user(&self) -> &str {
        &self.conversation_user
    }

    /// 开新对话：清空本地 transcript，换新的 NeuroGate `user` 会话键。
    pub fn new_conversation(&mut self) {
        self.records.clear();
        self.conversation_user = new_conversation_user();
        self.save();
        self.save_meta();
    }

    pub fn ui_messages(&self) -> Vec<UiMessage> {
        let mut messages = Vec::new();
        for record in &self.records {
            match record {
                Record::User { text } => messages.push(UiMessage {
                    role: "user".into(),
                    text: text.clone(),
                }),
                Record::Assistant { text } => messages.push(UiMessage {
                    role: "assistant".into(),
                    text: text.clone(),
                }),
                Record::Tools { calls, .. } => {
                    for call in calls {
                        messages.push(UiMessage {
                            role: "tool".into(),
                            text: format!("{} → {}", call.name, call.result),
                        });
                    }
                }
            }
        }
        messages
    }

    fn save(&self) {
        let mut text = String::new();
        for record in &self.records {
            if let Ok(line) = serde_json::to_string(record) {
                text.push_str(&line);
                text.push('\n');
            }
        }
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let tmp = self.path.with_extension("tmp");
        if fs::write(&tmp, text.as_bytes()).is_ok() {
            let _ = fs::rename(&tmp, &self.path);
        }
    }

    fn save_meta(&self) {
        let meta = SessionMeta {
            conversation_user: self.conversation_user.clone(),
        };
        if let Ok(text) = serde_json::to_string_pretty(&meta) {
            if let Some(parent) = self.meta_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let tmp = self.meta_path.with_extension("tmp");
            if fs::write(&tmp, text.as_bytes()).is_ok() {
                let _ = fs::rename(&tmp, &self.meta_path);
            }
        }
    }
}

fn new_conversation_user() -> String {
    format!("supone-{}", uuid::Uuid::new_v4())
}

fn load_meta(path: &Path) -> SessionMeta {
    let Ok(text) = fs::read_to_string(path) else {
        return SessionMeta::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn is_neurogate(settings: &AgentSettings) -> bool {
    settings.key.starts_with("ngmk_")
}

pub async fn chat(shell: &Shell, session: &mut Session, text: &str) -> Result<Vec<UiMessage>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("请输入内容".into());
    }
    let settings = shell.agent_settings()?;
    session.records.push(Record::User {
        text: text.to_string(),
    });
    session.save();
    if is_neurogate(&settings) {
        return chat_neurogate(session, &settings, text).await;
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|err| err.to_string())?;
    for _ in 0..8 {
        let messages = build_messages(shell, session).await?;
        let response = post_chat(&client, &settings, messages).await?;
        if let Some(calls) = response.tool_calls {
            if !calls.is_empty() {
                let mut stored = Vec::new();
                for call in calls {
                    let raw = if call.function.arguments.trim().is_empty() {
                        "{}".to_string()
                    } else {
                        call.function.arguments
                    };
                    let (args, result) = match session.redactor.restore_tool_args(&raw) {
                        Ok(args) => {
                            let result = dispatch(shell, &call.function.name, &args).await;
                            (args, result)
                        }
                        Err(err) => (json!({}), format!("失败: {err}")),
                    };
                    stored.push(StoredCall {
                        id: call.id,
                        name: call.function.name,
                        args,
                        result,
                    });
                }
                let preface = response.content.unwrap_or_default();
                session.records.push(Record::Tools {
                    preface: session.redactor.unmask(&preface),
                    calls: stored,
                });
                session.save();
                continue;
            }
        }
        let reply = session
            .redactor
            .unmask(&response.content.unwrap_or_default());
        let reply = if reply.trim().is_empty() {
            "已执行。".to_string()
        } else {
            reply
        };
        session.records.push(Record::Assistant { text: reply });
        session.save();
        return Ok(session.ui_messages());
    }
    session.records.push(Record::Assistant {
        text: "已达到 8 步上限".into(),
    });
    session.save();
    Ok(session.ui_messages())
}

/// NeuroGate 多轮：固定 `user` 会话键；本轮只提交当前 user 消息；工具在网关侧 MCP。
/// Author: kejiqing
async fn chat_neurogate(
    session: &mut Session,
    settings: &AgentSettings,
    text: &str,
) -> Result<Vec<UiMessage>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(330))
        .build()
        .map_err(|err| err.to_string())?;
    let body = json!({
        "model": settings.model,
        "user": session.conversation_user(),
        "timeout": 300,
        "stream": false,
        "messages": [
            {"role": "user", "content": text}
        ]
    });
    let response = client
        .post(completions_url(&settings.base))
        .header("Authorization", format!("Bearer {}", settings.key))
        .json(&body)
        .send()
        .await
        .map_err(|err| format!("NeuroGate 请求失败: {err}"))?;
    let status = response.status();
    let raw = response.text().await.map_err(|err| err.to_string())?;
    if !status.is_success() {
        return Err(format!("NeuroGate 请求失败: HTTP {status} {}", short(&raw)));
    }
    let parsed: ChatResponse =
        serde_json::from_str(&raw).map_err(|err| format!("NeuroGate 响应无效: {err}"))?;
    let reply = parsed
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .unwrap_or_default();
    let reply = if reply.trim().is_empty() {
        "已执行。".to_string()
    } else {
        reply
    };
    session.records.push(Record::Assistant { text: reply });
    session.save();
    Ok(session.ui_messages())
}

async fn dispatch(shell: &Shell, name: &str, args: &Value) -> String {
    let outcome = match name {
        "list_profiles" => shell.list_text(),
        "import_subscription" => {
            let url = match arg_str(args, "url") {
                Ok(url) => url.to_string(),
                Err(err) => return format!("失败: {err}"),
            };
            let name = arg_str(args, "name").ok().map(str::to_string);
            shell.import_subscription(&url, name).await
        }
        "refresh_subscription" => match arg_str(args, "id") {
            Ok(id) => shell.refresh_subscription(id).await,
            Err(err) => Err(err),
        },
        "activate_profile" => match arg_str(args, "id") {
            Ok(id) => shell.activate_profile(id).await,
            Err(err) => Err(err),
        },
        "start_core" => shell.start().await.map(|_| "内核已启动".to_string()),
        "stop_core" => shell.stop().await.map(|_| "内核已停止".into()),
        "get_status" => shell.model_status().await,
        "set_mode" => match arg_str(args, "mode") {
            Ok(mode) => shell.set_mode(mode).await,
            Err(err) => Err(err),
        },
        "select_proxy" => {
            let group = match arg_str(args, "group") {
                Ok(group) => group.to_string(),
                Err(err) => return format!("失败: {err}"),
            };
            let name = match arg_str(args, "name") {
                Ok(name) => name.to_string(),
                Err(err) => return format!("失败: {err}"),
            };
            shell.select_proxy(&group, &name).await
        }
        "set_mixed_port" => match arg_u16(args, "port") {
            Ok(port) => shell.set_mixed_port(port).await,
            Err(err) => Err(err),
        },
        "set_tun" => match arg_bool(args, "enable") {
            Ok(enable) => shell.set_tun(enable).await,
            Err(err) => Err(err),
        },
        "list_tun_route_excludes" => Ok(shell.tun_route_excludes_text()),
        "add_tun_route_exclude" => match arg_str(args, "cidr") {
            Ok(cidr) => shell.add_tun_route_exclude(cidr).await,
            Err(err) => Err(err),
        },
        "remove_tun_route_exclude" => match arg_str(args, "cidr") {
            Ok(cidr) => shell.remove_tun_route_exclude(cidr).await,
            Err(err) => Err(err),
        },
        "set_system_proxy" => match arg_bool(args, "enable") {
            Ok(enable) => shell.set_system_proxy(enable).await,
            Err(err) => Err(err),
        },
        "set_allow_lan" => match arg_bool(args, "enable") {
            Ok(enable) => shell.set_allow_lan(enable).await,
            Err(err) => Err(err),
        },
        "explain_route" => match arg_str(args, "host") {
            Ok(host) => shell.explain_route(host).await,
            Err(err) => Err(err),
        },
        "probe_url" => match arg_str(args, "url") {
            Ok(url) => {
                let timeout_ms = args.get("timeout_ms").and_then(|v| v.as_u64());
                shell.probe_url_text(url, timeout_ms).await
            }
            Err(err) => Err(err),
        },
        "dns_lookup" => match arg_str(args, "host") {
            Ok(host) => shell.dns_lookup_text(host).await,
            Err(err) => Err(err),
        },
        "list_rules" => shell.list_rules_text().await,
        "list_connections" => {
            let host = arg_str(args, "host").ok();
            shell.list_connections_text(host).await
        },
        "net_overview" => shell.net_overview().await,
        "net_interfaces" => Ok(shell.net_interfaces()),
        "net_routes" => Ok(shell.net_routes()),
        other => Err(format!("未知工具 {other}")),
    };
    match outcome {
        Ok(text) => text,
        Err(err) => format!("失败: {err}"),
    }
}

async fn build_messages(shell: &Shell, session: &mut Session) -> Result<Vec<Value>, String> {
    learn(shell, &mut session.redactor);
    let status = shell.model_status().await?;
    let status = session.redactor.mask_text(&status);
    let mut messages = vec![json!({
        "role": "system",
        "content": format!("{SYSTEM}\n\n当前状态:\n{status}")
    })];
    let start = session.records.len().saturating_sub(HISTORY_LIMIT);
    let records = session.records[start..].to_vec();
    for record in &records {
        match record {
            Record::User { text } => messages.push(json!({
                "role": "user",
                "content": session.redactor.mask_text(text)
            })),
            Record::Assistant { text } => messages.push(json!({
                "role": "assistant",
                "content": session.redactor.mask_text(text)
            })),
            Record::Tools { preface, calls } => {
                let tool_calls = calls
                    .iter()
                    .map(|call| {
                        let masked = session.redactor.mask_value(call.args.clone());
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": {
                                "name": call.name,
                                "arguments": serde_json::to_string(&masked).unwrap_or_else(|_| "{}".into())
                            }
                        })
                    })
                    .collect::<Vec<_>>();
                let content = if preface.is_empty() {
                    Value::Null
                } else {
                    Value::String(session.redactor.mask_text(preface))
                };
                messages.push(json!({
                    "role": "assistant",
                    "content": content,
                    "tool_calls": tool_calls
                }));
                for call in calls {
                    let mut result = call.result.clone();
                    if result.chars().count() > MODEL_TEXT_LIMIT {
                        result = result.chars().take(MODEL_TEXT_LIMIT).collect();
                        result.push_str("…已截断");
                    }
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": call.id,
                        "content": session.redactor.mask_text(&result)
                    }));
                }
            }
        }
    }
    Ok(messages)
}

fn learn(shell: &Shell, redactor: &mut Redactor) {
    let sources = shell.redact_sources();
    redactor.learn_secret(&sources.secret);
    for url in sources.urls {
        redactor.learn_url(&url);
    }
    for yaml in sources.yaml_bodies {
        redactor.learn_yaml(&yaml);
    }
}

pub struct AgentSettings {
    pub base: String,
    pub key: String,
    pub model: String,
}

fn completions_url(base: &str) -> String {
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    }
}

async fn post_chat(
    client: &reqwest::Client,
    settings: &AgentSettings,
    messages: Vec<Value>,
) -> Result<AssistantMessage, String> {
    let body = json!({
        "model": settings.model,
        "temperature": 0,
        "max_tokens": 1024,
        "stream": false,
        "messages": messages,
        "tools": tool_schemas()
    });
    let response = client
        .post(completions_url(&settings.base))
        .header("Authorization", format!("Bearer {}", settings.key))
        .json(&body)
        .send()
        .await
        .map_err(|err| format!("模型请求失败: {err}"))?;
    let status = response.status();
    let text = response.text().await.map_err(|err| err.to_string())?;
    if !status.is_success() {
        return Err(format!("模型请求失败: HTTP {status} {}", short(&text)));
    }
    let parsed: ChatResponse =
        serde_json::from_str(&text).map_err(|err| format!("模型响应无效: {err}"))?;
    parsed
        .choices
        .into_iter()
        .next()
        .map(|choice| choice.message)
        .ok_or_else(|| "模型响应没有内容".into())
}

fn short(text: &str) -> String {
    text.chars().take(500).collect()
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| format!("缺少 {key}"))
}

fn arg_bool(args: &Value, key: &str) -> Result<bool, String> {
    args.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("缺少 {key}"))
}

fn arg_u16(args: &Value, key: &str) -> Result<u16, String> {
    let number = args
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("缺少 {key}"))?;
    u16::try_from(number).map_err(|_| "端口无效".to_string())
}

fn load_records(path: &Path) -> Vec<Record> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: AssistantMessage,
}

#[derive(Deserialize)]
struct AssistantMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Deserialize)]
struct ToolCall {
    id: String,
    function: FunctionCall,
}

#[derive(Deserialize)]
struct FunctionCall {
    name: String,
    arguments: String,
}

fn tool_schemas() -> Vec<Value> {
    vec![
        tool(
            "list_profiles",
            "列出订阅 id 和名称",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "import_subscription",
            "导入订阅。url 使用代号或状态中出现的地址",
            json!({
                "type":"object",
                "properties":{
                    "url":{"type":"string"},
                    "name":{"type":"string"}
                },
                "required":["url"]
            }),
        ),
        tool(
            "refresh_subscription",
            "按 id 刷新订阅",
            json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}),
        ),
        tool(
            "activate_profile",
            "切换当前订阅",
            json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"]}),
        ),
        tool("start_core", "启动 mihomo", json!({"type":"object","properties":{}})),
        tool("stop_core", "停止 mihomo", json!({"type":"object","properties":{}})),
        tool("get_status", "读取当前短状态", json!({"type":"object","properties":{}})),
        tool(
            "set_mode",
            "设置模式并写入覆盖",
            json!({
                "type":"object",
                "properties":{"mode":{"type":"string","enum":["rule","global","direct"]}},
                "required":["mode"]
            }),
        ),
        tool(
            "select_proxy",
            "在已有代理组中选择节点",
            json!({
                "type":"object",
                "properties":{"group":{"type":"string"},"name":{"type":"string"}},
                "required":["group","name"]
            }),
        ),
        tool(
            "set_mixed_port",
            "设置 mixed 端口",
            json!({"type":"object","properties":{"port":{"type":"integer"}},"required":["port"]}),
        ),
        tool(
            "set_tun",
            "开关 TUN",
            json!({"type":"object","properties":{"enable":{"type":"boolean"}},"required":["enable"]}),
        ),
        tool(
            "list_tun_route_excludes",
            "列出 tun.route-exclude-address（不进 TUN 的网段）",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "add_tun_route_exclude",
            "把 CIDR/IP 写入 tun.route-exclude-address，决定该网段不进 TUN；不是进 TUN 后的规则",
            json!({"type":"object","properties":{"cidr":{"type":"string"}},"required":["cidr"]}),
        ),
        tool(
            "remove_tun_route_exclude",
            "从 tun.route-exclude-address 删除一条",
            json!({"type":"object","properties":{"cidr":{"type":"string"}},"required":["cidr"]}),
        ),
        tool(
            "set_system_proxy",
            "开关系统代理（指向 mixed 口）",
            json!({"type":"object","properties":{"enable":{"type":"boolean"}},"required":["enable"]}),
        ),
        tool(
            "set_allow_lan",
            "开关 allow-lan",
            json!({"type":"object","properties":{"enable":{"type":"boolean"}},"required":["enable"]}),
        ),
        tool(
            "explain_route",
            "判定域名出口：规则 + DNS + 该 IP 的系统路由 + 组的当前节点",
            json!({
                "type":"object",
                "properties":{"host":{"type":"string"}},
                "required":["host"]
            }),
        ),
        tool(
            "probe_url",
            "经 mixed 探测目标 URL（当前生效配置）；链路只来自 /connections 连接日志，不做规则推理",
            json!({
                "type":"object",
                "properties":{
                    "url":{"type":"string"},
                    "timeout_ms":{"type":"integer","minimum":1000,"maximum":30000}
                },
                "required":["url"]
            }),
        ),
        tool(
            "dns_lookup",
            "解析域名 A/AAAA，并标注 GEOIP 国家码",
            json!({
                "type":"object",
                "properties":{"host":{"type":"string"}},
                "required":["host"]
            }),
        ),
        tool(
            "list_rules",
            "列出当前生效规则（最多 120 条）",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "list_connections",
            "列出当前连接；可按 host 过滤",
            json!({
                "type":"object",
                "properties":{"host":{"type":"string"}}
            }),
        ),
        tool(
            "net_overview",
            "本机默认路由、TUN、其它 VPN 冲突摘要",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "net_interfaces",
            "活跃网卡列表",
            json!({"type":"object","properties":{}}),
        ),
        tool(
            "net_routes",
            "IPv4 路由摘要",
            json!({"type":"object","properties":{}}),
        ),
    ]
}

fn tool(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "type": "function",
        "function": { "name": name, "description": description, "parameters": parameters }
    })
}
