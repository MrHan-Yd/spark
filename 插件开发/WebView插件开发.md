# WebView 插件开发

> 状态：**一期已实现**（2026-08-20）
> 适用版本：清单 `api_version: 2`
> 面向：前端开发者（HTML/CSS/JS），无需 Rust/C#
> 完整契约见 [插件开发规范.md](./插件开发规范.md)

---

## 1. 是什么

**WebView 插件 = 一个能被 Spark 打开的独立网页窗口。**

- 你写 `plugin.json`（清单）+ `index.html`（页面），可选 JS/CSS/图标/资源。
- Spark 用 WebView2 打开你的页面，并注入 `window.spark` 全局对象提供系统能力。
- 页面跑在独立窗口，崩溃/卡死不影响主程序。
- 用户在主输入框输入关键字触发，回车开窗。

适合：翻译、查询、小工具、信息展示类插件。需要 OCR/截图/重 IO/任意原生语言的看 [Native插件开发.md](./Native插件开发.md)。

---

## 2. 快速开始（3 分钟）

最小插件只要两个文件：

```
hello/
  plugin.json
  index.html
```

`plugin.json`：
```json
{
  "id": "com.spark.hello",
  "name": "Hello",
  "version": "0.1.0",
  "api_version": 2,
  "runtime": "webview",
  "main": "index.html",
  "permissions": [],
  "features": [
    { "type": "keyword", "keyword": "hello", "title": "Hello", "mode": "page" }
  ]
}
```

`index.html`：
```html
<!doctype html>
<html><head><meta charset="utf-8"><title>Hello</title></head>
<body style="font-family:system-ui;padding:24px">
  <h3>你好，Spark</h3>
  <div>你输入的是：<code id="out"></code></div>
  <script>
    document.getElementById('out').textContent = spark.input.text
  </script>
</body></html>
```

**加载**：设置 → 插件 → 加载开发目录 → 选 `hello/` 文件夹。
**触发**：主输入框输 `hello Spark` → 回车 → 弹窗显示"你输入的是：Spark"。

---

## 3. 目录结构

```
my-plugin/
  plugin.json          # 清单（必须）
  index.html           # 页面入口（必须，main 指向它）
  icon.png             # 图标，建议 128×128（可选但强烈建议）
  preload.js           # 自定义预加载脚本（可选，§6）
  assets/              # 任意静态资源：js/css/图片/字体（可选）
  README.md            # 给用户看的说明（可选）
```

页面内用相对路径引用资源（`assets/app.js`、`./style.css`），Spark 以 `file://` 协议加载，相对路径基于插件根目录解析。

### 落盘位置

| 类型 | 位置 |
|------|------|
| 正式安装 | `<Spark 安装目录>/plugins/<id>/` |
| 开发模式 | 你选的本地目录（不拷贝，改文件即重载） |
| 插件状态 | `<data_dir>/plugins-state.json`（应用管理） |
| 插件私有数据 | `<data_dir>/plugin-data/<id>/`（仅插件可读写） |

`<data_dir>` = `%APPDATA%/Spark`，或便携模式 `<安装目录>/data`。

---

## 4. 清单 `plugin.json`

UTF-8 无 BOM 的 JSON。WebView 插件相关字段：

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `id` | string | 是 | 反向域名，全局唯一，如 `com.spark.translate`；必须含 `.` |
| `name` | string | 是 | 显示名，建议 ≤ 8 字符 |
| `version` | string | 是 | 语义化版本，如 `0.1.0` |
| `api_version` | number | 是 | 当前 `2` |
| `runtime` | string | 是 | 固定 `"webview"` |
| `main` | string | 是 | 必须 `.html` 文件，通常 `index.html` |
| `icon` | string | 否 | 图标相对路径，建议 png 128×128 |
| `author` / `description` / `homepage` | string | 否 | 元信息 |
| `permissions` | string[] | 否 | 能力白名单，见 §5 |
| `features` | object[] | 是 | 触发入口，至少 1 个，见 §5（webview 至少一个 `mode: page`） |
| `preload` | string | 否 | 自定义预加载脚本路径，§6 |
| `window` | object | 否 | 窗口尺寸/行为，§4.2 |

### 4.2 `window`

| 子字段 | 类型 | 默认 | 说明 |
|--------|------|------|------|
| `width` / `height` | number | 480 / 360 | 初始尺寸（逻辑像素） |
| `minWidth` / `minHeight` | number | 240 / 180 | 最小尺寸 |
| `resizable` | bool | true | 可否调整大小 |
| `alwaysOnTop` | bool | false | 默认置顶 |
| `frame` | bool | true | 显示系统标题栏；`false` 由插件自绘 |

---

## 5. 触发方式 `features`

`features` 数组定义用户如何进入插件。一期只支持 `type: keyword` + `mode: page`。

### 5.1 `type: keyword`（关键字触发，一期主推）

用户在主输入框输 `<keyword> <参数>`，主列表出现一条插件命令项，回车开窗。

```json
{
  "type": "keyword",
  "keyword": "tr",
  "title": "翻译",
  "subtitle": "输入 tr 翻译后面的内容",
  "mode": "page",
  "placeholder": "输入要翻译的文本…"
}
```

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `keyword` | string | 是 | 触发关键字，不含空格，建议 1-4 字符；全局唯一，冲突时先装先生效并提示 |
| `title` | string | 是 | 列表项标题 |
| `subtitle` | string | 否 | 列表项副标题 |
| `mode` | string | 是 | webview 插件固定 `"page"` |
| `placeholder` | string | 否 | 进入插件后输入框占位提示 |

进入插件后 `spark.input.text` = 去掉 `tr ` 前缀后的内容。

### 5.2 二期触发方式（未实现）

- `type: regex`：主输入框匹配正则时追加入口（如输入 IP 自动出现查询插件）。
- `type: root`：挂在根输入框，每次输入都参与匹配（配合 `spark.onInput` 实时响应）。

一期不要使用，不会被识别。

---

## 6. `spark.*` API

页面加载时 Spark 注入 `window.spark`（只读、不可重写）。所有返回 Promise 的方法均异步。**未授权能力在页面侧即拒绝，host 侧二次鉴权。**

> 下方标注 ✅ = 一期已实现，🚧 = 规范已定但未实现。

### 6.1 输入上下文 `spark.input` ✅

只读属性，进入插件时填充。

```js
spark.input.text       // string，去掉触发前缀后的用户输入
spark.input.command    // string，触发的关键字，如 "tr"
spark.input.rawQuery   // string，主输入框原始完整内容
```

### 6.2 窗口控制 `spark.window` ✅

```js
await spark.window.setTitle(title)
await spark.window.resize(width, height)
await spark.window.center()
await spark.window.close()
await spark.window.setAlwaysOnTop(enabled)   // 需 window.alwaysOnTop 权限
```

### 6.3 剪贴板 `spark.clipboard` ✅（需 `clipboard` 权限）

```js
const text = await spark.clipboard.readText()       // 返回字符串
await spark.clipboard.writeText(text)
const b64 = await spark.clipboard.readImage()       // 返回 base64 PNG 或 null
```

### 6.4 通知 `spark.notify` ✅（需 `notify` 权限）

```js
await spark.notify.show({ title: string, body?: string })
```

### 6.5 私有存储 `spark.db` ✅（默认开放，无需声明）

每插件独立键值沙箱，落盘 `plugin-data/<id>/db`。

```js
await spark.db.set(key, value)      // value 可 JSON 序列化
const v = await spark.db.get(key)   // 不存在返回 null
await spark.db.remove(key)
const keys = await spark.db.keys()  // string[]
await spark.db.clear()
```

### 6.6 事件 `spark.on*` ✅

```js
spark.onEnter(cb)                   // 进入插件窗口、DOM 就绪后触发
spark.onInput(cb => (text))         // 主输入框文本变化（二期挂载模式用）
spark.onResize(cb => (w, h))
spark.onClose(cb)                   // 窗口即将关闭前触发，可做存盘
```

### 6.7 开发模式 `spark.dev` ✅（仅开发目录加载时）

```js
spark.dev.openDevTools()
```

### 6.8 待实现 🚧

以下在规范中已定义，一期未实现，调用会抛 `UNAVAILABLE`：

| API | 权限 | 说明 |
|-----|------|------|
| `spark.net.fetch(url, init)` | `net` | 网络请求，走 host 代理 |
| `spark.shell.openExternal(path)` | `shell.open` | 系统默认程序打开 |
| `spark.fs.read(path)` / `spark.fs.write(path, text)` | `fs.read` / `fs.write` | 高危，授权定范围 |

### 6.9 自定义预加载 `preload.js` ✅

清单声明 `preload` 字段时，Spark 在 `spark` 注入后、页面代码前额外执行该脚本。用于注入自定义全局工具或封装 `spark.*`。

```js
// preload.js：把 spark.db 封装成带前缀的命名空间
window.myStore = {
  get: (k) => spark.db.get('my_' + k),
  set: (k, v) => spark.db.set('my_' + k, v)
}
```

---

## 7. 权限 `permissions`

清单声明所需能力，**首次启用/打开时用户授权**。未声明的调用抛 `PERMISSION_DENIED`。

| 权限 | 解锁 API | 风险 | 一期 |
|------|----------|------|------|
| `clipboard` | `spark.clipboard.*` | 中 | ✅ |
| `notify` | `spark.notify.*` | 低 | ✅ |
| `window.alwaysOnTop` | `spark.window.setAlwaysOnTop` | 低 | ✅ |
| `db` | `spark.db.*` | 无 | ✅ 默认开放，无需声明 |
| `net` | `spark.net.*` | 中 | 🚧 |
| `shell.open` | `spark.shell.openExternal` | 中 | 🚧 |
| `fs.read` / `fs.write` | `spark.fs.*` | 高 | 🚧 |

错误码：`PERMISSION_DENIED` / `PERMISSION_SCOPE` / `NETWORK_FAILED` / `INVALID_ARGS` / `UNAVAILABLE`。

```js
try { await spark.clipboard.writeText('x') }
catch (e) {
  if (e.code === 'PERMISSION_DENIED') { /* 引导用户去设置授权 */ }
}
```

---

## 8. 开发与调试

### 8.1 加载开发目录

设置 → 插件 → 加载开发目录 → 选含 `plugin.json` 的文件夹。Spark **不拷贝**，源文件改动后重载即生效（重新触发关键字，或关窗再开）。开发目录加载的插件不可卸载（文件归你管），只能禁用。

### 8.2 DevTools

开发目录加载的插件窗口自动启用 WebView2 DevTools。右键 → 检查，或代码 `spark.dev.openDevTools()`。正式安装版默认关闭 DevTools。

### 8.3 热重载

改完 `index.html`/JS/CSS，关掉插件窗口重新触发关键字即可加载新代码。改 `plugin.json`（如加权限、改关键字）需在设置-插件页点"刷新"重新拉清单。

---

## 9. 打包与发布

### 9.1 打包

整个插件目录压成 zip，扩展名改 `.spark-plugin`：

```
translate.spark-plugin  (zip)
  ├─ plugin.json
  ├─ index.html
  ├─ icon.png
  └─ assets/
```

> zip 根目录直接是插件文件，不要外面再套一层目录。

### 9.2 发布

- **一期**：手动分发 `.spark-plugin`，用户在设置 → 插件 → 从本地安装。
- **二期**：插件市场（官方/自定义仓库 + registry.json 索引 + 一键安装/更新）。仓库搭建与发布流程见 [插件市场与仓库.md](./插件市场与仓库.md)。
- **签名**：官方插件随仓库 CI 自动签名（Ed25519，详见 [插件签名规范.md](./插件签名规范.md)）；三方签名待后续子阶段。未签名插件仍可安装，UI 不显"官方"角标。

#### 通过插件市场发布

1. 在插件仓库中创建 `插件名/版本号/` 目录，放入完整插件内容（`plugin.json` + `index.html` + 资源）。
2. 更新仓库根目录 `registry.json`，添加该插件条目。
3. 推送后用户即可在 Spark 设置 → 插件 → 插件市场浏览并一键安装。
4. 发布新版本：新建版本号目录 + 更新 `registry.json` 的 `latest` 和 `versions` 列表。

详见 [插件市场与仓库.md](./插件市场与仓库.md) §6 发布流程。

---

## 10. 完整示例：hello

仓库内置 `plugins/hello`，演示 `spark.*` 全套已实现 API。触发关键字 `hello`。

`plugin.json`：
```json
{
  "id": "com.spark.hello",
  "name": "Hello",
  "version": "0.1.0",
  "api_version": 2,
  "runtime": "webview",
  "main": "index.html",
  "author": "Spark 示例",
  "description": "输入 hello 打开示例插件页，演示 spark.* API",
  "permissions": ["clipboard", "notify"],
  "features": [
    {
      "type": "keyword", "keyword": "hello", "title": "Hello",
      "subtitle": "示例插件：回显输入并演示 spark.* API",
      "mode": "page", "placeholder": "输入任意文本…"
    }
  ],
  "window": { "width": 480, "height": 400, "resizable": true }
}
```

页面演示：读 `spark.input`、读写剪贴板、发通知、存取 `spark.db`、调整窗口、关窗前存盘。完整代码见 `plugins/hello/index.html`。

### 开发模式加载 hello（仓库内）

开发模式下 host 默认在 exe 同级找 `plugins/`，仓库需显式指定：

```bash
spark-host.exe --no-ui --plugins-dir D:/demo/test01/spark/plugins
```

或直接在设置-插件页"加载开发目录"选 `plugins/hello/`。

---

## 11. 安全约束

1. `spark.*` 是受信任通道，不要在插件页内 `eval` 不可信内容。
2. `net`（待实现）走 host 代理，host 可记录/限流，不得绕过。
3. `fs`（待实现）高危权限授权时限定目录，不得越界。
4. 不得收集用户隐私上传未声明用途的服务器。
5. 市场上架（二期）将强制代码签名；本地开发免签名。

---

> 本指南面向开发者实操。完整字段 schema、IPC 方法表、版本兼容策略见 [插件开发规范.md](./插件开发规范.md)。