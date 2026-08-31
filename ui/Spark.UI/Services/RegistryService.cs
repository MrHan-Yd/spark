using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Compression;
using System.Linq;
using System.Net;
using System.Net.Http;
using System.Security.Cryptography;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using Spark.UI.Models;

namespace Spark.UI.Services;

/// <summary>
/// 插件市场抓取、下载与安全解压服务
/// 遵循规范：《插件开发/插件市场与仓库.md》
/// </summary>
public static class RegistryService
{
    public const string OfficialRegistryUrl = "https://raw.githubusercontent.com/MrHan-Yd/spark-plugins/master/registry.json";

    /// <summary>
    /// 官方索引镜像（jsDelivr GitHub CDN）。raw.githubusercontent.com 部分直连线路会被
    /// GitHub 边缘节点直接拒绝（实测稳定返回 HTTP 400），而 jsDelivr 国内直连可达；
    /// 仅在官方地址抓取失败后作为回退候选，内容仍以逐插件签名校验兜底。
    /// 注意：jsDelivr 对分支引用有 CDN 缓存（可达数小时），回退源新鲜度略低于官方地址。
    /// </summary>
    public const string OfficialRegistryMirrorUrl = "https://cdn.jsdelivr.net/gh/MrHan-Yd/spark-plugins@master/registry.json";

    /// <summary>官方仓库 zipball 回退地址（索引未声明 zipball_url 时使用）。</summary>
    public const string OfficialZipballUrl = "https://github.com/MrHan-Yd/spark-plugins/archive/refs/heads/master.zip";

    private static readonly HttpClient Http = new()
    {
        Timeout = TimeSpan.FromSeconds(30)
    };

    private const long MaxZipSize = 50 * 1024 * 1024;      // 50 MiB
    private const long MaxUnpackSize = 100 * 1024 * 1024;  // 100 MiB
    private const int MaxEntryCount = 10000;
    private const long MaxIconSize = 2 * 1024 * 1024;      // 2 MiB，图标字节上限

    static RegistryService()
    {
        Http.DefaultRequestHeaders.UserAgent.ParseAdd("Spark-PluginMarket/1.0");
    }

    /// <summary>
    /// 显式按进程环境变量（http_proxy/https_proxy/all_proxy）构造的代理客户端，无则 null。
    /// HttpClient.DefaultProxy 在部分宿主上下文下不落实系统/环境代理（实测安装版进程环境
    /// 自带 http_proxy 仍走直连被拒），该客户端作为第二候选显式接管。
    /// </summary>
    private static readonly HttpClient? HttpViaEnvProxy = CreateEnvProxyClient();

    /// <summary>供更新下载等外部流程复用的环境变量代理客户端（无则 null；共享单例，调用方勿释放）。</summary>
    internal static HttpClient? EnvProxyClient => HttpViaEnvProxy;

    private static HttpClient? CreateEnvProxyClient()
    {
        // https 目标优先看 https_proxy；http_proxy/all_proxy 次之。
        // 兼容无 scheme 写法（"127.0.0.1:7890" 默认按 http），放行 socks5（.NET 8 SocketsHttpHandler 已支持）
        foreach (var name in new[] { "https_proxy", "HTTPS_PROXY", "http_proxy", "HTTP_PROXY", "all_proxy", "ALL_PROXY" })
        {
            var value = Environment.GetEnvironmentVariable(name);
            if (string.IsNullOrWhiteSpace(value)) continue;
            var raw = value.Trim();
            if (!raw.Contains("://", StringComparison.Ordinal)) raw = "http://" + raw;
            if (!Uri.TryCreate(raw, UriKind.Absolute, out var proxy)) continue;
            if (proxy.Scheme != "http" && proxy.Scheme != "https" && proxy.Scheme != "socks5") continue;

            var handler = new SocketsHttpHandler
            {
                Proxy = new WebProxy(proxy),
                UseProxy = true
            };
            var client = new HttpClient(handler) { Timeout = TimeSpan.FromSeconds(30) };
            client.DefaultRequestHeaders.UserAgent.ParseAdd("Spark-PluginMarket/1.0");
            return client;
        }
        return null;
    }

    /// <summary>为日志/错误摘要压缩目标地址，避免把完整 URL 塞进一行提示。</summary>
    private static string TargetLabel(string url)
    {
        if (url.Contains("raw.githubusercontent.com", StringComparison.OrdinalIgnoreCase)) return "raw.githubusercontent.com";
        if (url.Contains("cdn.jsdelivr.net", StringComparison.OrdinalIgnoreCase)) return "jsDelivr 镜像";
        if (Uri.TryCreate(url, UriKind.Absolute, out var u)) return u.Host;
        return url;
    }

    /// <summary>
    /// 依序尝试候选（客户端, URL）组合，返回第一个 HTTP 成功（2xx）的响应。
    /// 全部失败时抛出带候选结果摘要的聚合异常：
    /// - 至少拿到过一个 HTTP 状态 → HttpRequestException（StatusCode 取最后一个状态，
    ///   供"404=没有发布版本"这类按状态码分支的调用方继续工作）；
    /// - 全部候选都是超时（无任何 HTTP 状态）→ TaskCanceledException，让下载/更新侧
    ///   原有"超时"文案路径保持可达，不被误报成"网络不可达"。
    /// totalBudget 为整链预算封顶（覆盖在 30s/候选 之上），全候选超时的最坏静默等待由此收敛。
    /// 注意：成功响应的释放责任在调用方；失败候选的响应在循环内释放。
    /// </summary>
    private static async Task<HttpResponseMessage> GetSuccessAsync(
        IReadOnlyList<(HttpClient Client, string Url)> attempts, CancellationToken ct,
        TimeSpan? totalBudget = null)
    {
        CancellationTokenSource? cts = null;
        if (totalBudget is { } budget)
        {
            cts = CancellationTokenSource.CreateLinkedTokenSource(ct);
            cts.CancelAfter(budget);
        }
        var token = cts?.Token ?? ct;

        var summaries = new List<string>();
        Exception? last = null;
        HttpStatusCode? lastStatus = null;
        var timedOut = 0;
        for (int i = 0; i < attempts.Count; i++)
        {
            var (client, url) = attempts[i];
            try
            {
                var resp = await client.GetAsync(url, HttpCompletionOption.ResponseHeadersRead, token);
                if (resp.IsSuccessStatusCode) return resp;
                using (resp)
                {
                    lastStatus = resp.StatusCode;
                    summaries.Add($"{TargetLabel(url)}: HTTP {(int)resp.StatusCode}");
                }
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested)
            {
                // 调用方主动取消：透传，不当失败候选
                throw;
            }
            catch (Exception ex)
            {
                // 走到这里仍为 OCE/TaskCanceled 的一定是候选自身超时（client 30s 或链预算），
                // 单独归类便于聚合时区分"网络不通"与"超时"
                if (ex is OperationCanceledException)
                {
                    timedOut++;
                    summaries.Add($"{TargetLabel(url)}: 响应超时");
                }
                else
                {
                    summaries.Add($"{TargetLabel(url)}: {ex.Message}");
                }
                last = ex;
            }
        }
        var detail = summaries.Count > 0 ? string.Join("；", summaries) : "无可用地址";
        var message = $"所有候选源均请求失败（{detail}）。请检查网络或代理设置后重试";
        if (lastStatus is null && timedOut == attempts.Count && attempts.Count > 0)
        {
            throw new TaskCanceledException(message, last);
        }
        throw new HttpRequestException(message, last, lastStatus);
    }

    /// <summary>
    /// 抓取并解析仓库索引。官方源失败时自动回退：默认客户端 → 显式环境代理客户端 →
    /// jsDelivr 镜像（同两条客户端路径）；第三方源只做代理路径回退，不套官方镜像。
    /// schema 不符直接抛出（换源结果相同）；其余失败（网络/解析）整体重试一轮——
    /// 代理偶发返回 200 垃圾页时第二轮可自愈。
    /// </summary>
    public static async Task<RegistryIndexDto> FetchIndexAsync(string url, CancellationToken ct = default)
    {
        const int maxAttempts = 2;  // 1 次首发 + 1 次重试；每次内部已遍历代理/镜像候选链
        var lastEx = (Exception?)null;
        for (int attempt = 1; attempt <= maxAttempts; attempt++)
        {
            try
            {
                ct.ThrowIfCancellationRequested();
                var json = await FetchJsonWithFallbackAsync(url, ct);
                var options = new JsonSerializerOptions { PropertyNameCaseInsensitive = true };
                var index = JsonSerializer.Deserialize<RegistryIndexDto>(json, options)
                    ?? throw new InvalidDataException("registry.json 内容为空或格式不正确");

                if (index.Schema != 1)
                {
                    throw new NotSupportedException($"不支持的 registry schema 版本：{index.Schema} (要求 schema=1)");
                }

                // 过滤掉数据不完整的条目，避免 UI 崩坏
                index.Plugins = index.Plugins
                    .Where(p => !string.IsNullOrWhiteSpace(p.Id) && !string.IsNullOrWhiteSpace(p.Latest))
                    .ToList();

                // 第三方 registry.json 不可信：显式 null 会把 C# 属性初始化的默认值覆写为 null。
                // 在数据入口统一归一化，保证 ViewDto 的展示 getter（IconLetter/PermissionsSummary 等）
                // 在布局期绑定求值时永不抛 NRE；不改变正常数据的展示，也不会误过滤合法条目。
                foreach (var p in index.Plugins)
                {
                    p.Name = p.Name ?? "";
                    p.Description = p.Description ?? "";
                    p.Author = p.Author ?? "";
                    p.Homepage = p.Homepage ?? "";
                    p.Icon = p.Icon ?? "";
                    p.Runtime = p.Runtime ?? "webview";
                    p.Permissions = p.Permissions ?? new List<string>();
                    p.Versions = p.Versions is null
                        ? new List<RegistryVersionDto>()
                        : p.Versions.Where(v => v is not null).ToList();
                }

                return index;
            }
            catch (OperationCanceledException) { throw; }
            catch (NotSupportedException) { throw; }  // schema 不符：换源结果相同，直接失败
            catch (Exception ex)
            {
                lastEx = ex;
                if (attempt < maxAttempts && !ct.IsCancellationRequested)
                {
                    await Task.Delay(500 * attempt, ct);
                }
            }
        }
        throw lastEx ?? new IOException("抓取仓库索引失败");
    }

    /// <summary>
    /// 抓取索引 JSON 的候选链：同 URL 先后用默认/环境代理客户端请求；
    /// 官方源再补 jsDelivr 镜像（同两条客户端路径）。整链 45 秒预算封顶（含外层重试
    /// 至多 90 秒），避免全候选超时时刷新按钮长时间无响应。
    /// 抛出带候选结果摘要的聚合异常（见 GetSuccessAsync）。
    /// </summary>
    private static async Task<string> FetchJsonWithFallbackAsync(string url, CancellationToken ct)
    {
        var isOfficial = string.Equals(url, OfficialRegistryUrl, StringComparison.OrdinalIgnoreCase);
        var attempts = new List<(HttpClient Client, string Url)> { (Http, url) };
        if (HttpViaEnvProxy is not null) attempts.Add((HttpViaEnvProxy, url));
        if (isOfficial)
        {
            attempts.Add((Http, OfficialRegistryMirrorUrl));
            if (HttpViaEnvProxy is not null) attempts.Add((HttpViaEnvProxy, OfficialRegistryMirrorUrl));
        }

        using var resp = await GetSuccessAsync(attempts, ct, TimeSpan.FromSeconds(45));
        return await resp.Content.ReadAsStringAsync(ct);
    }

    /// <summary>
    /// 代理回退版 GET 文本：默认客户端失败后用环境变量代理客户端重试（用于检查更新等
    /// 非 GitHub raw 域名的请求；raw 域名走 FetchIndexAsync 的镜像链）。
    /// 整链 45 秒预算封顶。
    /// </summary>
    internal static async Task<string> GetStringWithProxyFallbackAsync(string url, CancellationToken ct = default)
    {
        var attempts = new List<(HttpClient Client, string Url)> { (Http, url) };
        if (HttpViaEnvProxy is not null) attempts.Add((HttpViaEnvProxy, url));
        using var resp = await GetSuccessAsync(attempts, ct, TimeSpan.FromSeconds(45));
        return await resp.Content.ReadAsStringAsync(ct);
    }

    /// <summary>
    /// raw.githubusercontent.com → jsDelivr 镜像改写（仅 GitHub raw URL 可改写，其余返回 null）。
    /// </summary>
    private static string? ToJsDelivrMirror(string url)
    {
        if (!Uri.TryCreate(url, UriKind.Absolute, out var u)
            || u.Scheme != "https"
            || u.Host != "raw.githubusercontent.com") return null;
        var segs = u.AbsolutePath.TrimStart('/').Split('/');
        if (segs.Length < 4 || segs.Any(s => s.Length == 0)) return null;
        return $"https://cdn.jsdelivr.net/gh/{segs[0]}/{segs[1]}@{segs[2]}/{string.Join('/', segs.Skip(3))}";
    }

    /// <summary>
    /// 下载市场插件图标字节（registry.json 的 icon 为 http(s) 绝对 URL）。
    /// 失败/超限返回 null，由调用方退回字母占位——图标是装饰性资源，静默降级即可。
    /// 候选链：默认客户端 → 环境代理客户端 → jsDelivr 镜像（raw.githubusercontent.com
    /// 部分直连线路被 GitHub 边缘拒绝，见 OfficialRegistryMirrorUrl 注释）。
    /// 显式 15s 总时长 CTS 兜底：ResponseHeadersRead 模式下 HttpClient.Timeout 只覆盖到响应头，
    /// 正文读取（慢速滴流/停滞服务器）必须由它约束，否则任务永久挂起占住连接。
    /// </summary>
    public static async Task<byte[]?> DownloadIconAsync(string? url, CancellationToken ct = default)
    {
        if (string.IsNullOrWhiteSpace(url)
            || !Uri.TryCreate(url, UriKind.Absolute, out var uri)
            || (uri.Scheme != "http" && uri.Scheme != "https"))
        {
            App.Log("MarketIcon", $"icon url 无效: {url}");
            return null;
        }

        try
        {
            using var cts = CancellationTokenSource.CreateLinkedTokenSource(ct);
            cts.CancelAfter(TimeSpan.FromSeconds(15));

            var attempts = new List<(HttpClient Client, string Url)> { (Http, uri.ToString()) };
            if (HttpViaEnvProxy is not null) attempts.Add((HttpViaEnvProxy, uri.ToString()));
            var mirror = ToJsDelivrMirror(uri.ToString());
            if (mirror is not null) attempts.Add((Http, mirror));

            using var resp = await GetSuccessAsync(attempts, cts.Token);
            var len = resp.Content.Headers.ContentLength;
            if (len.HasValue && len.Value > MaxIconSize)
            {
                App.Log("MarketIcon", $"icon 超限 {len.Value}B: {url}");
                return null;
            }

            using var ms = new MemoryStream();
            await using var stream = await resp.Content.ReadAsStreamAsync(cts.Token);
            await stream.CopyToAsync(ms, cts.Token);
            if (ms.Length > MaxIconSize)
            {
                App.Log("MarketIcon", $"icon 超限 {ms.Length}B: {url}");
                return null;
            }
            return ms.ToArray();
        }
        catch (OperationCanceledException)
        {
            App.Log("MarketIcon", $"icon 超时/取消: {url}");
            return null;
        }
        catch (Exception ex)
        {
            App.Log("MarketIcon", $"icon 下载异常 {ex.GetType().Name}: {ex.Message} url={url}");
            return null;
        }
    }

    /// <summary>
    /// 比较两个语义化版本号 (例如 "0.2.1" 与 "0.1.9")
    /// 返回 >0 表示 a > b; =0 表示 a == b; <0 表示 a < b
    /// 与 host 端 cmp_version 风格一致：显式去掉 'v'/'V' 前缀再分段比较。
    /// </summary>
    public static int CompareVersion(string? a, string? b)
    {
        if (string.IsNullOrEmpty(a) && string.IsNullOrEmpty(b)) return 0;
        if (string.IsNullOrEmpty(a)) return -1;
        if (string.IsNullOrEmpty(b)) return 1;

        // 对齐 host 的 parse_version_tuple：跳过前导非数字（兼容 "v0.1.0"）
        var normA = TrimLeadingNonDigits(a);
        var normB = TrimLeadingNonDigits(b);

        var segsA = normA.Split('.');
        var segsB = normB.Split('.');
        var len = Math.Max(segsA.Length, segsB.Length);

        for (int i = 0; i < len; i++)
        {
            var partA = i < segsA.Length ? segsA[i] : "0";
            var partB = i < segsB.Length ? segsB[i] : "0";

            if (int.TryParse(partA, out var numA) && int.TryParse(partB, out var numB))
            {
                if (numA != numB) return numA.CompareTo(numB);
            }
            else
            {
                var cmp = string.Compare(partA, partB, StringComparison.OrdinalIgnoreCase);
                if (cmp != 0) return cmp;
            }
        }
        return 0;
    }

    /// <summary>跳过前导非 ASCII 数字字符，对齐 host 端 parse_version_tuple 的 trim_start_matches。</summary>
    private static string TrimLeadingNonDigits(string s)
    {
        int i = 0;
        while (i < s.Length && !char.IsAsciiDigit(s[i])) i++;
        return i == 0 ? s : s[i..];
    }

    /// <summary>
    /// 下载 GitHub 仓库 zipball 并提取指定插件目录
    /// </summary>
    public static async Task<string> DownloadAndExtractZipballAsync(
        string zipballUrl,
        string versionPath,
        string? expectedSha256,
        CancellationToken ct = default,
        Action<DownloadProgressReport>? progress = null)
    {
        var tempZip = Path.Combine(Path.GetTempPath(), $"spark_market_{Guid.NewGuid():N}.zip");
        var tempExtractDir = Path.Combine(Path.GetTempPath(), $"spark_plugin_{Guid.NewGuid():N}");

        try
        {
            await DownloadToFileAsync(zipballUrl, tempZip, MaxZipSize, ct, progress);

            if (!string.IsNullOrEmpty(expectedSha256))
            {
                var actualHash = ComputeSha256(tempZip);
                if (!string.Equals(actualHash, expectedSha256, StringComparison.OrdinalIgnoreCase))
                {
                    throw new InvalidDataException($"Zipball SHA-256 校验失败: 期望 {expectedSha256}, 实际 {actualHash}");
                }
            }

            // 规范化 versionPath，统一用 "/"
            var normVersionPath = versionPath.Trim().Replace('\\', '/').Trim('/');
            if (string.IsNullOrEmpty(normVersionPath))
            {
                throw new ArgumentException("插件版本路径不能为空", nameof(versionPath));
            }

            Directory.CreateDirectory(tempExtractDir);

            using (var archive = ZipFile.OpenRead(tempZip))
            {
                if (archive.Entries.Count > MaxEntryCount)
                {
                    throw new InvalidDataException($"Zip 包内文件过多 ({archive.Entries.Count} > {MaxEntryCount})");
                }

                // GitHub zipball 格式顶层总是唯一根目录，形如 "spark-plugins-master/..."
                var firstEntry = archive.Entries.FirstOrDefault(e => !string.IsNullOrEmpty(e.FullName));
                if (firstEntry == null) throw new InvalidDataException("Zip 包为空");

                var topSeg = firstEntry.FullName.Split('/')[0];
                var targetPrefix = $"{topSeg}/{normVersionPath}/";

                long totalBytes = 0;
                var extractedCount = 0;

                foreach (var entry in archive.Entries)
                {
                    if (string.IsNullOrEmpty(entry.FullName)) continue;
                    var entryNorm = entry.FullName.Replace('\\', '/');

                    if (!entryNorm.StartsWith(targetPrefix, StringComparison.OrdinalIgnoreCase))
                    {
                        continue;
                    }

                    var rel = entryNorm.Substring(targetPrefix.Length);
                    if (string.IsNullOrWhiteSpace(rel) || rel.EndsWith('/'))
                    {
                        continue; // 目录跳过
                    }

                    // Zip Slip 防御：严格检查目标完整路径必须在 tempExtractDir 内部
                    var destPath = Path.GetFullPath(Path.Combine(tempExtractDir, rel));
                    var rootPathWithSlash = Path.GetFullPath(tempExtractDir);
                    if (!rootPathWithSlash.EndsWith(Path.DirectorySeparatorChar.ToString()))
                    {
                        rootPathWithSlash += Path.DirectorySeparatorChar;
                    }

                    if (!destPath.StartsWith(rootPathWithSlash, StringComparison.OrdinalIgnoreCase))
                    {
                        throw new InvalidDataException($"检测到潜在的 Zip Slip 路径穿越攻击: {entry.FullName}");
                    }

                    totalBytes += entry.Length;
                    if (totalBytes > MaxUnpackSize)
                    {
                        throw new InvalidDataException($"解压总大小超过配额限制 ({MaxUnpackSize / 1024 / 1024} MiB)");
                    }

                    var dir = Path.GetDirectoryName(destPath);
                    if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);

                    entry.ExtractToFile(destPath, overwrite: true);
                    extractedCount++;
                }

                if (extractedCount == 0)
                {
                    throw new DirectoryNotFoundException($"在仓库压缩包内未找到插件目录: {versionPath}");
                }

                if (!File.Exists(Path.Combine(tempExtractDir, "plugin.json")))
                {
                    throw new FileNotFoundException("提取出的插件目录中缺少 plugin.json");
                }
            }

            return tempExtractDir;
        }
        catch
        {
            CleanupTemp(tempExtractDir);
            throw;
        }
        finally
        {
            try { if (File.Exists(tempZip)) File.Delete(tempZip); } catch { }
        }
    }

    /// <summary>
    /// 直接下载并解压预打包的插件 zip (用于第三方仓库直接提供 url)
    /// </summary>
    public static async Task<string> DownloadDirectZipAsync(
        string zipUrl,
        string? expectedSha256,
        CancellationToken ct = default,
        Action<DownloadProgressReport>? progress = null)
    {
        var tempZip = Path.Combine(Path.GetTempPath(), $"spark_market_{Guid.NewGuid():N}.zip");
        var tempExtractDir = Path.Combine(Path.GetTempPath(), $"spark_plugin_{Guid.NewGuid():N}");

        try
        {
            await DownloadToFileAsync(zipUrl, tempZip, MaxZipSize, ct, progress);

            if (!string.IsNullOrEmpty(expectedSha256))
            {
                var actualHash = ComputeSha256(tempZip);
                if (!string.Equals(actualHash, expectedSha256, StringComparison.OrdinalIgnoreCase))
                {
                    throw new InvalidDataException($"Zip SHA-256 校验失败: 期望 {expectedSha256}, 实际 {actualHash}");
                }
            }

            Directory.CreateDirectory(tempExtractDir);

            using (var archive = ZipFile.OpenRead(tempZip))
            {
                if (archive.Entries.Count > MaxEntryCount)
                {
                    throw new InvalidDataException($"Zip 包内文件过多 ({archive.Entries.Count} > {MaxEntryCount})");
                }

                long totalBytes = 0;
                var rootPathWithSlash = Path.GetFullPath(tempExtractDir);
                if (!rootPathWithSlash.EndsWith(Path.DirectorySeparatorChar.ToString()))
                {
                    rootPathWithSlash += Path.DirectorySeparatorChar;
                }

                foreach (var entry in archive.Entries)
                {
                    if (string.IsNullOrEmpty(entry.FullName) || entry.FullName.EndsWith('/')) continue;

                    var destPath = Path.GetFullPath(Path.Combine(tempExtractDir, entry.FullName));
                    if (!destPath.StartsWith(rootPathWithSlash, StringComparison.OrdinalIgnoreCase))
                    {
                        throw new InvalidDataException($"检测到潜在的 Zip Slip 路径穿越攻击: {entry.FullName}");
                    }

                    totalBytes += entry.Length;
                    if (totalBytes > MaxUnpackSize)
                    {
                        throw new InvalidDataException($"解压总大小超过配额限制 ({MaxUnpackSize / 1024 / 1024} MiB)");
                    }

                    var dir = Path.GetDirectoryName(destPath);
                    if (!string.IsNullOrEmpty(dir)) Directory.CreateDirectory(dir);

                    entry.ExtractToFile(destPath, overwrite: true);
                }

                if (!File.Exists(Path.Combine(tempExtractDir, "plugin.json")))
                {
                    // 若包内包含一层单根目录，尝试查找
                    var subDirs = Directory.GetDirectories(tempExtractDir);
                    if (subDirs.Length == 1 && File.Exists(Path.Combine(subDirs[0], "plugin.json")))
                    {
                        return subDirs[0];
                    }
                    throw new FileNotFoundException("插件 zip 包根目录缺少 plugin.json");
                }
            }

            return tempExtractDir;
        }
        catch
        {
            CleanupTemp(tempExtractDir);
            throw;
        }
        finally
        {
            try { if (File.Exists(tempZip)) File.Delete(tempZip); } catch { }
        }
    }

    private static async Task DownloadToFileAsync(string url, string destPath, long maxBytes, CancellationToken ct, Action<DownloadProgressReport>? progress = null)
    {
        // 网络层异常统一翻成中文正文，原文进 InnerException（App.Log 可溯源）。
        // 保持 HttpRequestException 类型不变：安装侧"zipball 失败回退官方地址"的 catch 依赖它。
        try
        {
            // 候选链：默认客户端 → 环境代理客户端（整体禁网时逐路径重试；GitHub 存档域名
            // github.com/codeload 直连通常可达，raw 域名已由索引/图标的镜像链覆盖）。
            // 整链 45 秒预算封顶：弱网下两个候选先后响应头超时也不会让进度条静默卡超 45 秒，
            // 且全候选超时按超时语义抛出（走下方"下载插件包超时"文案，不误报成网络错误）。
            var attempts = new List<(HttpClient Client, string Url)> { (Http, url) };
            if (HttpViaEnvProxy is not null) attempts.Add((HttpViaEnvProxy, url));
            using var response = await GetSuccessAsync(attempts, ct, TimeSpan.FromSeconds(45));

            var contentLength = response.Content.Headers.ContentLength;
            if (contentLength.HasValue && contentLength.Value > maxBytes)
            {
                throw new InvalidDataException($"下载文件体积过大 ({contentLength.Value / 1024 / 1024} MiB > {maxBytes / 1024 / 1024} MiB)");
            }

            using var stream = await response.Content.ReadAsStreamAsync(ct);
            using var fs = new FileStream(destPath, FileMode.Create, FileAccess.Write, FileShare.None, 81920, useAsync: true);

            // 进度节流：读块频率高，按时间窗口 80ms 回调一次；收尾必报一次终值（Total 可能为 null=总量未知）。
            var throttle = System.Diagnostics.Stopwatch.StartNew();
            var buffer = new byte[81920];
            long totalRead = 0;
            int read;
            while ((read = await stream.ReadAsync(buffer, 0, buffer.Length, ct)) > 0)
            {
                totalRead += read;
                if (totalRead > maxBytes)
                {
                    throw new InvalidDataException($"下载流超过最大限制 ({maxBytes / 1024 / 1024} MiB)");
                }
                await fs.WriteAsync(buffer, 0, read, ct);
                if (progress is not null && throttle.ElapsedMilliseconds >= 80)
                {
                    throttle.Restart();
                    progress(new DownloadProgressReport(totalRead, contentLength));
                }
            }
            progress?.Invoke(new DownloadProgressReport(totalRead, contentLength));
        }
        catch (HttpRequestException ex)
        {
            var detail = ex.StatusCode is { } sc
                ? $"服务器返回 HTTP {(int)sc}"
                : "网络错误或仓库地址不可达";
            throw new HttpRequestException($"下载插件包失败（{detail}）", ex);
        }
        catch (TaskCanceledException ex) when (!ct.IsCancellationRequested)
        {
            throw new HttpRequestException("下载插件包超时，请检查网络后重试", ex);
        }
    }

    public static string ComputeSha256(string filePath)
    {
        using var sha = SHA256.Create();
        using var stream = File.OpenRead(filePath);
        var hash = sha.ComputeHash(stream);
        return BitConverter.ToString(hash).Replace("-", "").ToLowerInvariant();
    }

    public static void CleanupTemp(string? tempDir)
    {
        if (string.IsNullOrEmpty(tempDir)) return;
        try
        {
            if (Directory.Exists(tempDir)) Directory.Delete(tempDir, recursive: true);
        }
        catch
        {
            // 尽力清理
        }
    }
}

/// <summary>下载进度快照（字节）。Total=null 表示服务端未声明 Content-Length（总量未知，UI 走不确定条）。</summary>
public readonly record struct DownloadProgressReport(long Received, long? Total);
