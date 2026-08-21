# 插件市场二期实现计划

## 架构决策：UI 驱动式市场（零新 Rust 依赖）

关键发现：host 无 HTTP 客户端/zip crate，离线可能加不了 Rust 依赖；而 UI 已有 `HttpClient`（更新下载在用）+ `System.IO.Compression.ZipArchive`（.NET 8 内置，无需 NuGet）。

**流程**：UI fetch registry.json → UI 下载 zipball → UI 用 ZipArchive 提取版本子目录 → UI 调用**已有的** `host.plugin.install { path }` 交给 host 做真正的安装（复制/版本比较/状态持久化）。

- 市场浏览不是热路径，放 UI 合理
- host 的插件生命周期逻辑（install/uninstall/state）原样复用，不改动
- **唯一 Rust 改动**：`HostConfig` + `SetConfigParams` 加一个 `plugin_registry_url: Option<String>` 字段，让自定义仓库 URL 跟其他设置一起持久化

## 仓库结构（已确认：中央索引 + 版本文件夹 + zipball 提取）

```
spark-plugins/                         ← GitHub: MrHan-Yd/spark-plugins
  registry.json                        ← 中央索引，一次请求拿全量
  translate/                           ← 文件夹名 = 插件名（展示用）
    plugin.json                        ← 元信息（供人浏览参考）
    0.1.0/                             ← 版本文件夹 = 可安装的插件内容
      plugin.json                      ← 真正的清单
      index.html
      assets/
    0.2.0/
      plugin.json
      index.html
  echo/
    plugin.json
    0.1.0/
      plugin.json
      spark-plugin-echo.exe
```

**registry.json schema：**
```json
{
  "schema": 1,
  "name": "Spark 官方插件仓库",
  "zipball_url": "https://github.com/MrHan-Yd/spark-plugins/archive/refs/heads/main.zip",
  "updated": "2026-08-21T10:00:00Z",
  "plugins": [
    {
      "id": "com.spark.translate",
      "name": "翻译",
      "description": "输入 tr 触发翻译",
      "author": "Spark",
      "homepage": "https://github.com/MrHan-Yd/spark-plugins/tree/main/translate",
      "icon": null,
      "runtime": "webview",
      "latest": "0.1.0",
      "versions": [
        {
          "version": "0.1.0",
          "path": "translate/0.1.0",
          "url": null,
          "sha256": null,
          "size": null,
          "released": "2026-08-20"
        }
      ]
    }
  ]
}
```

**下载逻辑**：
- `version.url` 有值 → 直接下载该 URL 的 zip（预打包场景）
- `version.url` 为 null → 下载 `registry.json.zipball_url`（整仓 zip），提取 `<top-folder>/<version.path>/` 子目录（GitHub zipball 顶层是 `owner-repo-<hash>/`，需剥离）

## 常量

- 官方仓库 registry.json URL：`https://raw.githubusercontent.com/MrHan-Yd/spark-plugins/main/registry.json`（C# 常量，紧跟现有 `GithubRepo` 常量）
- 自定义仓库 URL：用户在设置里填，存 `HostConfig.plugin_registry_url`

## 任务拆分（5 个阶段，跨多次完成）

先在项目里建 **`docs/PLUGIN_MARKETPLACE_TASKS.md`** 任务清单文件（带 checkbox），然后逐阶段执行。

### 阶段 1：Rust config 字段 + IPC 协议（小改）
- `crates/host/src/config.rs`：`HostConfig` 加 `plugin_registry_url: Option<PathBuf>`（用 String 更合适，存 URL），默认 None
- `crates/ipc/src/protocol.rs`：`SetConfigParams` 加 `plugin_registry_url: Option<String>`
- `crates/host/src/ipc_server.rs`：`host.set_config` 合并逻辑加新字段
- `cargo fmt && cargo test --workspace`

### 阶段 2：C# DTOs + RegistryService
- `Models/RegistryDto.cs`：`RegistryIndexDto`、`RegistryPluginDto`、`RegistryVersionDto`
- `Services/HostIpcClient.cs`：`HostConfigDto` / `HostConfigUpdate` 加 `PluginRegistryUrl`
- `Services/RegistryService.cs`（新建）：
  - `FetchIndexAsync(url) → RegistryIndexDto`
  - `DownloadAndExtractAsync(zipballUrl, pluginPath) → tempDir`（ZipArchive 提取子目录）
  - `DownloadDirectAsync(url) → tempDir`（预打包 zip 直接解压）
  - HttpClient 复用更新下载的 timeout/retry 模式

### 阶段 3：UI 市场面板（XAML + code-behind）
- `MainWindow.xaml`：插件设置页加子面板「插件市场」
  - 来源选择：官方仓库 / 自定义仓库（RadioButton 或 ComboBox）
  - 自定义仓库 URL 输入框 + 保存按钮（调 `host.set_config`）
  - 插件列表 ListView：名称、描述、作者、最新版本、已装版本、安装/更新按钮
  - 加载中 / 空状态 / 网络错误状态
- `MainWindow.xaml.cs`：
  - `LoadMarketplaceAsync(source)` — fetch + 渲染
  - `OnInstallFromRegistry(id, version)` — 下载 → 提取 → `host.plugin.install` → 刷新
  - 已装版本对比（调 `host.plugin.list` 交叉比对）

### 阶段 4：文档
- 更新 `插件开发/插件开发规范.md`：路线图二期加市场、§marketplace 新章节
- 新建 `插件开发/插件市场与仓库.md`：registry.json schema、仓库目录结构、如何发布插件到仓库、如何配置自定义仓库
- 更新 `插件开发/WebView插件开发.md` / `Native插件开发.md`：发布章节加市场引用

### 阶段 5：测试 + Code Auditor
- `cargo test --workspace && cargo fmt`
- `dotnet build` UI
- 建示例 `registry.json` + 把 hello/echo 插件登记进去做端到端验证
- 按 AGENTS.md 闭环：调 Code Auditor 审查 → 修复 → PASSED

## 本次会话目标

先完成任务清单文件 + 阶段 1（Rust 小改）+ 阶段 2（C# DTOs + RegistryService），即基础设施层。阶段 3（UI 面板）和 4（文档）视进度推进，剩余留到下次。