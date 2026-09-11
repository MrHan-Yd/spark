namespace Spark.UI.Services;

/// <summary>
/// 市场侧**纯规则**：标签净化、版本比较、分类归组、筛选判定、索引条目容错。
///
/// 为什么单独成文件：这些逻辑目前是"唯一没有自动化测试的一层"（UI 进程）。放在这里就
/// **不依赖任何 WinUI 类型**，测试工程可以直接 `&lt;Compile Include&gt;` 链接本文件编译，
/// 不必引用 WinUI/WinExe 工程（引用会拖起 XAML 运行时，且 Brush/Visibility 之类在没有
/// UI 线程时无法构造）。改本文件时必须保持"零 WinUI 依赖"，否则测试工程会编译不过——
/// 这是刻意设计的护栏。
/// </summary>
public static class MarketRules
{
    // ────────────────────────────── 分类标签（规范 §3.3）──────────────────────────────

    /// <summary>单个标签的最大长度（UTF-16 码元）。超长标签整条丢弃而非截断——截断会把两个
    /// 不同的长标签截成同一个组名。中文分类名一般 2-5 字，12 已很宽松。</summary>
    public const int MaxTagLength = 12;

    /// <summary>单个插件最多保留的标签数（超出丢弃后续），防三方仓库塞一堆标签把卡片标签行
    /// 与「分类」筛选栏撑爆。</summary>
    public const int MaxTagsPerPlugin = 4;

    /// <summary>未打标签（或标签全被净化掉）的插件归入的分组名。与真实标签可能重名，
    /// 故内部一律用本常量比较，不写字面量。</summary>
    public const string UntaggedGroup = "未分类";

    /// <summary>卡片标签行最多展示几个（超出以省略号收尾），避免撑高卡片。</summary>
    public const int TagsSummaryMax = 3;

    /// <summary>单个标签是否可用（净化与打包期校验共用同一条判据）。</summary>
    public static bool IsTagAcceptable(string? tag)
    {
        if (tag is null) return false;
        // 内部先 Trim：调用方无需预处理，空白串/全空白都判不可用（NormalizeTags 与
        // 打包期校验共用本判据，语义必须自包含）。
        tag = tag.Trim();
        if (tag.Length == 0) return false;
        if (tag.Length > MaxTagLength) return false;
        // 控制字符/换行会破坏分组头与筛选按钮的排版，且常出现在注入尝试里。
        foreach (var ch in tag) if (char.IsControl(ch)) return false;
        return true;
    }

    /// <summary>
    /// 标签净化（registry.json 是三方不可信数据，规范 §9.5 容错）：Trim → 非空 → 拒控制字符
    /// → 限长 → 按忽略大小写去重 → 限量。不合法标签静默丢弃（不因此拒绝整个插件条目）；
    /// 全部不合法时返回空表，该插件在 UI 归入 <see cref="UntaggedGroup"/>。
    ///
    /// 注意 <c>Trim</c> 先于校验：`" ok "` 是合法标签（去空白后合法），而 `"   "` 不合法。
    /// </summary>
    public static List<string> NormalizeTags(IEnumerable<string?>? raw)
    {
        var result = new List<string>();
        if (raw is null) return result;

        var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var item in raw)
        {
            if (result.Count >= MaxTagsPerPlugin) break;
            if (item is null) continue;

            var tag = item.Trim();
            if (!IsTagAcceptable(tag)) continue;
            if (seen.Add(tag)) result.Add(tag);
        }
        return result;
    }

    /// <summary>归组用的分类键：取首个标签；无标签（或标签全被净化掉）归入「未分类」。
    /// 一个插件只能出现在一个组里，否则多标签插件会在列表里重复出现、安装状态难以对齐。</summary>
    public static string PrimaryTag(IReadOnlyList<string>? tags)
        => tags is { Count: > 0 } ? tags[0] : UntaggedGroup;

    /// <summary>卡片上的标签文案（前 <paramref name="max"/> 个 + 省略号）。</summary>
    public static string TagsSummary(IReadOnlyList<string>? tags, int max = TagsSummaryMax)
    {
        if (tags is null || tags.Count == 0) return "";
        var take = Math.Max(1, max);
        var text = string.Join(" · ", tags.Take(take));
        return tags.Count > take ? text + " …" : text;
    }

    /// <summary>分类筛选：按"任一标签命中"匹配（一个插件可归多个分类）。<paramref name="activeTag"/>
    /// 为 null = 不按分类过滤。</summary>
    public static bool MatchesTag(IReadOnlyList<string>? tags, string? activeTag)
    {
        if (activeTag is null) return true;
        if (tags is null) return false;
        foreach (var t in tags)
        {
            if (string.Equals(t, activeTag, StringComparison.OrdinalIgnoreCase)) return true;
        }
        return false;
    }

    /// <summary>
    /// 分组顺序：非「未分类」组按插件数降序、同数按组名序，<see cref="UntaggedGroup"/>
    /// 恒排最后（它是兜底组，不该占"最热门分类"的位置）。传入按列表顺序排列的"各条目的
    /// primary tag"，返回去重后的分组顺序。组内顺序由调用方按原列表顺序保留。
    /// </summary>
    public static List<string> GroupOrder(IReadOnlyList<string>? primaryTagsInOrder)
    {
        var result = new List<string>();
        if (primaryTagsInOrder is null) return result;

        var counts = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase);
        var firstSeen = new List<string>();
        foreach (var tag in primaryTagsInOrder)
        {
            if (counts.TryGetValue(tag, out var n)) counts[tag] = n + 1;
            else { counts[tag] = 1; firstSeen.Add(tag); }
        }

        return firstSeen
            .OrderByDescending(t => !IsUntagged(t))
            .ThenByDescending(t => counts[t])
            .ThenBy(t => t, StringComparer.CurrentCulture)
            .ToList();
    }

    /// <summary>是否为兜底组（用常量比较，避免与真实标签 "未分类" 的字面量重名比较出错）。</summary>
    public static bool IsUntagged(string? tag) => string.Equals(tag, UntaggedGroup, StringComparison.Ordinal);

    // ────────────────────────────── 索引条目容错（规范 §9.5）──────────────────────────────

    /// <summary>
    /// 索引条目是否可用：id 与 latest 都必须非空白。任一缺失即整条跳过（不崩、不误装），
    /// 其余条目照常展示——这是《插件市场与仓库》§9.5「跳过不完整的插件条目」的判据。
    /// </summary>
    public static bool IsPluginEntryUsable(string? id, string? latest)
        => !string.IsNullOrWhiteSpace(id) && !string.IsNullOrWhiteSpace(latest);

    /// <summary>
    /// 索引 schema 是否受支持。当前只支持 schema=1；更高版本要拒绝整源（换源结果相同），
    /// 而不是逐条跳过——版本语义变了就无从判断字段含义。
    /// </summary>
    public static bool IsSchemaSupported(int schema) => schema == 1;

    // ────────────────────────────── 版本比较（更新/降级判定）──────────────────────────────

    /// <summary>
    /// 比较两个版本号：a &gt; b 返回正、a &lt; b 返回负、相等返回 0。
    ///
    /// **语义与 host 侧 <c>cmp_version</c> 完全对齐**（market 的"可更新/可降级"判定以此为准，
    /// 两端不一致就会出现"装得进但市场说能更新"的错位）：
    /// - 跳过前导非数字（兼容 `v0.1.0`）；整串无数字 → 解析失败。
    /// - 每段**只取连续数字前缀**（`0beta` → `0`，`0.1.0beta` ≡ `0.1.0`），空段补 0。
    /// - 只取前三段，第四段及以后忽略（`1.0.0.1` ≡ `1.0.0`）。
    /// - 一方能解析、另一方不能 → 能解析的大；双方都不能 → 回退**序数**字符串比较保证全序。
    ///
    /// 与 host 的唯一已知偏差：段数值超过 <see cref="int.MaxValue"/> 时 host 会解析失败当 0，
    /// 这里选择饱和到上限（保持单调，不产生"超大版本反而变小"的怪象）。
    /// </summary>
    public static int CompareVersion(string? a, string? b)
    {
        var av = TryParseVersionTuple(a);
        var bv = TryParseVersionTuple(b);
        if (av.HasValue && bv.HasValue) return av.Value.CompareTo(bv.Value);
        if (av.HasValue) return 1;
        if (bv.HasValue) return -1;
        // 双方都解析不出数字段 → 序数字符串比较（host 的 str::cmp 是字节序，对 ASCII 等价）。
        return string.CompareOrdinal(a, b);
    }

    /// <summary>
    /// host `parse_version_tuple` 的对齐实现：解析出 (major, minor, patch)，整串无数字返回 null。
    /// </summary>
    private static (int Major, int Minor, int Patch)? TryParseVersionTuple(string? s)
    {
        if (string.IsNullOrEmpty(s)) return null;
        var digits = TrimLeadingNonDigits(s);
        if (digits.Length == 0) return null;

        var segments = digits.Split('.');
        return (SegmentNumber(segments, 0), SegmentNumber(segments, 1), SegmentNumber(segments, 2));
    }

    /// <summary>第 <paramref name="index"/> 段的连续数字前缀（无则 0；缺段 0；饱和防溢出）。</summary>
    private static int SegmentNumber(string[] segments, int index)
    {
        if (index >= segments.Length) return 0;
        var seg = segments[index];
        long n = 0;
        var i = 0;
        while (i < seg.Length && char.IsAsciiDigit(seg[i]))
        {
            n = n * 10 + (seg[i] - '0');
            if (n > int.MaxValue) return int.MaxValue;   // 饱和：超长数字段不回绕
            i++;
        }
        return (int)n;
    }

    /// <summary>跳过前导非数字（`v0.1.0` → `0.1.0`）；全是非数字时返回空串。</summary>
    public static string TrimLeadingNonDigits(string s)
    {
        if (string.IsNullOrEmpty(s)) return s ?? "";
        var i = 0;
        while (i < s.Length && !char.IsAsciiDigit(s[i])) i++;
        return i == 0 ? s : s[i..];
    }

    /// <summary>索引目标版本相对已装版本的更新方向（market 卡片按钮语义）。
    /// 未安装（installedVersion 为空）时既不是更新也不是降级——按钮文案由"已安装"决定。</summary>
    public static (bool CanUpdate, bool CanDowngrade) UpdateDirection(string? targetVersion, string? installedVersion)
    {
        if (string.IsNullOrEmpty(installedVersion)) return (false, false);
        var cmp = CompareVersion(targetVersion, installedVersion);
        return (cmp > 0, cmp < 0);
    }
}
