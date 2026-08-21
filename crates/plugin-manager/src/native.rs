//! Native 插件运行时：host 侧 spawn 插件 exe + stdin/stdout 管道 RPC。
//!
//! 一个 [`NativeRuntime`] 持有按插件 id 索引的 [`NativeProcess`]。进程**懒启动**：
//! 首次 query/invoke 命中某插件时才 spawn 并 `plugin.initialize` 握手；之后常驻，
//! 直到崩溃、超时或 host 退出。
//!
//! 线程模型：每个活跃进程跑一个独立**读帧线程**，把 stdout 帧泵入 mpsc 通道；
//! RPC 调用方（持有 host 锁的搜索/invoke 线程）发请求后 `recv_timeout` 取响应。
//! 这样读不阻塞调用方、超时可控、崩溃（EOF）能被检测。写（发请求）只在调用方线程，
//! 单写者，无需同步。

use crate::{LoadedPlugin, PluginError, PluginManager, PluginRuntime};
use spark_ipc::{
    decode_line, encode_line, read_frame, write_frame, InvokeParams, InvokeResult, JsonRpcResponse,
    PluginInitializeParams, PluginInitializeResult, PluginMethod, QueryParams, QueryResult,
    API_VERSION,
};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// 构造一个 `ErrorKind::Other` 的 io 错误包成 PluginError::Io（用于非直接 IO 的失败信息）。
fn io_other(msg: impl Into<String>) -> PluginError {
    PluginError::Io(io::Error::new(io::ErrorKind::Other, msg.into()))
}

/// 把帧编解码错误转成 PluginError::Io（Other）。
fn ipc_err(e: spark_ipc::IpcError) -> PluginError {
    io_other(e.to_string())
}

/// 启动一个 native 进程所需的全部信息（owned，避免借用冲突）。
/// host 侧从 `LoadedPlugin` 构造一次后即可放下不可变借用，再可变调用运行时。
#[derive(Clone)]
pub struct NativeSpawnInfo {
    pub id: String,
    pub exe: PathBuf,
    pub root: PathBuf,
    pub granted: Vec<String>,
}

impl NativeSpawnInfo {
    pub(crate) fn from_plugin(p: &LoadedPlugin) -> Self {
        Self {
            id: p.manifest.id.clone(),
            exe: p.root.join(&p.manifest.main),
            root: p.root.clone(),
            granted: p.granted.clone(),
        }
    }
}

/// 单次 RPC 默认超时。native 插件是重型组件，给足时间但避免永久挂起拖死 host。
const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// 读帧线程推给调用方的条目：要么是一条响应，要么是“读端已断”（EOF/IO 错误）。
enum FrameEvent {
    Response(JsonRpcResponse),
    Eof(String),
}

struct NativeProcess {
    plugin_id: String,
    /// 保留 Child 句柄：drop 时自动 kill；shutdown 时显式 wait。
    child: Child,
    stdin: ChildStdin,
    /// 读帧线程 → 调用方的通道。每次 RPC 前调用方期望恰好收到一条 Response。
    rx: mpsc::Receiver<FrameEvent>,
    next_id: u64,
}

impl NativeProcess {
    /// spawn 插件 exe 并发 initialize 握手。exe 路径 = `info.exe`。
    fn spawn(info: &NativeSpawnInfo) -> Result<Self, PluginError> {
        if !info.exe.is_file() {
            return Err(PluginError::Manifest(format!(
                "native plugin exe not found: {}",
                info.exe.display()
            )));
        }
        let mut cmd = Command::new(&info.exe);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // 插件自用日志走 stderr，host 不解析
            .current_dir(&info.root);
        // CREATE_NO_WINDOW：避免无头 native 插件 exe 闪控制台窗口。
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW = 0x08000000
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| io_other(format!("spawn native plugin {:?}: {e}", info.exe.display())))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io_other("native plugin stdin not captured"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io_other("native plugin stdout not captured"))?;
        let (tx, rx) = mpsc::channel::<FrameEvent>();
        spawn_reader(stdout, tx);

        let mut proc = Self {
            plugin_id: info.id.clone(),
            child,
            stdin,
            rx,
            next_id: 1,
        };
        proc.initialize(info)?;
        info!(id = %proc.plugin_id, "native plugin process ready");
        Ok(proc)
    }

    fn next_req_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    /// 发送 plugin.initialize 并等响应。
    fn initialize(
        &mut self,
        info: &NativeSpawnInfo,
    ) -> Result<PluginInitializeResult, PluginError> {
        let params = PluginInitializeParams {
            id: info.id.clone(),
            permissions: info.granted.clone(),
            api_version: API_VERSION,
        };
        let resp = self.rpc(
            PluginMethod::Initialize.as_str(),
            serde_json::to_value(&params)?,
        )?;
        let result: PluginInitializeResult = serde_json::from_value(resp).map_err(|e| {
            PluginError::Manifest(format!("initialize response decode failed: {e}"))
        })?;
        if result.plugin_id != info.id {
            warn!(
                host = %info.id,
                plugin = %result.plugin_id,
                "native plugin reported different id; continuing"
            );
        }
        Ok(result)
    }

    /// query：发 plugin.query，等 QueryResult。
    fn query(&mut self, params: QueryParams) -> Result<QueryResult, PluginError> {
        let resp = self.rpc(PluginMethod::Query.as_str(), serde_json::to_value(&params)?)?;
        Ok(serde_json::from_value(resp)?)
    }

    /// invoke：发 plugin.invoke，等 InvokeResult。
    fn invoke(&mut self, params: InvokeParams) -> Result<InvokeResult, PluginError> {
        let resp = self.rpc(
            PluginMethod::Invoke.as_str(),
            serde_json::to_value(&params)?,
        )?;
        Ok(serde_json::from_value(resp)?)
    }

    /// 发一帧请求并等**对应 id** 的响应（带超时）。
    ///
    /// 响应 id 必须与本次请求 id 一致；不一致（插件多回一帧 / 自发帧 / 串号）
    /// 即视为协议错误返回 Err，由 `with_proc` 丢弃进程、下次重建——杜绝静默错位。
    /// 正常 1:1 lockstep 响应 id 必然匹配，不会误杀。
    fn rpc(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        let id = self.next_req_id();
        let expected_id = serde_json::Value::from(id);
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let line = encode_line(&req).map_err(ipc_err)?;
        write_frame(&mut self.stdin, line.as_bytes()).map_err(ipc_err)?;

        match self.rx.recv_timeout(DEFAULT_RPC_TIMEOUT) {
            Ok(FrameEvent::Response(resp)) => {
                // id 校验：不符 = 协议错误（多余/串号/自发帧），丢弃进程重建。
                if resp.id.as_ref() != Some(&expected_id) {
                    return Err(io_other(format!(
                        "native plugin {} {method}: response id {:?} != request id {id}",
                        self.plugin_id, resp.id
                    )));
                }
                if let Some(err) = resp.error {
                    return Err(io_other(format!(
                        "plugin {method} error {}: {}",
                        err.code, err.message
                    )));
                }
                resp.result
                    .ok_or_else(|| io_other(format!("plugin {method} returned no result")))
            }
            Ok(FrameEvent::Eof(reason)) => Err(io_other(format!(
                "native plugin {} pipe closed: {reason}",
                self.plugin_id
            ))),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(io_other(format!(
                "native plugin {} {method} timed out after {:?}",
                self.plugin_id, DEFAULT_RPC_TIMEOUT
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(io_other(format!(
                "native plugin {} reader disconnected",
                self.plugin_id
            ))),
        }
    }

    /// 优雅关闭：发 shutdown notification，轮询 1s 等退出；超时则强杀。
    /// 用 try_wait 轮询而非 std 没有的 wait_timeout。
    fn shutdown(&mut self) {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": PluginMethod::Shutdown.as_str(),
            "params": serde_json::Value::Null,
        });
        if let Ok(line) = encode_line(&req) {
            let _ = write_frame(&mut self.stdin, line.as_bytes());
        }
        let _ = self.stdin.flush();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match self.child.try_wait() {
                Ok(Some(_status)) => {
                    debug!(id = %self.plugin_id, "native plugin exited cleanly");
                    return;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return,
            }
        }
        warn!(id = %self.plugin_id, "native plugin did not exit, killing");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for NativeProcess {
    fn drop(&mut self) {
        // 兜底：未走 shutdown 时也确保子进程被回收，避免僵尸。
        let _ = self.child.kill();
        let _ = self.child.try_wait();
    }
}

/// 启动读帧线程：循环读 stdout 帧 → 解析为 JsonRpcResponse → 推通道；
/// 读到 EOF/错误时推一条 Eof 后退出（调用方的 recv 会拿到它）。
fn spawn_reader(stdout: ChildStdout, tx: mpsc::Sender<FrameEvent>) {
    thread::Builder::new()
        .name("spark-native-reader".into())
        .spawn(move || {
            let mut reader = stdout;
            loop {
                match read_frame(&mut reader) {
                    Ok(Some(body)) => {
                        let s = String::from_utf8_lossy(&body);
                        match decode_line::<JsonRpcResponse>(&s) {
                            Ok(resp) => {
                                if tx.send(FrameEvent::Response(resp)).is_err() {
                                    return; // 调用方已丢弃进程
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(FrameEvent::Eof(format!("bad frame: {e}")));
                                return;
                            }
                        }
                    }
                    Ok(None) => {
                        let _ = tx.send(FrameEvent::Eof("eof".into()));
                        return;
                    }
                    Err(e) => {
                        let _ = tx.send(FrameEvent::Eof(format!("read error: {e}")));
                        return;
                    }
                }
            }
        })
        .ok();
}

/// 管理所有已启用的 native 插件进程。
#[derive(Default)]
pub struct NativeRuntime {
    procs: HashMap<String, NativeProcess>,
}

/// native 插件的关键字/前缀路由匹配结果。
#[derive(Debug, Clone)]
pub struct NativeMatch {
    pub plugin_id: String,
    /// 去掉前缀后的用户输入。
    pub input: String,
    /// 匹配到的 command name（invoke 时用于区分）。
    pub command: String,
}

impl NativeRuntime {
    /// 执行一次 native query：确保进程已启动，发 query，返回结果项。
    /// 进程崩溃/超时 → 丢弃进程，下次自动重 spawn；本次返回空结果。
    pub fn query(
        &mut self,
        info: &NativeSpawnInfo,
        text: &str,
        limit: u32,
    ) -> Result<QueryResult, PluginError> {
        let params = QueryParams {
            text: text.to_string(),
            limit,
        };
        self.with_proc(info, |proc| proc.query(params))
            .or_else(|e| {
                warn!(?e, id = %info.id, "native query failed");
                Ok(QueryResult {
                    items: vec![],
                    partial: false,
                })
            })
    }

    /// 执行 native invoke。
    pub fn invoke(
        &mut self,
        info: &NativeSpawnInfo,
        params: InvokeParams,
    ) -> Result<InvokeResult, PluginError> {
        self.with_proc(info, |proc| proc.invoke(params))
    }

    /// 取/启动某插件进程，执行一次调用。失败时清理该插件进程（下次重 spawn）。
    fn with_proc<F, R>(&mut self, info: &NativeSpawnInfo, f: F) -> Result<R, PluginError>
    where
        F: FnOnce(&mut NativeProcess) -> Result<R, PluginError>,
    {
        let id = info.id.clone();
        // 已有进程 → 直接用。
        if let Some(proc) = self.procs.get_mut(&id) {
            return match f(proc) {
                Ok(v) => Ok(v),
                Err(e) => {
                    // 进程级失败（IO/超时）：丢弃，下次重 spawn。
                    self.procs.remove(&id);
                    Err(e)
                }
            };
        }
        // 懒启动。
        let mut proc = NativeProcess::spawn(info)?;
        let r = f(&mut proc);
        if r.is_err() {
            // 握手或首次调用就失败：丢弃，不留半死进程。
            drop(proc);
        } else {
            self.procs.insert(id, proc);
        }
        r
    }

    /// host 退出前调用：向所有进程发 shutdown 并 wait。
    pub fn shutdown_all(&mut self) {
        for (_, mut proc) in self.procs.drain() {
            proc.shutdown();
        }
    }

    /// 关停指定插件 id 的 native 进程（覆盖更新前调用：占用中的 .exe 无法删除）。
    /// 未运行则无操作。
    pub fn shutdown_plugin(&mut self, id: &str) {
        if let Some(mut proc) = self.procs.remove(id) {
            proc.shutdown();
        }
    }
}

impl Drop for NativeRuntime {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

impl PluginManager {
    /// native 插件关键字/前缀路由：在 enabled 的 native 插件 `mode=="list"` commands 中匹配。
    ///
    /// 匹配规则：command.prefix 非空时按前缀匹配（大小写不敏感），命中后按字节偏移
    /// 截取原始输入（**保留用户原大小写**，与 webview `find_keyword_match` 对齐）；
    /// prefix 为空时退化为 command.name 精确匹配。首个命中插件胜出。
    /// 仅路由 `mode=="list"`：`page` 模式 native 自建窗口属二期，不在此发 query。
    pub fn find_native_match(&self, text: &str) -> Option<NativeMatch> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        for p in &self.plugins {
            if !p.enabled || !matches!(p.manifest.runtime, PluginRuntime::Native) {
                continue;
            }
            for cmd in &p.manifest.commands {
                // 只路由 list 模式；page 模式 native 二期未实现。
                if cmd.mode != "list" {
                    continue;
                }
                if let Some(prefix) = cmd.prefix.as_deref().filter(|s| !s.is_empty()) {
                    let pfx_lower = prefix.to_ascii_lowercase();
                    if trimmed
                        .to_ascii_lowercase()
                        .strip_prefix(&pfx_lower)
                        .is_some()
                    {
                        // prefix 为 ASCII，按字节偏移截原始输入，保留原大小写。
                        let input = trimmed[prefix.len()..].to_string();
                        return Some(NativeMatch {
                            plugin_id: p.manifest.id.clone(),
                            input,
                            command: cmd.name.clone(),
                        });
                    }
                } else if trimmed.to_ascii_lowercase() == cmd.name.to_ascii_lowercase() {
                    return Some(NativeMatch {
                        plugin_id: p.manifest.id.clone(),
                        input: String::new(),
                        command: cmd.name.clone(),
                    });
                }
            }
        }
        None
    }

    /// 查找 enabled 的 native 插件（供 host 路由后取 manifest/root）。
    pub fn native_plugin(&self, id: &str) -> Option<&LoadedPlugin> {
        self.plugins.iter().find(|p| {
            p.manifest.id == id && p.enabled && matches!(p.manifest.runtime, PluginRuntime::Native)
        })
    }

    pub fn native_plugin_mut(&mut self, id: &str) -> Option<&mut LoadedPlugin> {
        self.plugins.iter_mut().find(|p| {
            p.manifest.id == id && p.enabled && matches!(p.manifest.runtime, PluginRuntime::Native)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PluginManager;
    use std::fs;
    use std::path::Path;

    fn make_native_plugin(dir: &Path, id: &str, prefix: &str) {
        fs::create_dir_all(dir).unwrap();
        let json = format!(
            r#"{{
                "id": "{id}", "name": "N", "version": "0.1.0", "api_version": 1,
                "runtime": "native", "main": "{id}.exe",
                "commands":[{{"name":"echo","title":"Echo","mode":"list","prefix":"{prefix}"}}]
            }}"#
        );
        fs::write(dir.join("plugin.json"), json).unwrap();
    }

    #[test]
    fn find_native_match_by_prefix() {
        let tmp = std::env::temp_dir().join("spark_native_match");
        let _ = fs::remove_dir_all(&tmp);
        make_native_plugin(&tmp.join("com.spark.echo"), "com.spark.echo", "echo ");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.echo")).unwrap();

        let m = pm.find_native_match("echo hello world").unwrap();
        assert_eq!(m.plugin_id, "com.spark.echo");
        assert_eq!(m.input, "hello world");
        assert_eq!(m.command, "echo");
    }

    #[test]
    fn find_native_match_preserves_input_case() {
        // 回归：input 必须保留用户原大小写（曾因 to_ascii_lowercase 截取而丢失）。
        let tmp = std::env::temp_dir().join("spark_native_case");
        let _ = fs::remove_dir_all(&tmp);
        make_native_plugin(&tmp.join("com.spark.e"), "com.spark.e", "echo ");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.e")).unwrap();

        let m = pm.find_native_match("echo Hello World").unwrap();
        assert_eq!(m.input, "Hello World");
    }

    #[test]
    fn find_native_match_skips_page_mode() {
        // page 模式 native command 不应路由到 query（二期）。
        let tmp = std::env::temp_dir().join("spark_native_page");
        let _ = fs::remove_dir_all(&tmp);
        let dir = tmp.join("com.spark.pg");
        fs::create_dir_all(&dir).unwrap();
        let json = r#"{
            "id":"com.spark.pg","name":"N","version":"0.1.0","api_version":1,
            "runtime":"native","main":"pg.exe",
            "commands":[{"name":"pg","title":"Pg","mode":"page","prefix":"pg "}]
        }"#;
        fs::write(dir.join("plugin.json"), json).unwrap();
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&dir).unwrap();
        assert!(pm.find_native_match("pg hi").is_none());
    }

    #[test]
    fn find_native_match_no_prefix_falls_back_to_name() {
        let tmp = std::env::temp_dir().join("spark_native_name");
        let _ = fs::remove_dir_all(&tmp);
        let dir = tmp.join("com.spark.np");
        fs::create_dir_all(&dir).unwrap();
        let json = r#"{
            "id":"com.spark.np","name":"N","version":"0.1.0","api_version":1,
            "runtime":"native","main":"np.exe",
            "commands":[{"name":"calc","title":"Calc","mode":"list"}]
        }"#;
        fs::write(dir.join("plugin.json"), json).unwrap();
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&dir).unwrap();

        let m = pm.find_native_match("calc").unwrap();
        assert_eq!(m.plugin_id, "com.spark.np");
        assert!(m.input.is_empty());
    }

    #[test]
    fn find_native_match_skips_disabled_and_webview() {
        let tmp = std::env::temp_dir().join("spark_native_skip");
        let _ = fs::remove_dir_all(&tmp);
        make_native_plugin(&tmp.join("com.spark.e"), "com.spark.e", "e ");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.e")).unwrap();
        pm.set_enabled("com.spark.e", false).unwrap();
        assert!(pm.find_native_match("e hi").is_none());
    }

    #[test]
    fn native_plugin_lookup_filters_runtime() {
        let tmp = std::env::temp_dir().join("spark_native_lookup");
        let _ = fs::remove_dir_all(&tmp);
        make_native_plugin(&tmp.join("com.spark.n"), "com.spark.n", "n ");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.n")).unwrap();
        assert!(pm.native_plugin("com.spark.n").is_some());
        assert!(pm.native_plugin("com.spark.does-not-exist").is_none());
        assert!(pm.native_plugin_mut("com.spark.n").is_some());
    }

    // 注：进程 spawn / 管道 RPC 属集成测试，需真实 echo exe；此处不跑，
    // 由 host 集成层与 echo 二进制覆盖。运行时逻辑经 with_proc 已收敛单点。
    #[test]
    fn runtime_default_is_empty_and_shutdown_is_noop() {
        let mut rt = NativeRuntime::default();
        rt.shutdown_all(); // 不应 panic
    }
}
