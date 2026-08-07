//! Named Pipe JSON-RPC server (NDJSON) for Host ↔ UI.

use crate::app::SharedHost;
use anyhow::{Context, Result};
use spark_ipc::{
    decode_line, encode_line, HostMethod, InvokeParams, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, QueryParams, QueryResult, PIPE_PATH,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use tracing::{debug, info, warn};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

/// Connected UI writers for Host→UI notifications.
#[derive(Default, Clone)]
pub struct UiHub {
    inner: Arc<Mutex<Vec<ClientWriter>>>,
}

#[derive(Clone, Copy)]
struct ClientWriter {
    id: u64,
    handle: SendHandle,
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

    fn register(&self, handle: SendHandle) -> u64 {
        let id = NEXT_CLIENT.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut g) = self.inner.lock() {
            g.push(ClientWriter { id, handle });
            info!(client = id, clients = g.len(), "UI connected");
        }
        id
    }

    fn unregister(&self, id: u64) {
        if let Ok(mut g) = self.inner.lock() {
            g.retain(|c| c.id != id);
            info!(client = id, clients = g.len(), "UI disconnected");
        }
    }

    pub fn broadcast_line(&self, line: &str) {
        // 在锁外快照句柄，避免持锁期间做阻塞写（WriteFile 可能卡在对端不读的
        // 僵尸连接上）；写不成功就把该客户端标记为失效，下次广播前清理。
        let payload = format!("{line}\n");
        let bytes = payload.as_bytes();
        let snapshot: Vec<ClientWriter> = self.inner.lock().map(|g| g.clone()).unwrap_or_default();
        let mut dead = Vec::new();
        for c in snapshot.iter() {
            match write_all_handle(c.handle.raw(), bytes) {
                Ok(()) => {}
                Err(e) => {
                    warn!(client = c.id, ?e, "broadcast write failed");
                    dead.push(c.id);
                }
            }
        }
        if !dead.is_empty() {
            if let Ok(mut g) = self.inner.lock() {
                g.retain(|c| !dead.contains(&c.id));
            }
        }
    }

    /// 后台线程广播，保证调用方（消息循环线程）永远不被 pipe I/O 卡住。
    pub fn notify_toggle_async(&self) {
        let hub = self.clone();
        let _ = std::thread::Builder::new()
            .name("spark-broadcast".into())
            .spawn(move || {
                hub.notify_toggle();
            });
    }

    pub fn notify_show(&self) {
        if let Ok(line) = encode_line(&JsonRpcNotification::ui_show()) {
            self.broadcast_line(&line);
        }
    }

    pub fn notify_toggle(&self) {
        if let Ok(line) = encode_line(&JsonRpcNotification::ui_toggle()) {
            self.broadcast_line(&line);
        }
    }

    pub fn client_count(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
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
    let result = client_read_loop(pipe, &host, &hub);
    hub.unregister(client_id);
    unsafe {
        let _ = DisconnectNamedPipe(pipe.raw());
        let _ = CloseHandle(pipe.raw());
    }
    result
}

fn client_read_loop(pipe: SendHandle, host: &SharedHost, hub: &UiHub) -> Result<()> {
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
                    if let Err(e) = dispatch_line(line, raw, host, hub) {
                        warn!(?e, "dispatch");
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

fn dispatch_line(line: &str, pipe: HANDLE, host: &SharedHost, hub: &UiHub) -> Result<()> {
    let req: JsonRpcRequest = decode_line(line)?;
    let id = req.id.clone();

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
        crate::toggle_signal::signal_toggle();
        hub.notify_toggle_async();
        return Ok(());
    }

    let resp = match req.method.as_str() {
        m if m == HostMethod::Query.as_str() => {
            let params: QueryParams = serde_json::from_value(req.params).unwrap_or(QueryParams {
                text: String::new(),
                limit: 50,
            });
            let items = {
                let g = host.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
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
            let params: InvokeParams = serde_json::from_value(req.params)?;
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
