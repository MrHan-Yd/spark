# UI 原型说明 — macOS 玻璃风格

> 可交互原型目录：`ui-prototype/`  
> 产品功能见 [FEATURES.md](./FEATURES.md)

---

## 1. 如何预览

```text
用浏览器打开：
  D:\demo\test01\ui-prototype\index.html
```

或在项目目录：

```powershell
Start-Process ui-prototype\index.html
```

**操作：**

| 操作 | 效果 |
|------|------|
| `Alt+Space` / `Ctrl+Space` | 显示/隐藏主面板 |
| 输入 | 过滤结果 |
| `↑↓` Enter | 选择 / 执行 |
| `Tab` | 动作菜单 |
| `Esc` | 清空或关闭 |
| 右下角圆点 | 托盘菜单 → 设置 |
| 设置 · 外观 | 深色/浅色玻璃、宽度、减少动画 |
| 设置 · 热键 | Alt+Space / Ctrl+Space 预设 |

---

## 2. 视觉方向

对标 **macOS Spotlight / Raycast** 的玻璃质感，而非 Windows Acrylic 默认模板。

| 要素 | 规格 |
|------|------|
| 材质 | 高模糊（~40px）+ 饱和度提升 + 半透明底 |
| 边缘 | 细白边 / 内高光渐变，模拟玻璃折光 |
| 阴影 | 大而软的投影，浮在桌面上方 |
| 圆角 | 主窗约 16px，列表行 12px |
| 字体 | 系统字体栈；搜索框 ~20px；列表 14/12 |
| 强调色 | macOS 蓝 `#0A84FF` / `#007AFF` |
| 图标 | 圆角方块 + 渐变（原型用字母；正式版用真实图标） |
| 动效 | 轻微 scale + fade 弹出；可关 |

### 深色玻璃（默认）

- 底：`rgba(28,28,30,0.55)` + blur  
- 文字：近白层级 92% / 55% / 38%  
- 选中行：蓝色半透明叠层  

### 浅色玻璃

- 底：`rgba(255,255,255,0.58)` + blur  
- 需更亮墙纸才“透”得出层次（原型桌面已模拟）

---

## 3. 界面结构（原型已覆盖）

1. **主搜索窗**：搜索框、结果列表、底栏快捷键提示  
2. **动作表**：Tab 展开次要动作  
3. **设置**：通用 / 热键 / 外观 / 插件  
4. **托盘菜单**：显示、暂停热键、设置、退出  

未做独立页：插件市场、剪贴板历史（P2+）。

---

## 4. 落到 WinUI 3 的映射建议

| 原型（CSS） | WinUI / 原生 |
|-------------|--------------|
| `backdrop-filter: blur` | `AcrylicBrush` / `DesktopAcrylic` / Win11 `MicaAlt` + 自定义半透明（玻璃感更强时用 Acrylic） |
| 细边框高光 | `Border` + 1px `LinearGradientBrush` 或主题资源 |
| 列表虚拟化 | `ListView` / `ItemsRepeater`（必须） |
| 圆角阴影 | `CornerRadius` + 系统 `ThemeShadow` 或自定义 |
| 深浅色 | `RequestedTheme` + 资源字典 |
| 弹出动画 | `Composition` 缩放透明度 |

**注意：** 完整 CSS 玻璃在 WinUI 上要靠 Acrylic + 图层，不要嵌 WebView 只为皮肤（性能优先）。原型仅作视觉与交互参考。

---

## 5. 设计 token（实现时沿用）

```text
--launcher-width:  680 (560–840 可调)
--radius-window:   16
--radius-row:      12
--blur:            40
--row-height:      ~56 (含 padding)
--max-visible:     ~7–9 行
--font-search:     20
--font-title:      14
--font-subtitle:   12
--accent:          #0A84FF (dark) / #007AFF (light)
```

---

## 6. 文件

```text
ui-prototype/
  index.html    结构
  styles.css    玻璃主题与布局
  app.js        交互与假数据
docs/
  UI_PROTOTYPE.md  本文
```
