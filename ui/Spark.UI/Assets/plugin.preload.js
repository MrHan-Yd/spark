// Spark 插件预加载脚本 —— 注入 window.spark（《插件开发规范》§8）。
// 由 PluginWindow 通过 AddScriptToExecuteOnDocumentCreatedAsync 在文档创建前执行。
// 上下文（input/permissions/dev）由宿主在本脚本前注入 __SPARK_BOOTSTRAP__。
(function () {
  'use strict';

  var boot = window.__SPARK_BOOTSTRAP__ || {};
  delete window.__SPARK_BOOTSTRAP__;

  var granted = {};
  (boot.granted || []).forEach(function (p) { granted[p] = true; });

  // ── 消息桥：seq 配对的 request/response ────────────────────────────────
  // 本脚本在页面任何代码之前执行，此刻抓住原始的 chrome.webview 并锁住引用：
  // 页面之后即使改写 window.chrome，也劫持不到已发出的 spark.* 调用。
  var bridge = window.chrome.webview;
  var post = bridge.postMessage.bind(bridge);
  var listen = bridge.addEventListener.bind(bridge);

  var seq = 0;
  var pending = Object.create(null);

  function sparkError(code, message) {
    var e = new Error(message || code);
    e.code = code;
    return e;
  }

  listen('message', function (ev) {
    var msg = ev.data;
    if (!msg || msg.__spark !== 1) return;
    if (msg.kind === 'event') { emit(msg.event, msg.payload); return; }
    var p = pending[msg.seq];
    if (!p) return;
    delete pending[msg.seq];
    if (msg.ok) p.resolve(msg.data);
    else p.reject(sparkError((msg.error && msg.error.code) || 'UNAVAILABLE',
                             msg.error && msg.error.message));
  });

  function call(capability, method, args) {
    return new Promise(function (resolve, reject) {
      var id = ++seq;
      pending[id] = { resolve: resolve, reject: reject };
      try {
        post({
          __spark: 1, seq: id, capability: capability, method: method,
          args: args === undefined ? null : args
        });
      } catch (e) {
        delete pending[id];
        reject(sparkError('UNAVAILABLE', String(e)));
      }
    });
  }

  // 未授权能力在页面侧就拒绝，省一次 IPC；host 侧仍会二次鉴权。
  function guarded(permission, capability, method) {
    return function (args) {
      if (permission && !granted[permission]) {
        return Promise.reject(sparkError('PERMISSION_DENIED',
          '未授权：' + permission + '（请在设置-插件中授权）'));
      }
      return call(capability, method, args);
    };
  }

  // ── 事件 ───────────────────────────────────────────────────────────────
  var listeners = Object.create(null);

  function on(name, cb) {
    if (typeof cb !== 'function') return;
    (listeners[name] || (listeners[name] = [])).push(cb);
  }

  function emit(name, payload) {
    var arr = listeners[name];
    if (!arr) return;
    for (var i = 0; i < arr.length; i++) {
      try {
        if (name === 'resize') arr[i](payload && payload.width, payload && payload.height);
        else if (name === 'input') arr[i](payload && payload.text);
        else arr[i]();
      } catch (e) {
        console.error('[spark] ' + name + ' handler failed', e);
      }
    }
  }

  // ── spark.* ────────────────────────────────────────────────────────────
  var input = boot.input || {};
  var spark = {
    input: Object.freeze({
      text: input.text || '',
      command: input.command || '',
      rawQuery: input.rawQuery || ''
    }),

    window: {
      setTitle: function (title) { return call('window', 'set_title', { title: String(title) }); },
      resize: function (w, h) { return call('window', 'resize', { width: w, height: h }); },
      center: function () { return call('window', 'center'); },
      close: function () { return call('window', 'close'); },
      setAlwaysOnTop: guarded('window.alwaysOnTop', 'window', 'set_always_on_top')
    },

    // 剪贴板不做 granted 本地快拦：每次调用都交给 host 按"声明+授权"鉴权，
    // 中途在插件管理里勾选授权后，已开着的窗口无需重开即时生效；
    // 被拒错误也会冒给宿主 UI 显示可见的权限提示。
    clipboard: {
      readText: clipReadText,
      writeText: clipWriteText,
      readImage: function () {
        return call('clipboard', 'read_image')
          .then(function (r) { return (r && r.data) || null; });
      }
    },

    notify: {
      show: function (opts) {
        opts = opts || {};
        return guarded('notify', 'notify', 'show')({
          title: String(opts.title || ''), body: opts.body ? String(opts.body) : ''
        }).then(function () { return undefined; });
      }
    },

    db: {
      set: function (key, value) { return call('db', 'set', { key: String(key), value: value }); },
      get: function (key) { return call('db', 'get', { key: String(key) }); },
      remove: function (key) { return call('db', 'remove', { key: String(key) }); },
      keys: function () { return call('db', 'keys'); },
      clear: function () { return call('db', 'clear'); }
    },

    // native 纯应用插件专属：调用插件 exe 的自定义逻辑（host 经 plugin.page RPC
    // 转发，返回插件自定义 JSON）。不设权限——exe 与页面同源同信任级；
    // webview 插件调用会得到 UNAVAILABLE（没有 exe 可转发）。
    rpc: function (method, args) {
      return call('rpc', String(method || ''), args === undefined ? null : args);
    },

    // 以下能力 host 端尚未实现：preload 提供 API 桩，调用统一返回 UNAVAILABLE。
    // 这样开发者照规范写不会拿到 TypeError，而是清晰的“未实现”错误。
    net: {
      fetch: function (url, init) {
        return guarded('net', 'net', 'fetch')({ url: String(url), init: init || null });
      }
    },

    shell: {
      openExternal: function (target) {
        return guarded('shell.open', 'shell', 'open_external')({ target: String(target) });
      }
    },

    fs: {
      read: function (path) {
        return guarded('fs.read', 'fs', 'read')({ path: String(path) });
      },
      write: function (path, text) {
        return guarded('fs.write', 'fs', 'write')({ path: String(path), text: String(text) });
      }
    },

    onEnter: function (cb) { on('enter', cb); },
    onInput: function (cb) { on('input', cb); },
    onResize: function (cb) { on('resize', cb); },
    onClose: function (cb) { on('close', cb); }
  };

  if (boot.dev) {
    spark.dev = { openDevTools: function () { return call('dev', 'open_devtools'); } };
  }

  Object.defineProperty(window, 'spark', { value: spark, writable: false, configurable: false });

  // ── 剪贴板收口：程序化访问只认 spark 桥一条路 ─────────────────────────
  // navigator.clipboard 是绕开权限体系的 web 标准后门（Chromium 对 writeText
  // 有用户手势即放行），整站替换为桥代理，让每个调用都过 host 鉴权；
  // Navigator.prototype 上的原生访问器一并删除，防止页面从原型找回原装入口。
  // 用户手势的原生 Ctrl+C/V（选区复制/粘贴）不经过这些 JS API，不受影响。
  function clipReadText() {
    // host 回 { text }，这里解包成规范约定的裸字符串。
    return call('clipboard', 'read_text')
      .then(function (r) { return (r && r.text) || ''; });
  }

  function clipWriteText(text) {
    return call('clipboard', 'write_text', { text: String(text) })
      .then(function () { return undefined; });
  }

  var shimClipboard = Object.freeze({
    readText: clipReadText,
    writeText: clipWriteText,
    // 复杂格式（ClipboardItem）host 端未实现，显式拒绝而非静默失败，
    // 避免开发者把能力缺口误判成权限或参数问题。
    read: function () { return Promise.reject(sparkError('UNAVAILABLE', 'clipboard.read 未实现，请使用 readText')); },
    write: function () { return Promise.reject(sparkError('UNAVAILABLE', 'clipboard.write 未实现，请使用 writeText')); }
  });
  // 替换失败安全：极老引擎上 delete/defineProperty 可能抛错，退化为原生入口，
  // 此时 host 桥仍是权威防线（页面伪造 postMessage 过不了 host 的声明+授权校验）。
  try {
    delete Navigator.prototype.clipboard;
    Object.defineProperty(navigator, 'clipboard', {
      value: shimClipboard, writable: false, configurable: false
    });
  } catch (e) { /* 保持原生入口，由 host 桥兜底鉴权 */ }

  // document.execCommand('copy'/'cut') 是同步 API，绕不开异步桥鉴权：
  // 未授权直接返回 false 拦死（页面自己的"复制失败"兜底分支会给出反馈）；
  // 已授权放行原生行为。快照取页面加载时刻的 granted——中途勾选授权后
  // navigator.clipboard / spark.clipboard 即时生效，execCommand 需重开页面。
  // 包在 Document.prototype 上而非 document 实例上，防止页面从原型取原装方法绕过。
  var origExecCommand = Document.prototype.execCommand;
  Document.prototype.execCommand = function (cmd) {
    var c = String(cmd || '').toLowerCase();
    if (!granted['clipboard'] && (c === 'copy' || c === 'cut')) return false;
    return origExecCommand.apply(this, arguments);
  };

  // DOM 就绪后派发 enter；宿主关窗前会先投递 close 事件。
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function () { emit('enter'); });
  } else {
    emit('enter');
  }
})();
