using System.Collections.Concurrent;
using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using Spark.UI.Models;

namespace Spark.UI.Services;

/// <summary>
/// 长连接 NDJSON：一条读循环分发 response / notification；请求用 TCS 等待。
/// </summary>
public sealed class HostIpcClient : IAsyncDisposable
{
    public const string DefaultPipeName = "spark.host.ipc";

    private NamedPipeClientStream? _pipe;
    private StreamWriter? _writer;
    private readonly SemaphoreSlim _writeLock = new(1, 1);
    private readonly ConcurrentDictionary<int, TaskCompletionSource<JsonElement>> _pending = new();
    private CancellationTokenSource? _loopCts;
    private Task? _loopTask;
    private int _nextId = 1;
    private int _connectGate;
    /// <summary>读循环已退出（连接假死/对端消失）。IsConnected 判假 → 下一次调用强制重连，
    /// 否则 _pipe.IsConnected 可能长期保持 true（管道句柄状态未刷新），请求写进死连接全部超时。</summary>
    private long _connectionGeneration;
    private volatile bool _loopDead;

    public bool IsConnected => _pipe?.IsConnected == true && !_loopDead;

    public event Action<string>? HostNotification;

    /// <summary>连接真正建立后触发（EnsureConnectedAsync 从"未连接"变为"已连接"时，
    /// 含冷启动 spawn 拉 host 成功的场景）。调用方据此补做被降级吞掉的查询：
    /// host 冷启动期间 QueryAsync 会回退 DemoData，host 上线不补查的话用户会一直看到演示结果。</summary>
    public event Action? Connected;

    /// <summary>上次尝试拉起 host 进程的时刻（节流，防失败重试风暴重复 spawn）。</summary>
    private long _lastSpawnAttemptTicks;
    /// <summary>已记过 QueryFallback 日志的连接代（-1 = 尚未记过）。断连期间打字每键一条
    /// 同步文件 IO 会拖慢输入，同一断连段只记一条，重连成功后复位。</summary>
    private long _fallbackLoggedGen = -1;

    /// <summary>同一连接代只记一次降级日志（诊断保留首条，其余静默）。</summary>
    private void LogFallbackOnce(string tag, Exception ex)
    {
        var gen = Interlocked.Read(ref _connectionGeneration);
        if (Interlocked.CompareExchange(ref _fallbackLoggedGen, gen, gen) == gen) return;
        Interlocked.Exchange(ref _fallbackLoggedGen, gen);
        App.Log(tag, ex);
    }

    public async Task EnsureConnectedAsync(CancellationToken ct = default)
    {
        if (IsConnected) return;
        if (Interlocked.CompareExchange(ref _connectGate, 1, 0) != 0)
        {
            // 另一路正在连
            for (var i = 0; i < 30 && !IsConnected; i++)
                await Task.Delay(100, ct);
            return;
        }
        try
        {
            if (IsConnected) return;
            await TearDownAsync();

            Exception? last = null;
            // 本地管道正常应毫秒级连上；超时给足 1.5s，避免客户端先放弃、服务端
            // 后 accept 造成僵尸连接。host 不在运行时：第 2 次失败后拉起 host 进程
            // 再继续重试（spawn + bootstrap 索引构建约 2-4s，8 次 ≈ 最坏 13s 覆盖）。
            for (var attempt = 0; attempt < 8; attempt++)
            {
                ct.ThrowIfCancellationRequested();
                if (attempt == 2) TrySpawnHostIfMissing();
                var pipe = new NamedPipeClientStream(".", DefaultPipeName, PipeDirection.InOut,
                    PipeOptions.Asynchronous);
                try
                {
                    await pipe.ConnectAsync(1500, ct);
                    var generation = Interlocked.Increment(ref _connectionGeneration);
                    _pipe = pipe;
                    _writer = new StreamWriter(pipe, new UTF8Encoding(false), 64 * 1024, leaveOpen: true)
                    {
                        AutoFlush = true,
                        NewLine = "\n"
                    };
                    _loopCts = new CancellationTokenSource();
                    _loopDead = false;
                    Interlocked.Exchange(ref _fallbackLoggedGen, -1); // 新连接段允许再记一条降级
                    _loopTask = Task.Run(() => ReadLoopAsync(_loopCts.Token, generation));
                    last = null;
                    try { Connected?.Invoke(); } catch { /* 订阅者异常不拖垮连接 */ }
                    return;
                }
                catch (Exception ex)
                {
                    last = ex;
                    try { await pipe.DisposeAsync(); } catch { /* ignore */ }
                    await Task.Delay(150, ct);
                }
            }
            if (last is not null)
                App.Log("HostConnect", last);
        }
        finally
        {
            Interlocked.Exchange(ref _connectGate, 0);
        }
    }

    /// <summary>host 进程缺失时由 UI 拉起（节流 20s）：开机自启/手动场景 host 可能没在跑，
    /// UI 只管连接从不 spawn 的话，冷启动窗口内所有查询都静默降级成演示数据。
    /// --no-ui：UI 本尊已在运行，明确告诉 host 别再管 UI 拉起；spawn 失败仅记日志，
    /// 后续连接失败仍走原有降级路径。</summary>
    private void TrySpawnHostIfMissing()
    {
        var now = Environment.TickCount64;
        if (now - Interlocked.Read(ref _lastSpawnAttemptTicks) < 20_000) return;
        Interlocked.Exchange(ref _lastSpawnAttemptTicks, now);
        try
        {
            var exe = FindHostExe();
            if (string.IsNullOrEmpty(exe)) return;
            // Dispose 只释放本进程的句柄跟踪对象，不影响已启动的 host 子进程
            using (var proc = Process.Start(new ProcessStartInfo(exe)
            {
                UseShellExecute = false,
                CreateNoWindow = true,
                Arguments = "--no-ui",
            }))
            {
                App.Log("Ipc", $"host not running; spawned by UI (--no-ui, pid={proc?.Id})");
            }
        }
        catch (Exception ex)
        {
            App.Log("HostSpawn", ex);
        }
    }

    /// <summary>
    /// 定位 spark-host.exe：优先 UI 同目录（安装布局 {app}\Spark.exe + {app}\spark-host.exe）；
    /// 开发布局回退到仓库根 target/{debug,release}\spark-host.exe
    /// （UI 在 ui/Spark.UI/bin/{Debug,Release}/net8.0-…/win-x64，上溯 6 层到仓库根）。
    /// 返回 null 表示找不到 host。
    /// </summary>
    public static string? FindHostExe()
    {
        var baseDir = AppContext.BaseDirectory;
        var beside = Path.Combine(baseDir, "spark-host.exe");
        if (File.Exists(beside)) return beside;
        var repo = Path.GetFullPath(Path.Combine(
            baseDir, "..", "..", "..", "..", "..", ".."));
        foreach (var profile in new[] { "debug", "release" })
        {
            var p = Path.Combine(repo, "target", profile, "spark-host.exe");
            if (File.Exists(p)) return p;
        }
        return null;
    }

    private async Task ReadLoopAsync(CancellationToken ct, long generation)
    {
        var localPipe = _pipe;
        if (localPipe is null) return;
        App.Log("Ipc", "read loop started");
        var reader = new StreamReader(localPipe, Encoding.UTF8, false, 64 * 1024, leaveOpen: true);
        // 单飞行读：同一时刻只挂一个 ReadLineAsync。空闲超时（Host 无保活）时绝不能
        // 放弃当前读另起新读——旧读仍挂在管道上成为"幽灵读"，后续到达的响应会被它
        // 消费掉而 UI 永远收不到（表现为连接"活着"但所有请求 8s 超时、回退 DemoData）。
        // 超时后继续等同一个 task，直到它完成或出错。
        var readTask = reader.ReadLineAsync(ct).AsTask();
        _ = readTask.ContinueWith(t => _ = t.Exception,
            TaskContinuationOptions.OnlyOnFaulted);
        try
        {
            while (!ct.IsCancellationRequested && localPipe.IsConnected)
            {
                string? line;
                try
                {
                    line = await readTask.WaitAsync(TimeSpan.FromSeconds(15));
                }
                catch (TimeoutException)
                {
                    // 空闲探活：写探针确认写方向还通（探针是 notification id=-1，
                    // Host 会回 method-not-found 错误行，由同一个读消费，无副作用）。
                    try
                    {
                        await WriteLineAsync("{\"jsonrpc\":\"2.0\",\"id\":-1,\"method\":\"host.ping\"}", ct);
                    }
                    catch
                    {
                        break;
                    }
                    continue;
                }
                catch
                {
                    break;
                }
                if (line is null) break;
                // 当前读已完成：消费这一行后，才允许挂下一个读（保持单飞行不变式）
                readTask = reader.ReadLineAsync(ct).AsTask();
                _ = readTask.ContinueWith(t => _ = t.Exception,
                    TaskContinuationOptions.OnlyOnFaulted);
                if (string.IsNullOrWhiteSpace(line)) continue;
                try
                {
                    using var doc = JsonDocument.Parse(line);
                    var root = doc.RootElement;

                    if (root.TryGetProperty("method", out var methodEl)
                        && !root.TryGetProperty("result", out _)
                        && !root.TryGetProperty("error", out _))
                    {
                        var hasId = root.TryGetProperty("id", out var idEl)
                                    && idEl.ValueKind is not JsonValueKind.Null
                                    && idEl.ValueKind is not JsonValueKind.Undefined;
                        if (!hasId)
                        {
                            HostNotification?.Invoke(methodEl.GetString() ?? "");
                            continue;
                        }
                    }

                    if (root.TryGetProperty("id", out var rid))
                    {
                        var id = rid.ValueKind == JsonValueKind.Number
                            ? rid.GetInt32()
                            : int.TryParse(rid.ToString(), out var n) ? n : -1;
                        if (id >= 0 && _pending.TryRemove(id, out var tcs))
                        {
                            if (root.TryGetProperty("error", out var err))
                            {
                                var msg = err.TryGetProperty("message", out var m)
                                    ? m.GetString() ?? "ipc error"
                                    : "ipc error";
                                // host 错误原文是英文（Rust thiserror），在此包一层：正文中文、原文进 InnerException
                                tcs.TrySetException(HostErrorText.ToException(msg));
                            }
                            else if (root.TryGetProperty("result", out var result))
                            {
                                tcs.TrySetResult(result.Clone());
                            }
                            else
                            {
                                tcs.TrySetResult(default);
                            }
                        }
                    }
                }
                catch
                {
                    // 单行解析失败忽略
                }
            }
        }
        finally
        {
            if (generation == Volatile.Read(ref _connectionGeneration))
            {
                _loopDead = true;
                foreach (var kv in _pending)
                    kv.Value.TrySetCanceled();
                _pending.Clear();
            }
        }
    }

    public async Task<QueryResultDto> QueryAsync(string text, int limit = 50, CancellationToken ct = default)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected)
        {
            LogFallbackOnce("QueryFallback", new InvalidOperationException("not connected"));
            return DemoData.Query(text);
        }

        try
        {
            var result = await CallAsync("host.query", new { text, limit }, ct);
            if (result.ValueKind == JsonValueKind.Undefined)
            {
                LogFallbackOnce("QueryFallback",
                    new InvalidOperationException($"undefined result; writerNull={_writer is null}"));
                return DemoData.Query(text);
            }
            return JsonSerializer.Deserialize<QueryResultDto>(result.GetRawText())
                   ?? new QueryResultDto();
        }
        catch (Exception ex)
        {
            LogFallbackOnce("QueryFallback", ex);
            return DemoData.Query(text);
        }
    }

    public async Task<JsonElement?> InvokeAsync(string itemId, string actionId, string text,
        CancellationToken ct = default)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected) return null;
        try
        {
            var el = await CallAsync("host.invoke",
                new { item_id = itemId, action_id = actionId, text }, ct);
            return el.ValueKind == JsonValueKind.Undefined ? null : el;
        }
        catch
        {
            return null;
        }
    }

    /// <summary>内置系统命令清单（设置页"内置命令"栏；host 不可达返回空列表）。</summary>
    public async Task<List<BuiltinInfoDto>> GetBuiltinsAsync(CancellationToken ct = default)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected) return new List<BuiltinInfoDto>();
        try
        {
            var el = await CallAsync("host.get_builtins", new { }, ct);
            if (el.ValueKind == JsonValueKind.Array)
            {
                return JsonSerializer.Deserialize<List<BuiltinInfoDto>>(el.GetRawText()) ?? new();
            }
        }
        catch
        {
            // 忽略：设置页降级为空列表
        }
        return new List<BuiltinInfoDto>();
    }

    public async Task<HostConfigDto?> GetConfigAsync(CancellationToken ct = default)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected) return null;
        try
        {
            var el = await CallAsync("host.get_config", new { }, ct);
            return el.ValueKind == JsonValueKind.Undefined
                ? null
                : JsonSerializer.Deserialize<HostConfigDto>(el.GetRawText());
        }
        catch (Exception ex)
        {
            App.Log("GetHostConfig", ex);
            return null;
        }
    }

    public async Task<bool> SetConfigAsync(HostConfigUpdate update, CancellationToken ct = default)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected) return false;
        try
        {
            var result = await CallAsync("host.set_config", update, ct);
            return result.ValueKind == JsonValueKind.Object
                && result.TryGetProperty("ok", out var ok)
                && ok.ValueKind == JsonValueKind.True;
        }
        catch (Exception ex)
        {
            App.Log("SetHostConfig", ex);
            return false;
        }
    }

    /// <summary>把唤起热键预设推给 host（兼容旧调用方）。</summary>
    public Task SetHotkeyAsync(string hotkey, CancellationToken ct = default) =>
        SetConfigAsync(new HostConfigUpdate { HotkeyToggle = hotkey }, ct);

    // ─── 插件（host.plugin.*，见《插件开发规范》§15）──────────────────────

    /// <summary>已装插件清单；host 不可达返回空列表。</summary>
    public async Task<List<PluginInfoDto>> PluginListAsync(CancellationToken ct = default)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected) return new List<PluginInfoDto>();
        try
        {
            var el = await CallAsync("host.plugin.list", new { }, ct);
            if (el.ValueKind == JsonValueKind.Array)
                return JsonSerializer.Deserialize<List<PluginInfoDto>>(el.GetRawText()) ?? new();
        }
        catch (Exception ex)
        {
            App.Log("PluginList", ex);
        }
        return new List<PluginInfoDto>();
    }

    /// <summary>打开插件所需信息；不可用（未启用/非 webview/缺文件）时返回 null。</summary>
    public async Task<PluginOpenInfoDto?> PluginOpenAsync(string id, string input, string command,
        CancellationToken ct = default)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected) return null;
        try
        {
            var el = await CallAsync("host.plugin.open", new { id, input, command }, ct);
            return el.ValueKind == JsonValueKind.Object
                ? JsonSerializer.Deserialize<PluginOpenInfoDto>(el.GetRawText())
                : null;
        }
        catch (Exception ex)
        {
            App.Log("PluginOpen", ex);
            return null;
        }
    }

    /// <summary>从本地目录导入安装；返回安装结果（新装/更新/需确认降级）。
    /// force=true 时强制覆盖（用于降级确认后重试）。失败抛出（调用方展示原因）。</summary>
    public async Task<PluginInstallOutcomeDto> PluginInstallAsync(string path, bool force = false,
        CancellationToken ct = default)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected) throw HostErrorText.HostUnavailable();
        var el = await CallAsync("host.plugin.install", new { path, force }, ct);
        if (el.ValueKind == JsonValueKind.Object && el.TryGetProperty("id", out _))
            return JsonSerializer.Deserialize<PluginInstallOutcomeDto>(el.GetRawText())
                ?? throw new InvalidOperationException("后台未返回有效的安装结果");
        throw new InvalidOperationException("后台返回了无法识别的安装结果");
    }

    /// <summary>加载开发目录（不拷贝），返回插件 id；失败抛出。</summary>
    public Task<string> PluginDevLoadAsync(string dir, CancellationToken ct = default) =>
        PluginIdCallAsync("host.plugin.devload", new { dir }, ct);

    public Task<bool> PluginUninstallAsync(string id, CancellationToken ct = default) =>
        PluginOkCallAsync("host.plugin.uninstall", new { id }, ct);

    public Task<bool> PluginToggleAsync(string id, bool enabled, CancellationToken ct = default) =>
        PluginOkCallAsync("host.plugin.toggle", new { id, enabled }, ct);

    public Task<bool> PluginGrantAsync(string id, IEnumerable<string> permissions,
        CancellationToken ct = default) =>
        PluginOkCallAsync("host.plugin.grant", new { id, permissions = permissions.ToArray() }, ct);

    public Task<bool> PluginSetDirAsync(string path, bool migrate, CancellationToken ct = default) =>
        PluginOkCallAsync("host.plugin.set_dir", new { path, migrate }, ct);

    /// <summary>
    /// spark.* 特权能力桥：把插件页的调用转给 host 执行，返回 <c>data</c> 字段。
    /// host 侧鉴权（未授权返回 error），失败以异常抛出供 preload 转成 Promise reject。
    /// </summary>
    public async Task<JsonElement> PluginApiAsync(string pluginId, string capability, string method,
        JsonElement args, CancellationToken ct = default)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected) throw HostErrorText.HostUnavailable();

        var el = await CallAsync("host.plugin.api",
            new { plugin_id = pluginId, capability, method, args }, ct);
        if (el.ValueKind != JsonValueKind.Object)
            throw new InvalidOperationException("后台返回了无法识别的能力调用结果");
        return el.TryGetProperty("data", out var data) ? data.Clone() : default;
    }

    private async Task<string> PluginIdCallAsync(string method, object paramsObj, CancellationToken ct)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected) throw HostErrorText.HostUnavailable();
        var el = await CallAsync(method, paramsObj, ct);
        if (el.ValueKind == JsonValueKind.Object && el.TryGetProperty("id", out var idEl))
            return idEl.GetString() ?? "";
        throw new InvalidOperationException($"{method}: 后台返回了无法识别的结果");
    }

    private async Task<bool> PluginOkCallAsync(string method, object paramsObj, CancellationToken ct)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected) return false;
        try
        {
            var el = await CallAsync(method, paramsObj, ct);
            return el.ValueKind == JsonValueKind.Object
                && el.TryGetProperty("ok", out var ok)
                && ok.ValueKind == JsonValueKind.True;
        }
        catch (Exception ex)
        {
            App.Log(method, ex);
            return false;
        }
    }

    private async Task WriteLineAsync(string line, CancellationToken ct)
    {
        await _writeLock.WaitAsync(ct);
        try
        {
            if (_writer is null) throw new IOException("pipe writer unavailable");
            await _writer.WriteLineAsync(line.AsMemory(), ct);
        }
        finally
        {
            _writeLock.Release();
        }
    }

    private async Task<JsonElement> CallAsync(string method, object paramsObj, CancellationToken ct)
    {
        if (!IsConnected || _writer is null)
            return default;

        var id = 0;
        TaskCompletionSource<JsonElement>? tcs = null;
        try
        {
            id = Interlocked.Increment(ref _nextId);
            tcs = new TaskCompletionSource<JsonElement>(TaskCreationOptions.RunContinuationsAsynchronously);
            _pending[id] = tcs;

            var req = new Dictionary<string, object?>
            {
                ["jsonrpc"] = "2.0",
                ["id"] = id,
                ["method"] = method,
                ["params"] = paramsObj
            };
            var json = JsonSerializer.Serialize(req);

            await WriteLineAsync(json, ct);

            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(ct);
            timeout.CancelAfter(TimeSpan.FromSeconds(8));
            using var reg = timeout.Token.Register(() =>
            {
                if (_pending.TryRemove(id, out var p))
                    p.TrySetException(new TimeoutException(HostErrorText.Translate("host ipc timeout")));
            });

            try
            {
                return await tcs.Task;
            }
            catch (TimeoutException)
            {
                await TearDownAsync();
                throw;
            }
        }
        catch
        {
            if (id != 0 && tcs is not null && _pending.TryRemove(id, out var pending))
                pending.TrySetCanceled();
            throw;
        }
    }

    private async Task TearDownAsync()
    {
        try { _loopCts?.Cancel(); } catch { /* ignore */ }
        try
        {
            if (_loopTask is not null)
                await Task.WhenAny(_loopTask, Task.Delay(200));
        }
        catch { /* ignore */ }

        await _writeLock.WaitAsync();
        try
        {
            _loopTask = null;
            _loopCts?.Dispose();
            _loopCts = null;
            if (_writer is not null)
            {
                try { await _writer.DisposeAsync(); } catch { /* ignore */ }
            }
            _writer = null;
            if (_pipe is not null)
            {
                try { await _pipe.DisposeAsync(); } catch { /* ignore */ }
            }
            _pipe = null;
            foreach (var kv in _pending)
                kv.Value.TrySetCanceled();
            _pending.Clear();
        }
        finally
        {
            _writeLock.Release();
        }
    }

    public async ValueTask DisposeAsync() => await TearDownAsync();
}

public sealed class HostConfigDto
{
    [JsonPropertyName("hotkey_toggle")]
    public string HotkeyToggle { get; set; } = "Alt+Space";
    // hide_on_focus_lost / hide_on_execute 已固化为始终开启的默认行为：
    // host 旧 config 仍会下发这两个字段，此处有意不映射（未知字段反序列化时跳过）。
    [JsonPropertyName("max_results")]
    public uint MaxResults { get; set; } = 50;
    [JsonPropertyName("plugins_dir")]
    public string? PluginsDir { get; set; }
    [JsonPropertyName("hotkey_enabled")]
    public bool HotkeyEnabled { get; set; } = true;
    [JsonPropertyName("launch_on_startup")]
    public bool LaunchOnStartup { get; set; }
    /// <summary>严格模式（3.2）：仅安装带有效签名的插件，默认关。</summary>
    [JsonPropertyName("strict_mode")]
    public bool StrictMode { get; set; }
    /// <summary>用户导入的"受信任开发者"三方公钥表（3.3）。</summary>
    [JsonPropertyName("trusted_pubkeys")]
    public List<TrustedPubkeyDto> TrustedPubkeys { get; set; } = new();
    /// <summary>插件市场仓库 URL 列表（空列表 = 仅官方仓库）。</summary>
    [JsonPropertyName("plugin_registry_urls")]
    public List<string> PluginRegistryUrls { get; set; } = new();
}

public sealed class TrustedPubkeyDto
{
    /// <summary>开发者公钥标识（signature.json 的 key_id）。</summary>
    [JsonPropertyName("key_id")]
    public string KeyId { get; set; } = "";
    /// <summary>base64 编码的 32 字节 Ed25519 公钥。</summary>
    [JsonPropertyName("public_key")]
    public string PublicKey { get; set; } = "";
    /// <summary>展示用备注。</summary>
    [JsonPropertyName("note")]
    public string Note { get; set; } = "";
}

public sealed class HostConfigUpdate
{
    [JsonPropertyName("hotkey_toggle")]
    public string? HotkeyToggle { get; set; }
    [JsonPropertyName("launch_on_startup")]
    public bool? LaunchOnStartup { get; set; }
    [JsonPropertyName("strict_mode")]
    public bool? StrictMode { get; set; }
    /// <summary>全量替换受信任开发者表；null 表示不动。</summary>
    [JsonPropertyName("trusted_pubkeys")]
    public List<TrustedPubkeyDto>? TrustedPubkeys { get; set; }
    /// <summary>全量替换插件市场仓库 URL 列表；null 表示不动。</summary>
    [JsonPropertyName("plugin_registry_urls")]
    public List<string>? PluginRegistryUrls { get; set; }

    public HostConfigUpdate Clone() => new()
    {
        HotkeyToggle = HotkeyToggle,
        LaunchOnStartup = LaunchOnStartup,
        StrictMode = StrictMode,
        TrustedPubkeys = TrustedPubkeys,
        PluginRegistryUrls = PluginRegistryUrls,
    };

    public bool Equals(HostConfigUpdate? other) => other is not null
        && HotkeyToggle == other.HotkeyToggle
        && LaunchOnStartup == other.LaunchOnStartup
        && StrictMode == other.StrictMode
        && TrustedPubkeys == other.TrustedPubkeys
        && PluginRegistryUrls == other.PluginRegistryUrls;
}

public static class DemoData
{
    public static readonly CandidateDto[] Seed =
    {
        new() { Id = "app.wt", Title = "Windows Terminal", Subtitle = "应用程序", Score = 1, Source = "app", Target = "wt.exe" },
        new() { Id = "app.code", Title = "Visual Studio Code", Subtitle = "最近", Score = 0.98f, Source = "history" },
        new() { Id = "app.chrome", Title = "Google Chrome", Subtitle = "应用程序", Score = 0.95f, Source = "app" },
        new() { Id = "app.explorer", Title = "文件资源管理器", Subtitle = "系统", Score = 0.8f, Source = "app", Target = @"C:\Windows\explorer.exe" },
        new() { Id = "sys.settings", Title = "设置", Subtitle = "打开启动器设置", Score = 0.84f, Source = "builtin" },
    };

    private static readonly Dictionary<string, string[]> Keys = new(StringComparer.OrdinalIgnoreCase)
    {
        ["app.wt"] = ["wt", "terminal", "终端"],
        ["app.code"] = ["code", "vscode"],
        ["app.chrome"] = ["chrome", "浏览器"],
        ["sys.settings"] = ["settings", "设置", "prefs"],
        ["app.explorer"] = ["explorer", "资源"],
    };

    public static CandidateDto? Find(string id) =>
        Seed.FirstOrDefault(x => x.Id.Equals(id, StringComparison.OrdinalIgnoreCase));

    public static QueryResultDto Query(string text)
    {
        var q = text.Trim();
        IEnumerable<CandidateDto> items;
        if (string.IsNullOrEmpty(q))
            items = Seed.Take(8);
        else
            items = Seed.Where(x =>
                x.Title.Contains(q, StringComparison.OrdinalIgnoreCase) ||
                (x.Subtitle?.Contains(q, StringComparison.OrdinalIgnoreCase) ?? false) ||
                x.Id.Contains(q, StringComparison.OrdinalIgnoreCase) ||
                (Keys.TryGetValue(x.Id, out var ks) &&
                 ks.Any(k => k.Contains(q, StringComparison.OrdinalIgnoreCase) ||
                             q.Contains(k, StringComparison.OrdinalIgnoreCase))));

        var list = items.Select((x, i) => new CandidateDto
        {
            Id = x.Id,
            Title = x.Title,
            Subtitle = x.Subtitle,
            Score = x.Score,
            Source = x.Source,
            Target = x.Target,
            IconPath = x.IconPath,
            Shortcut = i < 9 ? $"{i + 1}" : ""
        }).ToList();

        return new QueryResultDto { Items = list };
    }
}
