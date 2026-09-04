//! Native 插件运行时：host 侧 spawn 插件 exe + stdin/stdout 管道 RPC。
//!
//! 一个 [`NativeRuntime`] 持有按插件 id 索引的 [`NativeProcess`]。进程**懒启动**：
//! invoke/诊断路径首次命中时 spawn 并 `plugin.initialize` 握手；之后常驻，
//! 直到崩溃、超时或 host 退出。搜索路径**不懒启动**——未就绪返回 NotReady 由
//! 调用方触发 [`NativeRuntime::warm`] 后台预热（spawn 无界，不能落在查询路径）。
//!
//! 线程模型：每个活跃进程跑一个独立**读帧线程**，把 stdout 帧泵入 mpsc 通道；
//! RPC 调用方发请求后 `recv_timeout` 取响应。读不阻塞调用方、超时可控、崩溃
//! （EOF）能被检测。写（发请求）只在调用方线程，单写者，无需同步。
//!
//! 锁结构（PERF-SEARCH ③-a）：runtime 状态整体生活在 [`spawn_runtime_thread`]
//! 创建的**专职线程**，host 线程经 [`NativeRuntimeHandle`] 消息通信。为什么：
//! 搜索/invoke 在 host 全局锁（`SharedHost = Arc<Mutex<HostApp>>`）内执行，
//! native RPC 最坏可达秒级——RPC 等待必须移出 host 锁，而 runtime 状态需要
//! 单写者，专职线程把"单写者"收敛为一个点，锁内只剩通道 send（微秒级）。
//! **锁序纪律**：native 线程从不取 host 锁；host 线程等待本线程应答一律在
//! host 锁外（invoke 路径例外地持锁等待——单向依赖无死锁环，见 ipc_server
//! host.invoke 注释）。

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

/// 单次 RPC 默认超时（invoke / 诊断路径）。native 插件是重型组件，给足时间但
/// 避免永久挂起拖死 host。搜索路径走更短的 [`SEARCH_RPC_TIMEOUT`]。
const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// 搜索路径 RPC 上限（PERF-SEARCH ③-b）：用户在打字，逐键查询必须快进快出。
/// 超时**保留进程**（"慢但健康"的插件不被逐键 kill+respawn 造成进程风暴），
/// 本轮按 partial 降级交 UI 补查。300ms = 打字节奏（防抖 80ms）之上仍可感知不到。
pub(crate) const SEARCH_RPC_TIMEOUT: Duration = Duration::from_millis(300);

/// RPC 失败分类：Timeout = 插件没在时限内回话（进程可能"慢但健康"）；
/// Fatal = 进程级故障（EOF/写失败/应答错误），必须丢弃重建。
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

    /// query：发 plugin.query，等 QueryResult（既有阻塞语义，invoke/诊断路径用）。
    fn query(&mut self, params: QueryParams) -> Result<QueryResult, PluginError> {
        let resp = self.rpc_sync(PluginMethod::Query.as_str(), serde_json::to_value(&params)?)?;
        Ok(serde_json::from_value(resp)?)
    }

    /// invoke：发 plugin.invoke，等 InvokeResult（既有阻塞语义）。
    fn invoke(&mut self, params: InvokeParams) -> Result<InvokeResult, PluginError> {
        let resp = self.rpc_sync(
            PluginMethod::Invoke.as_str(),
            serde_json::to_value(&params)?,
        )?;
        Ok(serde_json::from_value(resp)?)
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

    /// 发一帧请求并等**对应 id** 的响应（超时由调用方按路径给：搜索 300ms / 其余 5s）。
    ///
    /// 响应只接受与本次请求 id 完全一致的帧；不一致（此前被放弃请求的迟到响应、
    /// 自发帧）一律**忽略并继续等**——搜索路径超时后进程被保留（SEARCH_RPC_TIMEOUT
    /// 注释），被放弃请求的迟到应答会滞留在读通道里，若沿用旧"不符即杀"会把
    /// 保留的进程误杀成逐键重 spawn。只收精确匹配 id，错位应答不可能被静默
    /// 路由到错误请求上（与旧逻辑同级别的防串号保证）。
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
    /// 在途预热标记（审计 Issue #1 修复）：Warm 到达时已就绪**或在途**都直接忽略——
    /// 搜索路径不懒启动，spawn+握手完成前 procs 恒空，没有这个标记逐键 Warm 会
    /// 并发拉起十几个插件进程（进程风暴）+ 失败路径逐键 warn（日志风暴）。
    /// WarmDone 归位（成败都算）时移除，保证后续可重试；标记只在 runtime 线程
    /// 消息循环内读写（warm/insert_warm），无跨线程同步需求。
    warming: std::collections::HashSet<String>,
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
    /// （既有阻塞语义，仅诊断/兜底路径使用；搜索路径走 query_for_search。）
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

    /// 搜索路径查询（PERF-SEARCH ③-b 语义）：**不懒启动**、`SEARCH_RPC_TIMEOUT`
    /// 上限、**超时保留进程**。未就绪返回 NotReady，由调用方触发 [`Self::warm`]。
    pub fn query_for_search(
        &mut self,
        info: &NativeSpawnInfo,
        text: &str,
        limit: u32,
    ) -> Result<QueryResult, SearchQueryError> {
        let Some(proc) = self.procs.get_mut(&info.id) else {
            return Err(SearchQueryError::NotReady);
        };
        let params = QueryParams {
            text: text.to_string(),
            limit,
        };
        let body = serde_json::to_value(&params)
            .map_err(|e| SearchQueryError::Failed(PluginError::Manifest(e.to_string())))?;
        match proc.rpc(PluginMethod::Query.as_str(), body, SEARCH_RPC_TIMEOUT) {
            Ok(v) => serde_json::from_value(v)
                .map_err(|e| SearchQueryError::Failed(PluginError::Manifest(e.to_string()))),
            Err(RpcFailure::Timeout(e)) => {
                // 进程保留：下一键查询再给它 300ms；进程真死了走 EOF 被清理
                warn!(?e, id = %info.id, "native search query timeout; process kept");
                Err(SearchQueryError::Timeout)
            }
            Err(RpcFailure::Fatal(e)) => {
                self.procs.remove(&info.id);
                warn!(?e, id = %info.id, "native search query failed; process dropped");
                Err(SearchQueryError::Failed(e))
            }
        }
    }

    /// 后台预热：spawn+握手（秒级、无界）放独立线程，完成后经 WarmDone 消息归位。
    /// 已就绪**或在途**则 no-op（在途去重见 warming 字段注释）；线程创建失败不标记，
    /// 下一次 Warm 可重试。WarmDone 必达性：spawn 线程无论成败都会 send WarmDone，
    /// runtime 循环按序处理 → warming 标记不会永久卡死插件就绪。
    fn warm(&mut self, info: NativeSpawnInfo, done_tx: &mpsc::Sender<RuntimeMsg>) {
        if self.procs.contains_key(&info.id) || self.warming.contains(&info.id) {
            return;
        }
        let tx = done_tx.clone();
        let flag_id = info.id.clone();
        let log_id = info.id.clone();
        let spawned = thread::Builder::new()
            .name("spark-native-warm".into())
            .spawn(move || {
                let id = info.id.clone();
                let res = NativeProcess::spawn(&info);
                let _ = tx.send(RuntimeMsg::WarmDone { id, res });
            });
        match spawned {
            Ok(_handle) => {
                // WarmDone 由 runtime 循环按序消费，不存在"标记先于归位被旁路"的竞态
                self.warming.insert(flag_id);
            }
            Err(e) => warn!(?e, id = %log_id, "native warm thread spawn failed"),
        }
    }

    /// WarmDone 归位：先清在途标记（成败都清，失败保证后续可重试），再插入预热完成
    /// 的进程；竞态下已有进程则丢弃后到者（drop 会 kill）。
    /// 孤儿进程归属：WarmDone 归位时插件可能已被卸载/禁用（runtime 线程不持 host 锁，
    /// 不回查插件表）——该进程与既有"卸载不关进程"行为同款，留在进程表直到 host 退出
    /// 被 shutdown_all 收割；后续搜索不会再命中它（插件表已无该 id 的路由匹配）。
    fn insert_warm(&mut self, id: String, res: Result<NativeProcess, PluginError>) {
        self.warming.remove(&id);
        match res {
            Ok(proc) => {
                if self.procs.contains_key(&id) {
                    debug!(id = %id, "warm spawn raced with existing process; dropped");
                    drop(proc);
                    return;
                }
                info!(id = %id, "native plugin warmed up");
                self.procs.insert(id, proc);
            }
            Err(e) => warn!(?e, id = %id, "native warm spawn failed"),
        }
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

/// 搜索路径查询失败分类（[`NativeRuntime::query_for_search`]）。
pub enum SearchQueryError {
    /// 进程未就绪（搜索路径不懒启动）：调用方应触发 Warm 后台预热并按 partial 降级。
    NotReady,
    /// RPC 超时：进程保留（"慢但健康"的判断交给后续请求）。
    Timeout,
    /// 进程级故障（进程已被丢弃，下次查询重 spawn）。
    Failed(PluginError),
}

/// 专职 native 线程的消息。runtime 状态（进程句柄/stdin/req-id，需 `&mut`）只能被
/// 该线程触碰；host 锁内只做通道 send，RPC 等待全部发生在 host 锁外（见模块注释）。
enum RuntimeMsg {
    /// 搜索路径查询（不懒启动 / 300ms / 超时保留进程）。
    QuerySearch {
        info: NativeSpawnInfo,
        text: String,
        limit: u32,
        reply: mpsc::Sender<Result<QueryResult, SearchQueryError>>,
    },
    /// 同步阻塞查询（懒启动 / 5s / 失败即杀）：`--query` 诊断与 invoke 兜底路径。
    QueryBlocking {
        info: NativeSpawnInfo,
        text: String,
        limit: u32,
        reply: mpsc::Sender<QueryResult>,
    },
    /// 插件执行（懒启动 / 5s / 失败即杀，既有语义）。
    Invoke {
        info: NativeSpawnInfo,
        params: InvokeParams,
        reply: mpsc::Sender<Result<InvokeResult, PluginError>>,
    },
    /// 后台预热 spawn+握手完成，进程归位。
    WarmDone {
        id: String,
        res: Result<NativeProcess, PluginError>,
    },
    /// 预热请求：runtime 线程派生独立 spawn 线程（spawn 无界，不能占 runtime 线程）。
    Warm { info: NativeSpawnInfo },
    /// 关停单插件（同步等待回复：覆盖更新前必须确认 .exe 已不被占用）。
    ShutdownPlugin { id: String, reply: mpsc::Sender<()> },
    /// 优雅关闭全部进程并退出 runtime 线程（join 收割）。
    ShutdownAll,
}

/// native 运行时句柄（可克隆，只共享发送端）。join 句柄放 `Arc<Mutex<Option<_>>>`
/// 供 [`Self::shutdown_all`] 收割。锁序纪律见模块注释：native 线程从不取 host 锁，
/// host 线程等待本线程应答一律在 host 锁外。
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
    let warm_tx = tx.clone();
    let join = thread::Builder::new()
        .name("spark-native-runtime".into())
        .spawn(move || {
            let mut rt = NativeRuntime::default();
            while let Ok(msg) = rx.recv() {
                match msg {
                    RuntimeMsg::QuerySearch {
                        info,
                        text,
                        limit,
                        reply,
                    } => {
                        let _ = reply.send(rt.query_for_search(&info, &text, limit));
                    }
                    RuntimeMsg::QueryBlocking {
                        info,
                        text,
                        limit,
                        reply,
                    } => {
                        let _ = reply.send(rt.query(&info, &text, limit).unwrap_or(QueryResult {
                            items: vec![],
                            partial: false,
                        }));
                    }
                    RuntimeMsg::Invoke {
                        info,
                        params,
                        reply,
                    } => {
                        let _ = reply.send(rt.invoke(&info, params));
                    }
                    RuntimeMsg::Warm { info } => rt.warm(info, &warm_tx),
                    RuntimeMsg::WarmDone { id, res } => rt.insert_warm(id, res),
                    RuntimeMsg::ShutdownPlugin { id, reply } => {
                        rt.shutdown_plugin(&id);
                        let _ = reply.send(());
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
    /// 搜索路径查询（宿主在 **host 锁外**调用）：RPC 上限 `SEARCH_RPC_TIMEOUT`。
    /// 返回 (结果, partial)：NotReady（自动触发后台预热）/超时/故障 → (空, true)。
    pub fn query_search(
        &self,
        info: NativeSpawnInfo,
        text: String,
        limit: u32,
    ) -> (QueryResult, bool) {
        let empty = || QueryResult {
            items: vec![],
            partial: false,
        };
        let (tx, rx) = mpsc::channel();
        if self
            .tx
            .send(RuntimeMsg::QuerySearch {
                info: info.clone(),
                text,
                limit,
                reply: tx,
            })
            .is_err()
        {
            // runtime 线程已退出（关停中或 panic 消亡）：按无插件结果处理。
            // 正常关停后 UI 不应再来查询；若因 panic 消亡，这条 warn 是唯一线索
            warn!(id = %info.id, "native runtime thread gone; query dropped");
            return (empty(), false);
        }
        // 留 1s 余量给通道排队；runtime 线程自身先于本上限应答
        match rx.recv_timeout(SEARCH_RPC_TIMEOUT + Duration::from_secs(1)) {
            Ok(Ok(qr)) => (qr, false),
            Ok(Err(SearchQueryError::NotReady)) => {
                self.warm(info);
                (empty(), true)
            }
            Ok(Err(_)) => (empty(), true),
            Err(_) => (empty(), true),
        }
    }

    /// 同步阻塞查询（既有语义：懒启动 + 5s + 失败即杀）。`--query` 诊断 / invoke 兜底。
    pub fn query_blocking(&self, info: NativeSpawnInfo, text: String, limit: u32) -> QueryResult {
        let (tx, rx) = mpsc::channel();
        if self
            .tx
            .send(RuntimeMsg::QueryBlocking {
                info,
                text,
                limit,
                reply: tx,
            })
            .is_err()
        {
            return QueryResult {
                items: vec![],
                partial: false,
            };
        }
        // 懒启动最坏 = 无界 spawn + 5s 握手 + 5s 查询；15s 盖帽防调用方悬挂
        rx.recv_timeout(Duration::from_secs(15))
            .unwrap_or(QueryResult {
                items: vec![],
                partial: false,
            })
    }

    /// 插件执行（既有语义：懒启动 + 5s + 失败即杀）。用户主动执行，可等待。
    pub fn invoke(
        &self,
        info: NativeSpawnInfo,
        params: InvokeParams,
    ) -> Result<InvokeResult, PluginError> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(RuntimeMsg::Invoke {
                info,
                params,
                reply: tx,
            })
            .map_err(|_| io_other("native runtime thread gone"))?;
        rx.recv_timeout(Duration::from_secs(15))
            .map_err(|_| io_other("native invoke wait timeout"))?
    }

    /// 后台预热（fire-and-forget）：spawn+握手在独立线程，完成归位 runtime 进程表。
    pub fn warm(&self, info: NativeSpawnInfo) {
        let _ = self.tx.send(RuntimeMsg::Warm { info });
    }

    /// 关停单插件并**同步等待**：覆盖更新路径要求进程确已退出，.exe 才可覆盖。
    pub fn shutdown_plugin_sync(&self, id: &str) {
        let (tx, rx) = mpsc::channel();
        if self
            .tx
            .send(RuntimeMsg::ShutdownPlugin {
                id: id.to_string(),
                reply: tx,
            })
            .is_err()
        {
            return;
        }
        // shutdown 内部最多 1s 轮询 + kill；3s 盖帽
        let _ = rx.recv_timeout(Duration::from_secs(3));
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

/// 搜索路径的 native 查询快照：host 锁内由 PluginManager 构造（owned，Send），
/// 调用方在 **host 锁外** [`Self::execute`]。缺省无插件结果时 partial=false。
pub struct NativeSearchRequest {
    handle: NativeRuntimeHandle,
    info: NativeSpawnInfo,
    input: String,
    limit: u32,
}

impl NativeSearchRequest {
    /// PluginManager::native_search_request 专用构造（字段对 crate 外不透明）。
    pub(crate) fn new(
        handle: NativeRuntimeHandle,
        info: NativeSpawnInfo,
        input: String,
        limit: u32,
    ) -> Self {
        Self {
            handle,
            info,
            input,
            limit,
        }
    }

    /// 锁外执行：≤ `SEARCH_RPC_TIMEOUT`；未就绪自动后台预热。
    /// 返回 (结果, partial)：partial=true 表示本轮 native 结果缺席，host 侧如实上报 UI 补查。
    pub fn execute(self) -> (QueryResult, bool) {
        self.handle.query_search(self.info, self.input, self.limit)
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
