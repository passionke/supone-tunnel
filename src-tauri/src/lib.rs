// 作者: kejiqing

mod agent;
mod corebin;
mod mcp;
mod netenv;
mod observe;
mod privkill;
mod profile;
mod redact;
mod runtime;
mod settings;
mod shell;
mod sysproxy;

pub use privkill::main_from_args as privkill_main;

use agent::{ConversationSummary, Session, UiMessage};
use std::sync::Arc;
use tokio::sync::watch;
use profile::ProfileMeta;
use runtime::{ProxyGroup, TrafficSample};
use settings::{AppSettings, SettingsView};
use shell::{Shell, Status};
use tauri::{AppHandle, Manager, RunEvent, State, WindowEvent};

#[cfg(desktop)]
use tauri::menu::{CheckMenuItem, Menu, MenuItem, Submenu};
#[cfg(desktop)]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

#[cfg(desktop)]
struct TrayModeMenu {
    rule: CheckMenuItem<tauri::Wry>,
    global: CheckMenuItem<tauri::Wry>,
    direct: CheckMenuItem<tauri::Wry>,
}

struct AppState {
    shell: std::sync::Arc<Shell>,
    session: tokio::sync::Mutex<Session>,
    /// 对话进行中取消：true 表示用户点了停止。Author: kejiqing
    chat_cancel: Arc<watch::Sender<bool>>,
    mcp_stop: std::sync::Mutex<Option<tokio::sync::watch::Sender<bool>>>,
    #[cfg(desktop)]
    tray_mode: TrayModeMenu,
}

#[tauri::command]
async fn runtime_status(state: State<'_, AppState>) -> Result<Status, String> {
    state.shell.status().await
}

#[tauri::command]
async fn runtime_start(state: State<'_, AppState>) -> Result<Status, String> {
    state.shell.start().await
}

#[tauri::command]
async fn runtime_stop(state: State<'_, AppState>) -> Result<(), String> {
    state.shell.stop().await
}

#[tauri::command]
async fn runtime_traffic(state: State<'_, AppState>) -> Result<TrafficSample, String> {
    state.shell.traffic_sample().await
}

#[tauri::command]
async fn runtime_set_mode(
    app: AppHandle,
    state: State<'_, AppState>,
    mode: String,
) -> Result<String, String> {
    let text = state.shell.set_mode(&mode).await?;
    #[cfg(desktop)]
    sync_tray_mode(&app);
    #[cfg(mobile)]
    let _ = app;
    Ok(text)
}

#[tauri::command]
async fn runtime_groups(state: State<'_, AppState>) -> Result<Vec<ProxyGroup>, String> {
    state.shell.groups().await
}

#[tauri::command]
async fn runtime_select(
    state: State<'_, AppState>,
    group: String,
    name: String,
) -> Result<String, String> {
    state.shell.select_proxy(&group, &name).await
}

#[tauri::command]
async fn profile_list(state: State<'_, AppState>) -> Result<Vec<ProfileMeta>, String> {
    state.shell.list_profiles()
}

#[tauri::command]
async fn profile_import(
    state: State<'_, AppState>,
    url: String,
    name: Option<String>,
) -> Result<String, String> {
    state.shell.import_subscription(&url, name).await
}

#[tauri::command]
async fn profile_refresh(state: State<'_, AppState>, id: String) -> Result<String, String> {
    state.shell.refresh_subscription(&id).await
}

#[tauri::command]
async fn profile_activate(state: State<'_, AppState>, id: String) -> Result<String, String> {
    state.shell.activate_profile(&id).await
}

#[tauri::command]
fn profile_read_override(state: State<'_, AppState>) -> Result<String, String> {
    state.shell.read_override()
}

#[tauri::command]
async fn profile_write_override(state: State<'_, AppState>, yaml: String) -> Result<(), String> {
    state.shell.write_override(&yaml).await
}

#[tauri::command]
async fn profile_reset_override(
    state: State<'_, AppState>,
    id: Option<String>,
) -> Result<String, String> {
    state.shell.reset_override(id.as_deref()).await
}

#[tauri::command]
async fn runtime_set_tun(state: State<'_, AppState>, enable: bool) -> Result<Status, String> {
    state.shell.set_tun(enable).await?;
    state.shell.status().await
}

#[tauri::command]
async fn runtime_set_system_proxy(
    state: State<'_, AppState>,
    enable: bool,
) -> Result<Status, String> {
    state.shell.set_system_proxy(enable).await?;
    state.shell.status().await
}

#[tauri::command]
async fn agent_history(state: State<'_, AppState>) -> Result<Vec<UiMessage>, String> {
    let session = state.session.lock().await;
    Ok(session.ui_messages())
}

#[tauri::command]
async fn agent_list_conversations(
    state: State<'_, AppState>,
) -> Result<Vec<ConversationSummary>, String> {
    let session = state.session.lock().await;
    Ok(session.list_conversations())
}

#[tauri::command]
async fn agent_send(state: State<'_, AppState>, text: String) -> Result<Vec<UiMessage>, String> {
    let _ = state.chat_cancel.send(false);
    let mut cancel = state.chat_cancel.subscribe();
    let mut session = state.session.lock().await;
    agent::chat(&state.shell, &mut session, &text, &mut cancel).await
}

#[tauri::command]
async fn agent_cancel(state: State<'_, AppState>) -> Result<(), String> {
    let _ = state.chat_cancel.send(true);
    Ok(())
}

#[tauri::command]
async fn agent_new_conversation(state: State<'_, AppState>) -> Result<Vec<UiMessage>, String> {
    let mut session = state.session.lock().await;
    session.new_conversation();
    Ok(session.ui_messages())
}

#[tauri::command]
async fn agent_open_conversation(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<UiMessage>, String> {
    let mut session = state.session.lock().await;
    session.open_conversation(&id)?;
    Ok(session.ui_messages())
}

#[tauri::command]
fn settings_get(state: State<'_, AppState>) -> SettingsView {
    state.shell.settings_view()
}

#[tauri::command]
fn settings_save(state: State<'_, AppState>, settings: AppSettings) -> Result<SettingsView, String> {
    let view = state.shell.save_settings(settings)?;
    // 保存后重挂 MCP，供 NeuroGate 立即用新 token/bind。
    if let Ok(mut guard) = state.mcp_stop.lock() {
        if let Some(tx) = guard.take() {
            let _ = tx.send(false);
        }
        *guard = mcp::spawn(
            std::sync::Arc::clone(&state.shell),
            state.shell.settings.mcp_config(),
        );
    }
    Ok(view)
}

#[tauri::command]
async fn settings_download_mihomo(state: State<'_, AppState>) -> Result<SettingsView, String> {
    state.shell.download_mihomo().await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let shell = std::sync::Arc::new(
                Shell::open(dir.clone())
                    .map_err(|err| -> Box<dyn std::error::Error> { err.into() })?,
            );
            // 上次强杀/崩溃可能留下系统代理指向已死的 mixed 口，先清掉。
            shell.heal_stale_proxy_blocking();
            // TUN+境外 DNS 曾把整机流量搞乱：启动时救场（关 TUN 意图、国内 nameserver）。
            match shell.recover_safe_egress() {
                Ok(msg) => eprintln!("[supone] {msg}"),
                Err(err) => eprintln!("[supone] 救场失败: {err}"),
            }
            let session = Session::open(dir);
            let (chat_cancel, _) = watch::channel(false);
            let chat_cancel = Arc::new(chat_cancel);
            let mcp_stop = mcp::spawn(std::sync::Arc::clone(&shell), shell.settings.mcp_config());

            #[cfg(desktop)]
            {
                let mode_now = shell.configured_mode();
                let rule = CheckMenuItem::with_id(
                    app,
                    "mode-rule",
                    "规则",
                    true,
                    mode_now == "rule",
                    None::<&str>,
                )?;
                let global = CheckMenuItem::with_id(
                    app,
                    "mode-global",
                    "全局",
                    true,
                    mode_now == "global",
                    None::<&str>,
                )?;
                let direct = CheckMenuItem::with_id(
                    app,
                    "mode-direct",
                    "直连",
                    true,
                    mode_now == "direct",
                    None::<&str>,
                )?;
                let mode = Submenu::with_id_and_items(
                    app,
                    "mode",
                    "模式",
                    true,
                    &[&rule, &global, &direct],
                )?;
                let show = MenuItem::with_id(app, "show", "打开窗口", true, None::<&str>)?;
                let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&mode, &show, &quit])?;
                let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))?;
                app.manage(AppState {
                    shell,
                    session: tokio::sync::Mutex::new(session),
                    chat_cancel,
                    mcp_stop: std::sync::Mutex::new(mcp_stop),
                    tray_mode: TrayModeMenu {
                        rule,
                        global,
                        direct,
                    },
                });
                TrayIconBuilder::with_id("main")
                    .tooltip("Supone Tunnel")
                    .icon(icon)
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .on_tray_icon_event(|tray, event| {
                        // 展开菜单前按覆盖里的真实模式刷新勾选（含 MCP / 对话改过的）。Author: kejiqing
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            sync_tray_mode(tray.app_handle());
                        }
                    })
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "quit" => {
                            let state = app.state::<AppState>();
                            state.shell.shutdown_blocking();
                            app.exit(0);
                        }
                        "show" => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.unminimize();
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        other => {
                            let id = other.to_string();
                            let app = app.clone();
                            tauri::async_runtime::spawn(async move {
                                if let Err(err) = tray_action(&app, &id).await {
                                    eprintln!("托盘操作失败: {err}");
                                }
                            });
                        }
                    })
                    .build(app)?;
            }

            #[cfg(mobile)]
            {
                app.manage(AppState {
                    shell,
                    session: tokio::sync::Mutex::new(session),
                    chat_cancel,
                    mcp_stop: std::sync::Mutex::new(mcp_stop),
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            #[cfg(desktop)]
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
            #[cfg(mobile)]
            {
                let _ = (window, event);
            }
        })
        .invoke_handler(tauri::generate_handler![
            runtime_status,
            runtime_start,
            runtime_stop,
            runtime_traffic,
            runtime_set_mode,
            runtime_set_tun,
            runtime_set_system_proxy,
            runtime_groups,
            runtime_select,
            profile_list,
            profile_import,
            profile_refresh,
            profile_activate,
            profile_read_override,
            profile_write_override,
            profile_reset_override,
            agent_history,
            agent_list_conversations,
            agent_send,
            agent_cancel,
            agent_new_conversation,
            agent_open_conversation,
            settings_get,
            settings_save,
            settings_download_mihomo
        ])
        .build(tauri::generate_context!())
        .expect("启动失败")
        .run(|app, event| match event {
            RunEvent::ExitRequested { .. } | RunEvent::Exit => {
                let state = app.state::<AppState>();
                if let Ok(mut guard) = state.mcp_stop.lock() {
                    if let Some(tx) = guard.take() {
                        let _ = tx.send(false);
                    }
                }
                state.shell.shutdown_blocking();
            }
            _ => {}
        });
}

#[cfg(desktop)]
async fn tray_action(app: &AppHandle, id: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    match id {
        "mode-rule" => {
            state.shell.set_mode("rule").await?;
        }
        "mode-global" => {
            state.shell.set_mode("global").await?;
        }
        "mode-direct" => {
            state.shell.set_mode("direct").await?;
        }
        _ => return Ok(()),
    }
    sync_tray_mode(app);
    Ok(())
}

/// 托盘「模式」三项互斥勾选，对齐 configured_mode。Author: kejiqing
#[cfg(desktop)]
fn sync_tray_mode(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mode = state.shell.configured_mode();
    let _ = state.tray_mode.rule.set_checked(mode == "rule");
    let _ = state.tray_mode.global.set_checked(mode == "global");
    let _ = state.tray_mode.direct.set_checked(mode == "direct");
}
