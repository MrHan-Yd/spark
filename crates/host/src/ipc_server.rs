//! Named Pipe JSON-RPC server (NDJSON) for Host ↔ UI.

use crate::app::SharedHost;
use crate::hotkey::Hotkey;
use anyhow::{Context, Result};
use spark_ipc::{
    decode_line, encode_line, HostMethod, InvokeParams, JsonRpcRequest, JsonRpcResponse,
    PluginApiParams, PluginDevLoadParams, PluginGrantParams, PluginIdParams, PluginInstallParams,
    PluginOpenParams, PluginSetDirParams, PluginToggleParams, QueryParams, QueryResult,
    SetConfigParams, PIPE_PATH,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use tracing::{debug, info, warn};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE,
    INVALID_HANDLE_VALUE, LPARAM, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Connected UI writers for Host→UI notifications.
#[derive(Default, Clone)]
pub struct UiHub {
    /// (client_id, pipe handle)——持有句柄才能向 UI 推送 notification。
    inner: Arc<Mutex<Vec<(u64, SendHandle)>>>,
}

/// HANDLE wrapper marked Send (pipe I/O is serialized by our design).
#[derive(Clone, Copy)]
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}
impl SendHandle {
    fn raw(self) -> HANDLE {
        self.0
    }
}

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

impl UiHub {
    pub fn new() -> Self {
        Self::default()
    }

    fn register(&self, pipe: SendHandle) -> u64 {
        let id = NEXT_CLIENT.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut g) = self.inner.lock() {
            g.push((id, pipe));
            info!(client = id, clients = g.len(), "UI connected");
        }
        id
    }

    fn unregister(&self, id: u64) {
        if let Ok(mut g) = self.inner.lock() {
            g.retain(|&(c, _)| c != id);
            info!(client = id, clients = g.len(), "UI disconnected");
        }
    }

    pub fn client_count(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// 向所有已连接的 UI 推送一条 JSON-RPC notification（无 id，UI 触发 HostNotification）。
    /// 单条写失败（客户端已断开）即移除该客户端，不中断其余。
    /// 注意：host 的管道句柄是同步的（CreateNamedPipeW 未带 FILE_FLAG_OVERLAPPED），
    /// 若 client_read_loop 的 ReadFile 正挂起，同句柄 WriteFile 会被阻塞到读方向返回
    /// （实测 11-30s），因此热路径（如退出通知）应改用命名事件而非此广播。
    /// 当前无调用方，保留备用（未来若 pipe 改异步可重新启用）。
    #[allow(dead_code)]
    pub fn broadcast(&self, method: &str) {
        let line = format!("{{\"jsonrpc\":\"2.0\",\"method\":\"{method}\"}}\n");
        if let Ok(mut g) = self.inner.lock() {
            g.retain(|(_, handle)| write_all_handle(handle.raw(), line.as_bytes()).is_ok());
        }
    }
}

fn write_all_handle(handle: HANDLE, bytes: &[u8]) -> Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        let mut written = 0u32;
        unsafe {
            WriteFile(handle, Some(&bytes[offset..]), Some(&mut written), None)
                .context("WriteFile")?;
        }
        if written == 0 {
            anyhow::bail!("WriteFile wrote 0");
        }
        offset += written as usize;
    }
    // 注意：不要对 pipe 调 FlushFileBuffers —— 字节模式命名管道上它会阻塞
    // 直到对端读走全部数据；对端（UI）一旦没在消费，Host 消息循环就会永久卡死，
    // 表现为 Alt+Space 用过几次后彻底失灵。WriteFile 对管道本身就是同步语义。
    Ok(())
}

/// Spawn accept loop on a background thread.
pub fn spawn(host: SharedHost) -> UiHub {
    let hub = UiHub::new();
    let hub_clone = hub.clone();
    thread::Builder::new()
        .name("spark-ipc".into())
        .spawn(move || {
            if let Err(e) = accept_loop(host, hub_clone) {
                warn!(?e, "ipc accept loop exited");
            }
        })
        .expect("spawn ipc thread");
    hub
}

fn accept_loop(host: SharedHost, hub: UiHub) -> Result<()> {
    info!(pipe = PIPE_PATH, "IPC server listening");
    loop {
        let pipe = create_pipe_instance().context("CreateNamedPipe")?;
        match unsafe { ConnectNamedPipe(pipe, None) } {
            Ok(()) => {}
            Err(e) => {
                if e.code().0 as u32 != ERROR_PIPE_CONNECTED.0 {
                    warn!(?e, "ConnectNamedPipe");
                    unsafe {
                        let _ = CloseHandle(pipe);
                    }
                    continue;
                }
            }
        }
        let host2 = host.clone();
        let hub2 = hub.clone();
        let pipe = SendHandle(pipe);
        thread::Builder::new()
            .name("spark-ipc-client".into())
            .spawn(move || {
                if let Err(e) = handle_client(pipe, host2, hub2) {
                    debug!(?e, "client session ended");
                }
            })
            .ok();
    }
}

fn create_pipe_instance() -> Result<HANDLE> {
    let name: Vec<u16> = PIPE_PATH.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            None,
        )
    };
    if handle.is_invalid() || handle.0 == INVALID_HANDLE_VALUE.0 {
        anyhow::bail!("CreateNamedPipeW failed");
    }
    Ok(handle)
}

fn handle_client(pipe: SendHandle, host: SharedHost, hub: UiHub) -> Result<()> {
    let client_id = hub.register(pipe);
    let result = client_read_loop(pipe, &host);
    hub.unregister(client_id);
    unsafe {
        let _ = DisconnectNamedPipe(pipe.raw());
        let _ = CloseHandle(pipe.raw());
    }
    result
}

fn client_read_loop(pipe: SendHandle, host: &SharedHost) -> Result<()> {
    let raw = pipe.raw();
    let mut buf = vec![0u8; 64 * 1024];
    let mut acc: Vec<u8> = Vec::new();
    loop {
        let mut read = 0u32;
        let ok = unsafe { ReadFile(raw, Some(buf.as_mut_slice()), Some(&mut read), None) };
        match ok {
            Ok(()) if read > 0 => {
                acc.extend_from_slice(&buf[..read as usize]);
                while let Some(pos) = acc.iter().position(|&b| b == b'\n') {
                    let line_bytes: Vec<u8> = acc.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line_bytes);
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Err(e) = dispatch_line(line, raw, host) {
                        warn!(?e, "dispatch");
                        let id = serde_json::from_str::<serde_json::Value>(line)
                            .ok()
                            .and_then(|v| v.get("id").cloned());
                        if id.is_some() {
                            let _ = reply(raw, &JsonRpcResponse::error(id, -32603, e.to_string()));
                        }
                    }
                }
            }
            Ok(()) => break,
            Err(e) => {
                let code = e.code().0 as u32;
                if code == ERROR_NO_DATA.0 || code == ERROR_BROKEN_PIPE.0 {
                    break;
                }
                debug!(?e, "ReadFile end");
                break;
            }
        }
    }
    Ok(())
}

fn dispatch_line(line: &str, pipe: HANDLE, host: &SharedHost) -> Result<()> {
    let req: JsonRpcRequest = match decode_line(line) {
        Ok(req) => req,
        Err(e) => {
            let raw_id = serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| v.get("id").cloned());
            let resp = JsonRpcResponse::error(raw_id, -32700, e.to_string());
            reply(pipe, &resp)?;
            return Ok(());
        }
    };
    if req.jsonrpc != "2.0" {
        let resp = JsonRpcResponse::error(req.id.clone(), -32600, "jsonrpc must be 2.0");
        reply(pipe, &resp)?;
        return Ok(());
    }
    let id = req.id.clone();

    let no_params = |value: &serde_json::Value| value.is_null() || value == &serde_json::json!({});
    if (req.method == HostMethod::Toggle.as_str()
        || req.method == "ui.toggle"
        || req.method == "ui.show"
        || req.method == HostMethod::Show.as_str()
        || req.method == "host.exit")
        && !no_params(&req.params)
    {
        return reply(
            pipe,
            &JsonRpcResponse::error(id, -32602, "method does not accept params"),
        );
    }

    if req.method == HostMethod::Toggle.as_str()
        || req.method == "ui.toggle"
        || req.method == "ui.show"
        || req.method == HostMethod::Show.as_str()
    {
        // Reply first so one-shot clients read the response.
        if id.is_some() {
            let resp = JsonRpcResponse::result(id, serde_json::json!({"ok": true}));
            reply(pipe, &resp)?;
        }
        // 只打命名事件（唯一 toggle 通道）；不再广播 pipe ui.toggle ——
        // 双通道竞态会让 UI 收到第二次 toggle（见 win_loop::on_toggle 注释）。
        crate::toggle_signal::signal_toggle();
        return Ok(());
    }

    if req.method == "host.exit" {
        // 优雅退出（静默安装/CLI 用）：向主窗口投递 WM_SPARK_EXIT，
        // 走与托盘"退出"相同的路径（广播 ui.exit + PostQuitMessage）。
        if id.is_some() {
            let resp = JsonRpcResponse::result(id, serde_json::json!({"ok": true}));
            reply(pipe, &resp)?;
        }
        #[allow(static_mut_refs)]
        unsafe {
            if let Some(hwnd) = crate::win_loop::EXIT_HWND {
                let _ = PostMessageW(
                    Some(hwnd),
                    crate::win_loop::WM_SPARK_EXIT,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
        return Ok(());
    }

    let resp = match req.method.as_str() {
        m if m == HostMethod::Query.as_str() => {
            let params: QueryParams = match serde_json::from_value(req.params) {
                Ok(params) => params,
                Err(e) => return reply(pipe, &JsonRpcResponse::error(id, -32602, e.to_string())),
            };
            if params.text.len() > 4096 || params.limit > 500 {
                return reply(
                    pipe,
                    &JsonRpcResponse::error(id, -32602, "invalid query parameters"),
                );
            }
            let items = {
                let mut g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                let mut hits = g.search(&params.text);
                let limit = if params.limit == 0 {
                    g.config.max_results
                } else {
                    params.limit.min(g.config.max_results)
                };
                hits.truncate(limit as usize);
                hits
            };
            let result = QueryResult {
                items,
                partial: false,
            };
            JsonRpcResponse::result(id, serde_json::to_value(result)?)
        }
        m if m == HostMethod::Invoke.as_str() => {
            let params: InvokeParams = match serde_json::from_value(req.params) {
                Ok(params) => params,
                Err(e) => return reply(pipe, &JsonRpcResponse::error(id, -32602, e.to_string())),
            };
            if params.item_id.is_empty()
                || params.action_id.is_empty()
                || params.item_id.len() > 1024
                || params.action_id.len() > 256
                || params.text.len() > 4096
            {
                return reply(
                    pipe,
                    &JsonRpcResponse::error(id, -32602, "invalid invoke parameters"),
                );
            }
            let result = {
                let mut g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                g.invoke(&params)?
            };
            JsonRpcResponse::result(id, serde_json::to_value(result)?)
        }
        m if m == HostMethod::GetConfig.as_str() => {
            let g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
            JsonRpcResponse::result(id, serde_json::to_value(&g.config)?)
        }
        m if m == HostMethod::SetConfig.as_str() => {
            let params: SetConfigParams = match serde_json::from_value(req.params) {
                Ok(params) => params,
                Err(e) => return reply(pipe, &JsonRpcResponse::error(id, -32602, e.to_string())),
            };
            if let Some(hk) = params.hotkey_toggle.as_deref() {
                if let Err(e) = Hotkey::parse(hk) {
                    return reply(
                        pipe,
                        &JsonRpcResponse::error(id, -32602, format!("invalid hotkey: {e}")),
                    );
                }
            }
            // 受信任三方公钥表：先整体校验再落 config（非法条目给 UI 明确错误）。
            if let Some(entries) = &params.trusted_pubkeys {
                let mut g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                if let Err(msg) = g.apply_trusted_pubkeys(entries) {
                    return reply(pipe, &JsonRpcResponse::error(id, -32602, msg));
                }
            }
            let hotkey_changed = {
                let mut g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                let old = g.config.clone();
                if let Some(hk) = params.hotkey_toggle {
                    if hk != g.config.hotkey_toggle {
                        info!(old = %g.config.hotkey_toggle, new = %hk, "hotkey updated");
                        g.config.hotkey_toggle = hk;
                    }
                }
                if let Some(value) = params.hide_on_focus_lost {
                    g.config.hide_on_focus_lost = value;
                }
                if let Some(value) = params.hide_on_execute {
                    g.config.hide_on_execute = value;
                }
                if let Some(value) = params.launch_on_startup {
                    g.config.launch_on_startup = value;
                }
                if let Some(value) = params.strict_mode {
                    g.config.strict_mode = value;
                }
                if let Some(entries) = params.trusted_pubkeys {
                    // apply_trusted_pubkeys 已在上面整体校验通过；这里只落盘。
                    g.config.trusted_pubkeys = entries;
                }
                if let Some(urls) = params.plugin_registry_urls {
                    g.config.plugin_registry_urls = urls;
                }
                let changed = g.config != old;
                if changed {
                    if let Err(e) = g.config.save() {
                        g.config = old;
                        return reply(
                            pipe,
                            &JsonRpcResponse::error(id, -32000, format!("config save failed: {e}")),
                        );
                    }
                }
                changed && g.config.hotkey_toggle != old.hotkey_toggle
            };
            if hotkey_changed {
                // 重注册必须走主消息循环线程（与 HOTKEY_PAUSED/托盘开关同一路径）
                #[allow(static_mut_refs)]
                unsafe {
                    if let Some(hwnd) = crate::win_loop::EXIT_HWND {
                        let _ = PostMessageW(
                            Some(hwnd),
                            crate::win_loop::WM_SPARK_REHOTKEY,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            }
            JsonRpcResponse::result(id, serde_json::json!({"ok": true}))
        }
        m if m == HostMethod::GetBuiltins.as_str() => {
            // 内置命令清单（设置页展示用，无需锁 host）
            JsonRpcResponse::result(id, serde_json::to_value(spark_index::builtin::infos())?)
        }
        m if m == HostMethod::PluginList.as_str() => {
            let g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
            JsonRpcResponse::result(id, serde_json::to_value(g.plugin_list())?)
        }
        m if m == HostMethod::PluginInstall.as_str() => {
            let params: PluginInstallParams = match serde_json::from_value(req.params) {
                Ok(p) => p,
                Err(e) => return reply(pipe, &JsonRpcResponse::error(id, -32602, e.to_string())),
            };
            if params.path.is_empty() || params.path.len() > 4096 {
                return reply(pipe, &JsonRpcResponse::error(id, -32602, "invalid path"));
            }
            let outcome = {
                let mut g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                g.plugin_install(&params.path, params.force, params.require_signature)?
            };
            JsonRpcResponse::result(id, serde_json::to_value(outcome)?)
        }
        m if m == HostMethod::PluginUninstall.as_str() => {
            let params: PluginIdParams = match serde_json::from_value(req.params) {
                Ok(p) => p,
                Err(e) => return reply(pipe, &JsonRpcResponse::error(id, -32602, e.to_string())),
            };
            if params.id.is_empty() || params.id.len() > 256 {
                return reply(pipe, &JsonRpcResponse::error(id, -32602, "invalid id"));
            }
            {
                let mut g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                g.plugin_uninstall(&params.id)?;
            }
            JsonRpcResponse::result(id, serde_json::json!({ "ok": true }))
        }
        m if m == HostMethod::PluginToggle.as_str() => {
            let params: PluginToggleParams = match serde_json::from_value(req.params) {
                Ok(p) => p,
                Err(e) => return reply(pipe, &JsonRpcResponse::error(id, -32602, e.to_string())),
            };
            if params.id.is_empty() || params.id.len() > 256 {
                return reply(pipe, &JsonRpcResponse::error(id, -32602, "invalid id"));
            }
            {
                let mut g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                g.plugin_toggle(&params.id, params.enabled)?;
            }
            JsonRpcResponse::result(id, serde_json::json!({ "ok": true }))
        }
        m if m == HostMethod::PluginGrant.as_str() => {
            let params: PluginGrantParams = match serde_json::from_value(req.params) {
                Ok(p) => p,
                Err(e) => return reply(pipe, &JsonRpcResponse::error(id, -32602, e.to_string())),
            };
            if params.id.is_empty() || params.id.len() > 256 {
                return reply(pipe, &JsonRpcResponse::error(id, -32602, "invalid id"));
            }
            {
                let mut g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                g.plugin_grant(&params.id, params.permissions)?;
            }
            JsonRpcResponse::result(id, serde_json::json!({ "ok": true }))
        }
        m if m == HostMethod::PluginDevLoad.as_str() => {
            let params: PluginDevLoadParams = match serde_json::from_value(req.params) {
                Ok(p) => p,
                Err(e) => return reply(pipe, &JsonRpcResponse::error(id, -32602, e.to_string())),
            };
            if params.dir.is_empty() || params.dir.len() > 4096 {
                return reply(pipe, &JsonRpcResponse::error(id, -32602, "invalid dir"));
            }
            let loaded_id = {
                let mut g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                g.plugin_devload(&params.dir)?
            };
            JsonRpcResponse::result(id, serde_json::json!({ "id": loaded_id }))
        }
        m if m == HostMethod::PluginOpen.as_str() => {
            let params: PluginOpenParams = match serde_json::from_value(req.params) {
                Ok(p) => p,
                Err(e) => return reply(pipe, &JsonRpcResponse::error(id, -32602, e.to_string())),
            };
            if params.id.is_empty() || params.id.len() > 256 {
                return reply(pipe, &JsonRpcResponse::error(id, -32602, "invalid id"));
            }
            let info = {
                let g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                g.plugin_open(&params.id)?
            };
            JsonRpcResponse::result(id, serde_json::to_value(info)?)
        }
        m if m == HostMethod::PluginSetDir.as_str() => {
            let params: PluginSetDirParams = match serde_json::from_value(req.params) {
                Ok(p) => p,
                Err(e) => return reply(pipe, &JsonRpcResponse::error(id, -32602, e.to_string())),
            };
            if params.path.is_empty() || params.path.len() > 4096 {
                return reply(pipe, &JsonRpcResponse::error(id, -32602, "invalid path"));
            }
            {
                let mut g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                g.plugin_set_dir(&params.path, params.migrate)?;
            }
            JsonRpcResponse::result(id, serde_json::json!({ "ok": true }))
        }
        m if m == HostMethod::PluginApi.as_str() => {
            let params: PluginApiParams = match serde_json::from_value(req.params) {
                Ok(p) => p,
                Err(e) => return reply(pipe, &JsonRpcResponse::error(id, -32602, e.to_string())),
            };
            if params.plugin_id.is_empty()
                || params.plugin_id.len() > 256
                || params.capability.is_empty()
                || params.method.is_empty()
                || params.capability.len() > 64
                || params.method.len() > 64
            {
                return reply(
                    pipe,
                    &JsonRpcResponse::error(id, -32602, "invalid api params"),
                );
            }
            let data = {
                let mut g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                g.plugin_api(&params)?
            };
            JsonRpcResponse::result(id, serde_json::json!({ "ok": true, "data": data }))
        }
        other => JsonRpcResponse::error(id, -32601, format!("method not found: {other}")),
    };
    reply(pipe, &resp)
}

fn reply(pipe: HANDLE, resp: &JsonRpcResponse) -> Result<()> {
    let line = encode_line(resp)?;
    write_all_handle(pipe, format!("{line}\n").as_bytes())
}

/// Second-instance / CLI: connect and send toggle.
pub fn send_toggle_to_running_host() -> Result<()> {
    send_request_to_host(HostMethod::Toggle.as_str(), serde_json::json!({}))
}

pub fn send_request_to_host(method: &str, params: serde_json::Value) -> Result<()> {
    let name: Vec<u16> = PIPE_PATH.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .context("CreateFile pipe (is spark-host running?)")?;

    let req = JsonRpcRequest::new(1, method, params);
    let line = encode_line(&req)?;
    write_all_handle(handle, format!("{line}\n").as_bytes())?;

    let mut buf = [0u8; 4096];
    let mut read = 0u32;
    let _ = unsafe { ReadFile(handle, Some(&mut buf), Some(&mut read), None) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    Ok(())
}
