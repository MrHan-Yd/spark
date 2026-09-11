namespace Spark.UI.Services;

/// <summary>
/// host 错误码约定（《插件开发规范》§8.6）：host 以 <c>"CODE: detail"</c> 形式回错误消息，
/// 页面侧把它还原成 <c>error.code</c> 供插件分支（<c>sparkError</c> / <c>Promise rejection</c>）。
///
/// 本类是**唯一事实来源**：<see cref="Spark.UI.Views.PluginWindow.ClassifyError"/> 与测试工程
/// 都引用这里的表（测试工程不引用 WinUI 工程，故本文件必须保持零 WinUI 依赖）。
///
/// 新增 host 错误码时三处必须同步，漏一处都会出问题：
/// ① host 抛出处（Rust）；② 本表 <see cref="Recognized"/>；③ 规范 §8.6 的错误码表。
/// 漏 ② 不会报错、只会静默降级成 <see cref="Unavailable"/>——插件按 code 分支就失真
/// （这正是 clipboard 图片写入上线时踩过、并由测试堵住的坑）；漏 ③ 则插件开发者无从知晓该码存在。
/// </summary>
public static class PluginErrorCodes
{
    /// <summary>未声明或用户拒绝该权限。</summary>
    public const string PermissionDenied = "PERMISSION_DENIED";

    /// <summary>超出授权范围（如 fs 路径不在授权目录）。</summary>
    public const string PermissionScope = "PERMISSION_SCOPE";

    /// <summary>网络请求失败。</summary>
    public const string NetworkFailed = "NETWORK_FAILED";

    /// <summary>参数不合法。</summary>
    public const string InvalidArgs = "INVALID_ARGS";

    /// <summary>单次内容超过上限（fs 文本 10MB；clipboard 文本 10MB、图片 32MB）。</summary>
    public const string FileTooLarge = "FILE_TOO_LARGE";

    /// <summary>内容格式不支持（如 clipboard.write 的 imagePng 无法解码为位图）。</summary>
    public const string UnsupportedFormat = "UNSUPPORTED_FORMAT";

    /// <summary>该能力在当前平台/版本不可用；同时充当**兜底码**。</summary>
    public const string Unavailable = "UNAVAILABLE";

    /// <summary>
    /// 会被识别为 <c>error.code</c> 的前缀表。顺序无关紧要（各码互不为前缀），
    /// 但兜底码放最后，让"更具体的码先命中"的阅读直觉成立。
    /// </summary>
    public static readonly string[] Recognized =
    {
        PermissionDenied,
        PermissionScope,
        NetworkFailed,
        InvalidArgs,
        FileTooLarge,
        UnsupportedFormat,
        Unavailable,
    };

    /// <summary>消息里没有任何已识别前缀时的兜底码。</summary>
    public static string Fallback => Unavailable;

    /// <summary>
    /// 从 host 的 <c>"CODE: detail"</c> 消息还原 <c>error.code</c>。
    /// 识别不到任何前缀时返回 <see cref="Fallback"/>（不抛异常——错误分类失败不该再抛一个错）。
    /// </summary>
    public static string Classify(string? hostMessage)
    {
        var msg = hostMessage ?? "";
        foreach (var code in Recognized)
        {
            if (msg.Contains(code, StringComparison.Ordinal)) return code;
        }
        return Fallback;
    }

    /// <summary>是否为已识别的 host 错误码（测试与调试用）。</summary>
    public static bool IsRecognized(string? code)
        => !string.IsNullOrEmpty(code) && Recognized.Contains(code);
}
