using System;
using System.Collections.Concurrent;
using System.IO;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Storage.Streams;

namespace Spark.UI.Services;

/// <summary>
/// 插件图标统一加载：位图（png/jpg/ico…）走 BitmapImage（WIC 解码）；
/// svg 走 WinAppSDK 内置 SvgImageSource（SVG 1.1 静态子集，随框架自带，零新依赖）。
/// 两者都以 UriSource 延迟解码，与既有 BitmapImage 用法一致；文件缺失、扩展名
/// 声称 svg 但内容不是、或构造失败时返回 null，由调用方的既有兜底接管
/// （设置列表退回首字母占位，插件窗口退回内置 logo）。
/// 注意：SvgImageSource 真正的解码失败走 OpenFailed/ImageFailed 事件而非抛异常，
/// 此时与"位图字节损坏"同款表现——Image 区域留白，不再回退占位块；在设置列表的
/// 观感是"字母占位块隐藏 + 34px 空白"（IconImage 非空 → IconImageVisibility 为 Visible）。
/// 磁盘 IO（存在性检查/SVG 内容嗅探）在后台线程执行，仅对象构造回 UI 线程
/// （ImageSource 线程亲和）；同步版 Load 已移除，调用方一律 await LoadAsync。
/// </summary>
public static class PluginIconLoader
{
    private enum IconFileKind { Bitmap, Svg }

    public static async Task<ImageSource?> LoadAsync(string? path)
    {
        if (string.IsNullOrEmpty(path)) return null;
        var kind = await Task.Run(() => Probe(path));
        if (kind is null) return null;
        try
        {
            var uri = ToFileUri(path);
            return kind == IconFileKind.Svg ? new SvgImageSource(uri) : new BitmapImage(uri);
        }
        catch (Exception ex)
        {
            App.Log("PluginIcon", ex);
            return null;
        }
    }

    /// <summary>后台探测：null = 文件缺失，或"声称 svg 但内容不是"；否则返回文件种类。
    /// 内容嗅探：防"改了扩展名的位图"这类文件让 Image 区域整块留白，
    /// 返回 null 走占位兜底比留白好。真 SVG 解码失败（如含不支持的 SVG2 特性）无法同步判定。</summary>
    private static IconFileKind? Probe(string path)
    {
        if (!File.Exists(path)) return null;
        if (!IsSvg(path)) return IconFileKind.Bitmap;
        return LooksLikeSvg(path) ? IconFileKind.Svg : null;
    }

    private static bool IsSvg(string path)
        => string.Equals(Path.GetExtension(path), ".svg", StringComparison.OrdinalIgnoreCase);

    /// <summary>读文件头找 &lt;svg&gt; 标记；SVG 前可有 XML 声明/DOCTYPE/注释，头部留足余量。</summary>
    private static bool LooksLikeSvg(string path)
    {
        try
        {
            using var fs = File.OpenRead(path);
            var len = (int)Math.Min(fs.Length, 2048);
            var buf = new byte[len];
            var read = 0;
            while (read < len)
            {
                var n = fs.Read(buf, read, len - read);
                if (n <= 0) break;
                read += n;
            }
            return read > 0 && Encoding.ASCII.GetString(buf, 0, read)
                .Contains("<svg", StringComparison.OrdinalIgnoreCase);
        }
        catch (Exception ex)
        {
            App.Log("PluginIcon", ex);
            return false;
        }
    }

    /// <summary>框架标准转换：DOS 盘符路径 → file:///D:/...（三斜杠规范形），UNC → file://server/share/...；
    /// 空格/# 等保留字符由 Uri 自动转义。不走手拼 "file:///" + "/" + path——那会产出
    /// 四斜杠伪形（旧代码碰巧被容忍），且破坏 #/% 等保留字符的语义。
    /// GetFullPath 遇非法字符抛异常，由外层 catch 记日志并返回 null。</summary>
    private static Uri ToFileUri(string path)
        => new Uri(Path.GetFullPath(path));

    /// <summary>远程图标字节缓存（市场卡片用）：按 URL 去重下载，成功结果常驻内存；
    /// 失败结果（下载或解码）按值比对淘汰自己这条 Lazy——若刷新已插入新条目则不误删，
    /// 仅竞态窗口内多一次重复下载。仅内存缓存，无磁盘层。</summary>
    private static readonly ConcurrentDictionary<string, Lazy<Task<ImageSource?>>> RemoteCache = new();

    /// <summary>市场插件图标：下载（RegistryService，≤2MiB）+ 解码，同一 URL 并发只下载一次。
    /// 失败返回 null 由调用方保留字母占位；结果可被缓存，刷新秒回。</summary>
    public static Task<ImageSource?> LoadRemoteAsync(string? url)
    {
        if (string.IsNullOrWhiteSpace(url)) return Task.FromResult<ImageSource?>(null);
        var lazy = RemoteCache.GetOrAdd(url, CreateLazy);
        return lazy.Value;
    }

    /// <summary>工厂里先造 Lazy 再把它自己捕获进闭包，Core 才能做"只淘汰自己"的按值删除。</summary>
    private static Lazy<Task<ImageSource?>> CreateLazy(string url)
    {
        var lazy = default(Lazy<Task<ImageSource?>>)!;
        lazy = new Lazy<Task<ImageSource?>>(() => LoadRemoteCoreAsync(url, lazy),
            LazyThreadSafetyMode.ExecutionAndPublication);
        return lazy;
    }

    private static async Task<ImageSource?> LoadRemoteCoreAsync(string url, Lazy<Task<ImageSource?>> self)
    {
        var src = default(ImageSource?);
        var bytes = await RegistryService.DownloadIconAsync(url);
        if (bytes is { Length: > 0 })
        {
            src = await FromBytesAsync(bytes);
            App.Log("MarketIcon", $"decode {(src is null ? "FAIL" : "ok")} url={url} bytes={bytes.Length}");
        }
        else
        {
            App.Log("MarketIcon", $"download FAIL url={url}");
        }

        if (src is null)
        {
            // 失败不缓存，避免弱网/坏图标让本会话永远停在字母占位。
            // TryRemove(KeyValuePair) 按值比对：只删"自己"这条 Lazy；
            // 若刷新已插入新条目（URL 内容已变），新条目保留、下次自然重试。
            RemoteCache.TryRemove(new KeyValuePair<string, Lazy<Task<ImageSource?>>>(url, self));
        }
        return src;
    }

    /// <summary>字节 → ImageSource：内容嗅探分流（与本地一致）。UI 线程调用。
    /// SVG 的解码成败可同步判定（SetSourceAsync 返回状态）；位图失败走异常或
    /// ImageFailed 事件，后者无法同步感知——与本地路径位图损坏行为一致。</summary>
    public static async Task<ImageSource?> FromBytesAsync(byte[] bytes)
    {
        // 解码尺寸上限：防第三方仓库塞"字节小、像素巨大"的图在 UI 线程解码造成卡顿。
        // 34px 显示位 × 2.5 高 DPI 余量取 96；位图 DecodePixelWidth 保纵横比，
        // SVG 的 RasterizePixel 尺寸要求宽高都给，非方形 viewBox 有极小拉伸风险，图标场景可接受。
        const int DecodeCap = 96;

        var stream = new InMemoryRandomAccessStream();
        try
        {
            using (var writer = new DataWriter(stream.GetOutputStreamAt(0)))
            {
                writer.WriteBytes(bytes);
                await writer.StoreAsync();
                writer.DetachStream();
            }
            stream.Seek(0);

            if (Encoding.ASCII.GetString(bytes, 0, Math.Min(bytes.Length, 2048))
                .Contains("<svg", StringComparison.OrdinalIgnoreCase))
            {
                var svg = new SvgImageSource
                {
                    RasterizePixelWidth = DecodeCap,
                    RasterizePixelHeight = DecodeCap,
                };
                var status = await svg.SetSourceAsync(stream);
                if (status != SvgImageSourceLoadStatus.Success)
                    App.Log("MarketIcon", $"svg SetSourceAsync={status}");
                return status == SvgImageSourceLoadStatus.Success ? svg : null;
            }

            var bmp = new BitmapImage { DecodePixelWidth = DecodeCap };
            await bmp.SetSourceAsync(stream);
            return bmp;
        }
        catch (Exception ex)
        {
            App.Log("PluginIcon", ex);
            return null;
        }
        finally
        {
            stream.Dispose();
        }
    }
}