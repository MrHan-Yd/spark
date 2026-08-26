using System;
using System.Collections.Generic;
using System.IO;
using System.IO.Compression;
using System.Linq;
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

    /// <summary>官方仓库 zipball 回退地址（索引未声明 zipball_url 时使用）。</summary>
    public const string OfficialZipballUrl = "https://github.com/MrHan-Yd/spark-plugins/archive/refs/heads/master.zip";

    private static readonly HttpClient Http = new()
    {
        Timeout = TimeSpan.FromSeconds(30)
    };

    private const long MaxZipSize = 50 * 1024 * 1024;      // 50 MiB
    private const long MaxUnpackSize = 100 * 1024 * 1024;  // 100 MiB
    private const int MaxEntryCount = 10000;

    static RegistryService()
    {
        Http.DefaultRequestHeaders.UserAgent.ParseAdd("Spark-PluginMarket/1.0");
    }

    /// <summary>
    /// 抓取并解析仓库索引；遇到网络抖动自动重试最多 2 次。
    /// </summary>
    public static async Task<RegistryIndexDto> FetchIndexAsync(string url, CancellationToken ct = default)
    {
        const int maxAttempts = 3;  // 1 次首发 + 2 次重试
        var lastEx = (Exception?)null;
        for (int attempt = 1; attempt <= maxAttempts; attempt++)
        {
            try
            {
                ct.ThrowIfCancellationRequested();
                var json = await Http.GetStringAsync(url, ct);
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

                return index;
            }
            catch (OperationCanceledException) { throw; }
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
        CancellationToken ct = default)
    {
        var tempZip = Path.Combine(Path.GetTempPath(), $"spark_market_{Guid.NewGuid():N}.zip");
        var tempExtractDir = Path.Combine(Path.GetTempPath(), $"spark_plugin_{Guid.NewGuid():N}");

        try
        {
            await DownloadToFileAsync(zipballUrl, tempZip, MaxZipSize, ct);

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
        CancellationToken ct = default)
    {
        var tempZip = Path.Combine(Path.GetTempPath(), $"spark_market_{Guid.NewGuid():N}.zip");
        var tempExtractDir = Path.Combine(Path.GetTempPath(), $"spark_plugin_{Guid.NewGuid():N}");

        try
        {
            await DownloadToFileAsync(zipUrl, tempZip, MaxZipSize, ct);

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

    private static async Task DownloadToFileAsync(string url, string destPath, long maxBytes, CancellationToken ct)
    {
        using var response = await Http.GetAsync(url, HttpCompletionOption.ResponseHeadersRead, ct);
        response.EnsureSuccessStatusCode();

        var contentLength = response.Content.Headers.ContentLength;
        if (contentLength.HasValue && contentLength.Value > maxBytes)
        {
            throw new InvalidDataException($"下载文件体积过大 ({contentLength.Value / 1024 / 1024} MiB > {maxBytes / 1024 / 1024} MiB)");
        }

        using var stream = await response.Content.ReadAsStreamAsync(ct);
        using var fs = new FileStream(destPath, FileMode.Create, FileAccess.Write, FileShare.None, 81920, useAsync: true);

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
