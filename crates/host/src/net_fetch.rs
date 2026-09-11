//! `spark.net.fetch` 的 host 代理实现（插件开发规范 §8.3）。
//!
//! 分层：`parse_request` 是纯函数校验（ipc 锁内调用，零 IO）；
//! `execute` 是真实 HTTP（ipc_server **放锁后**调用——阻塞网络调用绝不持
//! host 锁，与 native rpc 的锁序纪律一致，否则一次慢请求冻结全部 IPC）。
//!
//! 一期走 WinHTTP 直连（`WINHTTP_ACCESS_TYPE_NO_PROXY`，与 builtins 的
//! fetch_http_ip 同口径），TLS 与重定向采用 WinHTTP 默认策略（禁 https→http
//! 降级）。响应体为文本语义（UTF-8 lossy），二进制资源后续增量。

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Foundation::GetLastError;
use windows::Win32::Networking::WinHttp::{
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_NO_PROXY,
    WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_ADDREQ_FLAG_REPLACE, WINHTTP_FLAG_SECURE,
    WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_OPTION_REDIRECT_POLICY,
    WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_RAW_HEADERS_CRLF, WINHTTP_QUERY_STATUS_CODE,
};

/// 请求体上限。
pub const MAX_REQUEST_BODY: usize = 1024 * 1024;
/// 响应体上限，超出按 NETWORK_FAILED 报告。
pub const MAX_RESPONSE_BODY: usize = 10 * 1024 * 1024;
/// 单次请求总预算。UI 桥对 net 的超时为 30s（HostIpcClient），host 留出
/// IPC 往返余量；各阶段超时按剩余预算收紧，总耗时被此预算兜住。
pub const TOTAL_BUDGET: Duration = Duration::from_secs(25);

const RESOLVE_CAP: Duration = Duration::from_secs(5);
const CONNECT_CAP: Duration = Duration::from_secs(10);
const SEND_CAP: Duration = Duration::from_secs(10);
const RECEIVE_CAP: Duration = Duration::from_secs(15);
const READ_CHUNK: usize = 64 * 1024;
const USER_AGENT: &str = "Spark-PluginNet/1.0";

/// 允许的请求方法（规范 §8.3：method 可选）。
const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// 托管头：帧控/hop-by-hop 语义由 host 统一接管，插件不得注入。除
/// Host/Content-Length（WinHTTP 依 URL/实际帧生成）外，必须拦下
/// Transfer-Encoding 与 hop-by-hop 头——否则"自动 Content-Length + 插件
/// 注入 TE"会构成 CL+TE 请求走私面。
const MANAGED_HEADERS: [&str; 7] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "proxy-connection",
    "keep-alive",
    "upgrade",
];

/// 请求头数量/单头字节上限（锁内即拒，不落到 WinHTTP 才报错）。
const MAX_HEADERS: usize = 64;
const MAX_HEADER_BYTES: usize = 32 * 1024;

/// 解析+校验后的代理请求（纯数据，可 Debug/Clone 供测试断言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// 代理响应；序列化即回包（`body` → camelCase `bodyText`，preload 再包装成
/// 规范形状 `{ status, headers, text(), json() }`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    /// camelCase 对单词 body 不生效，显式指定回包字段名。
    #[serde(rename = "bodyText")]
    pub body: String,
}

/// 执行参数：正式路径用 `Default`（TOTAL_BUDGET/MAX_RESPONSE_BODY），测试
/// 用小预算/小上限驱动超时与超限分支。
pub struct NetOpts {
    pub budget: Duration,
    pub max_body: usize,
}

impl Default for NetOpts {
    fn default() -> Self {
        Self {
            budget: TOTAL_BUDGET,
            max_body: MAX_RESPONSE_BODY,
        }
    }
}

/// 从 `plugin_api` 的 args 解析请求：`{ url, init?: { method?, headers?, body? } }`。
/// 任何不合法统一 `INVALID_ARGS:` 前缀（页面侧 ClassifyError 直接还原 code）。
pub fn parse_request(args: &serde_json::Value) -> Result<NetRequest> {
    let obj = args
        .as_object()
        .ok_or_else(|| anyhow!("INVALID_ARGS: net.fetch 需要 object args"))?;
    let url = obj
        .get("url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("INVALID_ARGS: url 必须为非空字符串"))?;
    parse_url(url)?;

    let init = match obj.get("init") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => Some(
            v.as_object()
                .ok_or_else(|| anyhow!("INVALID_ARGS: init 必须为 object"))?,
        ),
    };

    let method = match init.and_then(|o| o.get("method")) {
        None | Some(serde_json::Value::Null) => "GET".to_string(),
        Some(v) => {
            let m = v
                .as_str()
                .ok_or_else(|| anyhow!("INVALID_ARGS: method 必须为字符串"))?;
            let m = m.trim().to_ascii_uppercase();
            if m.is_empty() {
                bail!("INVALID_ARGS: method 不能为空");
            }
            if !METHODS.contains(&m.as_str()) {
                bail!("INVALID_ARGS: 不支持的 method {m}");
            }
            m
        }
    };

    let body = match init.and_then(|o| o.get("body")) {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => {
            let b = v
                .as_str()
                .ok_or_else(|| anyhow!("INVALID_ARGS: body 必须为字符串"))?;
            if method == "GET" || method == "HEAD" {
                bail!("INVALID_ARGS: {method} 不允许携带 body");
            }
            if b.len() > MAX_REQUEST_BODY {
                bail!("INVALID_ARGS: body 超过 {} 字节上限", MAX_REQUEST_BODY);
            }
            Some(b.to_string())
        }
    };

    let mut headers = Vec::new();
    if let Some(h) = init.and_then(|o| o.get("headers")) {
        let map = h
            .as_object()
            .ok_or_else(|| anyhow!("INVALID_ARGS: headers 必须为 object"))?;
        for (name, value) in map {
            let name = name.trim();
            if name.is_empty() {
                bail!("INVALID_ARGS: header 名不能为空");
            }
            let value = value
                .as_str()
                .ok_or_else(|| anyhow!("INVALID_ARGS: header {name} 值必须为字符串"))?;
            // NUL 会截断 WinHTTP 的宽字符串参数（静默截值），与 CR/LF 一并按注入处理。
            if name.contains(['\r', '\n', '\0', ':']) || value.contains(['\r', '\n', '\0']) {
                bail!("INVALID_ARGS: header {name} 含非法字符");
            }
            if headers.len() >= MAX_HEADERS {
                bail!("INVALID_ARGS: 请求头数量超过 {MAX_HEADERS} 上限");
            }
            if name.len() + value.len() > MAX_HEADER_BYTES {
                bail!("INVALID_ARGS: header {name} 超过 {MAX_HEADER_BYTES} 字节上限");
            }
            // 托管头过滤（含 Transfer-Encoding/hop-by-hop，见 MANAGED_HEADERS 注释）。
            let lname = name.to_ascii_lowercase();
            if MANAGED_HEADERS.contains(&lname.as_str()) {
                continue;
            }
            headers.push((name.to_string(), value.to_string()));
        }
    }

    Ok(NetRequest {
        url: url.to_string(),
        method,
        headers,
        body,
    })
}

/// URL 解析：scheme 仅 http/https；拒绝 userinfo；host 须 ASCII；支持
/// IPv6 括号形式；`#fragment` 剥离、`?query` 保留。
fn parse_url(url: &str) -> Result<(bool, String, u16, String)> {
    // 防御纵深：显式拒绝控制字符（CR/LF/NUL 等），不依赖 WinHTTP 的默认
    // 转义行为兜底——请求行/header 链路上任何裸控制字符都按注入处理。
    if url.chars().any(|c| c.is_control()) {
        bail!("INVALID_ARGS: url 含控制字符");
    }
    let (scheme_raw, rest) = url
        .split_once("://")
        .ok_or_else(|| anyhow!("INVALID_ARGS: url 缺少 scheme"))?;
    let scheme = scheme_raw.to_ascii_lowercase();
    let secure = match scheme.as_str() {
        "http" => false,
        "https" => true,
        _ => bail!("INVALID_ARGS: 仅支持 http/https（收到 {scheme}）"),
    };

    // authority 到第一个 '/'、'?' 或 '#' 为止。
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.is_empty() {
        bail!("INVALID_ARGS: url 缺少 host");
    }
    if authority.contains('@') {
        bail!("INVALID_ARGS: 不支持 userinfo 形式的 url");
    }

    // host[:port]；IPv6 为 [addr] 或 [addr]:port。
    let (host, port) = if let Some(addr) = authority.strip_prefix('[') {
        let close = addr
            .find(']')
            .ok_or_else(|| anyhow!("INVALID_ARGS: IPv6 缺少闭合括号"))?;
        let h = &addr[..close];
        let after = &addr[close + 1..];
        let p = if let Some(ps) = after.strip_prefix(':') {
            parse_port(ps)?
        } else if after.is_empty() {
            0
        } else {
            bail!("INVALID_ARGS: host 段非法")
        };
        (h.to_string(), p)
    } else {
        match authority.rsplit_once(':') {
            Some((h, ps)) => (h.to_string(), parse_port(ps)?),
            None => (authority.to_string(), 0),
        }
    };
    if host.is_empty() || !host.is_ascii() {
        bail!("INVALID_ARGS: host 须为非空 ASCII（一期不支持 IDN）");
    }

    let path_portion = &rest[end..];
    let path = match path_portion.find('#') {
        Some(hash) => &path_portion[..hash],
        None => path_portion,
    };
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    // "host?a=b"（无 /）的情况上面会拼成 "/?a=b"。
    let port = match port {
        0 => {
            if secure {
                443
            } else {
                80
            }
        }
        p => p,
    };
    Ok((secure, host, port, path))
}

fn parse_port(ps: &str) -> Result<u16> {
    match ps.parse::<u16>() {
        Ok(p) if p > 0 => Ok(p),
        _ => bail!("INVALID_ARGS: 端口 {ps} 不合法"),
    }
}

// ── WinHTTP 执行段 ───────────────────────────────────────────────────────────

/// 裸句柄守卫：任何 bail/`?` 返回路径上都自动 CloseHandle，杜绝泄漏。
struct HandleGuard(*mut core::ffi::c_void);
impl Drop for HandleGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

/// 各阻塞阶段前调用：返回剩余毫秒（耗尽即 NETWORK_FAILED 超时），
/// 并把四个阶段超时一并收紧为 min(cap, 剩余)——保证总耗时被预算兜底。
fn remaining_and_set_timeouts(
    handle: *mut core::ffi::c_void,
    start: Instant,
    budget: Duration,
) -> Result<i32> {
    let left = budget
        .checked_sub(start.elapsed())
        .ok_or_else(|| anyhow!("NETWORK_FAILED: 请求超时（总预算 {}s）", budget.as_secs()))?;
    let ms = left.as_millis().min(i32::MAX as u128) as i32;
    let clamp = |cap: Duration| (cap.as_millis() as i32).min(ms).max(1);
    unsafe {
        WinHttpSetTimeouts(
            handle,
            clamp(RESOLVE_CAP),
            clamp(CONNECT_CAP),
            clamp(SEND_CAP),
            clamp(RECEIVE_CAP),
        )
        .map_err(|e| anyhow!("NETWORK_FAILED: 设置超时失败: {e}"))?;
    }
    Ok(ms)
}

/// 正式路径：默认预算/上限执行。
pub fn execute(req: &NetRequest) -> Result<NetResponse> {
    execute_with(req, &NetOpts::default())
}

pub fn execute_with(req: &NetRequest, opts: &NetOpts) -> Result<NetResponse> {
    let (secure, host, port, path) = parse_url(&req.url)?;

    let host_w = wide(&host);
    let path_w = wide(&path);
    let verb_w = wide(&req.method);
    let version_w = wide("HTTP/1.1");
    let agent_w = wide(USER_AGENT);

    let start = Instant::now();
    unsafe {
        let hsession = HandleGuard(WinHttpOpen(
            PCWSTR::from_raw(agent_w.as_ptr()),
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if hsession.0.is_null() {
            return Err(anyhow!(
                "NETWORK_FAILED: WinHttpOpen 失败: {:#?}",
                GetLastError()
            ));
        }
        remaining_and_set_timeouts(hsession.0, start, opts.budget)?;

        let hconnect = HandleGuard(WinHttpConnect(
            hsession.0,
            PCWSTR::from_raw(host_w.as_ptr()),
            port,
            0,
        ));
        if hconnect.0.is_null() {
            return Err(anyhow!(
                "NETWORK_FAILED: 连接 {host}:{port} 失败: {:#?}",
                GetLastError()
            ));
        }

        let flags = if secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
        let hrequest = HandleGuard(WinHttpOpenRequest(
            hconnect.0,
            PCWSTR::from_raw(verb_w.as_ptr()),
            PCWSTR::from_raw(path_w.as_ptr()),
            PCWSTR::from_raw(version_w.as_ptr()),
            PCWSTR::null(),
            std::ptr::null(),
            flags,
        ));
        if hrequest.0.is_null() {
            return Err(anyhow!(
                "NETWORK_FAILED: 构造请求失败: {:#?}",
                GetLastError()
            ));
        }
        // 显式落地重定向策略（模块注释所声明）：禁 https→http 降级跳转；
        // 其余重定向沿用 WinHTTP 默认（自动跟随、跨主机不重放凭据类 Authorization）。
        WinHttpSetOption(
            Some(hrequest.0),
            WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP.to_le_bytes()),
        )
        .map_err(|e| anyhow!("NETWORK_FAILED: 设置重定向策略失败: {e}"))?;
        remaining_and_set_timeouts(hrequest.0, start, opts.budget)?;

        if !req.headers.is_empty() {
            // 注意：本机 WinHTTP 对"尾随 \r\n"的头串返回 E_INVALIDARG（探针实测），
            // 因此只用 "\r\n" 作头间分隔、末尾不补换行。
            let mut line = String::new();
            for (i, (name, value)) in req.headers.iter().enumerate() {
                if i > 0 {
                    line.push_str("\r\n");
                }
                line.push_str(name);
                line.push_str(": ");
                line.push_str(value);
            }
            let mut header_w: Vec<u16> = line.encode_utf16().collect();
            header_w.push(0);
            WinHttpAddRequestHeaders(
                hrequest.0,
                &header_w,
                WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
            )
            .map_err(|e| anyhow!("INVALID_ARGS: 请求头非法: {e}"))?;
        }

        let body_bytes = req.body.as_deref().map(str::as_bytes);
        remaining_and_set_timeouts(hrequest.0, start, opts.budget)?;
        WinHttpSendRequest(
            hrequest.0,
            None,
            body_bytes.map(|b| b.as_ptr() as *const core::ffi::c_void),
            body_bytes.map_or(0, |b| b.len() as u32),
            body_bytes.map_or(0, |b| b.len() as u32),
            0,
        )
        .map_err(|e| anyhow!("NETWORK_FAILED: 发送请求失败: {e}"))?;

        remaining_and_set_timeouts(hrequest.0, start, opts.budget)?;
        WinHttpReceiveResponse(hrequest.0, std::ptr::null_mut())
            .map_err(|e| anyhow!("NETWORK_FAILED: 等待响应失败: {e}"))?;

        let status = query_status(hrequest.0)?;
        let headers = query_headers(hrequest.0)?;

        let mut body: Vec<u8> = Vec::new();
        loop {
            remaining_and_set_timeouts(hrequest.0, start, opts.budget)?;
            let mut avail: u32 = 0;
            WinHttpQueryDataAvailable(hrequest.0, &mut avail)
                .map_err(|e| anyhow!("NETWORK_FAILED: 查询数据量失败: {e}"))?;
            if avail == 0 {
                break;
            }
            let want = (avail as usize).min(READ_CHUNK);
            let mut chunk = vec![0u8; want];
            let mut got: u32 = 0;
            WinHttpReadData(
                hrequest.0,
                chunk.as_mut_ptr() as *mut _,
                want as u32,
                &mut got,
            )
            .map_err(|e| anyhow!("NETWORK_FAILED: 读取响应失败: {e}"))?;
            if got == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..got as usize]);
            if body.len() > opts.max_body {
                bail!("NETWORK_FAILED: 响应体超过 {} 字节上限", opts.max_body);
            }
        }

        Ok(NetResponse {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    }
}

fn query_status(hrequest: *mut core::ffi::c_void) -> Result<u16> {
    unsafe {
        let mut status: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            hrequest,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            std::ptr::null_mut(),
        )
        .map_err(|e| anyhow!("NETWORK_FAILED: 读取状态码失败: {e}"))?;
        u16::try_from(status).map_err(|_| anyhow!("NETWORK_FAILED: 状态码越界 {status}"))
    }
}

/// RAW_HEADERS_CRLF → `BTreeMap`（键小写；同名值以 ", " 合并；无冒号行跳过）。
fn query_headers(hrequest: *mut core::ffi::c_void) -> Result<BTreeMap<String, String>> {
    unsafe {
        let mut len: u32 = 0;
        let probe = WinHttpQueryHeaders(
            hrequest,
            WINHTTP_QUERY_RAW_HEADERS_CRLF,
            PCWSTR::null(),
            None,
            &mut len,
            std::ptr::null_mut(),
        );
        if probe.is_err() && len == 0 {
            return Err(anyhow!("NETWORK_FAILED: 读取响应头失败: {probe:?}"));
        }
        let mut buf = vec![0u16; (len as usize / 2).max(1)];
        let mut len2 = len;
        WinHttpQueryHeaders(
            hrequest,
            WINHTTP_QUERY_RAW_HEADERS_CRLF,
            PCWSTR::null(),
            Some(buf.as_mut_ptr() as *mut _),
            &mut len2,
            std::ptr::null_mut(),
        )
        .map_err(|e| anyhow!("NETWORK_FAILED: 读取响应头失败: {e}"))?;
        let raw = String::from_utf16_lossy(&buf[..len2 as usize / 2]);

        let mut map = BTreeMap::new();
        for line in raw.split("\r\n").skip(1) {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            if name.is_empty() {
                continue;
            }
            let value = value.trim().to_string();
            map.entry(name)
                .and_modify(|prev: &mut String| {
                    prev.push_str(", ");
                    prev.push_str(&value);
                })
                .or_insert(value);
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// 起一个单连接回环 HTTP 服务，返回端口号。`handler` 拿到已连接流后
    /// 自行读写（可断言请求、可延迟应答）；连接处理在 detached 线程。
    fn spawn_server(handler: impl FnOnce(std::net::TcpStream) + Send + 'static) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                handler(stream);
            }
        });
        port
    }

    /// 读完整 HTTP 请求（头 + 按 Content-Length 的体）并转为字符串。
    fn read_request(mut stream: std::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = stream.read(&mut byte).unwrap();
            if n == 0 {
                break;
            }
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&buf).to_string();
        let content_length = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if content_length > 0 {
            let mut body = vec![0u8; content_length];
            stream.read_exact(&mut body).unwrap();
            return head + &String::from_utf8_lossy(&body);
        }
        head
    }

    fn tiny_opts() -> NetOpts {
        NetOpts {
            budget: Duration::from_secs(5),
            max_body: MAX_RESPONSE_BODY,
        }
    }

    fn req_for(url: String, init: serde_json::Value) -> NetRequest {
        parse_request(&json!({ "url": url, "init": init })).unwrap()
    }

    // ── 校验（纯函数，零网络） ─────────────────────────────────────────────

    #[test]
    fn parse_defaults_and_normalization() {
        let r = parse_request(&json!({ "url": "https://example.com/a" })).unwrap();
        assert_eq!(r.method, "GET");
        assert!(r.body.is_none());
        let r =
            parse_request(&json!({ "url": "http://example.com", "init": { "method": "post" } }))
                .unwrap();
        assert_eq!(r.method, "POST");
    }

    #[test]
    fn parse_url_variants() {
        assert_eq!(
            parse_url("http://e.com").unwrap(),
            (false, "e.com".into(), 80, "/".into())
        );
        assert_eq!(
            parse_url("HTTPS://e.com:8443/x").unwrap(),
            (true, "e.com".into(), 8443, "/x".into())
        );
        // query 保留、fragment 剥离；"host?query" 无斜杠时补为 "/?query"。
        assert_eq!(
            parse_url("http://e.com/p?b=1#top").unwrap(),
            (false, "e.com".into(), 80, "/p?b=1".into())
        );
        assert_eq!(
            parse_url("http://e.com?b=1").unwrap(),
            (false, "e.com".into(), 80, "/?b=1".into())
        );
        assert_eq!(
            parse_url("http://[::1]:8080/x").unwrap(),
            (false, "::1".into(), 8080, "/x".into())
        );
        assert_eq!(
            parse_url("http://[2001:db8::1]/").unwrap(),
            (false, "2001:db8::1".into(), 80, "/".into())
        );
    }

    #[test]
    fn parse_rejects_bad_input() {
        for args in [
            json!({}),
            json!({ "url": "" }),
            json!({ "url": 42 }),
            json!({ "url": "example.com/no-scheme" }),
            json!({ "url": "ftp://example.com/x" }),
            json!({ "url": "http://例.com" }),
            json!({ "url": "http://user@example.com/x" }),
            json!({ "url": "http://e.com:0/x" }),
            json!({ "url": "http://e.com:99999/x" }),
            json!({ "url": "http://e.com/x", "init": "post" }),
            json!({ "url": "http://e.com/x", "init": { "method": "TRACE" } }),
            json!({ "url": "http://e.com/x", "init": { "method": 1 } }),
            json!({ "url": "http://e.com/x", "init": { "body": "b" } }),
            json!({ "url": "http://e.com/x", "init": { "method": "HEAD", "body": "b" } }),
            json!({ "url": "http://e.com/x", "init": { "body": 7 } }),
            json!({ "url": "http://e.com/x", "init": { "headers": "x" } }),
            json!({ "url": "http://e.com/x", "init": { "headers": { "X": 1 } } }),
            json!({ "url": "http://e.com/x", "init": { "headers": { "X\r\nEvil": "v" } } }),
            json!({ "url": "http://e.com/x", "init": { "headers": { "X": "v\r\nEvil: 1" } } }),
            json!({ "url": "http://e.com/x", "init": { "headers": { "": "v" } } }),
            json!({ "url": "http://e.com/x\r\nHost: evil" }),
            json!({ "url": "http://e.com/\0zero" }),
        ] {
            let err = parse_request(&args).unwrap_err();
            assert!(err.to_string().contains("INVALID_ARGS"), "{args}: {err}");
        }
        // init: null 与 init 缺省等价。
        parse_request(&json!({ "url": "http://e.com/x", "init": null })).unwrap();
    }

    #[test]
    fn parse_filters_managed_headers_and_caps_body() {
        let r = parse_request(&json!({ "url": "http://e.com/x", "init": {
            "method": "POST",
            "headers": { "Host": "evil.example", "Content-Length": "999", "content-length": "1",
                         "Transfer-Encoding": "chunked", "Connection": "keep-alive",
                         "upgrade": "websocket", "X-Ok": "y" },
            "body": "b",
        } }))
        .unwrap();
        // 帧控/hop-by-hop 头全部托管（防 CL+TE 走私），只剩业务头。
        assert_eq!(r.headers, vec![("X-Ok".to_string(), "y".to_string())]);
        let big = "x".repeat(MAX_REQUEST_BODY + 1);
        let err = parse_request(&json!({ "url": "http://e.com/x", "init": {
            "method": "POST", "body": big,
        } }))
        .unwrap_err();
        assert!(err.to_string().contains("INVALID_ARGS"), "{err}");
        // 恰好等于上限允许。
        let ok = "x".repeat(MAX_REQUEST_BODY);
        parse_request(
            &json!({ "url": "http://e.com/x", "init": { "method": "POST", "body": ok } }),
        )
        .unwrap();
        // 头数量/单头字节上限；头名值拒 NUL（会截断 WinHTTP 宽字符串参数）。
        let mut many = serde_json::Map::new();
        for i in 0..MAX_HEADERS + 5 {
            many.insert(format!("X-Bulk{i}"), json!("v"));
        }
        let args = json!({ "url": "http://e.com/x", "init": { "headers": many } });
        let err = parse_request(&args).unwrap_err();
        assert!(err.to_string().contains("INVALID_ARGS"), "{err}");
        let args = json!({ "url": "http://e.com/x",
            "init": { "headers": { "X-Big": "x".repeat(MAX_HEADER_BYTES + 1) } } });
        let err = parse_request(&args).unwrap_err();
        assert!(err.to_string().contains("INVALID_ARGS"), "{err}");
        let args = json!({ "url": "http://e.com/x", "init": { "headers": { "X\0N": "v" } } });
        let err = parse_request(&args).unwrap_err();
        assert!(err.to_string().contains("INVALID_ARGS"), "{err}");
        let args = json!({ "url": "http://e.com/x", "init": { "headers": { "X-N": "v\0" } } });
        let err = parse_request(&args).unwrap_err();
        assert!(err.to_string().contains("INVALID_ARGS"), "{err}");
    }

    // ── 回环集成（真实 WinHTTP 路径，127.0.0.1，不依赖外网） ────────────────

    #[test]
    fn http_200_roundtrip() {
        let port = spawn_server(|mut s| {
            let _ = read_request(s.try_clone().unwrap());
            s.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-Test: a\r\nX-Multi: v1\r\nX-Multi: v2\r\n\r\nhello net",
            )
            .unwrap();
        });
        let req = req_for(format!("http://127.0.0.1:{port}/x?a=b"), json!({}));
        let resp = execute_with(&req, &tiny_opts()).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "hello net");
        assert_eq!(resp.headers.get("content-type").unwrap(), "text/plain");
        assert_eq!(resp.headers.get("x-test").unwrap(), "a");
        // 同名响应头合并。
        assert_eq!(resp.headers.get("x-multi").unwrap(), "v1, v2");
        // 序列化即回包形状：camelCase bodyText + headers 对象。
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["status"], 200);
        assert_eq!(v["bodyText"], "hello net");
        assert_eq!(v["headers"]["content-type"], "text/plain");
    }

    #[test]
    fn post_body_and_headers_reach_server() {
        let port = spawn_server(|mut s| {
            let req = read_request(s.try_clone().unwrap());
            let lc = req.to_ascii_lowercase();
            let ok = req.contains("POST /echo HTTP/1.1")
                && req.contains("X-Spark: on")
                && req.contains("body=abc")
                // 插件覆写 Host/Content-Length 已被过滤：全请求只有一个 host 行。
                && lc.matches("host:").count() == 1
                && lc.matches("content-length:").count() == 1
                // 帧控/hop-by-hop 托管头不得由插件上 wire（防 CL+TE 走私）：
                // TE/Upgrade 零出现；Connection 仅 WinHTTP 自加的一行
                // （实测 WinHTTP 会自动发 "Connection: Keep-Alive"）。
                && !lc.contains("transfer-encoding:")
                && !lc.contains("upgrade:")
                && lc.matches("connection:").count() == 1;
            let body: &[u8] = if ok { b"ok" } else { b"bad" };
            s.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
            )
            .unwrap();
            s.write_all(body).unwrap();
        });
        let req = req_for(
            format!("http://127.0.0.1:{port}/echo"),
            json!({
                "method": "POST",
                "headers": { "X-Spark": "on", "Host": "evil.example", "Content-Length": "999",
                             "Transfer-Encoding": "chunked", "Connection": "keep-alive",
                             "Upgrade": "websocket" },
                "body": "body=abc"
            }),
        );
        assert!(!req
            .headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("host")));
        assert!(!req
            .headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("content-length")));
        assert!(!req
            .headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("transfer-encoding")));
        let resp = execute_with(&req, &tiny_opts()).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "ok");
    }

    #[test]
    fn status_404_passthrough() {
        let port = spawn_server(|mut s| {
            let _ = read_request(s.try_clone().unwrap());
            s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let req = req_for(format!("http://127.0.0.1:{port}/gone"), json!({}));
        let resp = execute_with(&req, &tiny_opts()).unwrap();
        assert_eq!(resp.status, 404);
        assert_eq!(resp.body, "");
    }

    #[test]
    fn timeout_on_slow_response() {
        let port = spawn_server(|s| {
            let _ = read_request(s.try_clone().unwrap());
            std::thread::sleep(Duration::from_millis(2000));
            let mut s = s;
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nz");
        });
        let req = req_for(format!("http://127.0.0.1:{port}/slow"), json!({}));
        let opts = NetOpts {
            budget: Duration::from_millis(400),
            max_body: MAX_RESPONSE_BODY,
        };
        let err = execute_with(&req, &opts).unwrap_err();
        assert!(err.to_string().contains("NETWORK_FAILED"), "{err}");
    }

    #[test]
    fn response_too_large_rejected() {
        let port = spawn_server(|mut s| {
            let _ = read_request(s.try_clone().unwrap());
            s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\n").as_bytes())
                .unwrap();
            s.write_all(&vec![b'x'; 4096]).unwrap();
        });
        let req = req_for(format!("http://127.0.0.1:{port}/big"), json!({}));
        let opts = NetOpts {
            budget: Duration::from_secs(5),
            max_body: 100,
        };
        let err = execute_with(&req, &opts).unwrap_err();
        assert!(err.to_string().contains("NETWORK_FAILED"), "{err}");
        assert!(err.to_string().contains("上限"), "{err}");
    }

    #[test]
    fn closed_port_reports_network_failed() {
        // 绑定后立即释放端口，再对它发起请求 → 连接失败。
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let req = req_for(format!("http://127.0.0.1:{port}/x"), json!({}));
        let err = execute_with(&req, &tiny_opts()).unwrap_err();
        assert!(err.to_string().contains("NETWORK_FAILED"), "{err}");
    }
}
