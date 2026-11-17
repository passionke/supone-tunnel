import { useEffect, useRef, useState } from "react";
import { renderMarkdown } from "./markdown";
import type {
  AppSettings,
  ConversationSummary,
  Profile,
  SettingsView,
  Status,
  UiMessage,
} from "./api";
import {
  agentCancel,
  agentHistory,
  agentListConversations,
  agentNewConversation,
  agentOpenConversation,
  agentSend,
  profileActivate,
  profileImport,
  profileList,
  profileReadOverride,
  profileRefresh,
  profileResetOverride,
  profileWriteOverride,
  runtimeSetMode,
  runtimeSetSystemProxy,
  runtimeSetTun,
  runtimeStart,
  runtimeStatus,
  runtimeStop,
  settingsDownloadMihomo,
  settingsGet,
  settingsSave,
} from "./api";
import { MessageActions } from "./components/message-actions";
import { ThinkingLoader } from "./components/thinking-loader";
import { ToolCallCard } from "./components/tool-call-card";
import { TrafficChart } from "./components/traffic-chart";

type Tab = "status" | "profiles" | "override" | "chat" | "settings";

export function App() {
  const [tab, setTab] = useState<Tab>("status");
  return (
    <div className="app">
      <header>
        <h1>Supone Tunnel</h1>
        <nav>
          <TabButton id="status" tab={tab} setTab={setTab} label="状态" />
          <TabButton id="profiles" tab={tab} setTab={setTab} label="订阅" />
          <TabButton id="override" tab={tab} setTab={setTab} label="覆盖" />
          <TabButton id="chat" tab={tab} setTab={setTab} label="对话" />
          <TabButton id="settings" tab={tab} setTab={setTab} label="设置" />
        </nav>
      </header>
      <main>
        {tab === "status" && <StatusPane />}
        {tab === "profiles" && <ProfilePane />}
        {tab === "override" && <OverridePane />}
        {tab === "chat" && <ChatPane />}
        {tab === "settings" && <SettingsPane />}
      </main>
    </div>
  );
}

function TabButton(props: { id: Tab; tab: Tab; setTab: (tab: Tab) => void; label: string }) {
  return (
    <button className={props.tab === props.id ? "active" : ""} onClick={() => props.setTab(props.id)}>
      {props.label}
    </button>
  );
}

function StatusPane() {
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  async function load() {
    setError("");
    try {
      setStatus(await runtimeStatus());
    } catch (err) {
      setError(message(err));
    }
  }

  useEffect(() => {
    void load();
  }, []);

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError("");
    try {
      await action();
      await load();
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }

  const mode = status?.mode ?? "rule";
  const modeLabel =
    mode === "global" ? "全局" : mode === "direct" ? "直连" : "规则";
  return (
    <section className="panel">
      <TrafficChart running={Boolean(status?.running)} />
      <div className="grid">
        <span>内核</span>
        <strong>{status?.running ? "运行中" : "已停止"}</strong>
        <span>版本</span>
        <span>{status?.version || "—"}</span>
        <span>订阅</span>
        <span>{status?.activeProfileName || "无"}</span>
        <span>模式</span>
        <span>{modeLabel}</span>
        <span>TUN</span>
        <strong>
          {status?.tunEnabled
            ? `开${status.tunDevice ? `（${status.tunDevice}）` : ""}`
            : "关"}
        </strong>
        <span>系统代理</span>
        <strong>
          {status?.systemProxy === true
            ? "开"
            : status?.systemProxy === false
              ? "关"
              : status?.systemProxyError || "未知"}
        </strong>
        <span>默认路由</span>
        <span>{status?.defaultRoute || "—"}</span>
        <span>出口说明</span>
        <span>{status?.trafficHint || "—"}</span>
        <span>mixed 端口</span>
        <span>{status?.mixedPort ?? "—"}</span>
        <span>控制器</span>
        <span>127.0.0.1:{status?.controllerPort ?? "—"}</span>
        <span>官方内核</span>
        <span className="url">{status?.mihomoBin || "未安装，请到设置下载"}</span>
        <span>对话</span>
        <span>{status?.agentReady ? "已授权" : "需订阅 token"}</span>
      </div>
      <div className="actions">
        <button
          type="button"
          role="switch"
          aria-checked={Boolean(status?.running)}
          aria-label={status?.running ? "停止内核" : "启动内核"}
          className={`power-switch${status?.running ? " is-on" : ""}`}
          disabled={busy}
          onClick={() =>
            void run(() => (status?.running ? runtimeStop() : runtimeStart()))
          }
        >
          <span className="power-switch__track" aria-hidden="true">
            <span className="power-switch__thumb" />
          </span>
          <span className="power-switch__text">{status?.running ? "运行中" : "已停止"}</span>
        </button>
        <button disabled={busy || mode === "rule"} onClick={() => void run(() => runtimeSetMode("rule"))}>
          规则
        </button>
        <button disabled={busy || mode === "global"} onClick={() => void run(() => runtimeSetMode("global"))}>
          全局
        </button>
        <button disabled={busy || mode === "direct"} onClick={() => void run(() => runtimeSetMode("direct"))}>
          直连
        </button>
        <button
          disabled={busy || !status?.running}
          onClick={() => void run(() => runtimeSetTun(!status?.tunEnabled))}
        >
          {status?.tunEnabled ? "关闭 TUN" : "开启 TUN"}
        </button>
        <button
          disabled={busy || (!status?.running && !status?.systemProxy)}
          onClick={() => void run(() => runtimeSetSystemProxy(!status?.systemProxy))}
        >
          {status?.systemProxy ? "关闭系统代理" : "开启系统代理"}
        </button>
        <button disabled={busy} onClick={() => void load()}>
          刷新
        </button>
      </div>
      {error && <p className="error">{error}</p>}
    </section>
  );
}

function ProfilePane() {
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [url, setUrl] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  async function load() {
    setError("");
    try {
      setProfiles(await profileList());
    } catch (err) {
      setError(message(err));
    }
  }

  useEffect(() => {
    void load();
  }, []);

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError("");
    try {
      await action();
      await load();
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="panel">
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void run(async () => {
            await profileImport(url.trim(), name.trim());
            setUrl("");
            setName("");
          });
        }}
      >
        <input
          type="url"
          required
          placeholder="https://phub.supone.vip/clash/<token>"
          value={url}
          onInput={(event) => setUrl(event.currentTarget.value)}
        />
        <input
          type="text"
          placeholder="名称，可空"
          value={name}
          onInput={(event) => setName(event.currentTarget.value)}
        />
        <button disabled={busy} type="submit">
          导入
        </button>
      </form>
      {profiles.length === 0 && <p className="muted">还没有订阅。</p>}
      {profiles.map((profile) => (
        <div className={profile.active ? "profile profile--active" : "profile"} key={profile.id}>
          <div>
            <strong>{profile.name}</strong>
            {profile.active ? <span className="profile-badge">当前</span> : null}
            <div className="url">{profile.url}</div>
            <div className="muted">{profile.updatedAt}</div>
          </div>
          <div className="actions">
            {!profile.active && (
              <button disabled={busy} onClick={() => void run(() => profileActivate(profile.id))}>
                启用
              </button>
            )}
            <button disabled={busy} onClick={() => void run(() => profileRefresh(profile.id))}>
              刷新
            </button>
            {profile.active && (
              <button
                disabled={busy}
                onClick={() => {
                  if (!window.confirm("清空本地覆盖，回到仅订阅原文？规则/DNS/TUN 排除等本地改动会丢失。")) {
                    return;
                  }
                  void run(() => profileResetOverride(profile.id));
                }}
              >
                恢复初始
              </button>
            )}
          </div>
        </div>
      ))}
      {error && <p className="error">{error}</p>}
    </section>
  );
}

function OverridePane() {
  const [yaml, setYaml] = useState("");
  const [error, setError] = useState("");
  const [hint, setHint] = useState("");
  const [busy, setBusy] = useState(false);

  function reload() {
    return profileReadOverride()
      .then(setYaml)
      .catch((err) => setError(message(err)));
  }

  useEffect(() => {
    void reload();
  }, []);

  return (
    <section className="panel">
      <p className="muted">这里编辑的是本地覆盖。订阅原文不会被改写。保存后合并，并在内核运行时重载。</p>
      <textarea className="yaml" value={yaml} onInput={(event) => setYaml(event.currentTarget.value)} />
      <div className="actions">
        <button
          className="primary"
          disabled={busy}
          onClick={() => {
            setBusy(true);
            setError("");
            setHint("");
            profileWriteOverride(yaml)
              .then(() => setHint("已保存"))
              .catch((err) => setError(message(err)))
              .finally(() => setBusy(false));
          }}
        >
          保存
        </button>
        <button
          disabled={busy}
          onClick={() => {
            if (!window.confirm("清空本地覆盖，回到仅订阅原文？规则/DNS/TUN 排除等本地改动会丢失。")) {
              return;
            }
            setBusy(true);
            setError("");
            setHint("");
            profileResetOverride()
              .then(async (text) => {
                setHint(text);
                await reload();
              })
              .catch((err) => setError(message(err)))
              .finally(() => setBusy(false));
          }}
        >
          清空覆盖（恢复初始）
        </button>
      </div>
      {hint && <p className="muted">{hint}</p>}
      {error && <p className="error">{error}</p>}
    </section>
  );
}

function ChatPane() {
  const [messages, setMessages] = useState<UiMessage[]>([]);
  const [conversations, setConversations] = useState<ConversationSummary[]>([]);
  // 本地 activeId：下拉是受控组件，必须立刻跟上用户选择，否则会弹回旧项看起来「没生效」。Author: kejiqing
  const [activeId, setActiveId] = useState("");
  const [text, setText] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [sending, setSending] = useState(false);
  const thread = useRef<HTMLDivElement>(null);

  function applyConversations(list: ConversationSummary[]) {
    setConversations(list);
    const active = list.find((item) => item.active);
    if (active) {
      setActiveId(active.id);
    }
  }

  async function refreshConversations() {
    try {
      applyConversations(await agentListConversations());
    } catch (err) {
      setError(message(err));
    }
  }

  useEffect(() => {
    Promise.all([agentHistory(), agentListConversations()])
      .then(([history, list]) => {
        setMessages(history);
        applyConversations(list);
      })
      .catch((err) => setError(message(err)));
  }, []);

  useEffect(() => {
    const node = thread.current;
    if (node) {
      node.scrollTop = node.scrollHeight;
    }
  }, [messages, sending, activeId]);

  const locked = busy || sending;

  function send() {
    const content = text.trim();
    if (locked || content === "") {
      return;
    }
    setBusy(true);
    setSending(true);
    setError("");
    setMessages((prev) => [...prev, { role: "user", text: content }]);
    setText("");
    agentSend(content)
      .then(async (next) => {
        setMessages(next);
        await refreshConversations();
      })
      .catch(async (err) => {
        const errText = message(err);
        if (errText.includes("已取消")) {
          setError("");
        } else {
          setError(errText);
        }
        try {
          setMessages(await agentHistory());
          await refreshConversations();
        } catch {
          // 历史记录读取失败时保留当前对话。
        }
      })
      .finally(() => {
        setSending(false);
        setBusy(false);
      });
  }

  function cancel() {
    if (!sending) {
      return;
    }
    agentCancel().catch((err) => setError(message(err)));
  }

  function startNew() {
    if (locked) {
      return;
    }
    setBusy(true);
    setError("");
    agentNewConversation()
      .then(async (next) => {
        // 先拉列表再清消息，避免「消息已空、下拉仍显示旧会话」的闪一下。Author: kejiqing
        const list = await agentListConversations();
        applyConversations(list);
        setMessages(next);
        setText("");
      })
      .catch((err) => setError(message(err)))
      .finally(() => setBusy(false));
  }

  function openConversation(id: string) {
    if (locked || !id || id === activeId) {
      return;
    }
    const previousId = activeId;
    const previousMessages = messages;
    setBusy(true);
    setError("");
    setActiveId(id);
    setMessages([]);
    setText("");
    agentOpenConversation(id)
      .then(async (next) => {
        setMessages(next);
        await refreshConversations();
      })
      .catch(async (err) => {
        setActiveId(previousId);
        setMessages(previousMessages);
        setError(message(err));
        try {
          await refreshConversations();
        } catch {
          // 恢复列表失败时至少把下拉与消息弹回原会话。
        }
      })
      .finally(() => setBusy(false));
  }

  return (
    <section className="panel chat-panel">
      <div className="chat-bar">
        <div className="chat-bar-left">
          <strong>对话</strong>
          <label className="chat-history">
            <span className="sr-only">对话历史</span>
            <select
              value={activeId}
              disabled={locked || conversations.length === 0}
              onChange={(event) => openConversation(event.currentTarget.value)}
            >
              {conversations.length === 0 && <option value="">新对话</option>}
              {conversations.map((item) => (
                <option key={item.id} value={item.id}>
                  {item.title}
                  {item.updatedAt ? ` · ${item.updatedAt}` : ""}
                </option>
              ))}
            </select>
          </label>
        </div>
        <button type="button" className="ghost" disabled={locked} onClick={startNew}>
          新对话
        </button>
      </div>
      <div className="chat" ref={thread} key={activeId}>
        {messages.length === 0 && !sending && (
          <p className="chat-empty">
            壳内对话是 NeuroGate 聊天脑（不改本机配置）。改出口/规则请用本机 PI + MCP。需先在「订阅」导入
            https://phub.supone.vip/clash/…
          </p>
        )}
        {messages.map((item, index) => (
          <ChatMessage item={item} key={`${activeId}-${item.role}-${index}`} />
        ))}
        {sending && (
          <div className="chat-thinking">
            <ThinkingLoader
              phases={["处理中", "调用工具", "整理结果"]}
              doneLabel={(seconds) => `已用时 ${seconds}s`}
              size="sm"
            />
          </div>
        )}
      </div>
      <form
        className="composer"
        onSubmit={(event) => {
          event.preventDefault();
          if (locked) {
            return;
          }
          send();
        }}
      >
        <textarea
          className="chat-input"
          rows={1}
          placeholder="Enter 发送，Shift+Enter 换行"
          value={text}
          disabled={locked}
          onInput={(event) => setText(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key !== "Enter" || event.shiftKey) {
              return;
            }
            event.preventDefault();
            // WebKit（Safari / Tauri macOS）确认候选时先把 compositionend 交给脚本，
            // 随后的 Enter keydown 上 isComposing 已是 false，keyCode 仍为 229。
            // Author: kejiqing
            if (event.nativeEvent.isComposing || event.keyCode === 229) {
              return;
            }
            if (locked) {
              return;
            }
            send();
          }}
        />
        {sending ? (
          <button className="danger" type="button" onClick={cancel}>
            停止
          </button>
        ) : (
          <button className="primary" type="submit" disabled={locked || text.trim() === ""}>
            发送
          </button>
        )}
      </form>
      {error && <p className="error chat-error">{error}</p>}
    </section>
  );
}

/** 后端 tool 行是 `name → result`。Author: kejiqing */
function parseToolLine(text: string): { name: string; result: string } {
  const sep = " → ";
  const at = text.indexOf(sep);
  if (at <= 0) {
    return { name: "tool", result: text };
  }
  return {
    name: text.slice(0, at).trim() || "tool",
    result: text.slice(at + sep.length),
  };
}

function ChatMessage(props: { item: UiMessage }) {
  const { item } = props;
  if (item.role === "tool") {
    const tool = parseToolLine(item.text);
    return (
      <div className="chat-tool">
        <ToolCallCard name={tool.name} status="success" result={tool.result} summary="已执行" />
      </div>
    );
  }
  if (item.role === "assistant") {
    return (
      <div className="bubble assistant" data-ui-message>
        <div className="md" dangerouslySetInnerHTML={{ __html: renderMarkdown(item.text) }} />
        <MessageActions text={item.text} actions={["copy"]} label="消息操作" />
      </div>
    );
  }
  return (
    <div className="bubble user">
      <div className="md" dangerouslySetInnerHTML={{ __html: renderMarkdown(item.text) }} />
    </div>
  );
}

function SettingsPane() {
  const [form, setForm] = useState<AppSettings>({
    agentBaseUrl: "",
    agentApiKey: "",
    agentCallExpiresAt: "",
    agentSubscriptionToken: "",
    agentHubUserId: 0,
    agentModel: "agent",
    mihomoBin: "",
    mcpEnabled: false,
    mcpBind: "0.0.0.0:18765",
    mcpToken: "",
  });
  const [resolved, setResolved] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [hint, setHint] = useState("");
  const [busy, setBusy] = useState(false);
  const [mcpTokenVisible, setMcpTokenVisible] = useState(false);

  function apply(view: SettingsView) {
    setForm({
      agentBaseUrl: view.agentBaseUrl,
      agentApiKey: view.agentApiKey,
      agentCallExpiresAt: view.agentCallExpiresAt,
      agentSubscriptionToken: view.agentSubscriptionToken,
      agentHubUserId: view.agentHubUserId || 0,
      agentModel: view.agentModel || "agent",
      mihomoBin: view.mihomoBin,
      mcpEnabled: view.mcpEnabled,
      mcpBind: view.mcpBind || "0.0.0.0:18765",
      mcpToken: view.mcpToken,
    });
    setResolved(view.resolvedMihomoBin);
  }

  useEffect(() => {
    settingsGet()
      .then(apply)
      .catch((err) => setError(message(err)));
  }, []);

  async function run(action: () => Promise<SettingsView>, okText: string) {
    setBusy(true);
    setError("");
    setHint("");
    try {
      apply(await action());
      setHint(okText);
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="panel">
      <p className="muted">
        壳内对话走 NeuroGate 聊天脑（不挂本机工具）。改 mihomo 用本机 PI + MCP。对话凭证由订阅换取，并带 Hub user_id 归因。
      </p>
      <div className="grid settings-grid">
        <label htmlFor="agent-model">对话模型</label>
        <input
          id="agent-model"
          type="text"
          placeholder="agent"
          value={form.agentModel}
          onInput={(event) => setForm({ ...form, agentModel: event.currentTarget.value })}
        />
        <label htmlFor="mihomo-bin">内核路径</label>
        <input
          id="mihomo-bin"
          type="text"
          placeholder="可空；留空则用应用目录里的官方 mihomo"
          value={form.mihomoBin}
          onInput={(event) => setForm({ ...form, mihomoBin: event.currentTarget.value })}
        />
        <label htmlFor="mcp-enabled">本机 MCP</label>
        <label className="check">
          <input
            id="mcp-enabled"
            type="checkbox"
            checked={form.mcpEnabled}
            onChange={(event) => setForm({ ...form, mcpEnabled: event.currentTarget.checked })}
          />
          启用 HTTP + Token（给 NeuroGate 挂载）
        </label>
        <label htmlFor="mcp-bind">MCP 监听</label>
        <input
          id="mcp-bind"
          type="text"
          placeholder="0.0.0.0:18765"
          value={form.mcpBind}
          onInput={(event) => setForm({ ...form, mcpBind: event.currentTarget.value })}
        />
        <label htmlFor="mcp-token">MCP Token</label>
        <div className="secret-row">
          <input
            id="mcp-token"
            type={mcpTokenVisible ? "text" : "password"}
            placeholder="Bearer token"
            value={form.mcpToken}
            onInput={(event) => setForm({ ...form, mcpToken: event.currentTarget.value })}
          />
          <button
            type="button"
            className="ghost"
            onClick={() => setMcpTokenVisible((v) => !v)}
          >
            {mcpTokenVisible ? "隐藏" : "查看明文"}
          </button>
        </div>
      </div>
      <div className="actions">
        <button
          className="primary"
          disabled={busy}
          onClick={() => void run(() => settingsSave(form), "设置已保存")}
        >
          保存
        </button>
        <button
          disabled={busy}
          onClick={() => void run(() => settingsDownloadMihomo(), "已下载官方 mihomo")}
        >
          下载官方内核
        </button>
      </div>
      <p className="muted">当前内核：{resolved || "未找到"}</p>
      <p className="muted">
        MCP：
        {form.mcpEnabled
          ? `开 → http://<本机>:${form.mcpBind.includes(":") ? form.mcpBind.split(":").pop() : "18765"}/mcp`
          : "关"}
      </p>
      {hint && <p className="muted">{hint}</p>}
      {error && <p className="error">{error}</p>}
    </section>
  );
}

function message(err: unknown): string {
  if (typeof err === "string") {
    return err;
  }
  if (err instanceof Error) {
    return err.message;
  }
  return "操作失败";
}
