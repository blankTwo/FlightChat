#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize};
use url::Url;
#[cfg(windows)]
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2_2, COREWEBVIEW2_COOKIE_SAME_SITE_KIND_LAX,
    COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE, COREWEBVIEW2_COOKIE_SAME_SITE_KIND_STRICT,
};
#[cfg(windows)]
use webview2_com::{take_pwstr, GetCookiesCompletedHandler};
#[cfg(windows)]
use windows_core::{BOOL, HSTRING, Interface, PWSTR};

const CHAT_WINDOW_LABEL: &str = "chatgpt-session";
static SPLIT_VIEW_EPOCH: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamEvent {
    kind: String,
    text: Option<String>,
    conversation_id: Option<String>,
    title: Option<String>,
    error: Option<String>,
    messages: Option<Vec<HistoryMessage>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryMessage {
    role: String,
    text: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportedCookie {
    name: String,
    value: String,
    domain: String,
    path: Option<String>,
    expiration_date: Option<f64>,
    http_only: Option<bool>,
    secure: Option<bool>,
    same_site: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportedCookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    expiration_date: Option<f64>,
    http_only: bool,
    secure: bool,
    same_site: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImageAttachment {
    name: String,
    data_url: String,
}

fn is_openai_cookie_domain(domain: &str) -> bool {
    let domain = domain.trim().trim_start_matches('.').to_ascii_lowercase();
    domain == "chatgpt.com" || domain.ends_with(".chatgpt.com") || domain == "openai.com" || domain.ends_with(".openai.com")
}

#[tauri::command]
fn import_cookies(app: AppHandle, cookies: Vec<ImportedCookie>) -> Result<usize, String> {
    if cookies.is_empty() || cookies.len() > 128 {
        return Err("请选择 1 到 128 条 Cookie".to_string());
    }
    if cookies.iter().any(|cookie| {
        cookie.name.is_empty() || cookie.value.len() > 16_384 || !is_openai_cookie_domain(&cookie.domain)
    }) {
        return Err("Cookie 格式无效，或包含非 chatgpt.com / openai.com 域".to_string());
    }
    let window = app
        .get_webview_window(CHAT_WINDOW_LABEL)
        .ok_or_else(|| "请先打开网页登录窗口".to_string())?;
    let imported_count = cookies.len();

    #[cfg(windows)]
    {
        let event_app = app.clone();
        window.with_webview(move |webview| {
            let result = (|| unsafe {
                let core = webview.controller().CoreWebView2().map_err(|error| error.to_string())?;
                let core = core.cast::<ICoreWebView2_2>().map_err(|error| error.to_string())?;
                let manager = core.CookieManager().map_err(|error| error.to_string())?;
                for cookie in cookies {
                    let path = cookie.path.unwrap_or_else(|| "/".to_string());
                    let native = manager.CreateCookie(
                        &HSTRING::from(cookie.name), &HSTRING::from(cookie.value),
                        &HSTRING::from(cookie.domain), &HSTRING::from(path),
                    ).map_err(|error| error.to_string())?;
                    if let Some(expires) = cookie.expiration_date.filter(|value| value.is_finite()) {
                        native.SetExpires(expires).map_err(|error| error.to_string())?;
                    }
                    if let Some(value) = cookie.http_only { native.SetIsHttpOnly(value).map_err(|error| error.to_string())?; }
                    if let Some(value) = cookie.secure { native.SetIsSecure(value).map_err(|error| error.to_string())?; }
                    if let Some(value) = cookie.same_site.as_deref() {
                        let same_site = match value.to_ascii_lowercase().as_str() {
                            "strict" => COREWEBVIEW2_COOKIE_SAME_SITE_KIND_STRICT,
                            "lax" => COREWEBVIEW2_COOKIE_SAME_SITE_KIND_LAX,
                            _ => COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE,
                        };
                        native.SetSameSite(same_site).map_err(|error| error.to_string())?;
                    }
                    manager.AddOrUpdateCookie(&native).map_err(|error| error.to_string())?;
                }
                Ok::<(), String>(())
            })();
            if let Err(error) = result {
                let _ = event_app.emit_to("main", "flight://stream", StreamEvent { kind: "error".to_string(), text: None, conversation_id: None, title: None, error: Some(format!("Cookie 导入失败：{error}")), messages: None });
            }
        }).map_err(|error| error.to_string())?;
    }
    #[cfg(not(windows))]
    return Err("当前平台暂不支持原生 Cookie 导入".to_string());

    window.navigate(Url::parse("https://chatgpt.com/").map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    Ok(imported_count)
}

#[tauri::command]
fn export_cookies(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(CHAT_WINDOW_LABEL)
        .ok_or_else(|| "请先打开网页登录窗口".to_string())?;

    #[cfg(windows)]
    {
        let callback_target = window.clone();
        window
            .with_webview(move |webview| {
                let callback_window = callback_target.clone();
                let setup_window = callback_target.clone();
                let setup = unsafe { (|| -> Result<(), String> {
                    let core = webview.controller().CoreWebView2().map_err(|error| error.to_string())?;
                    let core = core.cast::<ICoreWebView2_2>().map_err(|error| error.to_string())?;
                    let manager = core.CookieManager().map_err(|error| error.to_string())?;
                manager
                    .GetCookies(
                        windows_core::PCWSTR::null(),
                        &GetCookiesCompletedHandler::create(Box::new(move |error_code, cookie_list| {
                            let result = (|| -> Result<Vec<ExportedCookie>, String> {
                                if error_code.is_err() {
                                    return Err(format!("WebView2 读取 Cookie 失败：{error_code:?}"));
                                }
                                let Some(cookie_list) = cookie_list else { return Ok(Vec::new()); };
                                let mut count = 0;
                                cookie_list.Count(&mut count).map_err(|error| error.to_string())?;
                                let mut exported = Vec::with_capacity(count as usize);
                                for index in 0..count {
                                    let cookie = cookie_list.GetValueAtIndex(index).map_err(|error| error.to_string())?;
                                    let mut name = PWSTR::null();
                                    cookie.Name(&mut name).map_err(|error| error.to_string())?;
                                    let mut value = PWSTR::null();
                                    cookie.Value(&mut value).map_err(|error| error.to_string())?;
                                    let mut domain = PWSTR::null();
                                    cookie.Domain(&mut domain).map_err(|error| error.to_string())?;
                                    let domain = take_pwstr(domain);
                                    if !is_openai_cookie_domain(&domain) { continue; }
                                    let mut path = PWSTR::null();
                                    cookie.Path(&mut path).map_err(|error| error.to_string())?;
                                    let mut http_only = BOOL::default();
                                    cookie.IsHttpOnly(&mut http_only).map_err(|error| error.to_string())?;
                                    let mut secure = BOOL::default();
                                    cookie.IsSecure(&mut secure).map_err(|error| error.to_string())?;
                                    let mut is_session = BOOL::default();
                                    cookie.IsSession(&mut is_session).map_err(|error| error.to_string())?;
                                    let mut expires = -1.0;
                                    cookie.Expires(&mut expires).map_err(|error| error.to_string())?;
                                    let mut same_site = COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE;
                                    cookie.SameSite(&mut same_site).map_err(|error| error.to_string())?;
                                    let same_site = match same_site {
                                        COREWEBVIEW2_COOKIE_SAME_SITE_KIND_STRICT => "strict",
                                        COREWEBVIEW2_COOKIE_SAME_SITE_KIND_LAX => "lax",
                                        _ => "none",
                                    }.to_string();
                                    exported.push(ExportedCookie {
                                        name: take_pwstr(name), value: take_pwstr(value), domain,
                                        path: take_pwstr(path),
                                        expiration_date: (!is_session.as_bool() && expires.is_finite() && expires >= 0.0).then_some(expires),
                                        http_only: http_only.as_bool(), secure: secure.as_bool(), same_site,
                                    });
                                }
                                Ok(exported)
                            })();
                            let script = match result {
                                Ok(cookies) => match serde_json::to_string(&cookies) {
                                    Ok(cookies) => format!("window.__flightChat?.completeCookieExport?.({cookies});"),
                                    Err(error) => format!("window.__flightChat?.failCookieExport?.({});", serde_json::to_string(&error.to_string()).unwrap_or_else(|_| "'Cookie 序列化失败'".to_string())),
                                },
                                Err(error) => format!("window.__flightChat?.failCookieExport?.({});", serde_json::to_string(&error).unwrap_or_else(|_| "'Cookie 导出失败'".to_string())),
                            };
                            let _ = callback_window.eval(&script);
                            Ok(())
                        })),
                    )
                    .map_err(|error| error.to_string())?;
                    Ok(())
                })() };
                if let Err(error) = setup {
                    let script = format!("window.__flightChat?.failCookieExport?.({});", serde_json::to_string(&error).unwrap_or_else(|_| "'Cookie 导出失败'".to_string()));
                    let _ = setup_window.eval(&script);
                }
            })
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    #[cfg(not(windows))]
    Err("当前平台暂不支持原生 Cookie 导出".to_string())
}

#[tauri::command]
fn relay_stream_event(app: AppHandle, payload: StreamEvent) -> Result<(), String> {
    app.emit_to("main", "flight://stream", payload)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn enter_flight_mode(app: AppHandle, history_limit: Option<usize>) -> Result<(), String> {
    let history_limit = history_limit.unwrap_or(5);
    if !(1..=100).contains(&history_limit) {
        return Err("历史回显条数需要在 1 到 100 之间".to_string());
    }
    let window = app
        .get_webview_window(CHAT_WINDOW_LABEL)
        .ok_or_else(|| "请先打开网页登录窗口并完成登录".to_string())?;
    window
        .eval(CHATGPT_BRIDGE_SCRIPT)
        .map_err(|error| format!("无法初始化网页消息桥接：{error}"))?;
    window
        .eval(&format!("window.__flightChat?.loadHistory?.({history_limit});"))
        .map_err(|error| format!("无法读取当前会话历史：{error}"))?;
    window.hide().map_err(|error| error.to_string())?;

    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "未找到 Flight 主窗口".to_string())?;
    main.show().map_err(|error| error.to_string())?;
    main.set_focus().map_err(|error| error.to_string())?;
    app.emit_to(
        "main",
        "flight://stream",
        StreamEvent {
            kind: "flight_active".to_string(),
            text: None,
            conversation_id: None,
            title: None,
            error: None,
            messages: None,
        },
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
fn prepare_web_bridge(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(CHAT_WINDOW_LABEL)
        .ok_or_else(|| "请先打开网页登录窗口并完成登录".to_string())?;
    window
        .eval(CHATGPT_BRIDGE_SCRIPT)
        .map_err(|error| format!("无法初始化网页消息桥接：{error}"))
}

#[tauri::command]
fn exit_flight_mode(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(CHAT_WINDOW_LABEL)
        .ok_or_else(|| "网页登录窗口尚未创建".to_string())?;
    window.show().map_err(|error| error.to_string())?;
    window.set_focus().map_err(|error| error.to_string())?;
    // Re-running the bridge resets controls that were disabled while entering Flight.
    window
        .eval(CHATGPT_BRIDGE_SCRIPT)
        .map_err(|error| format!("无法恢复网页控制按钮：{error}"))
}

#[tauri::command]
fn new_conversation(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(CHAT_WINDOW_LABEL)
        .ok_or_else(|| "网页登录窗口尚未创建".to_string())?;
    let url = Url::parse("https://chatgpt.com/").map_err(|error| error.to_string())?;
    window.navigate(url).map_err(|error| error.to_string())
}

#[tauri::command]
fn load_conversation_list(app: AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window(CHAT_WINDOW_LABEL)
        .ok_or_else(|| "网页登录窗口尚未创建".to_string())?;
    window
        .eval("window.__flightChat?.loadConversationList?.();")
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn select_conversation(app: AppHandle, conversation_id: String) -> Result<(), String> {
    let conversation_id = conversation_id.trim();
    if conversation_id.is_empty() || conversation_id.len() > 128 || !conversation_id.chars().all(|character| character.is_ascii_alphanumeric() || character == '-') {
        return Err("会话标识无效".to_string());
    }
    let window = app
        .get_webview_window(CHAT_WINDOW_LABEL)
        .ok_or_else(|| "网页登录窗口尚未创建".to_string())?;
    let url = Url::parse(&format!("https://chatgpt.com/c/{conversation_id}"))
        .map_err(|error| error.to_string())?;
    window.navigate(url).map_err(|error| error.to_string())
}

#[tauri::command]
fn send_message(app: AppHandle, text: String, image: Option<ImageAttachment>, preserve_composer: Option<bool>) -> Result<(), String> {
    let message = text.trim();
    if message.is_empty() && image.is_none() {
        return Err("消息或图片不能为空".to_string());
    }
    if message.chars().count() > 40_000 {
        return Err("单条消息不能超过 40,000 个字符".to_string());
    }
    if let Some(image) = &image {
        if image.name.len() > 240 || !image.data_url.starts_with("data:image/") || image.data_url.len() > 14 * 1024 * 1024 {
            return Err("图片格式无效或超过 10MB".to_string());
        }
    }

    let window = app
        .get_webview_window(CHAT_WINDOW_LABEL)
        .ok_or_else(|| "网页登录窗口尚未创建".to_string())?;
    let message_json = serde_json::to_string(message).map_err(|error| error.to_string())?;
    let image_json = serde_json::to_string(&image).map_err(|error| error.to_string())?;
    window
        .eval(&format!(r#"
          (() => {{
            const bridge = window.__flightChat;
            if (!bridge?.send) {{
              window.__TAURI__?.core?.invoke('relay_stream_event', {{ payload: {{ kind: 'error', error: '网页桥接尚未就绪，请先执行测试发送。' }} }});
              return;
            }}
            Promise.resolve(bridge.send({message_json}, {image_json}, {})).catch((error) => {{
              window.__TAURI__?.core?.invoke('relay_stream_event', {{ payload: {{ kind: 'error', error: String(error?.message || error) }} }});
            }});
          }})();
        "#, preserve_composer.unwrap_or(false)))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn update_command_draft(app: AppHandle, text: String) -> Result<(), String> {
    let draft = text.trim();
    if !(draft.starts_with('@') || draft.starts_with('/')) || draft.chars().count() > 240 {
        return Err("命令草稿必须以 @ 或 / 开头，且不超过 240 个字符".to_string());
    }
    let window = app.get_webview_window(CHAT_WINDOW_LABEL).ok_or_else(|| "网页登录窗口尚未创建".to_string())?;
    let draft_json = serde_json::to_string(draft).map_err(|error| error.to_string())?;
    window.eval(&format!("window.__flightChat?.updateCommandDraft?.({draft_json});"))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn select_command_option(app: AppHandle, text: String) -> Result<(), String> {
    let label = text.trim();
    if label.is_empty() || label.chars().count() > 240 { return Err("命令候选内容无效".to_string()); }
    let window = app.get_webview_window(CHAT_WINDOW_LABEL).ok_or_else(|| "网页登录窗口尚未创建".to_string())?;
    let label_json = serde_json::to_string(label).map_err(|error| error.to_string())?;
    window.eval(&format!("window.__flightChat?.selectCommandOption?.({label_json});"))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn update_command_suffix(app: AppHandle, text: String) -> Result<(), String> {
    if text.chars().count() > 40_000 { return Err("命令后的文字不能超过 40,000 个字符".to_string()); }
    let window = app.get_webview_window(CHAT_WINDOW_LABEL).ok_or_else(|| "网页登录窗口尚未创建".to_string())?;
    let text_json = serde_json::to_string(&text).map_err(|error| error.to_string())?;
    window.eval(&format!("window.__flightChat?.updateCommandSuffix?.({text_json});"))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn clear_command_selection(app: AppHandle, text: String) -> Result<(), String> {
    if text.chars().count() > 40_000 { return Err("命令后的文字不能超过 40,000 个字符".to_string()); }
    let window = app.get_webview_window(CHAT_WINDOW_LABEL).ok_or_else(|| "网页登录窗口尚未创建".to_string())?;
    let text_json = serde_json::to_string(&text).map_err(|error| error.to_string())?;
    window.eval(&format!("window.__flightChat?.clearCommandSelection?.({text_json});"))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn set_split_view(app: AppHandle, enabled: bool) -> Result<(), String> {
    let epoch = SPLIT_VIEW_EPOCH.fetch_add(1, Ordering::SeqCst) + 1;
    let main = app.get_webview_window("main").ok_or_else(|| "未找到 Flight 主窗口".to_string())?;
    let chat = app.get_webview_window(CHAT_WINDOW_LABEL).ok_or_else(|| "网页登录窗口尚未创建".to_string())?;
    if !enabled {
        // Restore Flight first. A large ChatGPT page can keep its WebView
        // thread busy; hiding it must not keep the user from returning to the
        // main window or leave this command pending.
        main.show().map_err(|error| error.to_string())?;
        main.set_size(PhysicalSize::new(1240, 840)).map_err(|error| error.to_string())?;
        main.center().map_err(|error| error.to_string())?;
        main.set_focus().map_err(|error| error.to_string())?;
        tauri::async_runtime::spawn(async move {
            if SPLIT_VIEW_EPOCH.load(Ordering::SeqCst) == epoch {
                let _ = chat.hide();
            }
        });
        return Ok(());
    }

    let monitor = main.current_monitor().map_err(|error| error.to_string())?
        .or(main.primary_monitor().map_err(|error| error.to_string())?)
        .ok_or_else(|| "未找到可用显示器".to_string())?;
    let position = monitor.position();
    let size = monitor.size();
    let left_width = (size.width / 2).max(520);
    let right_width = size.width.saturating_sub(left_width).max(520);
    let height = size.height.saturating_sub(36).max(620);
    main.show().map_err(|error| error.to_string())?;
    main.set_position(PhysicalPosition::new(position.x, position.y)).map_err(|error| error.to_string())?;
    main.set_size(PhysicalSize::new(left_width, height)).map_err(|error| error.to_string())?;
    chat.show().map_err(|error| error.to_string())?;
    chat.set_position(PhysicalPosition::new(position.x + left_width as i32, position.y)).map_err(|error| error.to_string())?;
    chat.set_size(PhysicalSize::new(right_width, height)).map_err(|error| error.to_string())?;
    main.set_focus().map_err(|error| error.to_string())?;
    // A delayed hide from the prior close can reach WebView2 after the first
    // show. Reassert visibility while this exact split request is current.
    let chat_for_reveal = chat.clone();
    tauri::async_runtime::spawn(async move {
        for delay in [150_u64, 600, 1200] {
            std::thread::sleep(std::time::Duration::from_millis(delay));
            if SPLIT_VIEW_EPOCH.load(Ordering::SeqCst) != epoch { return; }
            let _ = chat_for_reveal.show();
        }
    });
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            enter_flight_mode,
            prepare_web_bridge,
            exit_flight_mode,
            new_conversation,
            load_conversation_list,
            select_conversation,
            send_message,
            update_command_draft,
            select_command_option,
            update_command_suffix,
            clear_command_selection,
            set_split_view,
            relay_stream_event,
            import_cookies,
            export_cookies
        ])
        .run(tauri::generate_context!())
        .expect("error while running Flight Chat");
}

// This script runs inside the authenticated ChatGPT webview. It leaves the page's
// own fetch response untouched and reads a clone solely to relay user-visible SSE deltas.
const CHATGPT_BRIDGE_SCRIPT: &str = r#"
(() => {
  if (window.__flightChat) {
    // A client-side route switch can replace the page subtree (including our
    // floating controls) without replacing window. Reinstall missing controls.
    window.__flightChat.installFlightModeButton?.();
    window.__flightChat.installPageTrimButton?.();
    window.__flightChat.installCookieImportButton?.();
    window.__flightChat.installCookieExportButton?.();
    const existingButton = document.getElementById('flight-enter-mode');
    if (existingButton) { existingButton.disabled = false; existingButton.textContent = '进入飞行模式'; }
    return;
  }

  const relay = (payload) => {
    const invoke = window.__TAURI__?.core?.invoke || window.__TAURI_INTERNALS__?.invoke;
    const result = invoke?.('relay_stream_event', { payload });
    result?.catch?.(() => {});
  };

  const relayTest = (payload) => {
    const invoke = window.__TAURI__?.core?.invoke || window.__TAURI_INTERNALS__?.invoke;
    if (!invoke) return Promise.reject(new Error('网页未获得 Tauri 调用接口'));
    return Promise.resolve(invoke('relay_stream_event', { payload }));
  };

  const splitSseBlocks = (source) => source.replace(/\r\n/g, '\n').split('\n\n');

  const emitPatch = (patch) => {
    if (!patch || typeof patch !== 'object') return;
    if (patch.p === '/message/content/parts/0' && patch.o === 'append' && typeof patch.v === 'string') {
      window.__flightNetworkDeltaObserved = true;
      relay({ kind: 'delta', text: patch.v });
    }
  };

  const inspectPayload = (payload, eventName) => {
    if (!payload || typeof payload !== 'object') return;

    if (payload.conversation_id) {
      relay({ kind: 'conversation', conversationId: payload.conversation_id });
    }
    if (payload.type === 'title_generation' && payload.title) {
      relay({ kind: 'title', title: payload.title, conversationId: payload.conversation_id });
    }
    if (payload.type === 'message_stream_complete') {
      relay({ kind: 'complete', conversationId: payload.conversation_id });
    }
    if (payload.error) {
      relay({ kind: 'error', error: typeof payload.error === 'string' ? payload.error : '网页端返回异常' });
    }

    const message = payload.v?.message;
    if (message?.author?.role === 'assistant' && message.channel === 'final') {
      relay({ kind: 'assistant_start', conversationId: payload.v?.conversation_id || payload.conversation_id });
    }

    if (eventName === 'delta') {
      if (typeof payload.v === 'string') {
        window.__flightNetworkDeltaObserved = true;
        relay({ kind: 'delta', text: payload.v });
      }
      if (Array.isArray(payload.v)) payload.v.forEach(emitPatch);
    }
  };

  const handleBlock = (block) => {
    const lines = block.split('\n');
    const eventName = lines.find((line) => line.startsWith('event:'))?.slice(6).trim() || '';
    const data = lines.filter((line) => line.startsWith('data:')).map((line) => line.slice(5).trim()).join('\n');
    if (!data) return;
    if (data === '[DONE]') {
      relay({ kind: 'complete' });
      return;
    }
    try {
      inspectPayload(JSON.parse(data), eventName);
    } catch (_) {
      // Some framing records are intentionally non-JSON, such as delta_encoding.
    }
  };

  const observeResponse = async (response) => {
    if (!response.body) return;
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const blocks = splitSseBlocks(buffer);
        buffer = blocks.pop() || '';
        blocks.forEach(handleBlock);
      }
      buffer += decoder.decode();
      if (buffer.trim()) handleBlock(buffer);
    } catch (error) {
      relay({ kind: 'error', error: '无法读取网页回复流' });
    }
  };

  const nativeFetch = window.fetch.bind(window);
  // The site already requests the full conversation when a history item is
  // opened. Keep only its newest useful turns for Flight instead of issuing a
  // second, equally large request when the user enters flight mode.
  const conversationHistoryCache = new Map();
  const conversationHistoryPending = new Map();
  let conversationListCache = [];
  const cacheConversationResponse = (conversationId, response) => {
    const task = response.json()
      .then((conversation) => {
        const messages = historyFromApi(conversation).slice(-50);
        conversationHistoryCache.set(conversationId, messages);
        // The conversation response is often ready before ChatGPT's dynamic
        // route chunks finish loading. Hand Flight its newest two messages now
        // so the user can leave the skeleton screen immediately.
        const newestMessages = messages.slice(-2);
        if (newestMessages.length) {
          relay({ kind: 'history', conversationId, messages: newestMessages });
        }
        while (conversationHistoryCache.size > 4) {
          conversationHistoryCache.delete(conversationHistoryCache.keys().next().value);
        }
      })
      .catch(() => {})
      .finally(() => conversationHistoryPending.delete(conversationId));
    conversationHistoryPending.set(conversationId, task);
  };
  const cacheConversationListResponse = (response) => {
    void response.json().then((payload) => {
      const conversations = conversationListFromApi(payload);
      if (!conversations.length) return;
      conversationListCache = conversations;
      relay({ kind: 'conversation_list', text: JSON.stringify(conversations) });
    }).catch(() => {});
  };
  window.fetch = async (...args) => {
    const response = await nativeFetch(...args);
    const input = args[0];
    const url = typeof input === 'string' ? input : input?.url || '';
    const isFastConversationStream = url.includes('/backend-api/f/conversation') && !url.includes('/prepare');
    const isResumeStream = /\/backend-api\/conversation\/[^/]+\/resume(?:\?|$)/.test(url);
    const conversationMatch = /\/backend-api\/conversation\/([^/?#]+)(?:\?|$)/.exec(url);
    const isConversationList = /\/backend-api\/conversations(?:\?|$)/.test(url);
    if (conversationMatch && response.ok) {
      cacheConversationResponse(conversationMatch[1], response.clone());
    }
    if (isConversationList && response.ok) cacheConversationListResponse(response.clone());
    if (isFastConversationStream || isResumeStream) {
      relay({ kind: 'probe', text: `已捕获回复流：${isResumeStream ? 'conversation/resume' : 'f/conversation'}` });
      void observeResponse(response.clone());
    }
    return response;
  };

  const assistantSelectors = [
    '[data-message-author-role="assistant"]',
    'article[data-testid^="conversation-turn-"] [data-message-author-role="assistant"]',
    'article[data-testid^="conversation-turn-"]',
    '[data-testid*="conversation-turn"]',
    '[data-testid*="message"] [data-message-author-role="assistant"]'
  ];
  const latestAssistantText = () => {
    for (const selector of assistantSelectors) {
      const nodes = [...document.querySelectorAll(selector)].filter((node) => node.offsetParent !== null);
      const last = nodes.at(-1);
      const text = last?.innerText?.trim();
      if (text) return text;
    }
    return '';
  };

  const asMarkdownFromDom = (node) => {
    let text = node?.innerText?.trim() || '';
    for (const pre of node?.querySelectorAll?.('pre') || []) {
      const code = pre.querySelector('code');
      const source = code?.innerText?.trim();
      if (!source || !text.includes(source)) continue;
      const language = [...code.classList].find((name) => name.startsWith('language-'))?.slice(9) || '';
      text = text.replace(source, `\`\`\`${language}\n${source}\n\`\`\``);
    }
    return text;
  };

  const historyFromDom = () => [...document.querySelectorAll('[data-message-author-role="user"], [data-message-author-role="assistant"]')]
    .filter((node) => node.offsetParent !== null)
    .map((node) => ({ role: node.getAttribute('data-message-author-role'), text: asMarkdownFromDom(node) }))
    .filter((message) => message.text);

  const conversationMessageNodes = () => [...document.querySelectorAll('[data-message-author-role="user"], [data-message-author-role="assistant"]')]
    .filter((node) => node.isConnected && !node.closest('#flight-page-trim'));

  // The app has already obtained the complete history through the API. Remove
  // older rendered turns from the webpage itself so its layout/paint work does
  // not keep growing for a long conversation. We deliberately remove only the
  // message roots, never a shared parent container owned by ChatGPT's React.
  const trimConversationDom = (keep = 2) => {
    const safeKeep = Math.min(Math.max(Number(keep) || 2, 1), 20);
    const nodes = conversationMessageNodes();
    const staleNodes = nodes.slice(0, Math.max(0, nodes.length - safeKeep));
    for (const node of staleNodes) node.remove();
    return staleNodes.length;
  };

  const historyFromApi = (conversation) => Object.values(conversation?.mapping || {})
    .map((node) => {
      const message = node?.message;
      const role = message?.author?.role;
      const parts = message?.content?.parts;
      const text = Array.isArray(parts)
        ? parts.filter((part) => typeof part === 'string').join('\n').trim()
        : typeof message?.content?.text === 'string' ? message.content.text.trim() : '';
      return { role, text, createdAt: message?.create_time || node?.create_time || 0 };
    })
    .filter((message) => (message.role === 'user' || message.role === 'assistant') && message.text)
    .sort((a, b) => a.createdAt - b.createdAt)
    .map(({ role, text }) => ({ role, text }));

  const conversationListFromApi = (payload) => {
    const containers = [payload, payload?.data, payload?.data?.data, payload?.result, payload?.result?.data]
      .filter((container) => container && typeof container === 'object');
    const rows = containers.flatMap((container) => Array.isArray(container) ? [container] : [container.items, container.conversations, container.results])
      .find((candidate) => Array.isArray(candidate)) || [];
    return rows
      .map((row) => ({
        id: typeof row?.id === 'string' ? row.id : typeof row?.conversation_id === 'string' ? row.conversation_id : '',
        title: typeof row?.title === 'string' && row.title.trim() ? row.title.trim() : typeof row?.metadata?.title === 'string' && row.metadata.title.trim() ? row.metadata.title.trim() : '未命名会话',
        updatedAt: Number(row?.update_time || row?.updated_at || row?.create_time || 0)
      }))
      .filter((row) => /^[A-Za-z0-9-]{1,128}$/.test(row.id))
      .sort((left, right) => right.updatedAt - left.updatedAt);
  };

  const loadConversationList = async () => {
    try {
      if (conversationListCache.length) {
        relay({ kind: 'conversation_list', text: JSON.stringify(conversationListCache) });
        return;
      }
      const urls = [
        '/backend-api/conversations?offset=0&limit=28&order=updated&is_archived=false&is_starred=false',
        '/backend-api/conversations?offset=0&limit=100&order=updated&is_archived=false&is_starred=false'
      ];
      for (const url of urls) {
        const response = await nativeFetch(url, { credentials: 'include' });
        if (!response.ok) continue;
        const conversations = conversationListFromApi(await response.json());
        if (!conversations.length) continue;
        conversationListCache = conversations;
        relay({ kind: 'conversation_list', text: JSON.stringify(conversations) });
        return;
      }
      throw new Error('响应中未找到会话标题');
    } catch (error) {
      relay({ kind: 'conversation_list_error', error: `无法读取历史会话列表：${String(error?.message || error)}` });
    }
  };

  const loadHistory = async (limit = 5) => {
    const conversationId = /^\/c\/([^/?#]+)/.exec(location.pathname)?.[1];
    if (!conversationId) return;
    const historyLimit = Number.isInteger(limit) ? Math.min(Math.max(limit, 1), 100) : 5;
    const pendingHistory = conversationHistoryPending.get(conversationId);
    if (pendingHistory) await pendingHistory;
    let messages = conversationHistoryCache.get(conversationId) || [];
    // A fallback is needed if the page was opened before the bridge was
    // installed or ChatGPT changes its detail endpoint. It intentionally reads
    // the already-rendered page instead of starting another large API request.
    if (!messages.length) messages = historyFromDom();
    // Keep the expensive Markdown rendering and IPC payload bounded. The
    // conversation remains intact in the hidden webpage, while Flight only
    // restores the newest messages requested by the user.
    messages = messages.slice(-historyLimit);
    relay({ kind: 'history', conversationId, messages });
    // Once Flight has the relevant history, release the hidden webpage from
    // rendering older turns as well. A page reload restores the full website.
    setTimeout(() => trimConversationDom(2), 0);
  };

  const openConversation = async (conversationId, limit = 2) => {
    if (!/^[A-Za-z0-9-]{1,128}$/.test(conversationId || '')) throw new Error('会话标识无效');
    relay({ kind: 'conversation_loading', conversationId });
    let messages = conversationHistoryCache.get(conversationId) || [];
    const pendingHistory = conversationHistoryPending.get(conversationId);
    if (!messages.length && pendingHistory) await pendingHistory;
    messages = conversationHistoryCache.get(conversationId) || [];
    if (!messages.length) {
      const response = await nativeFetch(`/backend-api/conversation/${encodeURIComponent(conversationId)}`, { credentials: 'include' });
      if (!response.ok) throw new Error(`会话请求失败 (${response.status})`);
      messages = historyFromApi(await response.json()).slice(-50);
      conversationHistoryCache.set(conversationId, messages);
    }
    relay({ kind: 'history', conversationId, messages: messages.slice(-Math.min(Math.max(Number(limit) || 2, 1), 50)) });
    // Let the actual ChatGPT page switch in the background so later messages
    // use the selected conversation's real composer and backend context.
    location.assign(`/c/${encodeURIComponent(conversationId)}`);
  };

  let latestDomText = latestAssistantText();
  let domCompleteTimer;
  let observedConversationId = /^\/c\/([^/?#]+)/.exec(location.pathname)?.[1] || '';
  let pageTrimTimer;
  const schedulePageTrim = () => {
    const conversationId = /^\/c\/([^/?#]+)/.exec(location.pathname)?.[1] || '';
    if (!conversationId) return;
    clearTimeout(pageTrimTimer);
    // ChatGPT adds a long history through several DOM batches. Resetting this
    // debounce on each batch lets it finish loading, then removes old roots
    // before the page stays interactive with a giant conversation tree.
    pageTrimTimer = setTimeout(() => {
      const currentId = /^\/c\/([^/?#]+)/.exec(location.pathname)?.[1] || '';
      if (currentId === conversationId) trimConversationDom(2);
    }, 650);
  };

  // ChatGPT changes history items through client-side navigation. Polling the
  // route is more reliable than depending on its private router events.
  setInterval(() => {
    if (!document.getElementById('flight-enter-mode')) installFlightModeButton();
    if (!document.getElementById('flight-page-trim')) installPageTrimButton();
    if (!document.getElementById('flight-cookie-import')) installCookieImportButton();
    if (!document.getElementById('flight-cookie-export')) installCookieExportButton();
    const nextConversationId = /^\/c\/([^/?#]+)/.exec(location.pathname)?.[1] || '';
    if (nextConversationId && nextConversationId !== observedConversationId) {
      observedConversationId = nextConversationId;
      schedulePageTrim();
    }
  }, 250);

  const inspectDomReply = () => {
    if (window.__flightNetworkDeltaObserved || Date.now() < (window.__flightDomFallbackNotBefore || 0)) return;
    const next = latestAssistantText();
    if (!next || next === latestDomText) return;
    const delta = next.startsWith(latestDomText) ? next.slice(latestDomText.length) : next;
    latestDomText = next;
    if (delta) relay({ kind: 'delta', text: delta });
    clearTimeout(domCompleteTimer);
    domCompleteTimer = setTimeout(() => relay({ kind: 'complete' }), 900);
  };
  const domObserver = new MutationObserver(() => {
    clearTimeout(window.__flightDomDebounce);
    window.__flightDomDebounce = setTimeout(inspectDomReply, 90);
    schedulePageTrim();
  });
  domObserver.observe(document.documentElement, { subtree: true, childList: true, characterData: true });

  // This deliberately bypasses network parsing. It proves that code running in
  // the authenticated webview can send an event all the way back to Flight.
  const installFlightModeButton = () => {
    if (document.getElementById('flight-enter-mode')) return;
    if (!document.body) {
      document.addEventListener('DOMContentLoaded', installFlightModeButton, { once: true });
      return;
    }
    const button = document.createElement('button');
    button.id = 'flight-enter-mode';
    button.type = 'button';
    button.textContent = '进入飞行模式';
    button.title = '隐藏网页，进入 Flight Chat';
    Object.assign(button.style, {
      position: 'fixed', right: '22px', bottom: '22px', zIndex: '2147483647',
      border: '0', borderRadius: '999px', padding: '10px 15px', cursor: 'pointer',
      background: '#5a351e', color: '#fffaf3', font: '600 13px system-ui, sans-serif',
      boxShadow: '0 8px 24px rgba(50, 29, 14, .26)'
    });
    button.addEventListener('click', async () => {
      button.disabled = true;
      button.textContent = '正在进入…';
      try {
        await relayTest({ kind: 'enter_flight' });
      } catch (error) {
        const message = String(error?.message || error).replace(/\s+/g, ' ').slice(0, 42);
        button.textContent = `桥接失败：${message}`;
        setTimeout(() => { button.textContent = '进入飞行模式'; button.disabled = false; }, 5000);
      }
    });
    document.body.appendChild(button);
  };

  const installPageTrimButton = () => {
    if (document.getElementById('flight-page-trim')) return;
    if (!document.body) {
      document.addEventListener('DOMContentLoaded', installPageTrimButton, { once: true });
      return;
    }
    const button = document.createElement('button');
    button.id = 'flight-page-trim';
    button.type = 'button';
    button.textContent = '精简页面';
    button.title = '移除较早消息的网页 DOM，仅保留最新两条；刷新网页可恢复';
    Object.assign(button.style, {
      position: 'fixed', right: '22px', bottom: '166px', zIndex: '2147483647',
      border: '1px solid #5a351e', borderRadius: '999px', padding: '9px 14px', cursor: 'pointer',
      background: '#fffaf3', color: '#5a351e', font: '600 13px system-ui, sans-serif'
    });
    button.addEventListener('click', () => {
      button.disabled = true;
      const removed = trimConversationDom(2);
      button.textContent = removed ? `已精简 ${removed} 条` : '无需精简';
      setTimeout(() => { button.textContent = '精简页面'; button.disabled = false; }, 1800);
    });
    document.body.appendChild(button);
  };

  const installCookieImportButton = () => {
    if (document.getElementById('flight-cookie-import')) return;
    if (!document.body) {
      document.addEventListener('DOMContentLoaded', installCookieImportButton, { once: true });
      return;
    }
    const button = document.createElement('button');
    button.id = 'flight-cookie-import';
    button.type = 'button';
    button.textContent = '导入 Cookie';
    button.title = '粘贴 Cookie-Editor 导出的 JSON';
    Object.assign(button.style, {
      position: 'fixed', right: '22px', bottom: '70px', zIndex: '2147483647',
      border: '1px solid #5a351e', borderRadius: '999px', padding: '9px 14px', cursor: 'pointer',
      background: '#fffaf3', color: '#5a351e', font: '600 13px system-ui, sans-serif'
    });
    button.addEventListener('click', () => {
      const mask = document.createElement('div');
      Object.assign(mask.style, { position: 'fixed', inset: '0', zIndex: '2147483647', display: 'grid', placeItems: 'center', background: 'rgba(38, 24, 14, .34)' });
      const panel = document.createElement('form');
      Object.assign(panel.style, { width: 'min(560px, calc(100vw - 40px))', padding: '22px', background: '#fffaf3', borderRadius: '14px', boxShadow: '0 18px 52px rgba(0,0,0,.28)', color: '#382417', font: '14px system-ui, sans-serif' });
      panel.innerHTML = '<strong style="font-size:17px">导入 Cookie</strong><p style="margin:8px 0 14px;color:#6a5648">粘贴 Cookie-Editor 导出的 JSON 数组，仅接受 chatgpt.com / openai.com Cookie。</p><textarea autofocus placeholder="[ { &quot;name&quot;: &quot;…&quot;, &quot;value&quot;: &quot;…&quot;, &quot;domain&quot;: &quot;.chatgpt.com&quot; } ]" style="display:block;width:100%;height:190px;resize:vertical;padding:10px;border:1px solid #d4c4b3;border-radius:8px;font:12px ui-monospace,monospace"></textarea><div style="display:flex;justify-content:flex-end;gap:10px;margin-top:14px"><button type="button" data-cancel style="border:0;background:transparent;padding:8px 12px;cursor:pointer">取消</button><button type="submit" style="border:0;background:#5a351e;color:#fffaf3;border-radius:7px;padding:9px 14px;cursor:pointer">导入并刷新</button></div><p data-status style="min-height:18px;margin:10px 0 0;color:#a13e31"></p>';
      const textarea = panel.querySelector('textarea');
      const status = panel.querySelector('[data-status]');
      panel.querySelector('[data-cancel]').addEventListener('click', () => mask.remove());
      panel.addEventListener('submit', async (event) => {
        event.preventDefault();
        try {
          const cookies = JSON.parse(textarea.value);
          if (!Array.isArray(cookies)) throw new Error('导出内容必须是 JSON 数组');
          status.textContent = '正在写入 Cookie…';
          const invoke = window.__TAURI__?.core?.invoke || window.__TAURI_INTERNALS__?.invoke;
          if (!invoke) throw new Error('网页未获得 Tauri 调用接口');
          const count = await invoke('import_cookies', { cookies });
          status.style.color = '#34723e';
          status.textContent = `已导入 ${count} 条，正在刷新网页…`;
          setTimeout(() => mask.remove(), 800);
        } catch (error) {
          status.textContent = String(error?.message || error);
        }
      });
      mask.appendChild(panel);
      document.body.appendChild(mask);
      textarea.focus();
    });
    document.body.appendChild(button);
  };

  const installCookieExportButton = () => {
    if (document.getElementById('flight-cookie-export')) return;
    if (!document.body) {
      document.addEventListener('DOMContentLoaded', installCookieExportButton, { once: true });
      return;
    }
    const button = document.createElement('button');
    button.id = 'flight-cookie-export';
    button.type = 'button';
    button.textContent = '导出 Cookie';
    button.title = '导出当前网页会话的 Cookie-Editor JSON';
    Object.assign(button.style, {
      position: 'fixed', right: '22px', bottom: '118px', zIndex: '2147483647',
      border: '1px solid #5a351e', borderRadius: '999px', padding: '9px 14px', cursor: 'pointer',
      background: '#fffaf3', color: '#5a351e', font: '600 13px system-ui, sans-serif'
    });
    button.addEventListener('click', async () => {
      button.disabled = true;
      button.textContent = '正在导出…';
      try {
        const invoke = window.__TAURI__?.core?.invoke || window.__TAURI_INTERNALS__?.invoke;
        if (!invoke) throw new Error('网页未获得 Tauri 调用接口');
        await invoke('export_cookies');
      } catch (error) {
        failCookieExport(String(error?.message || error));
      }
    });
    document.body.appendChild(button);
  };

  const copyText = async (text) => {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return;
    }
    const textarea = document.createElement('textarea');
    textarea.value = text;
    textarea.style.cssText = 'position:fixed;left:-9999px;top:0';
    document.body.appendChild(textarea);
    textarea.select();
    const copied = document.execCommand('copy');
    textarea.remove();
    if (!copied) throw new Error('浏览器未允许写入剪贴板');
  };

  const completeCookieExport = async (cookies) => {
    const button = document.getElementById('flight-cookie-export');
    try {
      await copyText(JSON.stringify(cookies, null, 2));
      if (button) {
        button.textContent = `已复制 ${cookies.length} 条`;
        setTimeout(() => { button.textContent = '导出 Cookie'; button.disabled = false; }, 1800);
      }
    } catch (error) {
      failCookieExport(String(error?.message || error));
    }
  };

  const failCookieExport = (message) => {
    const button = document.getElementById('flight-cookie-export');
    if (!button) return;
    button.textContent = '导出失败';
    button.title = String(message);
    setTimeout(() => { button.textContent = '导出 Cookie'; button.disabled = false; }, 3000);
  };

  const visible = (element) => Boolean(element && (element.offsetWidth || element.offsetHeight || element.getClientRects().length));
  const findComposer = () => {
    const selectors = [
      'textarea[data-id="root"]',
      'textarea',
      '[contenteditable="true"][data-id="root"]',
      '[contenteditable="true"][role="textbox"]',
      '[contenteditable="true"]'
    ];
    for (const selector of selectors) {
      const element = [...document.querySelectorAll(selector)].find(visible);
      if (element) return element;
    }
    return null;
  };

  const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));
  const normalizedCommandText = (value) => String(value || '').replace(/\s+/g, ' ').trim();

  const commandOptionElements = () => {
    const selectors = [
      '[role="listbox"] [role="option"]',
      '[role="menu"] [role="menuitem"]',
      '[data-radix-popper-content-wrapper] [role="option"]',
      '[data-radix-popper-content-wrapper] button',
      '[data-testid*="suggestion"] button'
    ];
    const composer = findComposer();
    const composerRect = composer?.getBoundingClientRect();
    const nearbyRows = composerRect ? [...document.querySelectorAll('button, [role="button"], [role="menuitem"], [role="option"], div')]
      .filter((element) => {
        const rect = element.getBoundingClientRect();
        const isBelow = rect.top >= composerRect.bottom - 8 && rect.top <= composerRect.bottom + 720;
        const isAbove = rect.bottom <= composerRect.top + 8 && rect.bottom >= composerRect.top - 720;
        return (isBelow || isAbove) && rect.height >= 26 && rect.height <= 100
          && rect.width >= 120 && rect.left >= composerRect.left - 90 && rect.left <= composerRect.right + 90;
      }) : [];
    const seen = new Set();
    return [...selectors.flatMap((selector) => [...document.querySelectorAll(selector)]), ...nearbyRows]
      .filter((element) => visible(element) && !element.disabled && element.innerText?.trim())
      .filter((element) => {
        const key = normalizedCommandText(element.innerText);
        if (seen.has(key)) return false;
        seen.add(key);
        return key.length <= 220;
      })
      .sort((left, right) => left.getBoundingClientRect().top - right.getBoundingClientRect().top)
      .slice(0, 12);
  };

  const relayCommandOptions = () => {
    const options = commandOptionElements().map((element) => normalizedCommandText(element.innerText));
    relay({ kind: 'command_suggestions', text: JSON.stringify(options) });
  };

  const updateCommandDraft = async (text) => {
    const composer = findComposer();
    if (!composer) throw new Error('未找到 ChatGPT 输入框');
    composer.focus();
    if (composer instanceof HTMLTextAreaElement) {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set;
      setter?.call(composer, text);
      composer.dispatchEvent(new Event('input', { bubbles: true }));
    } else {
      document.execCommand('selectAll', false);
      document.execCommand('insertText', false, text);
      composer.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: text }));
    }
    await sleep(90);
    relayCommandOptions();
    setTimeout(relayCommandOptions, 260);
  };

  const selectCommandOption = (label) => {
    const wanted = normalizedCommandText(label);
    const option = commandOptionElements().find((element) => normalizedCommandText(element.innerText) === wanted);
    if (!option) throw new Error('网页命令候选已更新，请重新选择');
    option.scrollIntoView({ block: 'nearest' });
    const targets = [option, ...option.querySelectorAll('*')];
    for (let parent = option.parentElement, depth = 0; parent && depth < 4; parent = parent.parentElement, depth += 1) targets.push(parent);
    const rect = option.getBoundingClientRect();
    for (const target of targets) {
      target.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true, clientX: rect.left + rect.width / 2, clientY: rect.top + rect.height / 2 }));
      target.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true, button: 0, buttons: 1, clientX: rect.left + rect.width / 2, clientY: rect.top + rect.height / 2 }));
      target.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, cancelable: true, clientX: rect.left + rect.width / 2, clientY: rect.top + rect.height / 2 }));
      target.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, cancelable: true, button: 0, clientX: rect.left + rect.width / 2, clientY: rect.top + rect.height / 2 }));
      target.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, button: 0, clientX: rect.left + rect.width / 2, clientY: rect.top + rect.height / 2 }));
    }
    window.__flightCommandSuffix = '';
  };

  const selectTrailingText = (root, count) => {
    if (!count) return false;
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    const nodes = [];
    while (walker.nextNode()) nodes.push(walker.currentNode);
    const end = nodes.at(-1);
    if (!end) return false;
    let remaining = count;
    for (let index = nodes.length - 1; index >= 0; index -= 1) {
      const node = nodes[index];
      if (remaining > node.data.length) { remaining -= node.data.length; continue; }
      const range = document.createRange();
      range.setStart(node, node.data.length - remaining);
      range.setEnd(end, end.data.length);
      const selection = window.getSelection();
      selection?.removeAllRanges();
      selection?.addRange(range);
      return true;
    }
    return false;
  };

  const updateCommandSuffix = (text) => {
    const composer = findComposer();
    if (!composer) throw new Error('未找到 ChatGPT 输入框');
    const previous = window.__flightCommandSuffix || '';
    let commonLength = 0;
    while (commonLength < previous.length && commonLength < text.length && previous[commonLength] === text[commonLength]) commonLength += 1;
    const removed = previous.slice(commonLength);
    const addition = text.slice(commonLength);
    if (!removed && !addition) return;
    composer.focus();
    if (composer instanceof HTMLTextAreaElement) {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set;
      const current = composer.value;
      setter?.call(composer, `${current.slice(0, Math.max(0, current.length - removed.length))}${addition}`);
      composer.dispatchEvent(new Event('input', { bubbles: true }));
    } else {
      if (removed && selectTrailingText(composer, removed.length)) {
        document.execCommand('delete', false);
        composer.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteContentBackward', data: null }));
      }
      if (addition) {
        const range = document.createRange();
        range.selectNodeContents(composer);
        range.collapse(false);
        const selection = window.getSelection();
        selection?.removeAllRanges();
        selection?.addRange(range);
        document.execCommand('insertText', false, addition);
        composer.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: addition }));
      }
    }
    window.__flightCommandSuffix = text;
  };

  const clearCommandSelection = (text) => {
    const composer = findComposer();
    if (!composer) throw new Error('未找到 ChatGPT 输入框');
    composer.focus();
    if (composer instanceof HTMLTextAreaElement) {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set;
      setter?.call(composer, text);
      composer.dispatchEvent(new Event('input', { bubbles: true }));
    } else {
      document.execCommand('selectAll', false);
      document.execCommand('insertText', false, text);
      composer.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: text }));
    }
    window.__flightCommandSuffix = '';
  };

  const attachImage = async (composer, image) => {
    const matched = /^data:([^;,]+);base64,(.+)$/i.exec(image.dataUrl || '');
    if (!matched) throw new Error('图片数据格式无效');
    const binary = atob(matched[2]);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
    const file = new File([new Blob([bytes], { type: matched[1] })], image.name || 'clipboard-image.png', { type: matched[1] });
    const clipboard = new DataTransfer();
    clipboard.items.add(file);
    composer.focus();
    composer.dispatchEvent(new ClipboardEvent('paste', {
      bubbles: true, cancelable: true, composed: true, clipboardData: clipboard
    }));
    await sleep(420);
  };

  const waitForComposer = async (timeout = 12_000) => {
    const deadline = Date.now() + timeout;
    let composer = findComposer();
    while (!composer && Date.now() < deadline) {
      await sleep(120);
      composer = findComposer();
    }
    if (!composer) throw new Error('网页会话仍在后台加载，请稍后重试');
    return composer;
  };

  const send = async (text, image, preserveComposer = false) => {
    // A fetch wrapper can be bypassed if the site saved a reference before the
    // bridge was injected. Reset the DOM fallback for every newly sent turn.
    window.__flightNetworkDeltaObserved = false;
    latestDomText = latestAssistantText();
    window.__flightDomFallbackNotBefore = Date.now() + 1400;
    clearTimeout(window.__flightDomFallbackTimer);
    window.__flightDomFallbackTimer = setTimeout(inspectDomReply, 1450);
    const composer = await waitForComposer();
    composer.focus();
    if (image) await attachImage(composer, image);
    if (preserveComposer && text) updateCommandSuffix(text);
    if (!preserveComposer && text && composer instanceof HTMLTextAreaElement) {
      const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set;
      setter?.call(composer, text);
      composer.dispatchEvent(new Event('input', { bubbles: true }));
    } else if (!preserveComposer && text) {
      document.execCommand('selectAll', false);
      document.execCommand('insertText', false, text);
      composer.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: text }));
    }
    await new Promise((resolve) => requestAnimationFrame(resolve));
    await sleep(120);
    const sendButton = [...document.querySelectorAll('button[data-testid="send-button"], button[aria-label]')].find((button) => {
      const label = `${button.getAttribute('aria-label') || ''} ${button.dataset.testid || ''}`.toLowerCase();
      return visible(button) && !button.disabled && (label.includes('send') || label.includes('发送'));
    });
    if (sendButton) {
      sendButton.click();
      return;
    }
    composer.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', bubbles: true, cancelable: true }));
    composer.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', code: 'Enter', bubbles: true, cancelable: true }));
    await sleep(160);
    const retryButton = document.querySelector('button[data-testid="send-button"]');
    if (retryButton && !retryButton.disabled) {
      retryButton.click();
      return;
    }
    throw new Error('未找到可用的 ChatGPT 发送按钮');
  };

  window.__flightChat = { send, loadHistory, loadConversationList, openConversation, updateCommandDraft, selectCommandOption, updateCommandSuffix, clearCommandSelection, installFlightModeButton, installPageTrimButton, installCookieImportButton, installCookieExportButton, completeCookieExport, failCookieExport };
  installFlightModeButton();
  installPageTrimButton();
  installCookieImportButton();
  installCookieExportButton();
  const currentConversationId = /^\/c\/([^/?#]+)/.exec(location.pathname)?.[1];
  relay({ kind: 'bridge_ready', conversationId: currentConversationId });
  if (currentConversationId) {
    // The page's own detail request may have happened before the bridge was
    // injected. Read the rendered latest turns again as the hidden route
    // finishes instead of issuing another large request.
    void loadHistory(2);
    setTimeout(() => void loadHistory(2), 1400);
    setTimeout(() => void loadHistory(2), 3400);
  }
  let sessionProbeCount = 0;
  const reportSessionState = () => {
    if (location.hostname !== 'chatgpt.com') {
      relay({ kind: 'session_login_required' });
      return;
    }
    if (findComposer()) {
      relay({ kind: 'session_ready' });
      return;
    }
    sessionProbeCount += 1;
    if (sessionProbeCount >= 30) {
      relay({ kind: 'session_login_required' });
      return;
    }
    setTimeout(reportSessionState, 400);
  };
  setTimeout(reportSessionState, 250);
  relay({ kind: 'probe', text: '网页探针已注入，正在等待回复流' });
})();
"#;
