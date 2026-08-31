using System;

namespace Spark.UI.Services;

/// <summary>
/// host (Rust) 侧错误原文是英文的 thiserror 显示串（见 crates/plugin-manager/src/error.rs），
/// 到达用户眼前前在此统一做一层中文映射。只翻译已知前缀/固定串，未识别的原文透传，
/// 保证 host 新增错误类型时 UI 仍能显示原始信息而不是空白。
/// </summary>
public static class HostErrorText
{
    /// <summary>把 host 回包 error.message 包装成用户可读异常：已知模式正文为中文，
    /// 英文原文保留在 InnerException 供 ui-crash.log（App.Log 打印完整异常链）溯源；未识别模式正文保持原文。</summary>
    public static InvalidOperationException ToException(string raw)
    {
        var zh = Translate(raw);
        return ReferenceEquals(zh, raw)
            ? new InvalidOperationException(raw)
            : new InvalidOperationException(zh, new InvalidOperationException(raw));
    }

    /// <summary>host 不可达的统一异常（调用方 guard：未连接即抛）。</summary>
    public static InvalidOperationException HostUnavailable() => new(Translate("host unavailable"));

    /// <summary>已知 host / IPC 错误模式 → 中文；未识别返回原文引用（调用方据此判断是否需要包 InnerException）。</summary>
    public static string Translate(string raw)
    {
        if (string.IsNullOrEmpty(raw)) return raw;
        if (raw.StartsWith("plugin signature invalid:", StringComparison.Ordinal))
            return $"插件签名无效（{raw["plugin signature invalid:".Length..].Trim()}），已拒绝安装。请从可信来源重新获取该插件";
        if (raw.StartsWith("plugin signature missing but required:", StringComparison.Ordinal))
            return $"该插件未带签名，且当前已开启\"仅安装带有效签名的插件\"，已拒绝安装（{raw["plugin signature missing but required:".Length..].Trim()}）";
        if (raw.StartsWith("invalid manifest:", StringComparison.Ordinal))
            return $"插件清单 (plugin.json) 无效：{raw["invalid manifest:".Length..].Trim()}";
        if (raw.StartsWith("io error:", StringComparison.Ordinal))
            return $"文件读写失败：{raw["io error:".Length..].Trim()}";
        if (raw.StartsWith("json error:", StringComparison.Ordinal))
            return $"插件配置文件解析失败：{raw["json error:".Length..].Trim()}";
        if (raw.StartsWith("verify io error:", StringComparison.Ordinal))
            return $"验签时读取文件失败：{raw["verify io error:".Length..].Trim()}";
        return raw switch
        {
            "host unavailable" => "无法连接 Spark 后台服务 (host)，请确认后台正在运行",
            "host ipc timeout" => "后台服务响应超时，请稍后重试",
            "ipc error" => "与后台服务通信出错",
            _ => raw,
        };
    }
}