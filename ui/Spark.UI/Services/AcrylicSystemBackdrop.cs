using Microsoft.UI.Composition;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace Spark.UI.Services;

/// <summary>可调参的桌面亚克力 backdrop（macOS vibrancy 同构）。
/// WinUI 框架在 OnTargetConnected 里直接传入 ICompositionSupportsSystemBackdrop
/// （1.6 的 AppWindow 不实现该接口，无法自己 As&lt;T&gt; 获取），这是官方支持的自定义
/// SystemBackdrop 路径：既能调 TintColor/TintOpacity/LuminosityOpacity，又由框架
/// 正确渲染在 XAML 内容层之后（直接 SetWindowCompositionAttribute 会被不透明的
/// XAML 合成层盖住，表现为窗口纯黑）。
/// 注意：回调必须自行 try/catch——全局 UnhandledException 会弹模态 MessageBox
/// 阻塞 UI 线程（表现为窗口卡死）。</summary>
public sealed class AcrylicSystemBackdrop : SystemBackdrop
{
    private DesktopAcrylicController? _controller;
    private bool _dark = true;

    protected override void OnTargetConnected(
        ICompositionSupportsSystemBackdrop connectedTarget, XamlRoot xamlRoot)
    {
        base.OnTargetConnected(connectedTarget, xamlRoot);
        try
        {
            if (_controller is not null) return;   // 已连接（同实例多窗口共享时不重复建）

            _controller = new DesktopAcrylicController();
            // 默认配置自动跟随激活状态 / 主题（XamlRoot.RequestedTheme）
            var config = GetDefaultSystemBackdropConfiguration(connectedTarget, xamlRoot);
            _controller.SetSystemBackdropConfiguration(config);
            _controller.AddSystemBackdropTarget(connectedTarget);
            ApplyTint();
            App.Log("Acrylic", "connected & configured");
        }
        catch (Exception ex)
        {
            App.Log("Acrylic", ex);
            try { _controller?.Dispose(); } catch { /* ignore */ }
            _controller = null;
        }
    }

    protected override void OnTargetDisconnected(ICompositionSupportsSystemBackdrop disconnectedTarget)
    {
        try { base.OnTargetDisconnected(disconnectedTarget); }
        catch (Exception ex) { App.Log("Acrylic", ex); }
        try
        {
            if (_controller is not null)
            {
                _controller.RemoveSystemBackdropTarget(disconnectedTarget);
                _controller.Dispose();
                _controller = null;
                App.Log("Acrylic", "disconnected");
            }
        }
        catch (Exception ex)
        {
            App.Log("Acrylic", ex);
            _controller = null;
        }
    }

    /// <summary>配置变化（主题/激活状态）时框架回调；1.6 在窗口隐藏/时序切换时传入的
    /// target 可能无效，基类实现会抛 ArgumentException——必须隔离，否则冒泡到全局
    /// UnhandledException 弹模态 MessageBox 把窗口卡死（已实测踩过）。</summary>
    protected override void OnDefaultSystemBackdropConfigurationChanged(
        ICompositionSupportsSystemBackdrop target, XamlRoot xamlRoot)
    {
        try
        {
            base.OnDefaultSystemBackdropConfigurationChanged(target, xamlRoot);
        }
        catch (Exception ex)
        {
            App.Log("Acrylic", ex);
        }
    }

    /// <summary>主题切换时更新 tint（由 MainWindow.ApplyTheme 调用）。</summary>
    public void ApplyTheme(bool dark)
    {
        _dark = dark;
        ApplyTint();
    }

    private void ApplyTint()
    {
        try
        {
            if (_controller is null) return;
            if (_dark)
            {
                // 深色：Win11 深色亚克力观感（半透明深灰 + 中等发光度）
                _controller.TintColor = Color.FromArgb(0xFF, 0x1C, 0x1C, 0x1E);
                _controller.TintOpacity = 0.5f;
                _controller.LuminosityOpacity = 0.55f;
                _controller.FallbackColor = Color.FromArgb(0xFF, 0x1C, 0x1C, 0x1E);
            }
            else
            {
                // 浅色：macOS vibrancy 参数（tint 约 45% 透度，内容色块透过玻璃柔化可见；
                // luminosity 0.9 制造奶白辉光）。FallbackColor 也设浅色，回退不出现黑块
                _controller.TintColor = Color.FromArgb(0xFF, 0xF2, 0xF5, 0xFA);
                _controller.TintOpacity = 0.45f;
                _controller.LuminosityOpacity = 0.9f;
                _controller.FallbackColor = Color.FromArgb(0xFF, 0xF2, 0xF5, 0xFA);
            }
        }
        catch (Exception ex)
        {
            App.Log("Acrylic", ex);
        }
    }
}
