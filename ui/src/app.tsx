import { useEffect, useRef, useState } from "react";
import { renderMarkdown } from "./markdown";
import type { AppSettings, Profile, ProxyGroup, SettingsView, Status, UiMessage } from "./api";
import {
  agentHistory,
  agentNewConversation,
  agentSend,
  profileActivate,
  profileImport,
  profileList,
  profileReadOverride,
  profileRefresh,
  profileWriteOverride,
  runtimeGroups,
  runtimeSelect,
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
  const [groups, setGroups] = useState<ProxyGroup[]>([]);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  async function load() {
    setError("");
    try {
      const next = await runtimeStatus();
      setStatus(next);
      setGroups(next.running ? await runtimeGroups() : []);
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
        <span>NeuroGate</span>
      </div>
      <div className="actions">
        <button className="primary" disabled={busy} onClick={() => void run(() => runtimeStart())}>
          启动
        </button>
        <button disabled={busy} onClick={() => void run(() => runtimeStop())}>
          停止
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
      {groups.map((group) => (
        <label className="group" key={group.name}>
          <span>{group.name}</span>
          <select
            value={group.now}
            disabled={busy}
            onChange={(event) => {
              const name = event.currentTarget.value;
              void run(() => runtimeSelect(group.name, name));
            }}
          >
            {group.all.map((name) => (
              <option key={name} value={name}>
                {name}
              </option>
            ))}
          </select>
        </label>
      ))}
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
          placeholder="订阅地址 https://"
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
        <div className="profile" key={profile.id}>
          <div>
            <strong>
              {profile.name}
              {profile.active ? " · 当前" : ""}
            </strong>
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
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    profileReadOverride()
      .then(setYaml)
      .catch((err) => setError(message(err)));
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
            profileWriteOverride(yaml)
              .catch((err) => setError(message(err)))
              .finally(() => setBusy(false));
          }}
        >
          保存
        </button>
      </div>
      {error && <p className="error">{error}</p>}
    </section>
  );
}

function ChatPane() {
  const [messages, setMessages] = useState<UiMessage[]>([]);
  const [text, setText] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const thread = useRef<HTMLDivElement>(null);

  useEffect(() => {
    agentHistory()
      .then(setMessages)
      .catch((err) => setError(message(err)));
  }, []);

  useEffect(() => {
    const node = thread.current;
    if (node) {
      node.scrollTop = node.scrollHeight;
    }
  }, [messages, busy]);

  function send() {
    const content = text.trim();
    if (busy || content === "") {
      return;
    }
    setBusy(true);
    setError("");
    setMessages((prev) => [...prev, { role: "user", text: content }]);
    setText("");
    agentSend(content)
      .then((next) => {
        setMessages(next);
      })
      .catch(async (err) => {
        setError(message(err));
        try {
          setMessages(await agentHistory());
        } catch {
          // 历史记录读取失败时保留当前对话。
        }
      })
      .finally(() => setBusy(false));
  }

  function startNew() {
    if (busy) {
      return;
    }
    setBusy(true);
    setError("");
    agentNewConversation()
      .then((next) => {
        setMessages(next);
        setText("");
      })
      .catch((err) => setError(message(err)))
      .finally(() => setBusy(false));
  }

  return (
    <section className="panel chat-panel">
      <div className="chat-bar">
        <strong>对话</strong>
        <button type="button" className="ghost" disabled={busy} onClick={startNew}>
          新对话
        </button>
      </div>
      <div className="chat" ref={thread}>
        {messages.length === 0 && !busy && (
          <p className="chat-empty">直接说要改的出口、规则或 DNS。</p>
        )}
        {messages.map((item, index) => (
          <ChatMessage item={item} key={`${item.role}-${index}`} />
        ))}
        {busy && (
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
          send();
        }}
      >
        <textarea
          className="chat-input"
          rows={1}
          placeholder="Enter 发送，Shift+Enter 换行"
          value={text}
          disabled={busy}
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
            send();
          }}
        />
        <button className="primary" type="submit" disabled={busy || text.trim() === ""}>
          {busy ? "…" : "发送"}
        </button>
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
    agentModel: "",
    mihomoBin: "",
    mcpEnabled: false,
    mcpBind: "0.0.0.0:18765",
    mcpToken: "",
  });
  const [resolved, setResolved] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [hint, setHint] = useState("");
  const [busy, setBusy] = useState(false);

  function apply(view: SettingsView) {
    setForm({
      agentBaseUrl: view.agentBaseUrl,
      agentApiKey: view.agentApiKey,
      agentModel: view.agentModel,
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
      <p className="muted">对话固定走 NeuroGate。这里只改内核路径和本机 MCP。</p>
      <div className="grid settings-grid">
        <label htmlFor="mihomo-bin">内核路径</label>
        <input
          id="mihomo-bin"
          type="text"
          placeholder="可空；留空则用应用目录里的官方 mihomo"
          value={form.mihomoBin}
          onInput={(event) => setForm({ ...form, mihomoBin: event.currentTarget.value })}
        />
        <label htmlFor="mcp-enabled">MCP（NeuroGate）</label>
        <label className="check">
          <input
            id="mcp-enabled"
            type="checkbox"
            checked={form.mcpEnabled}
            onChange={(event) => setForm({ ...form, mcpEnabled: event.currentTarget.checked })}
          />
          启用 HTTP + Token
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
        <input
          id="mcp-token"
          type="password"
          placeholder="Bearer token，给 NeuroGate inject"
          value={form.mcpToken}
          onInput={(event) => setForm({ ...form, mcpToken: event.currentTarget.value })}
        />
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
