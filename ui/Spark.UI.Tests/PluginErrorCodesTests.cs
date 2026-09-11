using Spark.UI.Services;
using Xunit;

namespace Spark.UI.Tests;

/// <summary>
/// host 错误码分类（<see cref="PluginErrorCodes"/>）。这是"host 抛出的码"与"插件看到的
/// error.code"之间的桥：漏识别一个码不会报错，只会静默降级成 UNAVAILABLE，让插件按
/// code 分支时失真——所以这张表必须有自动化护栏。
/// </summary>
public class PluginErrorCodesTests
{
    [Theory]
    [InlineData("PERMISSION_DENIED: clipboard")]
    [InlineData("PERMISSION_SCOPE: 路径不在授权目录")]
    [InlineData("NETWORK_FAILED: 连接超时")]
    [InlineData("INVALID_ARGS: clipboard.write 需要 object 参数")]
    [InlineData("FILE_TOO_LARGE: imagePng 解码后 33554433 字节，超过上限 33554432 字节")]
    [InlineData("UNSUPPORTED_FORMAT: imagePng 无法解码为位图：WIC 解码器: 0x88982F07")]
    [InlineData("UNAVAILABLE: rpc 仅 native 插件可用")]
    public void Every_documented_code_is_recognized(string hostMessage)
    {
        var (expected, _) = SplitCode(hostMessage);
        Assert.Equal(expected, PluginErrorCodes.Classify(hostMessage));
    }

    [Fact]
    public void Recognized_table_covers_every_documented_code()
    {
        // 规范 §8.6 的表与识别表必须逐项对齐：这张清单是对文档的镜像，改文档时这里要同步。
        var documented = new[]
        {
            PluginErrorCodes.PermissionDenied,
            PluginErrorCodes.PermissionScope,
            PluginErrorCodes.NetworkFailed,
            PluginErrorCodes.InvalidArgs,
            PluginErrorCodes.FileTooLarge,
            PluginErrorCodes.UnsupportedFormat,
            PluginErrorCodes.Unavailable,
        };
        foreach (var code in documented)
        {
            Assert.True(PluginErrorCodes.IsRecognized(code), $"规范 §8.6 的 {code} 不在识别表中");
            Assert.Contains(code, PluginErrorCodes.Recognized);
        }
        // 兜底码必须是已识别码之一（否则"识别不到"会产出插件认识不了的 code）。
        Assert.Contains(PluginErrorCodes.Fallback, PluginErrorCodes.Recognized);
        // 识别表本身不应有重复项（重复无害但说明表没人管）。
        Assert.Equal(codes_count(PluginErrorCodes.Recognized), PluginErrorCodes.Recognized.Length);
    }

    private static int codes_count(string[] items) => items.Distinct().Count();

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("随便一段没有前缀的消息")]
    [InlineData("ECONNRESET: 不是我们的码")]
    public void Unrecognized_messages_fall_back_to_unavailable(string? hostMessage)
    {
        Assert.Equal(PluginErrorCodes.Unavailable, PluginErrorCodes.Classify(hostMessage));
        Assert.Equal(PluginErrorCodes.Fallback, PluginErrorCodes.Classify(hostMessage));
    }

    [Fact]
    public void More_specific_code_wins_over_unavailable()
    {
        // host 偶尔会把 UNAVAILABLE 拼进 detail 文本；此时必须仍按具体码分类。
        Assert.Equal(
            PluginErrorCodes.InvalidArgs,
            PluginErrorCodes.Classify("INVALID_ARGS: method UNAVAILABLE"));
        Assert.Equal(
            PluginErrorCodes.PermissionDenied,
            PluginErrorCodes.Classify("PERMISSION_DENIED: capability is UNAVAILABLE here"));
    }

    [Fact]
    public void Codes_are_prefix_free_so_order_does_not_matter()
    {
        // 识别实现是"包含即命中"，各码互不为前缀才安全；若未来加入互为前缀的码，
        // 必须改成最长匹配。这条测试就是给那次改动留的提醒。
        var codes = PluginErrorCodes.Recognized;
        foreach (var a in codes)
        {
            foreach (var b in codes)
            {
                if (ReferenceEquals(a, b) || a == b) continue;
                Assert.False(a.Contains(b, StringComparison.Ordinal) || b.Contains(a, StringComparison.Ordinal),
                    $"错误码互为前缀会使命中顺序影响结果: {a} / {b}");
            }
        }
    }

    /// <summary>把 "CODE: detail" 拆开，用于断言"host 抛什么码就应识别出什么码"。</summary>
    private static (string Code, string _) SplitCode(string hostMessage)
    {
        var idx = hostMessage.IndexOf(':');
        return idx > 0 ? (hostMessage[..idx], hostMessage) : (hostMessage, hostMessage);
    }
}
