1. 从 `FloatingBallWindow.cs` 移除 `SetWindowRgn/CreateEllipticRgn` 及其所有调用，消除产生锯齿和凸点的 GDI 硬裁剪。
2. 在 `SetupChrome()` 中调用 `DwmExtendFrameIntoClientArea`，使用四边 `-1` margins，让整个 HWND 由 DWM 玻璃透明合成，窗口四角不再显示默认黑底。
3. 保留 `_ball` 的圆角/椭圆 XAML 视觉层、渐变玻璃背景和高光；增加一层轻微的 XAML 圆形边缘 Stroke（若当前版本没有边框则恢复为内部抗锯齿边框），不使用原生区域裁剪。
4. 确保 `SetPos` 和 DPI 变化只调整窗口尺寸，不再重建 GDI region；Logo、拖拽、贴边收起逻辑保持不变。
5. 编译、运行 Rust 门禁并交 Code Auditor 审查；通过后重启 Spark 供你查看最终圆润效果。