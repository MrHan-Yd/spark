//! Native 插件运行时：host 侧 spawn 插件 exe + stdin/stdout 管道 RPC。
//!
//! native 插件是**纯应用**模型：清单不带 commands/keywords，不进搜索框、不产生
//! 主窗口候选；exe 的唯一调用方是插件页面（PluginWindow/WebView2）经 host 转发的
//! `plugin.page` RPC。进程**懒启动**：页面首次 RPC 时 spawn 并 `plugin.initialize`
//! 握手；页面关闭时调用方 `shutdown_plugin` 优雅关停——"不打开就是不用"。
//!
//! 线程模型：每个活跃进程跑一个独立**读帧线程**，把 stdout 帧泵入 mpsc 通道；
//! RPC 调用方发请求后 `recv_timeout` 取响应。读不阻塞调用方、超时可控、崩溃
//! （EOF）能被检测。写（发请求）只在调用方线程，单写者，无需同步。
//!
//! 锁结构：runtime 状态整体生活在 [`spawn_runtime_thread`] 创建的**专职线程**，
//! host 线程经 [`NativeRuntimeHandle`] 消息通信。为什么：page RPC 最坏可达秒级，
//! 而 host 侧调用发生在全局锁（`SharedHost = Arc<Mutex<HostApp>>`）内——RPC 等待
//! 必须移出 host 锁，而 runtime 状态需要单写者，专职线程把"单写者"收敛为一个点，
//! 锁内只剩通道 send（微秒级）。**锁序纪律**：native 线程从不取 host 锁；host
//! 线程等待本线程应答一律在 host 锁外。**经批准的唯一例外**：卸载/覆盖安装路径
//! 的 `shutdown_plugin_sync`（删除/替换 .exe 前必须确认进程退出，有界 3s）——
//! 低频设置页操作，页面转发路径不走它（见 `NativePageRequest`）。

use crate::{LoadedPlugin, PluginError, PluginManager, PluginRuntime};
use spark_ipc::{
    decode_line, encode_line, read_frame, write_frame, JsonRpcResponse, PluginInitializeParams,
    PluginInitializeResult, PluginMethod, PluginPageParams, API_VERSION,
};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
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

/// 单次 RPC 默认超时（页面转发路径）。native 插件是重型组件，给足时间但
/// 避免永久挂起拖死 host。
const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// RPC 失败分类：Timeout = 插件没在时限内回话；Fatal = 进程级故障（EOF/写失败/
/// 应答错误）。**当前两者同策略处理**（`rpc_sync` 拍平 → `with_proc` 丢弃进程重建），
/// 保留分类只为错误信息可读；若未来引入"慢但健康"保留策略，在此分叉。
enum RpcFailure {
    Timeout(PluginError),
    Fatal(PluginError),
}

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
        let resp = self.rpc_sync(
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

    /// page：发 plugin.page，等插件自定义 JSON 结果（页面 spark.rpc 转发路径）。
    fn page_call(&mut self, params: PluginPageParams) -> Result<serde_json::Value, PluginError> {
        self.rpc_sync(PluginMethod::Page.as_str(), serde_json::to_value(&params)?)
    }

    /// 既有语义的 RPC：默认超时，任何失败都按进程级故障处理（with_proc 丢弃）。
    fn rpc_sync(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        match self.rpc(method, params, DEFAULT_RPC_TIMEOUT) {
            Ok(v) => Ok(v),
            Err(RpcFailure::Timeout(e)) | Err(RpcFailure::Fatal(e)) => Err(e),
        }
    }

    /// 发一帧请求并等**对应 id** 的响应（超时由调用方按路径给）。
    ///
    /// 响应只接受与本次请求 id 完全一致的帧；不一致（此前被放弃请求的迟到响应、
    /// 自发帧）一律**忽略并继续等**——被放弃请求的迟到应答会滞留在读通道里，
    /// 若沿用旧"不符即杀"会把保留的进程误杀。只收精确匹配 id，错位应答不可能被
    /// 静默路由到错误请求上。
    fn rpc(
        &mut self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, RpcFailure> {
        let id = self.next_req_id();
        let expected_id = serde_json::Value::from(id);
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let line = encode_line(&req).map_err(|e| RpcFailure::Fatal(ipc_err(e)))?;
        write_frame(&mut self.stdin, line.as_bytes())
            .map_err(ipc_err)
            .map_err(RpcFailure::Fatal)?;

        // 绝对截止时间（审计 Issue #2 修复）：不匹配帧"忽略并继续等"的窗口必须有
        // 总上界——若每次 recv 重新计时，插件以 ≥1 帧/超时窗 的速率发自发/通知帧
        // 可让本次 rpc 永不超时 → runtime 线程被占死、host 退出的 shutdown_all join
        // 永久悬挂。按剩余时间收帧保证总等待 ≤ timeout，join 因此有界
        // （最坏 = 一个 rpc 超时 + 每进程 shutdown ≤1s）。
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(RpcFailure::Timeout(io_other(format!(
                    "native plugin {} {method} timed out after {timeout:?}",
                    self.plugin_id
                ))));
            }
            match self.rx.recv_timeout(remaining) {
                Ok(FrameEvent::Response(resp)) => {
                    // id 不符 = 此前放弃请求的迟到应答或自发帧：忽略，继续等本次的
                    if resp.id.as_ref() != Some(&expected_id) {
                        debug!(
                            id = %self.plugin_id,
                            method,
                            got = ?resp.id,
                            expected = id,
                            "native plugin stale/unsolicited frame ignored"
                        );
                        continue;
                    }
                    if let Some(err) = resp.error {
                        return Err(RpcFailure::Fatal(io_other(format!(
                            "plugin {method} error {}: {}",
                            err.code, err.message
                        ))));
                    }
                    return resp.result.ok_or_else(|| {
                        RpcFailure::Fatal(io_other(format!("plugin {method} returned no result")))
                    });
                }
                Ok(FrameEvent::Eof(reason)) => {
                    return Err(RpcFailure::Fatal(io_other(format!(
                        "native plugin {} pipe closed: {reason}",
                        self.plugin_id
                    ))))
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(RpcFailure::Timeout(io_other(format!(
                        "native plugin {} {method} timed out after {:?}",
                        self.plugin_id, timeout
                    ))))
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(RpcFailure::Fatal(io_other(format!(
                        "native plugin {} reader disconnected",
                        self.plugin_id
                    ))))
                }
            }
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

impl NativeRuntime {
    /// 页面转发调用：确保进程已启动，发 plugin.page，返回插件自定义 JSON。
    /// 进程崩溃/超时 → 丢弃进程，下次自动重 spawn；本次返回错误（页面收到错误码）。
    pub fn page_call(
        &mut self,
        info: &NativeSpawnInfo,
        method: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        let params = PluginPageParams {
            method: method.to_string(),
            args,
        };
        self.with_proc(info, |proc| proc.page_call(params))
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

    /// 关停指定插件 id 的 native 进程（页面关闭 / 覆盖更新前调用：占用中的
    /// .exe 无法删除）。未运行则无操作。
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

/// 专职 native 线程的消息。runtime 状态（进程句柄/stdin/req-id，需 `&mut`）只能被
/// 该线程触碰；host 锁内只做通道 send，RPC 等待全部发生在 host 锁外（见模块注释）。
enum RuntimeMsg {
    /// 页面转发调用（懒启动 / 5s / 失败即杀）。
    PageCall {
        info: NativeSpawnInfo,
        method: String,
        args: serde_json::Value,
        reply: mpsc::Sender<Result<serde_json::Value, PluginError>>,
    },
    /// 关停单插件，同步等待回复（覆盖更新/卸载前必须确认 .exe 已不被占用）。
    ShutdownPlugin { id: String, reply: mpsc::Sender<()> },
    /// 关停单插件，不等待（页面关闭通知）：runtime 线程串行执行，最多占用
    /// runtime 线程自身 ≤1s，不占 host 锁。进程未运行则为 no-op。
    ShutdownPluginFire { id: String },
    /// 优雅关闭全部进程并退出 runtime 线程（join 收割）。
    ShutdownAll,
}

/// native 运行时句柄（可克隆，只共享发送端）。join 句柄放 `Arc<Mutex<Option<_>>>`
/// 供 [`Self::shutdown_all`] 收割。锁序纪律见模块注释：native 线程从不取 host 锁，
/// host 线程等待本线程应答一律在 host 锁外（唯一例外：卸载/覆盖路径的
/// `shutdown_plugin_sync`，有界 3s）。
#[derive(Clone)]
pub struct NativeRuntimeHandle {
    tx: mpsc::Sender<RuntimeMsg>,
    join: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
}

impl Default for NativeRuntimeHandle {
    fn default() -> Self {
        spawn_runtime_thread()
    }
}

/// 创建专职 native 线程。PluginManager 构造时调用一次；线程常驻直到
/// [`NativeRuntimeHandle::shutdown_all`]（host 退出路径显式调用）。
pub fn spawn_runtime_thread() -> NativeRuntimeHandle {
    let (tx, rx) = mpsc::channel::<RuntimeMsg>();
    let join = thread::Builder::new()
        .name("spark-native-runtime".into())
        .spawn(move || {
            let mut rt = NativeRuntime::default();
            while let Ok(msg) = rx.recv() {
                match msg {
                    RuntimeMsg::PageCall {
                        info,
                        method,
                        args,
                        reply,
                    } => {
                        let _ = reply.send(rt.page_call(&info, &method, args));
                    }
                    RuntimeMsg::ShutdownPlugin { id, reply } => {
                        rt.shutdown_plugin(&id);
                        let _ = reply.send(());
                    }
                    RuntimeMsg::ShutdownPluginFire { id } => {
                        rt.shutdown_plugin(&id);
                    }
                    RuntimeMsg::ShutdownAll => {
                        rt.shutdown_all();
                        break;
                    }
                }
            }
        })
        .expect("spawn native runtime thread");
    NativeRuntimeHandle {
        tx,
        join: Arc::new(Mutex::new(Some(join))),
    }
}

impl NativeRuntimeHandle {
    /// 页面转发调用（既有 invoke 语义：懒启动 + 5s + 失败即杀）。用户主动打开
    /// 页面触发，可等待。
    pub fn page_call(
        &self,
        info: NativeSpawnInfo,
        method: String,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, PluginError> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(RuntimeMsg::PageCall {
                info,
                method,
                args,
                reply: tx,
            })
            .map_err(|_| io_other("native runtime thread gone"))?;
        // 懒启动最坏 = 无界 spawn + 5s 握手 + 5s 调用；15s 盖帽防调用方悬挂
        rx.recv_timeout(Duration::from_secs(15))
            .map_err(|_| io_other("native page call wait timeout"))?
    }

    /// 关停单插件并**同步等待**：覆盖更新/卸载路径要求进程确已退出，.exe 才可
    /// 覆盖/删除。返回 false = 3s 内未确认退出（如 runtime 线程被在途 PageCall 占住），
    /// 调用方必须中止删改——否则半删目录（plugin.json 已删、exe 残留）需手动恢复。
    /// 页面关闭通知勿用本方法（见 [`Self::shutdown_plugin`]）。
    pub fn shutdown_plugin_sync(&self, id: &str) -> bool {
        let (tx, rx) = mpsc::channel();
        if self
            .tx
            .send(RuntimeMsg::ShutdownPlugin {
                id: id.to_string(),
                reply: tx,
            })
            .is_err()
        {
            // runtime 线程已退出：其 Drop 已 shutdown_all 收割全部子进程，无占用风险
            return true;
        }
        // shutdown 内部最多 1s 轮询 + kill；3s 盖帽
        rx.recv_timeout(Duration::from_secs(3)).is_ok()
    }

    /// 关停单插件，**不等待**（fire-and-forget）：页面关闭通知路径。等待发生在
    /// runtime 专职线程（≤1s/进程），调用方立即返回——host 锁内只花一次 send。
    /// 进程未运行（含 webview id）则为 no-op。
    pub fn shutdown_plugin(&self, id: &str) {
        let _ = self
            .tx
            .send(RuntimeMsg::ShutdownPluginFire { id: id.to_string() });
    }

    /// 优雅关闭全部进程并收割 runtime 线程。host 退出路径显式调用
    /// （句柄 Clone 副本不参与关闭——Clone 只共享发送端，无 Drop 副作用）。
    pub fn shutdown_all(&self) {
        if self.tx.send(RuntimeMsg::ShutdownAll).is_err() {
            return;
        }
        if let Ok(mut g) = self.join.lock() {
            if let Some(j) = g.take() {
                let _ = j.join();
            }
        }
    }
}

/// 页面转发调用的锁内快照：host 锁内由 [`PluginManager::native_page_request`]
/// 构造（owned，Send），调用方在 **host 锁外** [`Self::execute`]——懒启动 spawn、
/// 握手与 `plugin.page` 的等待（最坏 15s）全部发生在 host 锁外，绝不占 host 锁。
pub struct NativePageRequest {
    handle: NativeRuntimeHandle,
    info: NativeSpawnInfo,
    method: String,
    args: serde_json::Value,
}

impl NativePageRequest {
    /// PluginManager::native_page_request 专用构造（字段对 crate 外不透明）。
    pub(crate) fn new(
        handle: NativeRuntimeHandle,
        info: NativeSpawnInfo,
        method: String,
        args: serde_json::Value,
    ) -> Self {
        Self {
            handle,
            info,
            method,
            args,
        }
    }

    /// 锁外执行：懒启动 + 5s RPC（15s 盖帽）+ 失败即杀。
    ///
    /// 陈旧快照兜底：锁外期间插件被禁用/卸载时调用仍会完成——进程表由 runtime
    /// 线程持有、不回查插件表（与"卸载不关已运行进程"的既有孤儿语义一致）；
    /// exe 已被删则 spawn 直接失败回传页面，残留进程由随后的关窗/卸载 shutdown
    /// 或 host 退出路径收割。
    pub fn execute(self) -> Result<serde_json::Value, PluginError> {
        self.handle.page_call(self.info, self.method, self.args)
    }
}

/// 手动实现 Debug：句柄背后是子进程/管道（不可 derive），只暴露 id 与方法名。
impl std::fmt::Debug for NativePageRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativePageRequest")
            .field("plugin_id", &self.info.id)
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

impl PluginManager {
    /// 查找 enabled 的 native 插件（供 host 取 spawn 信息 / 鉴权路由）。
    pub fn native_plugin(&self, id: &str) -> Option<&LoadedPlugin> {
        self.plugins.iter().find(|p| {
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

    /// 写一个最小 native 纯应用清单（page 模型：无 commands，必有 page）。
    fn make_native_plugin(dir: &Path, id: &str) {
        fs::create_dir_all(dir).unwrap();
        let json = format!(
            r#"{{
                "id": "{id}", "name": "N", "version": "0.1.0", "api_version": 2,
                "runtime": "native", "main": "{id}.exe", "page": "page.html"
            }}"#
        );
        fs::write(dir.join("plugin.json"), json).unwrap();
    }

    #[test]
    fn native_plugin_lookup_filters_runtime() {
        let tmp = std::env::temp_dir().join("spark_native_lookup");
        let _ = fs::remove_dir_all(&tmp);
        make_native_plugin(&tmp.join("com.spark.n"), "com.spark.n");
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        pm.load_dev_dir(&tmp.join("com.spark.n")).unwrap();
        assert!(pm.native_plugin("com.spark.n").is_some());
        assert!(pm.native_plugin("com.spark.does-not-exist").is_none());
    }

    #[test]
    fn native_manifest_with_commands_fails_to_load() {
        // 纯应用模型：声明 commands 的 native 清单加载即被拒（无法进搜索框）。
        let tmp = std::env::temp_dir().join("spark_native_cmds_rejected");
        let _ = fs::remove_dir_all(&tmp);
        let dir = tmp.join("com.spark.cmd");
        fs::create_dir_all(&dir).unwrap();
        let json = r#"{
            "id":"com.spark.cmd","name":"N","version":"0.1.0","api_version":2,
            "runtime":"native","main":"cmd.exe","page":"page.html",
            "commands":[{"name":"c","title":"C","mode":"list","prefix":"c "}]
        }"#;
        fs::write(dir.join("plugin.json"), json).unwrap();
        let mut pm = PluginManager::with_dirs(tmp.join("plugins"), tmp.join("data"));
        assert!(pm.load_dev_dir(&dir).is_err());
    }

    // 注：进程 spawn / 管道 RPC 属集成测试，需真实 echo exe；此处不跑，
    // 由 host 集成层与 echo 二进制覆盖。运行时逻辑经 with_proc 已收敛单点。
    #[test]
    fn runtime_default_is_empty_and_shutdown_is_noop() {
        let mut rt = NativeRuntime::default();
        rt.shutdown_all(); // 不应 panic
    }
}
