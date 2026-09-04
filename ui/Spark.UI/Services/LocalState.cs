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
    public string Hotkey { get; set; } = "Alt+Space";
    public string Theme { get; set; } = "dark";
    public string DefaultView { get; set; } = "grid";
    public int WindowWidth { get; set; } = 800;
    /// <summary>上次拖拽后的窗口位置（物理像素，屏幕坐标）；-1 = 未记录，居中显示。</summary>
    public int WindowX { get; set; } = -1;
    public int WindowY { get; set; } = -1;
    /// <summary>桌面悬浮球开关（通用设置）：true = 常驻置顶悬浮球，点击唤起/隐藏主窗口。</summary>
    public bool FloatingBallEnabled { get; set; }
    /// <summary>悬浮球驻留模式：true = 贴边驻留（只露窄条，悬停滑出），
    /// false = 自由悬浮常显（任意位置）。旧设置缺省贴边。</summary>
    public bool BallDocked { get; set; } = true;
    /// <summary>悬浮球贴边方向（"left"/"right"/"top"/"bottom"）。</summary>
    public string BallEdge { get; set; } = "right";
    /// <summary>自由悬浮时悬浮球水平位置（物理像素，屏幕坐标）；-1 = 未记录，水平居中。</summary>
    public int BallX { get; set; } = -1;
    /// <summary>悬浮球垂直位置（物理像素，屏幕坐标）；-1 = 未记录，取工作区垂直 1/4 处。</summary>
    public int BallY { get; set; } = -1;
    /// <summary>开发者模式（通用设置）：开启后插件页显示调试按钮，插件窗口开放 DevTools。</summary>
    public bool DeveloperMode { get; set; }
}

public sealed class FavoritesState
{
    public List<FavGroupDto> Groups { get; set; } = new();
    public List<FavEntryDto> Items { get; set; } = new();
    public string ActiveGroup { get; set; } = "all";
    public bool Expanded { get; set; } = true;

    /// <summary>新用户零预置：只留「全部」兜底分组，收藏完全由用户右键「收藏到」自建；
    /// 空态由 RenderFavorites 的占位文案兜住，绝不写死演示应用。</summary>
    public static FavoritesState CreateDefault() => new()
    {
        Groups = { new() { Id = "all", Name = "全部" } },
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
