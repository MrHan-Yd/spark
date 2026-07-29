# Spark UI 原型说明 — 玻璃风格

> 产品：**Spark** · 可交互原型：`ui-prototype/`  
> 生产 UI：**C# WinUI 3**（非本 HTML）· 技术栈见 [TECH_STACK.md](./TECH_STACK.md)  
> 功能见 [FEATURES.md](./FEATURES.md)

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

## 4. 落到 C# WinUI 3 的映射建议

| 原型（CSS） | C# WinUI 3 |
|-------------|--------------|
| `backdrop-filter: blur` | `AcrylicBrush` / Desktop Acrylic；可辅 Mica |
| 细边框高光 | `Border` + 主题色 / 细 `Stroke` |
| 列表 / 平铺 | `ListView` 或 `ItemsRepeater`（**必须虚拟化**）；平铺用 `UniformGridLayout` 等 |
| 主窗内设置页 | 同一 `Window` 内切换 `Frame`/自定义区域（非第二窗） |
| 圆角阴影 | `CornerRadius` + `ThemeShadow` |
| 深浅色 | `RequestedTheme` + ResourceDictionary |
| 页面切换动画 | `Composition` / 内置导航过渡 |
| IPC | Named Pipe 客户端，DTO 对齐 `docs/DESIGN` |

**注意：**

- 生产实现是 **C# WinUI**，本目录 HTML **仅原型**  
- 不要用 WebView2 套原型当主界面（性能第一）  
- 系统托盘由 **Host（Rust）** 负责，不在 UI 窗内画假托盘

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
