## 目标
给搜索结果和收藏卡片加右键菜单：收藏（子菜单选分组）/ 取消收藏 / 以管理员身份打开；并把底栏已承诺的「Tab 动作」一起接上（对当前选中项弹出同一菜单）。

## 现状（探索结论）
- UI 无任何 Flyout/右键代码（全新）；Host 只支持 `open` / `reveal` 两个动作（`crates/host/src/shell.rs:72-79`），无 runas/管理员能力。
- 收藏是纯客户端（`LocalState.Fav` → favorites.json），**没有任何新增/移除路径**；收藏卡片渲染依赖 `_items ?? DemoData.Find(id)` 查候选（`MainWindow.xaml.cs:849`）——收藏项不在当前搜索结果里时卡片会消失。
- 收藏为空时渲染 5 个硬编码演示项（`:830`），不可移除——与"收藏夹里去不掉"直接冲突，需去掉。
- 两个列表视图共享 `_items`/`_active`/`OnItemClick`；`InvokeAsync` 硬编码 action "open"（`:1463`）。

## 改动

### 1. Host：新增 `runas` 动作（Rust）
- `crates/host/src/shell.rs`：新增 `shell_runas(target)`（同 `shell_open`，ShellExecuteW verb 换 `"runas"` 触发 UAC 提权）；`invoke_action` 增加 `"runas" => shell_runas(target)`。
- `cargo fmt` + `cargo test --workspace`。

### 2. 收藏项元数据快照（`ui/Spark.UI/Services/LocalState.cs`）
- `FavEntryDto` 增加可空字段 `Title` / `Target` / `IconPath`：收藏时快照，收藏卡片渲染优先用快照、缺省回退现有查找（兼容旧 favorites.json）。修掉"收藏后不在搜索结果里卡片消失"的问题。

### 3. 右键菜单（`ui/Spark.UI/MainWindow.xaml.cs`，代码构建 MenuFlyout）
- **结果列表/网格**：`ResultList/ResultGrid.AddHandler(RightTappedEvent, …, handledEventsToo: true)`；从 `e.OriginalSource` 沿可视树上溯到 item 容器，取 `DataContext as CandidateDto`，设 `_active` + `SyncSelection()`，在指针位置弹出菜单：
  - 打开（open）
  - 以管理员身份打开（runas，新 action）
  - 打开文件位置（reveal）
  - ──────
  - 收藏到 ▸ 子菜单（分组：全部/工作/开发/日常，动态构建）；若该项已收藏则改为「取消收藏」
- **收藏卡片**（`RenderFavorites` 里代码建的 Button）：`btn.RightTapped` → 菜单：[打开] / [取消收藏]（按 ItemId 移除全部分组条目）。

### 4. 收藏/取消收藏逻辑（`MainWindow.xaml.cs`）
- `AddFavorite(itemId, groupId)`：快照 Title/Target/IconPath；已在收藏则移动分组（与原型一致），否则新增；`SaveFav` + `RenderFavorites` + 自动展开收藏坞 + 底栏提示"已收藏到「开发」"。
- `RemoveFavorite(itemId)`：移除该 ItemId 的所有条目；`SaveFav` + `RenderFavorites` + 底栏提示。
- 去掉空收藏时的 5 个演示项回退（否则删光后出现无法移除的卡片）。

### 5. Tab 动作
- `OnRootKeyDown` 增加 `VirtualKey.Tab` 分支（`e.Handled = true`）：对 `_items[_active]` 通过 `ContainerFromIndex` 定位容器（列表/网格任一），弹同一菜单（Placement Bottom）。焦点在收藏坞时保持现有放行逻辑。

### 6. Invoke 重构（小）
- `InvokeAsync` 增加可选 `actionId` 参数（默认 "open"），右键菜单的"管理员/打开文件位置"复用同一执行链路（含错误处理、HideAfterInvoke）。管理员提权时 UAC 抢焦点 → 窗口按现有"失焦隐藏"逻辑隐藏，与普通启动行为一致，无需特殊处理。

## 涉及文件
- `crates/host/src/shell.rs`（runas）
- `ui/Spark.UI/Services/LocalState.cs`（FavEntryDto 快照字段）
- `ui/Spark.UI/MainWindow.xaml.cs`（菜单/收藏/移除/Tab/invoke）

## 验证
- `cargo test --workspace`、`cargo fmt`、`dotnet build ui/Spark.UI`；host 需重新编译并重启（runas 才生效），UI 重启生效。
- 收藏/取消收藏为纯客户端逻辑，无 Host 也能验证。