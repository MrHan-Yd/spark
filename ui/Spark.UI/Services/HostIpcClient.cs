using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using Spark.UI.Models;

namespace Spark.UI.Services;

/// <summary>
/// Host JSON-RPC 客户端。Host 未启动时回落演示数据，便于单独调试 UI。
/// </summary>
public sealed class HostIpcClient : IAsyncDisposable
{
    public const string DefaultPipeName = "spark.host.ipc";

    private NamedPipeClientStream? _pipe;
    private StreamReader? _reader;
    private StreamWriter? _writer;
    private int _nextId = 1;

    public bool IsConnected => _pipe?.IsConnected == true;

    public async Task EnsureConnectedAsync(CancellationToken ct = default)
    {
        if (IsConnected) return;
        try
        {
            _pipe = new NamedPipeClientStream(
                ".",
                DefaultPipeName,
                PipeDirection.InOut,
                PipeOptions.Asynchronous);

            await _pipe.ConnectAsync(200, ct);
            _reader = new StreamReader(_pipe, Encoding.UTF8, detectEncodingFromByteOrderMarks: false, bufferSize: 4096, leaveOpen: true);
            _writer = new StreamWriter(_pipe, Encoding.UTF8, bufferSize: 4096, leaveOpen: true) { AutoFlush = true };
        }
        catch
        {
            await DisposePipeAsync();
        }
    }

    public async Task<QueryResultDto> QueryAsync(string text, int limit = 50, CancellationToken ct = default)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected)
            return DemoData.Query(text);

        var id = _nextId++;
        var req = new
        {
            jsonrpc = "2.0",
            id,
            method = "host.query",
            @params = new { text, limit }
        };

        await _writer!.WriteLineAsync(JsonSerializer.Serialize(req).AsMemory(), ct);
        var line = await _reader!.ReadLineAsync(ct)
                   ?? throw new InvalidOperationException("host closed pipe");

        using var doc = JsonDocument.Parse(line);
        if (doc.RootElement.TryGetProperty("error", out var err))
            throw new InvalidOperationException(err.GetProperty("message").GetString() ?? "ipc error");

        var result = doc.RootElement.GetProperty("result");
        return JsonSerializer.Deserialize<QueryResultDto>(result.GetRawText())
               ?? new QueryResultDto();
    }

    public async Task InvokeAsync(string itemId, string actionId, string text, CancellationToken ct = default)
    {
        await EnsureConnectedAsync(ct);
        if (!IsConnected) return;

        var id = _nextId++;
        var req = new
        {
            jsonrpc = "2.0",
            id,
            method = "host.invoke",
            @params = new { item_id = itemId, action_id = actionId, text }
        };
        await _writer!.WriteLineAsync(JsonSerializer.Serialize(req).AsMemory(), ct);
        _ = await _reader!.ReadLineAsync(ct);
    }

    private async Task DisposePipeAsync()
    {
        if (_writer is not null) await _writer.DisposeAsync();
        if (_reader is not null) _reader.Dispose();
        if (_pipe is not null) await _pipe.DisposeAsync();
        _writer = null;
        _reader = null;
        _pipe = null;
    }

    public async ValueTask DisposeAsync() => await DisposePipeAsync();
}

public static class DemoData
{
    private static readonly CandidateDto[] Seed =
    {
        new() { Id = "app.wt", Title = "Windows Terminal", Subtitle = "应用程序", Score = 1, Source = "app" },
        new() { Id = "app.code", Title = "Visual Studio Code", Subtitle = "应用程序", Score = 0.95f, Source = "app" },
        new() { Id = "app.chrome", Title = "Google Chrome", Subtitle = "应用程序", Score = 0.9f, Source = "app" },
        new() { Id = "sys.settings", Title = "Spark 设置", Subtitle = "打开设置", Score = 0.8f, Source = "builtin" },
    };

    public static QueryResultDto Query(string text)
    {
        var q = text.Trim();
        IEnumerable<CandidateDto> items = string.IsNullOrEmpty(q)
            ? Seed
            : Seed.Where(x =>
                x.Title.Contains(q, StringComparison.OrdinalIgnoreCase)
                || x.Id.Contains(q, StringComparison.OrdinalIgnoreCase));
        return new QueryResultDto { Items = items.ToList() };
    }
}
