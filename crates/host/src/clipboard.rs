//! Win32 剪贴板读写（`spark.clipboard.*` 桥到 host 执行）。
//!
//! - 文本：CF_UNICODETEXT。
//! - 图片：读走 CF_DIBV5/CF_DIB → RGBA → WIC PNG 编码 → base64；写走
//!   base64 PNG → WIC 解码 → BGRA → CF_DIB（BITMAPINFOHEADER + 32bpp BI_RGB）。
//! - 剪贴板里没有图片、或图片是暂不支持的像素格式时返回 `null`
//!   （插件按"无图片"处理，不当作错误）。
//! - `read_item`/`write_item` 支撑 `spark.clipboard.read()/write()`（规范 §8.3）：
//!   一次打开剪贴板读/写全部支持格式，避免分次调用之间被别的进程改动而读到
//!   "半新半旧"的组合。

use anyhow::{anyhow, bail, Result};
use base64::Engine;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows::Win32::System::Ole::{CF_DIB, CF_UNICODETEXT};

/// `clipboard.write` 文本上限（UTF-8 字节；规范 §8.3 单次内容 10MB）。
pub const MAX_TEXT_BYTES: usize = 10 * 1024 * 1024;
/// `clipboard.write` 传入的 base64 字符串上限（解码后不得超过 [`MAX_IMAGE_BYTES`]）。
/// base64 膨胀率 4/3，留一点余量：允许比解码上限略长的输入，由解码后校验兜底。
const MAX_IMAGE_BASE64_LEN: usize = MAX_IMAGE_BYTES / 3 * 4 + 4096;
/// `clipboard.write` PNG 字节上限。
pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
/// 图片边长上限（防 base64 里塞超大地图撑爆内存）。
const MAX_IMAGE_EDGE: u32 = 16384;
/// 解码后像素缓冲上限（宽 × 高 × 4 字节）。
const MAX_IMAGE_PIXEL_BYTES: u64 = 64 * 1024 * 1024;

/// 剪贴板快照（`spark.clipboard.read`）。两种格式都是一次打开剪贴板内读到的，
/// 互相之间不会出现"半新半旧"。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipboardItem {
    /// 纯文本（`text/plain`）；剪贴板无文本时 `None`。
    pub text: Option<String>,
    /// PNG（`image/png`，base64，**不含** `data:` 前缀）；无图片/格式不支持时 `None`。
    pub image_png_base64: Option<String>,
}

impl ClipboardItem {
    /// 可用格式列表（`text/plain` / `image/png`），顺序固定便于插件与测试断言。
    pub fn types(&self) -> Vec<&'static str> {
        let mut t = Vec::new();
        if self.text.is_some() {
            t.push("text/plain");
        }
        if self.image_png_base64.is_some() {
            t.push("image/png");
        }
        t
    }
}

/// 打开剪贴板（带有限重试）。
///
/// `OpenClipboard` 是**独占**语义：只要别的进程/线程开着剪贴板，本次调用立刻失败
/// （`ERROR_ACCESS_DENIED` / 0x80070005）。这不是异常情况而是 Windows 剪贴板的常态——
/// 用户刚按过 Ctrl+C、有剪贴板工具在跑、甚至另一个 Spark 窗口刚写完都撞得上。
/// 官方建议即"失败后重试"，故做 10 次 × 10ms（最多 100ms）的有界重试后再放弃：
/// 有界是关键——无限等待会把 IPC 线程钉死（虽然已在 host 锁外，仍会拖住该连接
/// 上后续所有请求）。
fn open_clipboard() -> Result<()> {
    const ATTEMPTS: u32 = 10;
    let mut last: Option<windows::core::Error> = None;
    for attempt in 0..ATTEMPTS {
        match unsafe { OpenClipboard(None) } {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                if attempt + 1 < ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
    }
    Err(last.expect("ATTEMPTS >= 1，循环至少执行一次").into())
}

/// 读取剪贴板文本。空或非文本返回空串。
pub fn read_text() -> Result<String> {
    open_clipboard()?;
    let res = read_text_opened();
    unsafe {
        let _ = CloseClipboard();
    }
    res
}

/// 剪贴板已打开时的文本读取（`spark.clipboard.read` 复用它一次打开读多种格式）。
fn read_text_opened() -> Result<String> {
    unsafe {
        let handle = match GetClipboardData(CF_UNICODETEXT.0 as u32) {
            Ok(h) => h,
            Err(_) => return Ok(String::new()),
        };
        // GetClipboardData 返回 HANDLE；CF_UNICODETEXT 的底层是 HGLOBAL，
        // 用相同裸指针构造 HGLOBAL 传给 GlobalLock/GlobalSize/GlobalUnlock。
        let hglobal = HGLOBAL(handle.0);
        let ptr = GlobalLock(hglobal) as *const u16;
        if ptr.is_null() {
            return Ok(String::new());
        }
        let size = GlobalSize(hglobal);
        let len = size / 2;
        let slice = std::slice::from_raw_parts(ptr, len);
        let s = String::from_utf16_lossy(slice);
        let _ = GlobalUnlock(hglobal);
        Ok(s.trim_end_matches('\0').to_string())
    }
}

/// 写入剪贴板文本。
///
/// 资源管理：`GlobalAlloc` 分配的 `HGLOBAL` 在所有"未成功转移所有权给剪贴板"
/// 的路径上必须 `GlobalFree`；仅当 `SetClipboardData` 成功时所有权转移、不再释放。
pub fn write_text(text: &str) -> Result<()> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0); // 结尾 NUL
    let bytes = wide.len() * 2;
    unsafe {
        let hglobal = GlobalAlloc(GMEM_MOVEABLE, bytes)?;
        // 从此处起 hglobal 需要回收，除非 SetClipboardData 成功转移所有权。

        let ptr = GlobalLock(hglobal) as *mut u16;
        if ptr.is_null() {
            let _ = GlobalFree(Some(hglobal));
            return Err(anyhow!("GlobalLock returned null"));
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
        let _ = GlobalUnlock(hglobal);

        if let Err(e) = open_clipboard() {
            let _ = GlobalFree(Some(hglobal));
            return Err(e);
        }
        // clipboard 已打开：SetClipboardData 成功 → 所有权转移；失败 → 需回收。
        let transferred = (|| -> Result<()> {
            EmptyClipboard()?;
            let handle = HANDLE(hglobal.0);
            SetClipboardData(CF_UNICODETEXT.0 as u32, Some(handle))?;
            Ok(())
        })();
        let _ = CloseClipboard();
        if transferred.is_err() {
            let _ = GlobalFree(Some(hglobal));
        }
        transferred
    }
}

/// 写入剪贴板（`spark.clipboard.write`，规范 §8.3）：文本与图片可**同时**写入——
/// 两种格式都放上剪贴板、由接收方自选，这是标准剪贴板行为（不是"二选一"）。
///
/// 资源管理同 [`write_text`]：每个 `HGLOBAL` 在"未成功转移所有权给剪贴板"的路径上
/// 必须 `GlobalFree`。注意 `EmptyClipboard` 之后无法回滚：若第一个格式已成功写入、
/// 第二个失败，剪贴板会停在"只有第一个格式"并返回 Err——这是剪贴板 API 的固有限制，
/// 不做原子性伪装。参数校验与 PNG 解码都在打开剪贴板**之前**做完，因此常见失败
/// （格式非法、体积超限）不会先清空用户剪贴板。
pub fn write_item(text: Option<&str>, image_png_base64: Option<&str>) -> Result<()> {
    if text.is_none() && image_png_base64.is_none() {
        bail!("INVALID_ARGS: clipboard.write 需要 text 或 imagePng 至少一项");
    }
    if let Some(t) = text {
        if t.len() > MAX_TEXT_BYTES {
            bail!(
                "FILE_TOO_LARGE: clipboard.write 文本 {} 字节，超过上限 {MAX_TEXT_BYTES} 字节",
                t.len()
            );
        }
    }
    let dib = match image_png_base64 {
        Some(b64) => Some(png_base64_to_dib(b64)?),
        None => None,
    };

    // CF_UNICODETEXT 的字节视图（UTF-16LE + 结尾 NUL）。
    let mut wide_le: Vec<u8> = Vec::new();
    if let Some(t) = text {
        wide_le.reserve((t.len() + 1) * 2);
        for unit in t.encode_utf16() {
            wide_le.extend_from_slice(&unit.to_le_bytes());
        }
        wide_le.extend_from_slice(&0u16.to_le_bytes());
    }

    unsafe {
        // 先把所有块备好再动剪贴板：分配失败时剪贴板内容完好无损。
        let text_block = if text.is_some() {
            Some(global_from_bytes(&wide_le)?)
        } else {
            None
        };
        let dib_block = match dib.as_deref() {
            Some(bytes) => match global_from_bytes(bytes) {
                Ok(h) => Some(h),
                Err(e) => {
                    if let Some(h) = text_block {
                        let _ = GlobalFree(Some(h));
                    }
                    return Err(e);
                }
            },
            None => None,
        };

        if let Err(e) = open_clipboard() {
            if let Some(h) = text_block {
                let _ = GlobalFree(Some(h));
            }
            if let Some(h) = dib_block {
                let _ = GlobalFree(Some(h));
            }
            return Err(e);
        }

        let mut text_taken = false;
        let mut dib_taken = false;
        let res = (|| -> Result<()> {
            EmptyClipboard()?;
            if let Some(h) = text_block {
                SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(h.0)))?;
                text_taken = true; // 所有权已归剪贴板，下面不再释放
            }
            if let Some(h) = dib_block {
                SetClipboardData(CF_DIB.0 as u32, Some(HANDLE(h.0)))?;
                dib_taken = true;
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        if !text_taken {
            if let Some(h) = text_block {
                let _ = GlobalFree(Some(h));
            }
        }
        if !dib_taken {
            if let Some(h) = dib_block {
                let _ = GlobalFree(Some(h));
            }
        }
        res
    }
}

/// 把字节拷进一个新建的 `GMEM_MOVEABLE` 块（剪贴板数据的标准载体）。
/// 返回块的所有权归调用方，直到被 `SetClipboardData` 收走。
unsafe fn global_from_bytes(bytes: &[u8]) -> Result<HGLOBAL> {
    let hglobal = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
    let ptr = GlobalLock(hglobal) as *mut u8;
    if ptr.is_null() {
        let _ = GlobalFree(Some(hglobal));
        return Err(anyhow!("GlobalLock returned null"));
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
    let _ = GlobalUnlock(hglobal);
    Ok(hglobal)
}

/// base64 PNG → CF_DIB 字节块。解码/尺寸/体积任一不合法即 `Err`；错误信息以
/// 既有约定前缀开头（`INVALID_ARGS` / `FILE_TOO_LARGE` / `UNSUPPORTED_FORMAT`），
/// UI 侧 `ClassifyError` 无需改动即可识别。
fn png_base64_to_dib(b64: &str) -> Result<Vec<u8>> {
    if b64.len() > MAX_IMAGE_BASE64_LEN {
        bail!(
            "FILE_TOO_LARGE: imagePng base64 长度 {} 超过上限 {MAX_IMAGE_BASE64_LEN}",
            b64.len()
        );
    }
    let png = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| anyhow!("INVALID_ARGS: imagePng 不是合法 base64：{e}"))?;
    if png.len() > MAX_IMAGE_BYTES {
        bail!(
            "FILE_TOO_LARGE: imagePng 解码后 {} 字节，超过上限 {MAX_IMAGE_BYTES} 字节",
            png.len()
        );
    }
    let (w, h, bgra) = decode_png_bgra(&png)
        .map_err(|e| anyhow!("UNSUPPORTED_FORMAT: imagePng 无法解码为位图：{e}"))?;
    bgra_to_dib(w, h, &bgra)
}

/// 自顶向下 BGRA 像素 → CF_DIB 字节块（BITMAPINFOHEADER + 自底向上像素行）。
///
/// 用最通用的 40 字节头 + 32bpp `BI_RGB`：新旧应用通吃。`BI_RGB` 的 32bpp 没有
/// 定义 alpha 语义（要 alpha 得用 CF_DIBV5），但读取端 `dib_to_rgba` 对"alpha 全 0"
/// 的 32bpp 会强制不透明，正好覆盖这里的输出，读写往返一致。32bpp 每行天然 4 字节
/// 对齐，无需行填充。
fn bgra_to_dib(w: u32, h: u32, bgra_top_down: &[u8]) -> Result<Vec<u8>> {
    let stride = (w as usize) * 4;
    if w == 0 || h == 0 || bgra_top_down.len() < stride * (h as usize) {
        bail!("位图尺寸与像素缓冲不匹配（{w}x{h}）");
    }
    let mut out = Vec::with_capacity(40 + stride * h as usize);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize = BITMAPINFOHEADER
    out.extend_from_slice(&(w as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(h as i32).to_le_bytes()); // biHeight 正数 = 自底向上
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
                                                // biSizeImage = 整幅像素区大小（stride × 行数）：单行 stride 值是常见错填，
                                                // 严格解析该字段的接收方（部分图像库/办公软件）会拒绝或错读这张 DIB。
                                                // 图片尺寸已在上游按 MAX_IMAGE_PIXEL_BYTES 设卡，u32 不会溢出。
    out.extend_from_slice(&((stride as u64 * h as u64) as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes()); // biXPelsPerMeter（72dpi）
    out.extend_from_slice(&2835i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
    debug_assert_eq!(out.len(), 40);

    // CF_DIB 是自底向上，WIC 给的是自顶向下 → 逐行倒序拷贝。
    for row in (0..h as usize).rev() {
        out.extend_from_slice(&bgra_top_down[row * stride..(row + 1) * stride]);
    }
    Ok(out)
}

/// PNG 字节 → (宽, 高, 自顶向下 BGRA 像素)。尺寸/内存超限直接拒绝：
/// base64 输入的大小上限挡不住"小体积高压缩比图片解出巨量像素"这种情况，
/// 必须在解码拿到真实尺寸后再按像素量设卡。
fn decode_png_bgra(png: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    with_com(|| decode_png_bgra_inner(png))
}

fn decode_png_bgra_inner(png: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICImagingFactory, IWICPalette,
        WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER, STREAM_SEEK_SET};

    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| anyhow!("WIC factory: {e}"))?;

        // PNG 字节喂进内存流（CreateStreamOnHGlobal 传空句柄自动分配、随流释放）。
        let stream = CreateStreamOnHGlobal(HGLOBAL(std::ptr::null_mut()), true)
            .map_err(|e| anyhow!("mem stream: {e}"))?;
        let hr = stream.Write(
            png.as_ptr() as *const core::ffi::c_void,
            png.len() as u32,
            None,
        );
        if hr.is_err() {
            bail!("写入内存流失败: {hr:?}");
        }
        stream
            .Seek(0, STREAM_SEEK_SET, None)
            .map_err(|e| anyhow!("stream seek: {e}"))?;

        let decoder = factory
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .map_err(|e| anyhow!("WIC 解码器: {e}"))?;
        let frame = decoder.GetFrame(0).map_err(|e| anyhow!("WIC 帧: {e}"))?;

        let mut w = 0u32;
        let mut h = 0u32;
        frame
            .GetSize(&mut w, &mut h)
            .map_err(|e| anyhow!("WIC 尺寸: {e}"))?;
        if w == 0 || h == 0 {
            bail!("图像尺寸为 0");
        }
        let pixel_bytes = (w as u64) * (h as u64) * 4;
        if w > MAX_IMAGE_EDGE || h > MAX_IMAGE_EDGE || pixel_bytes > MAX_IMAGE_PIXEL_BYTES {
            bail!("图像尺寸超限（{w}x{h}）");
        }

        // 统一转 32bppBGRA：后续 CF_DIB 就按这个像素序原样落盘。
        let converter = factory
            .CreateFormatConverter()
            .map_err(|e| anyhow!("WIC 转换器: {e}"))?;
        converter
            .Initialize(
                &frame,
                &GUID_WICPixelFormat32bppBGRA,
                WICBitmapDitherTypeNone,
                None::<&IWICPalette>,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .map_err(|e| anyhow!("WIC 转换: {e}"))?;

        let mut buf = vec![0u8; pixel_bytes as usize];
        converter
            .CopyPixels(std::ptr::null(), w * 4, &mut buf)
            .map_err(|e| anyhow!("WIC 取像素: {e}"))?;
        Ok((w, h, buf))
    }
}

/// 读取剪贴板图片为 base64 PNG。无图片/不支持格式返回 None（→ 插件拿 null）。
pub fn read_image_png_base64() -> Result<Option<String>> {
    open_clipboard()?;
    let res = read_image_opened();
    unsafe {
        let _ = CloseClipboard();
    }
    res
}

/// 剪贴板已打开时的图片读取（`spark.clipboard.read` 复用它一次打开读多种格式）。
fn read_image_opened() -> Result<Option<String>> {
    use windows::Win32::System::Ole::CF_DIBV5;

    unsafe {
        // 优先 CF_DIBV5（带 alpha 通道语义），退 CF_DIB（旧应用常见）。
        let handle = match GetClipboardData(CF_DIBV5.0 as u32) {
            Ok(h) => h,
            Err(_) => match GetClipboardData(CF_DIB.0 as u32) {
                Ok(h) => h,
                Err(_) => return Ok(None), // 剪贴板没有图片
            },
        };
        let hglobal = HGLOBAL(handle.0);
        let ptr = GlobalLock(hglobal);
        if ptr.is_null() {
            return Ok(None);
        }
        let size = GlobalSize(hglobal);
        let bytes = std::slice::from_raw_parts(ptr as *const u8, size);
        let rgba = match dib_to_rgba(bytes) {
            Some(r) => r,
            None => {
                let _ = GlobalUnlock(hglobal);
                return Ok(None); // 暂不支持的像素格式 → 按无图片处理
            }
        };
        let _ = GlobalUnlock(hglobal);
        if rgba.2.is_empty() || rgba.0 == 0 || rgba.1 == 0 {
            return Ok(None);
        }
        let png = encode_png(rgba.0, rgba.1, &rgba.2)?;
        Ok(Some(base64::engine::general_purpose::STANDARD.encode(png)))
    }
}

/// 读剪贴板快照：一次打开剪贴板读全部支持格式（`spark.clipboard.read`，规范 §8.3）。
///
/// 为什么坚持"一次打开"：`OpenClipboard`/`CloseClipboard` 之间剪贴板被独占，分两次
/// 调用读文本与图片时，中间窗口期别的进程可以改掉剪贴板内容，插件会拿到
/// "文本来自 A、图片来自 B"的错配组合。空文本归一化为 `None`（与 `types` 语义一致）。
pub fn read_item() -> Result<ClipboardItem> {
    open_clipboard()?;
    let res = (|| -> Result<ClipboardItem> {
        let text = read_text_opened()?;
        let image = read_image_opened()?;
        Ok(ClipboardItem {
            text: if text.is_empty() { None } else { Some(text) },
            image_png_base64: image,
        })
    })();
    unsafe {
        let _ = CloseClipboard();
    }
    res
}

/// 剪贴板请求：`host.plugin.api` capability=`clipboard` 的锁外执行体。
///
/// 分两段是锁序纪律（与 net/fs/shell/rpc 同）：`parse_request` 在 host 锁内跑，
/// **只做参数校验、零 IO**；真正的 Win32 剪贴板访问与 WIC 编解码（图片路径可达
/// 数十毫秒）由 ipc_server 放锁后调 `execute`——否则一次读图会冻结全部 IPC，
/// 包括主窗口搜索的 host.query 热路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardRequest {
    ReadText,
    WriteText(String),
    ReadImage,
    ReadItem,
    WriteItem {
        text: Option<String>,
        image_png: Option<String>,
    },
}

/// 解析 `clipboard` capability 的方法与参数（锁内：纯校验，不碰剪贴板）。
pub fn parse_request(method: &str, args: &serde_json::Value) -> Result<ClipboardRequest> {
    let arg_str = |key: &str| -> Result<String> {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("INVALID_ARGS: clipboard.{method} 需要字符串字段 {key}"))
    };

    match method {
        "read_text" => Ok(ClipboardRequest::ReadText),
        "write_text" => {
            let text = arg_str("text")?;
            // 与 write_item 路径同口径（规范 §8.3 单次内容 10MB）：parse 阶段
            // 拒绝，避免超长文本进到锁外 execute 才在 GlobalAlloc 前的 UTF-16
            // 放大（×2）上失败，白打一次内存尖峰。
            if text.len() > MAX_TEXT_BYTES {
                bail!(
                    "FILE_TOO_LARGE: clipboard.writeText 文本 {} 字节，超过上限 {MAX_TEXT_BYTES} 字节",
                    text.len()
                );
            }
            Ok(ClipboardRequest::WriteText(text))
        }
        "read_image" => Ok(ClipboardRequest::ReadImage),
        "read_all" => Ok(ClipboardRequest::ReadItem),
        "write_item" => {
            let obj = args
                .as_object()
                .ok_or_else(|| anyhow!("INVALID_ARGS: clipboard.write 需要 object 参数"))?;
            let field = |key: &str| -> Result<Option<String>> {
                match obj.get(key) {
                    None | Some(serde_json::Value::Null) => Ok(None),
                    Some(v) => v.as_str().map(|s| Some(s.to_string())).ok_or_else(|| {
                        anyhow!("INVALID_ARGS: clipboard.write 的 {key} 必须为字符串")
                    }),
                }
            };
            let text = field("text")?;
            let image_png = field("image_png")?;
            if text.is_none() && image_png.is_none() {
                bail!("INVALID_ARGS: clipboard.write 需要 text 或 imagePng 至少一项");
            }
            if let Some(t) = &text {
                if t.len() > MAX_TEXT_BYTES {
                    bail!(
                        "FILE_TOO_LARGE: clipboard.write 文本 {} 字节，超过上限 {MAX_TEXT_BYTES} 字节",
                        t.len()
                    );
                }
            }
            Ok(ClipboardRequest::WriteItem { text, image_png })
        }
        other => bail!("INVALID_ARGS: clipboard method {other}"),
    }
}

/// 执行剪贴板请求（**锁外**调用），返回与 preload 约定的 JSON 形状。
pub fn execute(req: &ClipboardRequest) -> Result<serde_json::Value> {
    match req {
        ClipboardRequest::ReadText => Ok(serde_json::json!({ "text": read_text()? })),
        ClipboardRequest::WriteText(text) => {
            write_text(text)?;
            Ok(serde_json::json!({ "ok": true }))
        }
        ClipboardRequest::ReadImage => Ok(serde_json::json!({ "data": read_image_png_base64()? })),
        ClipboardRequest::ReadItem => {
            let item = read_item()?;
            Ok(serde_json::json!({
                "types": item.types(),
                "text": item.text,
                "image_png": item.image_png_base64,
            }))
        }
        ClipboardRequest::WriteItem { text, image_png } => {
            write_item(text.as_deref(), image_png.as_deref())?;
            Ok(serde_json::json!({ "ok": true }))
        }
    }
}

/// 解析 DIB 位图信息并转 RGBA（自顶向下、无行填充）。返回 (宽, 高, RGBA)。
/// 支持 32bpp（BI_RGB/BI_BITFIELDS/V4/V5 掩码，含 alpha）、24bpp、8bpp 调色板；
/// 其他格式（16bpp 等）返回 None → 插件按"无图片"处理。
fn dib_to_rgba(dib: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if dib.len() < 40 {
        return None;
    }
    let u16_at = |o: usize| -> u32 { u16::from_le_bytes([dib[o], dib[o + 1]]) as u32 };
    let u32_at = |o: usize| -> u32 {
        dib.get(o..o + 4)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .map_or(0, u32::from_le_bytes)
    };
    let i32_at = |o: usize| -> i32 {
        dib.get(o..o + 4)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .map_or(0, i32::from_le_bytes)
    };

    let header_size = u32_at(0) as usize;
    if !matches!(header_size, 40 | 52 | 56 | 108 | 124) {
        return None;
    }
    let width = i32_at(4);
    let height = i32_at(8);
    let bpp = u16_at(14);
    let compression = u32_at(16);
    let clr_used = u32_at(32);
    if width <= 0 || height == 0 {
        return None;
    }
    let top_down = height < 0;
    let (w, h) = (width as u32, height.unsigned_abs());
    let (w_us, h_us) = (w as usize, h as usize);
    let stride = ((w as u64 * bpp as u64 + 31) / 32 * 4) as usize;

    // 像素数据起点 = 头 + 调色板（仅 bpp<=8）+（40 头的 BI_BITFIELDS 内联 3 mask）。
    let palette_entries: usize = if bpp <= 8 {
        if clr_used > 0 {
            clr_used as usize
        } else {
            1usize << bpp
        }
    } else {
        0
    };
    let mut data_start = header_size + palette_entries * 4;
    if header_size == 40 && compression == 3 {
        data_start += 12;
    }
    if dib.len() < data_start {
        return None;
    }
    if stride.checked_mul(h_us)? > dib.len() - data_start {
        return None; // 数据不完整
    }
    // 保守配额：像素区不超 256MB，防极端位图拖垮编码。
    if stride.checked_mul(h_us)? > 256 * 1024 * 1024 {
        return None;
    }

    let (r_mask, g_mask, b_mask, a_mask): (u32, u32, u32, u32) = {
        let (mut r, mut g, mut b, mut a) = match header_size {
            // BITMAPV4HEADER(108)/BITMAPV5HEADER(124)：掩码在头内固定偏移
            // 40/44/48/52（R/G/B/A）——V5 只是在 AlphaMask 之后追加色彩管理字段；
            // 124 头是 CF_DIBV5 的标准形态，读错偏移会拿 CSType/Endpoints 当掩码。
            108 | 124 => (u32_at(40), u32_at(44), u32_at(48), u32_at(52)),
            // BI_BITFIELDS：掩码随头形态走。52/56 字节头（V4 的无色彩管理变体）
            // 掩码**本来就内嵌在头内** 40/44/48(/52)——这正是头长 52/56 的定义；
            // 从头后读会把像素数据当掩码（首行非纯黑时无回退救场 → 颜色错乱）。
            // 仅 40 字节的 BITMAPINFOHEADER 才是"3 个 DWORD mask 紧跟头"的形态。
            _ if compression == 3 => {
                let base = if header_size == 40 { header_size } else { 40 };
                let a = if header_size >= 56 { u32_at(52) } else { 0 };
                (u32_at(base), u32_at(base + 4), u32_at(base + 8), a)
            }
            _ if bpp == 32 => (0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0),
            _ => (0, 0, 0, 0),
        };
        // 掩码回退：写方未填掩码（实测 OS 由 BI_RGB 合成的 DIBV5 会带全 0 掩码、
        // WPF 自写 DIB 见过 (0,0,0xFF000000) 形态）→ 任一 R/G/B 缺失即整体回退
        // 标准布局，alpha 通道交给字节 3 + 伪透明检测，不按零掩码解码出全黑。
        if r == 0 || g == 0 || b == 0 {
            r = 0x00FF_0000;
            g = 0x0000_FF00;
            b = 0x0000_00FF;
            a = 0;
        }
        (r, g, b, a)
    };

    let row_at = |y: usize| -> &[u8] {
        let src_y = if top_down { y } else { h_us - 1 - y };
        &dib[data_start + src_y * stride..][..stride]
    };

    let out = match bpp {
        32 => {
            // BI_RGB 的 DIB 常见"alpha 全 0 的伪透明"：全 0 时按不透明处理。
            let all_zero = {
                let mut all_zero = true;
                'outer: for y in 0..h_us {
                    let row = row_at(y);
                    for x in 0..w_us {
                        if row[x * 4 + 3] != 0 {
                            all_zero = false;
                            break 'outer;
                        }
                    }
                }
                all_zero
            };
            let mut out = vec![0u8; w_us * h_us * 4];
            for y in 0..h_us {
                let row = row_at(y);
                let dst = &mut out[y * w_us * 4..][..w_us * 4];
                for x in 0..w_us {
                    let px = u32::from_le_bytes([
                        row[x * 4],
                        row[x * 4 + 1],
                        row[x * 4 + 2],
                        row[x * 4 + 3],
                    ]);
                    let a = if a_mask != 0 {
                        extract(px, a_mask)
                    } else if all_zero {
                        255
                    } else {
                        row[x * 4 + 3] // 标准 BGRX 布局第 4 字节即 alpha
                    };
                    dst[x * 4] = extract(px, r_mask);
                    dst[x * 4 + 1] = extract(px, g_mask);
                    dst[x * 4 + 2] = extract(px, b_mask);
                    dst[x * 4 + 3] = a;
                }
            }
            Some(out)
        }
        24 => {
            let mut out = vec![0u8; w_us * h_us * 4];
            for y in 0..h_us {
                let row = row_at(y);
                let dst = &mut out[y * w_us * 4..][..w_us * 4];
                for x in 0..w_us {
                    dst[x * 4] = row[x * 3 + 2]; // R（BGR 顺序）
                    dst[x * 4 + 1] = row[x * 3 + 1];
                    dst[x * 4 + 2] = row[x * 3];
                    dst[x * 4 + 3] = 255;
                }
            }
            Some(out)
        }
        8 => {
            // 调色板条目 = RGBQUAD（B,G,R,0）；索引查表。
            let mut out = vec![0u8; w_us * h_us * 4];
            for y in 0..h_us {
                let row = row_at(y);
                let dst = &mut out[y * w_us * 4..][..w_us * 4];
                for x in 0..w_us {
                    let idx = row[x] as usize;
                    if idx >= palette_entries {
                        continue;
                    }
                    let p = header_size + idx * 4;
                    dst[x * 4] = dib.get(p + 2).copied().unwrap_or(0);
                    dst[x * 4 + 1] = dib.get(p + 1).copied().unwrap_or(0);
                    dst[x * 4 + 2] = dib.get(p).copied().unwrap_or(0);
                    dst[x * 4 + 3] = 255;
                }
            }
            Some(out)
        }
        _ => None,
    }?;
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h, out))
}

/// 从像素按位掩码取通道并归一到 8bit。
fn extract(px: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = 32 - shift - mask.leading_zeros();
    let v = (px & mask) >> shift;
    if bits >= 8 {
        (v >> (bits - 8)) as u8
    } else {
        let max = (1u32 << bits) - 1;
        ((v * 255 + max / 2) / max) as u8
    }
}

/// 在 COM 会话内执行 WIC 操作。
///
/// COM 纪律：IPC 工作线程未初始化 COM——`CoInitializeEx` 必须成对
/// `CoUninitialize`（S_OK/S_FALSE 均算本线程拥有会话需配平；RPC_E_CHANGED_MODE
/// =已按别的模式初始化，不拥有本次会话、不释放）。
fn with_com<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };

    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let balance = hr.0 >= 0;
        let result = f();
        if balance {
            CoUninitialize();
        }
        result
    }
}

/// RGBA 内存位图 → PNG（WIC 编码器写 IWICStream 回读）。失败返回 Err
/// （此时调用方按无图片处理更友好，故调用侧转 None）。
///
/// 像素序与声明格式严格一致：`dib_to_rgba` 产出 **RGBA**，故声明
/// `GUID_WICPixelFormat32bppRGBA`（此前误配 BGRA 会 R/B 互换）。
fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    with_com(|| encode_png_inner(w, h, rgba))
}

/// COM 会话内的实际编码。
fn encode_png_inner(w: u32, h: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    use windows::Win32::Foundation::HGLOBAL;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat32bppRGBA,
        IWICImagingFactory, WICBitmapEncoderNoCache,
    };
    use windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER, STREAM_SEEK_SET};

    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| anyhow!("WIC factory: {e}"))?;
        let bitmap = factory
            .CreateBitmapFromMemory(w, h, &GUID_WICPixelFormat32bppRGBA, (w * 4) as u32, rgba)
            .map_err(|e| anyhow!("WIC bitmap: {e}"))?;
        // 编码目标：HGLOBAL 内存流（CreateStreamOnHGlobal 传空句柄自动分配、
        // 随流释放）。未绑定内容的 IWICStream 会让 encoder.Initialize 报
        // WINCODEC_ERR_COMPONENTNOTFOUND（0x88982F0C，实测）——必须用可写的
        // 真实内存流。
        let stream = CreateStreamOnHGlobal(HGLOBAL(std::ptr::null_mut()), true)
            .map_err(|e| anyhow!("mem stream: {e}"))?;
        let encoder = factory
            .CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())
            .map_err(|e| anyhow!("WIC encoder: {e}"))?;
        encoder
            .Initialize(&stream, WICBitmapEncoderNoCache)
            .map_err(|e| anyhow!("WIC encoder init: {e}"))?;
        let mut frame: Option<windows::Win32::Graphics::Imaging::IWICBitmapFrameEncode> = None;
        encoder
            .CreateNewFrame(&mut frame, std::ptr::null_mut())
            .map_err(|e| anyhow!("WIC frame: {e}"))?;
        let frame = frame.ok_or_else(|| anyhow!("WIC frame null"))?;
        frame
            .Initialize(None::<&windows::Win32::System::Com::StructuredStorage::IPropertyBag2>)
            .map_err(|e| anyhow!("WIC frame init: {e}"))?;
        frame
            .WriteSource(&bitmap, std::ptr::null())
            .map_err(|e| anyhow!("WIC write: {e}"))?;
        frame
            .Commit()
            .map_err(|e| anyhow!("WIC frame commit: {e}"))?;
        encoder
            .Commit()
            .map_err(|e| anyhow!("WIC encoder commit: {e}"))?;

        // 回头从流读出 PNG 字节（写指针在尾，先回到 0 再循环读到 0）。
        stream
            .Seek(0, STREAM_SEEK_SET, None)
            .map_err(|e| anyhow!("stream seek: {e}"))?;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];

        loop {
            let mut got: u32 = 0;
            let hr = stream.Read(
                buf.as_mut_ptr() as *mut core::ffi::c_void,
                buf.len() as u32,
                Some(&mut got),
            );
            if hr.is_err() {
                bail!("WIC stream read: {hr:?}");
            }
            if got == 0 {
                break;
            }
            out.extend_from_slice(&buf[..got as usize]);
            if out.len() > 64 * 1024 * 1024 {
                bail!("PNG 编码结果异常膨胀");
            }
        }
        if out.is_empty() {
            bail!("PNG 编码无输出");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 BITMAPV5HEADER（124B）+ 1 像素 32bpp DIB：掩码写头内 40/44/48/52。
    fn v5_dib(pixel_bgra: [u8; 4]) -> Vec<u8> {
        let mut dib = vec![0u8; 124 + 4];
        dib[0..4].copy_from_slice(&124u32.to_le_bytes()); // bV5Size
        dib[4..8].copy_from_slice(&1i32.to_le_bytes()); // width
        dib[8..12].copy_from_slice(&1i32.to_le_bytes()); // height（正=bottom-up）
        dib[12..14].copy_from_slice(&1u16.to_le_bytes()); // planes
        dib[14..16].copy_from_slice(&32u16.to_le_bytes()); // bpp
        dib[16..20].copy_from_slice(&0u32.to_le_bytes()); // BI_RGB
                                                          // 掩码固定偏移：R@40 G@44 B@48 A@52（V4/V5 同布局）。
        dib[40..44].copy_from_slice(&0x00FF_0000u32.to_le_bytes());
        dib[44..48].copy_from_slice(&0x0000_FF00u32.to_le_bytes());
        dib[48..52].copy_from_slice(&0x0000_00FFu32.to_le_bytes());
        dib[52..56].copy_from_slice(&0xFF00_0000u32.to_le_bytes());
        // 像素（B,G,R,A 字节序）
        dib[124..128].copy_from_slice(&pixel_bgra);
        dib
    }

    #[test]
    fn v5_header_masks_and_rgba_order() {
        // 🔴#3 回归：V5(124) 掩码必须读 40/44/48/52——此前读 52/56/60/64 会把
        // CSType('sRGB' 常量) / Endpoints 当成 G/A 掩码 → 颜色错乱。
        let dib = v5_dib([0x00, 0x00, 0xFF, 0xFF]); // 纯红像素（B=0,G=0,R=255,A=255）
        let (w, h, rgba) = dib_to_rgba(&dib).expect("v5 dib should parse");
        assert_eq!((w, h), (1, 1));
        assert_eq!(rgba, vec![255, 0, 0, 255], "R,G,B,A 字节序 + 掩码偏移正确");
    }

    #[test]
    fn bitfields_56_header_masks_read_in_header() {
        // 🔴 回归：56 字节头 + BI_BITFIELDS 的掩码内嵌在头内 40/44/48/52——
        // 此前从头后（=像素数据）读，首行像素非纯黑时无零掩码回退救场 → 颜色错乱。
        // 3 像素一行（红/绿/蓝），旧实现会把这三个像素的 u32 当 R/G/B 掩码。
        let mut dib = vec![0u8; 56 + 12];
        dib[0..4].copy_from_slice(&56u32.to_le_bytes());
        dib[4..8].copy_from_slice(&3i32.to_le_bytes()); // width
        dib[8..12].copy_from_slice(&1i32.to_le_bytes()); // height
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        dib[16..20].copy_from_slice(&3u32.to_le_bytes()); // BI_BITFIELDS
                                                          // 掩码在头内：R@40 G@44 B@48，A@52 置 0（交回退逻辑按不透明处理）。
        dib[40..44].copy_from_slice(&0x00FF_0000u32.to_le_bytes());
        dib[44..48].copy_from_slice(&0x0000_FF00u32.to_le_bytes());
        dib[48..52].copy_from_slice(&0x0000_00FFu32.to_le_bytes());
        // 像素（B,G,R,A）：红、绿、蓝。
        dib[56..60].copy_from_slice(&[0, 0, 255, 255]);
        dib[60..64].copy_from_slice(&[0, 255, 0, 255]);
        dib[64..68].copy_from_slice(&[255, 0, 0, 255]);
        let (w, h, rgba) = dib_to_rgba(&dib).expect("56B bitfields dib should parse");
        assert_eq!((w, h), (3, 1));
        assert_eq!(
            &rgba[0..12],
            &[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255],
            "掩码必须从头内读，颜色不随首行像素漂移"
        );
    }

    #[test]
    fn bitfields_52_header_masks_read_in_header() {
        // 52 字节头（只含 3 个掩码、无 A 掩码）：掩码同样在头内 40/44/48。
        let mut dib = vec![0u8; 52 + 12];
        dib[0..4].copy_from_slice(&52u32.to_le_bytes());
        dib[4..8].copy_from_slice(&3i32.to_le_bytes());
        dib[8..12].copy_from_slice(&1i32.to_le_bytes());
        dib[12..14].copy_from_slice(&1u16.to_le_bytes());
        dib[14..16].copy_from_slice(&32u16.to_le_bytes());
        dib[16..20].copy_from_slice(&3u32.to_le_bytes()); // BI_BITFIELDS
        dib[40..44].copy_from_slice(&0x00FF_0000u32.to_le_bytes());
        dib[44..48].copy_from_slice(&0x0000_FF00u32.to_le_bytes());
        dib[48..52].copy_from_slice(&0x0000_00FFu32.to_le_bytes());
        dib[52..56].copy_from_slice(&[0, 0, 255, 255]); // 红
        dib[56..60].copy_from_slice(&[0, 255, 0, 255]);
        dib[60..64].copy_from_slice(&[255, 0, 0, 255]);
        let (w, h, rgba) = dib_to_rgba(&dib).expect("52B bitfields dib should parse");
        assert_eq!((w, h), (3, 1));
        assert_eq!(
            &rgba[0..12],
            &[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255],
            "掩码从头内读；旧实现把像素当掩码 → 首像素绿/蓝错成红白系"
        );
    }

    #[test]
    fn rgb24_rows_are_bgr_bytes_flipped_up() {
        // 24bpp：行序 bottom-up；输出 RGBA（R 在首字节）。
        let mut dib = vec![0u8; 40 + 8]; // 40 头 + 2 像素行（stride 4：2px + 2 填充）
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&2i32.to_le_bytes());
        dib[8..12].copy_from_slice(&1i32.to_le_bytes());
        dib[14..16].copy_from_slice(&24u16.to_le_bytes());
        // 文件第 0 行 = 最底行（bottom-up）：[B,G,R]=蓝, [B,G,R]=红
        dib[40] = 255; // B
        dib[42] = 0; // R → 底行左像素 = 纯蓝 → 输出顶行应为 [0,0,255,255]
        dib[44] = 0;
        dib[45] = 255; // R → 底行右像素 = 纯红
        let (w, h, rgba) = dib_to_rgba(&dib).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(&rgba[0..4], &[0, 0, 255, 255], "顶行 = 文件底行（翻转）");
        assert_eq!(&rgba[4..8], &[255, 0, 0, 255]);
    }

    #[test]
    fn rgb32_all_zero_alpha_forced_opaque() {
        // BI_RGB 32bpp "alpha 全 0 伪透明" → 全部按不透明处理。
        let mut dib = v5_dib([0x00, 0x00, 0xFF, 0x00]);
        dib[0..4].copy_from_slice(&40u32.to_le_bytes()); // 降为 40 头（V5 掩码区变调色板外区域）
        let (w, h, rgba) = dib_to_rgba(&dib).unwrap();
        assert_eq!((w, h), (1, 1));
        assert_eq!(&rgba[3], &255, "全 0 alpha 强制 255");
    }

    #[test]
    fn palette8_index_lookup() {
        // 8bpp：调色板第 2 项 = 纯绿；像素字节 2 → 输出 RGBA [0,255,0,255]。
        let mut dib = vec![0u8; 40 + 4 * 4 + 4];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&1i32.to_le_bytes());
        dib[8..12].copy_from_slice(&1i32.to_le_bytes());
        dib[14..16].copy_from_slice(&8u16.to_le_bytes());
        dib[32..36].copy_from_slice(&2u32.to_le_bytes()); // clrUsed=2
                                                          // 调色板[1] = RGBQUAD(B,G,R,0) = (0,255,0)
        dib[40 + 4] = 0;
        dib[40 + 5] = 255;
        dib[40 + 6] = 0;
        dib[40 + 8] = 1; // 像素索引 = 1
        let (_, _, rgba) = dib_to_rgba(&dib).unwrap();
        assert_eq!(rgba, vec![0, 255, 0, 255]);
    }

    #[test]
    fn unsupported_format_returns_none() {
        // 16bpp：暂不支持 → None（调用方按无图片处理）。
        let mut dib = vec![0u8; 40 + 4];
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        dib[4..8].copy_from_slice(&1i32.to_le_bytes());
        dib[8..12].copy_from_slice(&1i32.to_le_bytes());
        dib[14..16].copy_from_slice(&16u16.to_le_bytes());
        assert!(dib_to_rgba(&dib).is_none());
    }
}

#[cfg(test)]
mod encode_tests {
    use super::*;

    #[test]
    fn encode_png_roundtrip_magic() {
        // COM/WIC 全链路冒烟：worker 线程（测试线程同样未初始化 COM）编码 2x2 RGBA。
        let mut px = vec![0u8; 16];
        // 顶行：红、绿；底行：蓝、白（RGBA 序）
        px[0..4].copy_from_slice(&[255, 0, 0, 255]);
        px[4..8].copy_from_slice(&[0, 128, 0, 255]);
        px[8..12].copy_from_slice(&[0, 0, 255, 255]);
        px[12..16].copy_from_slice(&[255, 255, 255, 255]);
        let png = encode_png(2, 2, &px).expect("encode 2x2 png");
        // PNG 魔数 + IHDR 宽高（2x2）
        assert_eq!(
            &png[0..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        );
        let w = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
        let h = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
        assert_eq!((w, h), (2, 2));
        assert!(png.len() > 30, "png should contain image data");
    }
}

#[cfg(test)]
mod write_tests {
    use super::*;

    /// 2x2 自顶向下 BGRA：顶行 红/绿，底行 蓝/白。
    fn bgra_2x2() -> ([[u8; 4]; 4], Vec<u8>) {
        let top_left = [0u8, 0, 255, 255]; // B,G,R,A = 红（BGRA 序）
        let top_right = [0, 128, 0, 255]; // 绿（G=128）
        let bottom_left = [255, 0, 0, 255]; // 蓝
        let bottom_right = [255, 255, 255, 255]; // 白
        let mut px = Vec::with_capacity(16);
        for p in [top_left, top_right, bottom_left, bottom_right] {
            px.extend_from_slice(&p);
        }
        ([top_left, top_right, bottom_left, bottom_right], px)
    }

    #[test]
    fn bgra_to_dib_header_and_bottom_up_row_order() {
        let (px4, px) = bgra_2x2();
        let dib = bgra_to_dib(2, 2, &px).expect("build dib");

        assert_eq!(dib.len(), 40 + 16, "40 字节头 + 2 行 × 8 字节");
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40);
        assert_eq!(
            i32::from_le_bytes(dib[4..8].try_into().unwrap()),
            2,
            "biWidth"
        );
        assert_eq!(
            i32::from_le_bytes(dib[8..12].try_into().unwrap()),
            2,
            "biHeight 正数 = 自底向上"
        );
        assert_eq!(
            u16::from_le_bytes(dib[12..14].try_into().unwrap()),
            1,
            "biPlanes"
        );
        assert_eq!(
            u16::from_le_bytes(dib[14..16].try_into().unwrap()),
            32,
            "biBitCount"
        );
        assert_eq!(
            u32::from_le_bytes(dib[16..20].try_into().unwrap()),
            0,
            "BI_RGB"
        );
        assert_eq!(
            u32::from_le_bytes(dib[20..24].try_into().unwrap()),
            16,
            "biSizeImage = stride × 高 = 8 × 2（整幅像素区，不是单行 stride）"
        );

        // 像素自底向上：文件里第一行是传入的底行。
        assert_eq!(&dib[40..44], &px4[2], "首行 = 底左（蓝）");
        assert_eq!(&dib[44..48], &px4[3], "次列 = 底右（白）");
        assert_eq!(&dib[48..52], &px4[0], "第三行 = 顶左（红）");
        assert_eq!(&dib[52..56], &px4[1]);
    }

    #[test]
    fn bgra_to_dib_rejects_mismatched_buffer() {
        let (_, px) = bgra_2x2();
        assert!(
            bgra_to_dib(4, 4, &px).is_err(),
            "像素数不足必须拒绝而非越界读"
        );
        assert!(bgra_to_dib(0, 2, &px).is_err());
    }

    #[test]
    fn write_item_argument_validation() {
        // 两种格式都不给 → INVALID_ARGS；超长文本 → FILE_TOO_LARGE。
        // 这两条都在打开剪贴板之前返回，测试进程不碰系统剪贴板。
        let e = write_item(None, None).unwrap_err().to_string();
        assert!(e.starts_with("INVALID_ARGS:"), "实际: {e}");

        let huge = "x".repeat(MAX_TEXT_BYTES + 1);
        let e = write_item(Some(&huge), None).unwrap_err().to_string();
        assert!(e.starts_with("FILE_TOO_LARGE:"), "实际: {e}");
    }

    #[test]
    fn parse_request_enforces_text_limit() {
        // parse 阶段（锁内纯校验）与锁外 write_item 同口径的 10MB 上限：
        // write_text 与 write_item 的 text 两条路径都要拦。
        let huge = "x".repeat(MAX_TEXT_BYTES + 1);
        let e = parse_request("write_text", &serde_json::json!({ "text": huge.clone() }))
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("FILE_TOO_LARGE:"), "实际: {e}");
        let e = parse_request("write_item", &serde_json::json!({ "text": huge }))
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("FILE_TOO_LARGE:"), "实际: {e}");

        // 恰好在上限内 → 通过。
        let ok = parse_request(
            "write_text",
            &serde_json::json!({ "text": "x".repeat(MAX_TEXT_BYTES) }),
        )
        .unwrap();
        assert!(matches!(ok, ClipboardRequest::WriteText(_)));
    }

    #[test]
    fn png_base64_to_dib_rejects_bad_input() {
        // 非法 base64
        let e = png_base64_to_dib("!!!not base64!!!")
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("INVALID_ARGS:"), "实际: {e}");

        // 合法 base64 但不是 PNG → 解码失败，报 UNSUPPORTED_FORMAT
        let junk = base64::engine::general_purpose::STANDARD.encode(b"not a png at all");
        let e = png_base64_to_dib(&junk).unwrap_err().to_string();
        assert!(e.starts_with("UNSUPPORTED_FORMAT:"), "实际: {e}");

        // 超长 base64 输入 → FILE_TOO_LARGE
        let long = "A".repeat(MAX_IMAGE_BASE64_LEN + 1);
        let e = png_base64_to_dib(&long).unwrap_err().to_string();
        assert!(e.starts_with("FILE_TOO_LARGE:"), "实际: {e}");
    }

    #[test]
    fn png_roundtrip_through_dib_preserves_pixels() {
        // 写路径（PNG → WIC 解码 → BGRA → CF_DIB）与读路径（CF_DIB → RGBA）串起来，
        // 逐像素断言。两侧若都搞错一次 R/B 或行序会互相抵消，所以必须比具体像素值，
        // 不能只比"能跑通"。
        let mut rgba = Vec::with_capacity(16);
        for p in [
            [255u8, 0, 0, 255],
            [0, 128, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 255, 255],
        ] {
            rgba.extend_from_slice(&p);
        }

        let png = encode_png(2, 2, &rgba).expect("encode png");
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let dib = png_base64_to_dib(&b64).expect("png → dib");

        let (w, h, back) = dib_to_rgba(&dib).expect("dib → rgba");
        assert_eq!((w, h), (2, 2));
        assert_eq!(back, rgba, "读写往返像素必须逐个一致");
    }
}
