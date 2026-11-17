import { invoke } from "@tauri-apps/api/core";

export type Status = {
  running: boolean;
  version: string;
  mode: string;
  mixedPort: number;
  activeProfileId: string | null;
  activeProfileName: string | null;
  systemProxy: boolean | null;
  systemProxyError: string | null;
  controllerPort: number;
  mihomoBin: string | null;
  agentReady: boolean;
  tunEnabled: boolean;
  tunDevice: string;
  defaultRoute: string;
  trafficHint: string;
};

export type AppSettings = {
  agentBaseUrl: string;
  agentApiKey: string;
  agentModel: string;
  mihomoBin: string;
  mcpEnabled: boolean;
  mcpBind: string;
  mcpToken: string;
};

export type SettingsView = AppSettings & {
  resolvedMihomoBin: string | null;
  agentReady: boolean;
};

export type Profile = {
  id: string;
  name: string;
  url: string;
  updatedAt: string;
  active: boolean;
};

export type ProxyGroup = {
  name: string;
  now: string;
  all: string[];
};

/** mihomo `/traffic` 瞬时上下行（字节/秒）。Author: kejiqing */
export type TrafficSample = {
  up: number;
  down: number;
  running: boolean;
};

export type UiMessage = {
  role: "user" | "assistant" | "tool";
  text: string;
};

export type ConversationSummary = {
  id: string;
  title: string;
  updatedAt: string;
  active: boolean;
};

export function runtimeStatus(): Promise<Status> {
  return invoke("runtime_status");
}

export function runtimeStart(): Promise<Status> {
  return invoke("runtime_start");
}

export function runtimeStop(): Promise<void> {
  return invoke("runtime_stop");
}

export function runtimeTraffic(): Promise<TrafficSample> {
  return invoke("runtime_traffic");
}

export function runtimeSetMode(mode: string): Promise<string> {
  return invoke("runtime_set_mode", { mode });
}

export function runtimeGroups(): Promise<ProxyGroup[]> {
  return invoke("runtime_groups");
}

export function runtimeSelect(group: string, name: string): Promise<string> {
  return invoke("runtime_select", { group, name });
}

export function profileList(): Promise<Profile[]> {
  return invoke("profile_list");
}

export function profileImport(url: string, name: string): Promise<string> {
  return invoke("profile_import", { url, name: name || null });
}

export function profileRefresh(id: string): Promise<string> {
  return invoke("profile_refresh", { id });
}

export function profileActivate(id: string): Promise<string> {
  return invoke("profile_activate", { id });
}

export function profileReadOverride(): Promise<string> {
  return invoke("profile_read_override");
}

export function profileWriteOverride(yaml: string): Promise<void> {
  return invoke("profile_write_override", { yaml });
}

export function profileResetOverride(id?: string | null): Promise<string> {
  return invoke("profile_reset_override", { id: id ?? null });
}

export function runtimeSetTun(enable: boolean): Promise<Status> {
  return invoke("runtime_set_tun", { enable });
}

export function runtimeSetSystemProxy(enable: boolean): Promise<Status> {
  return invoke("runtime_set_system_proxy", { enable });
}

export function agentHistory(): Promise<UiMessage[]> {
  return invoke("agent_history");
}

export function agentListConversations(): Promise<ConversationSummary[]> {
  return invoke("agent_list_conversations");
}

export function agentSend(text: string): Promise<UiMessage[]> {
  return invoke("agent_send", { text });
}

export function agentCancel(): Promise<void> {
  return invoke("agent_cancel");
}

export function agentNewConversation(): Promise<UiMessage[]> {
  return invoke("agent_new_conversation");
}

export function agentOpenConversation(id: string): Promise<UiMessage[]> {
  return invoke("agent_open_conversation", { id });
}

export function settingsGet(): Promise<SettingsView> {
  return invoke("settings_get");
}

export function settingsSave(settings: AppSettings): Promise<SettingsView> {
  return invoke("settings_save", { settings });
}

export function settingsDownloadMihomo(): Promise<SettingsView> {
  return invoke("settings_download_mihomo");
}
