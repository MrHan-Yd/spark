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

  // ── 出站收口第二层（《插件开发规范》§12）──────────────────────────────
  // 引擎层（PluginWindow 的 WebResourceRequested）已拦死 http/https——
  // fetch/XHR/子资源/sendBeacon(EventSource、Ping 上下文) 都过不去。但下面这些
  // 通道不走 HTTP 请求栈、引擎层拦不到，故在页面侧一并封死：
  //   WebSocket / WebTransport：HTTP 升级后的长连接；
  //   RTCPeerConnection：UDP 直连，可经 DataChannel 外传数据。
  // 本脚本先于页面任何代码执行，此处取走原生构造器并替换为拒绝桩（不可重定义、
  // 不可枚举），页面拿不到原生引用。插件出网请统一走 spark.net.fetch()。
  function denyChannel(name) {
    return function () {
      throw sparkError('PERMISSION_DENIED',
        '插件页禁止使用 ' + name + '，请改用 spark.net.fetch()（需 net 权限）');
    };
  }

  ['WebSocket', 'WebTransport', 'RTCPeerConnection', 'webkitRTCPeerConnection']
    .forEach(function (name) {
      if (!(name in window)) return;
      try {
        Object.defineProperty(window, name, {
          value: denyChannel(name),
          writable: false,
          configurable: false,
          enumerable: false
        });
      } catch (e) {
        // 个别宿主不允许重定义时静默降级：引擎层仍拦 HTTP，WebRTC 场景另有
        // PermissionRequested 全拒兜底。
      }
    });

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
      },
      // 一次读全部支持格式（规范 §8.3）：分次调用 readText/readImage 之间剪贴板
      // 可能被别的进程改动，插件会拿到"文本来自 A、图片来自 B"的错配组合，
      // 故提供一次打开剪贴板读全部格式的入口。
      read: clipReadItem,
      // 写剪贴板（覆盖全部内容）：{ text?, imagePng? } 至少给一项。
      write: clipWriteItem
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

    // 与 clipboard 同策略：不做 granted 本地快拦，每次调用交给 host 按
    // "声明+授权"鉴权——中途在插件管理勾选"访问网络"后，已开窗口无需重开
    // 即时生效；被拒错误也会冒给宿主 UI 显示可见的权限提示。
    net: {
      fetch: function (url, init) {
        return call('net', 'fetch', { url: String(url), init: init || null })
          .then(function (r) {
            // host 回 { status, headers, bodyText }；这里包装成规范 §8.3 形状
            // { status, headers, text(), json() }。
            var bodyText = (r && r.bodyText) || '';
            // headers 用 null 原型对象（服务端可控键名，__proto__ 等不会触发原型
            // 链行为），补自有 hasOwnProperty 绑定——插件照 Record 惯例调用
            // headers.hasOwnProperty(k) 不会因缺原型拿到 TypeError；先补后冻结
            // （strict 模式下冻结后赋值会抛 TypeError）。
            var headers = Object.assign(Object.create(null), (r && r.headers) || {});
            headers.hasOwnProperty = Object.prototype.hasOwnProperty;
            Object.freeze(headers);
            // 空响应体（204/空体）json() 按 null 处理——JSON.parse('') 会抛语法
            // 错，报 INVALID_ARGS 会让开发者把"无内容"误判成参数/格式问题。
            if (bodyText === '') return { status: (r && r.status) || 0, headers: headers, text: function () { return Promise.resolve(bodyText); }, json: function () { return Promise.resolve(null); } };
            return {
              status: (r && r.status) || 0,
              headers: headers,
              text: function () { return Promise.resolve(bodyText); },
              json: function () {
                return new Promise(function (resolve, reject) {
                  try { resolve(JSON.parse(bodyText)); }
                  catch (e) {
                    reject(sparkError('INVALID_ARGS', '响应不是合法 JSON: ' + e.message));
                  }
                });
              }
            };
          });
      }
    },

    // shell/fs 与 clipboard/net 同策略：不做 granted 本地快拦，每次调用交给
    // host 按"声明+授权"鉴权——中途在插件管理里勾选/配置后，已开着的窗口
    // 无需重开即时生效；被拒错误也会冒给宿主 UI 显示可见的权限提示。
    shell: {
      openExternal: function (target) {
        return call('shell', 'open_external', { target: String(target) })
          .then(function () { return undefined; });
      }
    },

    fs: {
      read: function (path) {
        return call('fs', 'read', { path: String(path) })
          .then(function (r) { return (r && r.text) || ''; });
      },
      write: function (path, text) {
        // 漏传 text 的调用会被 String() 变成 "undefined" 静默写盘——显式拒绝。
        if (text === undefined || text === null) {
          return Promise.reject(sparkError('INVALID_ARGS', 'fs.write 需要 text 参数'));
        }
        return call('fs', 'write', { path: String(path), text: String(text) })
          .then(function (r) { return (r && r.ok) === true; });
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

  // 一次读全部支持格式（规范 §8.3）：分次调用 readText/readImage 之间剪贴板可能被
  // 别的进程改动，插件会拿到"文本来自 A、图片来自 B"的错配组合，故提供一次打开
  // 剪贴板读全部格式的入口。无任何支持格式 → null（与 readImage 的 null 语义一致）。
  function clipReadItem() {
    return call('clipboard', 'read_all').then(function (r) {
      var types = (r && r.types) || [];
      if (!types.length) return null;
      return {
        types: types,
        text: (r && r.text) || null,
        imagePng: (r && r.image_png) || null,
        has: function (type) { return types.indexOf(String(type)) >= 0; }
      };
    });
  }

  // 写剪贴板（覆盖全部内容）：{ text?, imagePng? } 至少给一项；两者同时给出时
  // 两种格式都写入、由接收方自选（标准剪贴板行为，不是"二选一"）。
  // imagePng 与 readImage/read 的返回对称：裸 base64，不含 data: 前缀
  // （带前缀时宽容剥掉）。
  function clipWriteItem(item) {
    item = item || {};
    var text = (item.text === undefined || item.text === null) ? null : String(item.text);
    var imagePng = (item.imagePng === undefined || item.imagePng === null)
      ? null : String(item.imagePng);
    if (text === null && imagePng === null) {
      return Promise.reject(sparkError('INVALID_ARGS',
        'clipboard.write 需要 text 或 imagePng 至少一项'));
    }
    if (imagePng !== null && imagePng.indexOf('data:') === 0) {
      var comma = imagePng.indexOf(',');
      if (comma >= 0) imagePng = imagePng.slice(comma + 1);
    }
    return call('clipboard', 'write_item', { text: text, image_png: imagePng })
      .then(function () { return undefined; });
  }

  var shimClipboard = Object.freeze({
    readText: clipReadText,
    writeText: clipWriteText,
    // 多格式读写（规范 §8.3 spark.clipboard.read/write）复用同一批 host 方法，
    // 与 spark.clipboard 同一函数引用（页面改写 spark.clipboard 也换不掉它）。
    read: clipReadItem,
    write: clipWriteItem
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
