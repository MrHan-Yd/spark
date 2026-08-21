# 插件市场二期 — 实现任务清单

> 规范文档：[`插件开发/插件市场与仓库.md`](../插件开发/插件市场与仓库.md)
> 创建时间：2026-08-21
> 状态：Phase 1（文档）已完成，Phase 2-5 待实施

---

## 总览

插件市场功能分 5 个 Phase，可跨多次会话完成。

**阶段依赖**：Phase 1（文档）✅ → Phase 2（Rust config）→ Phase 3（C# 服务层，HostIpcClient 部分依赖 Phase 2）→ Phase 4（C# UI，依赖 Phase 3）→ Phase 5（联调，依赖全部）。Phase 3 的 `RegistryService.cs`（纯 HTTP/zip 逻辑）可与 Phase 2 并行开发，但 `HostIpcClient.cs` 的 `PluginRegistryUrls` 字段依赖 Phase 2 的协议变更。

**架构要点**：UI 驱动，零新 Rust 依赖。UI 用已有 `HttpClient` + .NET 8 内置 `ZipArchive` 完成抓取/下载/解压，安装调 host 已有的 `host.plugin.install`。host 只加一个配置字段 `plugin_registry_urls: Vec<String>`（自定义仓库 URL 列表，空=仅官方），通过已有的 `host.get_config` / `host.set_config` 管理，**不新增 IPC 方法**。

---

## Phase 1：规范文档 ✅ 已完成

- [x] 新建 `插件开发/插件市场与仓库.md` — 完整规范
- [x] 更新 `插件开发/插件开发规范.md` — 路线表 + marketplace 引用
- [x] 更新 `插件开发/WebView插件开发.md` — 发布章节加市场路径
- [x] 更新 `插件开发/Native插件开发.md` — 打包发布章节加市场路径
- [x] 新建本任务清单

---

## Phase 2：Rust 配置字段（小改，约 30 分钟）

> **不新增 IPC 方法**。复用已有的 `host.get_config` / `host.set_config`，只加一个字段。`HostMethod` 枚举不变。

### 2.1 HostConfig 加字段
- [ ] `crates/host/src/config.rs`
  - `HostConfig` struct 加 `pub plugin_registry_urls: Vec<String>`（默认空 `vec![]`）
  - `Default` 实现中设为 `vec![]`
  - `host.get_config` 返回值自动包含新字段（HostConfig 整体序列化，无需额外改动）

### 2.2 SetConfigParams 加字段
- [ ] `crates/ipc/src/protocol.rs`
  - `SetConfigParams` struct 加 `pub plugin_registry_urls: Option<Vec<String>>`

### 2.3 ipc_server 合并逻辑
- [ ] `crates/host/src/ipc_server.rs`
  - `host.set_config` handler 合并新字段：
    ```rust
    if let Some(urls) = params.plugin_registry_urls {
        config.plugin_registry_urls = urls;
    }
    ```
  - 空值语义：`None` = 不修改，`Some(vec)` = 替换整个列表，`Some(vec![])` = 清除全部
  - 在 `changed` 判断中加入新字段（影响 save 调用）

### 2.4 质量门禁
- [ ] `cargo fmt`
- [ ] `cargo test --workspace`
- [ ] Code Auditor 审计

### 涉及文件
```
crates/host/src/config.rs       — HostConfig + Default (plugin_registry_urls: Vec<String>)
crates/ipc/src/protocol.rs      — SetConfigParams (plugin_registry_urls: Option<Vec<String>>)
crates/host/src/ipc_server.rs   — set_config dispatch arm
```

---

## Phase 3：C# 服务层 + DTO（约 1 小时）

### 3.1 DTO 定义
- [ ] 新建 `ui/Spark.UI/Models/RegistryDto.cs`
  - `RegistryIndexDto` { Schema, Name, ZipballUrl, Updated, Plugins }
  - `RegistryPluginDto` { Id, Name, Description, Author, Homepage, Icon, Runtime, Permissions, Latest, Versions }
  - `RegistryVersionDto` { Version, Path, Url, Sha256, Size, Released }
  - `RegistryPluginViewDto`（UI 展示用：含 InstalledVersion, ButtonLabel, IsInstalled, CanUpdate）

### 3.2 HostIpcClient 加字段
- [ ] `ui/Spark.UI/Services/HostIpcClient.cs`
  - `HostConfigDto` 加 `PluginRegistryUrls`（`List<string>`，空列表 = 未配置）
  - `HostConfigUpdate` 加 `PluginRegistryUrls`（`List<string>?`，null = 不修改，空列表 = 清除）
  - 确认 `GetConfigAsync` / `SetConfigAsync` 序列化/反序列化正确

### 3.3 RegistryService
- [ ] 新建 `ui/Spark.UI/Services/RegistryService.cs`
  - `const OfficialRegistryUrl = "https://raw.githubusercontent.com/OWNER/spark-plugins/main/registry.json"`
  - `static async Task<RegistryIndexDto> FetchIndexAsync(string url, CancellationToken ct)`
    - HttpClient GET → JSON 反序列化 → 校验 `schema == 1` → 跳过不完整条目 → 返回索引
  - `static async Task<string> DownloadAndExtractAsync(string zipballUrl, string versionPath, string? expectedSha256, CancellationToken ct)`
    - 下载 zipball（50 MiB 上限 + 30s 超时）→ 校验 sha256（如有）→ ZipArchive 提取子目录
    - 顶层目录唯一性校验 + 目标路径存在性校验 + plugin.json 存在性校验
    - 执行规范 §9 全部安全校验（Zip Slip 规范化路径校验 + 解压配额 100 MiB/10000 文件）
    - 返回 temp 目录路径（GUID 命名）
  - `static async Task<string> DownloadDirectAsync(string zipUrl, string? expectedSha256, CancellationToken ct)`
    - 下载预打包 zip → 校验 sha256 → 全量解压 → 同样执行 §9 安全校验 → 返回 temp 目录路径
  - `static void CleanupTemp(string tempDir)` — try/catch 删除
  - `static string ComputeSha256(string filePath)` — SHA-256 计算
  - `static int CompareVersion(string a, string b)` — 语义化版本比较（split `.` → 逐段 int 比较）

### 3.4 本地版本比较
- [ ] `RegistryService` 或 helper：`CompareVersion(string a, string b) → int`
  - 语义化版本比较（split `.` → 逐段 int 比较），与 host 侧 `cmp_version` 逻辑对齐

### 涉及文件
```
ui/Spark.UI/Models/RegistryDto.cs          — 新建
ui/Spark.UI/Services/HostIpcClient.cs      — 加字段
ui/Spark.UI/Services/RegistryService.cs    — 新建
```

---

## Phase 4：C# 市场 UI（约 1.5 小时）

### 4.1 XAML 市场面板
- [ ] `ui/Spark.UI/MainWindow.xaml`
  - 插件设置页（`PanePlugins`）内增加「已安装 / 插件市场」切换
  - 市场子面板包含：
    - 源选择：ComboBox 下拉（官方仓库 + N 个自定义仓库），未配置自定义仓库时只有官方一项
    - 自定义仓库配置区：URL 列表（每行 TextBox + 删除按钮）、「添加仓库」按钮、「保存」按钮
    - 切换到自定义仓库时的安全提示文本（「你正在连接第三方仓库，插件安全性由仓库主人负责」）
    - 刷新按钮 + 加载中 ProgressRing + 错误文本
    - 插件浏览 ListView：每行 = 图标 + 名称 + 版本 + 描述 + 作者 + runtime 标签（webview/native）+ 声明的 permissions + 已装版本 + 安装/更新按钮
    - native 插件行显示警告标识（⚠ 原生插件拥有完整系统权限）
  - 元素命名：`MarketSourceCombo`、`MarketCustomUrlList`、`MarketAddUrlBtn`、`MarketSaveUrlsBtn`、`MarketCustomWarning`、`MarketRefreshBtn`、`MarketLoading`、`MarketError`、`MarketList`、`MarketEmpty`

### 4.2 Code-behind 处理器
- [ ] `ui/Spark.UI/MainWindow.xaml.cs`
  - `LoadMarketplaceAsync()` — 读当前选中源（ComboBox）→ FetchIndexAsync → 交叉比对 host.plugin.list → 构造 RegistryPluginViewDto 列表 → 绑定
  - `PopulateSourceCombo()` — 从 HostConfigDto.PluginRegistryUrls 填充下拉项（官方 + 每个自定义 URL）
  - `OnMarketSourceChanged()` — ComboBox 切换 → 重新 LoadMarketplaceAsync；选中自定义仓库时显示安全提示
  - `OnAddRegistryUrl()` — 在 URL 列表底部加一行空 TextBox
  - `OnRemoveRegistryUrl()` — 删除对应行
  - `OnSaveRegistryUrls()` — 收集所有 URL → host.set_config(plugin_registry_urls) → 刷新下拉项
  - `OnMarketRefresh()` — 重新 LoadMarketplaceAsync
  - `OnInstallFromRegistry()` — 从按钮 DataContext 取 plugin + version → DownloadAndExtractAsync 或 DownloadDirectAsync → host.plugin.install → 处理 outcome（installed/updated/confirm_downgrade）→ 刷新市场 + 已装列表 → 清理 temp
  - `OnInstallFromRegistry()` 中 native 插件安装前弹确认对话框「原生插件拥有完整系统权限，确认安装？」（规范 §13.2）
  - `SetMarketStatus(string text)` — 加载/错误状态显示
  - 在 `ShowPane("plugins")` 时初始化市场源 ComboBox（PopulateSourceCombo，读 HostConfig）
  - 自定义仓库选中时显示 `MarketCustomWarning` 安全提示

### 4.3 交互细节
- [ ] 安装中按钮禁用 + 显示「安装中…」
- [ ] 网络错误友好提示（不崩，显示「无法连接仓库，请检查网络或仓库地址」）
- [ ] 自定义仓库列表为空时 ComboBox 只有官方仓库一项，不显示自定义选项
- [ ] 保存自定义仓库 URL 后刷新 ComboBox 下拉项
- [ ] 安装成功后市场列表和已安装列表都刷新
- [ ] native 插件安装前二次确认（防用户误装高权限插件）
- [ ] 插件行展示声明 permissions（让用户安装前知道要授予什么能力）

### 涉及文件
```
ui/Spark.UI/MainWindow.xaml         — 市场面板 XAML
ui/Spark.UI/MainWindow.xaml.cs      — 处理器
```

---

## Phase 5：联调 + 审计（约 1 小时）

### 5.1 示例仓库
- [ ] 造一份本地示例 `registry.json` + 1-2 个插件目录
- [ ] 用本地 HTTP 服务器测试抓取：`python -m http.server 8888 --dir <示例仓库>`，自定义仓库地址填 `http://localhost:8888/registry.json`
- [ ] 下载测试：手动下载 GitHub zipball 验证 zip 结构与 §5.1 提取逻辑一致

### 5.2 端到端测试
- [ ] 浏览市场 → 插件列表正确展示
- [ ] 安装 webview 插件 → 插件列表出现 → 触发关键字可用
- [ ] 安装 native 插件 → 插件列表出现 → 触发关键字可用
- [ ] 更新已装插件（registry 版本更高）→ 版本号变化
- [ ] 降级确认弹窗 → 确认后 force 安装
- [ ] 自定义仓库切换 → 列表刷新 + 安全提示显示
- [ ] 多个自定义仓库配置 → ComboBox 下拉项正确展示
- [ ] 自定义仓库列表清空 → ComboBox 恢复只有官方仓库
- [ ] 网络错误 → 友好提示不崩
- [ ] 路径穿越防护（构造恶意 zip 测试）
- [ ] native 插件安装二次确认弹窗
- [ ] 非标准 registry.json（缺字段/格式错）→ 不崩，跳过不完整条目

### 5.3 质量门禁
- [ ] `cargo fmt`
- [ ] `cargo test --workspace`
- [ ] `dotnet build` UI 项目
- [ ] Code Auditor 审计（架构合规 + 正确性 + 安全）

### 5.4 文档终检
- [ ] 确认代码实现与 `插件市场与仓库.md` 规范一致
- [ ] 确认 IPC 方法名/字段名与规范 §7 一致

---

## 文件变更总览

| 文件 | Phase | 操作 |
|------|-------|------|
| `插件开发/插件市场与仓库.md` | 1 | 新建 ✅ |
| `插件开发/插件开发规范.md` | 1 | 更新 ✅ |
| `插件开发/WebView插件开发.md` | 1 | 更新 ✅ |
| `插件开发/Native插件开发.md` | 1 | 更新 ✅ |
| `docs/PLUGIN_MARKETPLACE_TASKS.md` | 1 | 新建 ✅ |
| `crates/host/src/config.rs` | 2 | 改 |
| `crates/ipc/src/protocol.rs` | 2 | 改 |
| `crates/host/src/ipc_server.rs` | 2 | 改 |
| `ui/Spark.UI/Models/RegistryDto.cs` | 3 | 新建 |
| `ui/Spark.UI/Services/HostIpcClient.cs` | 3 | 改 |
| `ui/Spark.UI/Services/RegistryService.cs` | 3 | 新建 |
| `ui/Spark.UI/MainWindow.xaml` | 4 | 改 |
| `ui/Spark.UI/MainWindow.xaml.cs` | 4 | 改 |

---

## Code Auditor 审计点（预判）

1. **Zip Slip**：规范化路径校验（`GetFullPath` + 前缀 containment），不靠 `..` 字符串匹配；拒绝绝对路径、反斜杠、符号链接 entry（规范 §9.1）
2. **zip bomb**：下载 50 MiB + 解压总 100 MiB + 文件数 10000 + 单文件 50 MiB 四重配额（规范 §9.2）
3. **超时**：下载 30 s 超时，`CancellationToken` 传递，不阻塞 UI 线程（规范 §9.3）
4. **sha256 校验**：有值时必须校验；格式非法（非 64 位 hex）拒绝安装，不跳过（规范 §9.4）
5. **registry.json 容错**：schema 校验、缺字段跳过、latest/versions 不一致以 versions 为准（规范 §9.5）
6. **temp 清理**：`finally` 块保证清理，GUID 唯一目录名防并发冲突（规范 §9.6）
7. **UI 线程**：下载/解压在后台线程，UI 更新回主线程（`DispatcherQueue`）
8. **架构合规**：市场浏览/下载是 UI 设置操作，非热路径，放 UI 层合规（AGENTS.md）
9. **install 复用**：不绕过 `host.plugin.install` 的版本比较/状态管理/权限逻辑
10. **IPC 一致性**：不新增 `HostMethod` 枚举变体，复用 `host.get_config`/`host.set_config`，`plugin_registry_urls: Vec<String>` 空值语义正确（None=不改，Some(vec)=替换，Some(vec![])=清除全部）
11. **semver 校验**：非标准版本号条目跳过并日志（规范 §9.5）
12. **native 插件安全提示**：安装前必须弹二次确认（规范 §13.2/§13.3）
13. **自定义仓库警告**：切到自定义仓库时显示安全提示（规范 §13.1/§4.3）
14. **权限透明度**：市场列表展示声明 permissions，让用户安装前知情（规范 §13.3）