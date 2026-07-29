using System.IO.Pipes;
using System.Text;
using System.Text.Json;
using Spark.UI.Models;

namespace Spark.UI.Services;

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
            _pipe = new NamedPipeClientStream(".", DefaultPipeName, PipeDirection.InOut, PipeOptions.Asynchronous);
            await _pipe.ConnectAsync(200, ct);
            _reader = new StreamReader(_pipe, Encoding.UTF8, false, 4096, true);
            _writer = new StreamWriter(_pipe, Encoding.UTF8, 4096, true) { AutoFlush = true };
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
        var req = new { jsonrpc = "2.0", id, method = "host.query", @params = new { text, limit } };
        await _writer!.WriteLineAsync(JsonSerializer.Serialize(req).AsMemory(), ct);
        var line = await _reader!.ReadLineAsync(ct) ?? throw new InvalidOperationException("host closed pipe");
        using var doc = JsonDocument.Parse(line);
        if (doc.RootElement.TryGetProperty("error", out var err))
            throw new InvalidOperationException(err.GetProperty("message").GetString() ?? "ipc error");
        return JsonSerializer.Deserialize<QueryResultDto>(doc.RootElement.GetProperty("result").GetRawText())
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
    public static readonly CandidateDto[] Seed =
    {
        new() { Id = "app.wt", Title = "Windows Terminal", Subtitle = "应用程序", Score = 1, Source = "应用", Icon = "Wt" },
        new() { Id = "app.code", Title = "Visual Studio Code", Subtitle = "最近 · 3 分钟前", Score = 0.98f, Source = "历史", Icon = "Vs" },
        new() { Id = "app.chrome", Title = "Google Chrome", Subtitle = "应用程序", Score = 0.95f, Source = "应用", Icon = "Ch" },
        new() { Id = "tool.calc", Title = "计算 128 * 32", Subtitle = "= 4096 · Enter 复制", Score = 0.9f, Source = "工具", Icon = "=" },
        new() { Id = "plugin.echo", Title = "Echo hello", Subtitle = "插件命令", Score = 0.88f, Source = "Echo", Icon = "Ec" },
        new() { Id = "file.readme", Title = "项目 README.md", Subtitle = @"D:\demo\test01\docs", Score = 0.85f, Source = "文件", Icon = "Md" },
        new() { Id = "sys.settings", Title = "设置", Subtitle = "打开启动器设置", Score = 0.84f, Source = "系统", Icon = "Se" },
        new() { Id = "app.explorer", Title = "文件资源管理器", Subtitle = "应用程序", Score = 0.8f, Source = "应用", Icon = "Ex" },
        new() { Id = "plugin.json", Title = "JSON 格式化", Subtitle = "剪贴板 · 需权限", Score = 0.76f, Source = "插件", Icon = "{}" },
        new() { Id = "sys.lock", Title = "锁定工作站", Subtitle = "系统操作 · 需确认", Score = 0.7f, Source = "系统", Icon = "Lk" },
        new() { Id = "file.arch", Title = "ARCHITECTURE.md", Subtitle = "docs · 今天", Score = 0.68f, Source = "文件", Icon = "Ar" },
        new() { Id = "web.g", Title = "g rust async", Subtitle = "Google 搜索", Score = 0.65f, Source = "快搜", Icon = "G" },
    };

    private static readonly Dictionary<string, string[]> Keys = new(StringComparer.OrdinalIgnoreCase)
    {
        ["app.wt"] = ["wt", "terminal", "终端"],
        ["app.code"] = ["code", "vscode"],
        ["app.chrome"] = ["chrome", "浏览器"],
        ["tool.calc"] = ["128", "算", "calc"],
        ["plugin.echo"] = ["echo", "hello"],
        ["file.readme"] = ["readme", "docs"],
        ["sys.settings"] = ["settings", "设置", "prefs"],
        ["app.explorer"] = ["explorer", "资源"],
        ["plugin.json"] = ["json", "格式"],
        ["sys.lock"] = ["lock", "锁屏"],
        ["file.arch"] = ["arch", "架构"],
        ["web.g"] = ["g ", "google", "rust"],
    };

    public static CandidateDto? Find(string id) =>
        Seed.FirstOrDefault(x => x.Id.Equals(id, StringComparison.OrdinalIgnoreCase));

    public static QueryResultDto Query(string text)
    {
        var q = text.Trim();
        IEnumerable<CandidateDto> items;
        if (string.IsNullOrEmpty(q))
        {
            items = Seed.Take(8);
        }
        else
        {
            items = Seed.Where(x =>
                x.Title.Contains(q, StringComparison.OrdinalIgnoreCase) ||
                (x.Subtitle?.Contains(q, StringComparison.OrdinalIgnoreCase) ?? false) ||
                x.Id.Contains(q, StringComparison.OrdinalIgnoreCase) ||
                (Keys.TryGetValue(x.Id, out var ks) &&
                 ks.Any(k => k.Contains(q, StringComparison.OrdinalIgnoreCase) ||
                             q.Contains(k, StringComparison.OrdinalIgnoreCase))));
        }
        return new QueryResultDto { Items = items.ToList() };
    }
}
