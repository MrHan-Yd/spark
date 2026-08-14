using System.Collections.Concurrent;
using System.IO.Pipes;
using System.Text;
using System.Text.Json;
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
    private volatile bool _loopDead;

    public bool IsConnected => _pipe?.IsConnected == true && !_loopDead;

    public event Action<string>? HostNotification;

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
            // 后 accept 造成僵尸连接。重试 6 次 ≈ 最坏 10s，足够等 Host 起来。
            for (var attempt = 0; attempt < 6; attempt++)
            {
                ct.ThrowIfCancellationRequested();
                var pipe = new NamedPipeClientStream(".", DefaultPipeName, PipeDirection.InOut,
                    PipeOptions.Asynchronous);
                try
                {
                    await pipe.ConnectAsync(1500, ct);
                    _pipe = pipe;
                    _writer = new StreamWriter(pipe, new UTF8Encoding(false), 64 * 1024, leaveOpen: true)
                    {
                        AutoFlush = true,
                        NewLine = "\n"
                    };
                    _loopCts = new CancellationTokenSource();
                    _loopDead = false;
                    _loopTask = Task.Run(() => ReadLoopAsync(_loopCts.Token));
                    last = null;
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

    private async Task ReadLoopAsync(CancellationToken ct)
    {
        if (_pipe is null) return;
        App.Log("Ipc", "read loop started");
        var reader = new StreamReader(_pipe, Encoding.UTF8, false, 64 * 1024, leaveOpen: true);
        // 单飞行读：同一时刻只挂一个 ReadLineAsync。空闲超时（Host 无保活）时绝不能
        // 放弃当前读另起新读——旧读仍挂在管道上成为"幽灵读"，后续到达的响应会被它
        // 消费掉而 UI 永远收不到（表现为连接"活着"但所有请求 8s 超时、回退 DemoData）。
        // 超时后继续等同一个 task，直到它完成或出错。
        var readTask = reader.ReadLineAsync(ct).AsTask();
        _ = readTask.ContinueWith(t => _ = t.Exception,
            TaskContinuationOptions.OnlyOnFaulted);
        try
        {
            while (!ct.IsCancellationRequested && _pipe is not null && _pipe.IsConnected)
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
                        _writer?.WriteLine("{\"jsonrpc\":\"2.0\",\"id\":-1,\"method\":\"host.ping\"}");
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
                                tcs.TrySetException(new InvalidOperationException(msg));
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
            _loopDead = true;
            foreach (var kv in _pending)
                kv.Value.TrySetCanceled();
            _pending.Clear();
        }
    }

    public async Task<QueryResultDto> QueryAsync(string text, int limit = 50, CancellationToken ct = default)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected)
        {
            App.Log("QueryFallback", new InvalidOperationException("not connected"));
            return DemoData.Query(text);
        }

        try
        {
            var result = await CallAsync("host.query", new { text, limit }, ct);
            if (result.ValueKind == JsonValueKind.Undefined)
            {
                App.Log("QueryFallback",
                    new InvalidOperationException($"undefined result; writerNull={_writer is null}"));
                return DemoData.Query(text);
            }
            return JsonSerializer.Deserialize<QueryResultDto>(result.GetRawText())
                   ?? new QueryResultDto();
        }
        catch (Exception ex)
        {
            App.Log("QueryFallback", ex);
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

    /// <summary>把唤起热键预设推给 host（host 负责注册全局热键并持久化到 config.toml）。
    /// host 不可达时静默放弃：连接建立/重建时会重新同步。</summary>
    public async Task SetHotkeyAsync(string hotkey, CancellationToken ct = default)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected) return;
        try
        {
            await CallAsync("host.set_config", new { hotkey_toggle = hotkey }, ct);
        }
        catch
        {
            // 热键注册失败不影响设置页本地状态；重连时会再推
        }
    }

    private async Task<JsonElement> CallAsync(string method, object paramsObj, CancellationToken ct)
    {
        if (!IsConnected || _writer is null)
            return default;

        var id = Interlocked.Increment(ref _nextId);
        var tcs = new TaskCompletionSource<JsonElement>(TaskCreationOptions.RunContinuationsAsynchronously);
        _pending[id] = tcs;

        var req = new Dictionary<string, object?>
        {
            ["jsonrpc"] = "2.0",
            ["id"] = id,
            ["method"] = method,
            ["params"] = paramsObj
        };
        var json = JsonSerializer.Serialize(req);

        await _writeLock.WaitAsync(ct);
        try
        {
            await _writer.WriteLineAsync(json.AsMemory(), ct);
        }
        finally
        {
            _writeLock.Release();
        }

        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(ct);
        timeout.CancelAfter(TimeSpan.FromSeconds(8));
        await using var reg = timeout.Token.Register(() =>
        {
            if (_pending.TryRemove(id, out var p))
                p.TrySetException(new TimeoutException("host ipc timeout"));
        });

        try
        {
            return await tcs.Task;
        }
        catch (TimeoutException)
        {
            // 请求写出去但没有响应 = 连接假死（对端消失/半死）。主动断开，
            // 让下一次 EnsureConnected 拿一条全新连接，而不是反复超时回退 DemoData。
            await TearDownAsync();
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

    public async ValueTask DisposeAsync() => await TearDownAsync();
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
