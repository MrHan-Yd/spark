## Spark 自动更新（Tauri 式：下载 → 静默安装 → 自动重启）

回答你的疑问：**自定义安装路径不需要 Program Files**。Inno Setup 用户级安装（`PrivilegesRequired=lowest`）同样有路径选择向导，默认 `%LOCALAPPDATA%\Programs\Spark`，用户可任意选目录；静默更新时把已装路径经 `/DIR=` 参数传回安装器原地覆盖，全程无 UAC。

### 组件 1：Inno Setup 安装器（新增）
- `installer/spark.iss`：用户级安装、路径可自定义、静默参数支持（`/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP- /DIR=<已装路径>`）
- 目录布局对齐 docs/ARCHITECTURE.md §12：`spark-host.exe + Spark.exe + Assets/ + resources/ + plugins/`
- `CloseApplications=force` 强制关闭运行中的 Spark.exe / spark-host.exe（解决运行时替换文件）
- `[Run]` 装完自动拉起 spark-host.exe（host 会再拉起 UI）→ 自动重启闭环
- `HKCU\Software\Spark\InstallPath` 记录安装路径供更新器读取；含卸载项
- `scripts/build_installer.ps1`：组装 dist + 调 ISCC 编译 + 算 sha256

### 组件 2：发布清单 `latest.json`（新增，CI 生成）
- `{"version":"0.2.0","url":"https://github.com/MrHan-Yd/spark/releases/download/v0.2.0/Spark-0.2.0-setup.exe","sha256":"..."}`
- 版本检查源从 GitHub API 换成 `raw.githubusercontent.com` 的 latest.json（检查/下载共用同一清单；顺带不再碰 API 限流）

### 组件 3：UI 更新逻辑（改 MainWindow.xaml + .cs）
- `OnCheckUpdate` 改拉 latest.json；有新版本时按钮变为"下载并安装"
- About 页新增 ProgressBar（目前代码库无进度控件，全新加）显示下载进度
- 下载到 `%LOCALAPPDATA%\Spark\update\` → 校验 SHA-256 → 读注册表拿已装路径 → 静默运行安装器 → 状态"正在安装…"→ UI 自行退出（安装器关 host、装完重启 host → host 拉起新 UI）
- 失败兜底：校验不过不执行安装，保留"打开下载页"按钮；沿用现有 `_checkingUpdate` 防重入

### 组件 4：GitHub Actions（新增 .github/workflows/release.yml）
- 触发：`v*` tag push
- cargo build --release → dotnet publish -c Release → 组装 dist → 装 Inno Setup 编译 → 算 sha256 → 生成 latest.json → gh release create 上传
- 顺带修正 Cargo.toml `repository = example/spark` 占位符为 `MrHan-Yd/spark`

### 验证
- `cargo test --workspace` + `cargo fmt`（AGENTS.md 要求）、`dotnet build`
- 本机有 Inno Setup 则编译安装包验证 .iss；没有则靠 CI 验证
- 手动端到端：安装 v0.1.0 → 打 v0.2.0 tag 发 Release → 应用内点更新走通

### 已知限制（不阻塞）
- 无代码签名证书：首次运行/更新时 SmartScreen 可能提示（Tauri 同样建议签名但非必需）
- 更新期间无界面约几秒（UI 退出 → 安装器重启 host）
- latest.json 必须与安装包同批上传（CI 保证）