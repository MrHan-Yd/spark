using System.Text.Json;
using Spark.UI.Models;

namespace Spark.UI.Services;

public static class LocalState
{
    private static readonly string Dir = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Spark");

    public static AppUiState Ui { get; private set; } = new();
    public static FavoritesState Fav { get; private set; } = FavoritesState.CreateDefault();

    public static void Load()
    {
        try
        {
            Directory.CreateDirectory(Dir);
            var uiPath = Path.Combine(Dir, "ui-settings.json");
            if (File.Exists(uiPath))
            {
                var s = JsonSerializer.Deserialize<AppUiState>(File.ReadAllText(uiPath));
                if (s is not null) Ui = s;
            }
            var favPath = Path.Combine(Dir, "favorites.json");
            if (File.Exists(favPath))
            {
                var f = JsonSerializer.Deserialize<FavoritesState>(File.ReadAllText(favPath));
                if (f is not null) Fav = f;
            }
        }
        catch
        {
            Ui = new AppUiState();
            Fav = FavoritesState.CreateDefault();
        }
    }

    public static void SaveUi()
    {
        try
        {
            Directory.CreateDirectory(Dir);
            File.WriteAllText(Path.Combine(Dir, "ui-settings.json"), JsonSerializer.Serialize(Ui));
        }
        catch { }
    }

    public static void SaveFav()
    {
        try
        {
            Directory.CreateDirectory(Dir);
            File.WriteAllText(Path.Combine(Dir, "favorites.json"), JsonSerializer.Serialize(Fav));
        }
        catch { }
    }
}

public sealed class AppUiState
{
    public bool LaunchOnStartup { get; set; } = true;
    public bool HideOnFocusLost { get; set; } = true;
    public bool HideAfterInvoke { get; set; } = true;
    public string Hotkey { get; set; } = "Alt+Space";
    public string Theme { get; set; } = "dark";
    public string DefaultView { get; set; } = "grid";
    public int WindowWidth { get; set; } = 800;
    /// <summary>上次拖拽后的窗口位置（物理像素，屏幕坐标）；-1 = 未记录，居中显示。</summary>
    public int WindowX { get; set; } = -1;
    public int WindowY { get; set; } = -1;
    public bool ReduceMotion { get; set; }
}

public sealed class FavoritesState
{
    public List<FavGroupDto> Groups { get; set; } = new();
    public List<FavEntryDto> Items { get; set; } = new();
    public string ActiveGroup { get; set; } = "all";
    public bool Expanded { get; set; } = true;

    public static FavoritesState CreateDefault() => new()
    {
        Groups =
        {
            new() { Id = "all", Name = "全部" },
            new() { Id = "work", Name = "工作" },
            new() { Id = "dev", Name = "开发" },
            new() { Id = "daily", Name = "日常" },
        },
        Items =
        {
            new() { ItemId = "app.wt", GroupId = "dev" },
            new() { ItemId = "app.code", GroupId = "dev" },
            new() { ItemId = "app.chrome", GroupId = "daily" },
            new() { ItemId = "app.explorer", GroupId = "work" },
        },
        ActiveGroup = "all",
        Expanded = true
    };
}

public sealed class FavGroupDto
{
    public string Id { get; set; } = "";
    public string Name { get; set; } = "";
}

public sealed class FavEntryDto
{
    public string ItemId { get; set; } = "";
    public string GroupId { get; set; } = "all";
    /// <summary>收藏时快照的展示信息：不在当前搜索结果里时也能渲染卡片（旧数据为 null，回退按 ItemId 查找）。</summary>
    public string? Title { get; set; }
    public string? Target { get; set; }
    public string? IconPath { get; set; }
}
