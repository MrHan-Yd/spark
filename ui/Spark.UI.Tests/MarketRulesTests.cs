using Spark.UI.Services;
using Xunit;

namespace Spark.UI.Tests;

/// <summary>
/// 市场分类/标签纯规则（<see cref="MarketRules"/>）。这些规则直接决定三方 registry.json
/// 数据如何进入 UI，属于"宁可测过头也别漏"的安全边界。
/// </summary>
public class MarketRulesTagTests
{
    [Fact]
    public void NormalizeTags_null_returns_empty()
    {
        var tags = MarketRules.NormalizeTags(null);
        Assert.NotNull(tags);
        Assert.Empty(tags);
    }

    [Fact]
    public void NormalizeTags_trims_and_keeps_valid_tags()
    {
        var tags = MarketRules.NormalizeTags(new List<string?> { "  开发工具  ", "文本处理", null, "" });
        Assert.Equal(new[] { "开发工具", "文本处理" }, tags);
    }

    [Fact]
    public void NormalizeTags_drops_overlong_tag_instead_of_truncating()
    {
        // 截断会把两个不同的长标签截成同一个组名，所以必须整条丢弃。
        var overlong = new string('长', MarketRules.MaxTagLength + 1);
        var tags = MarketRules.NormalizeTags(new List<string?> { overlong, "ok" });
        Assert.Equal(new[] { "ok" }, tags);
    }

    [Fact]
    public void NormalizeTags_exactly_max_length_is_accepted()
    {
        var tag = new string('长', MarketRules.MaxTagLength);
        Assert.Equal(new[] { tag }, MarketRules.NormalizeTags(new List<string?> { tag }));
    }

    [Fact]
    public void NormalizeTags_drops_control_characters()
    {
        // 控制字符/换行会破坏分组头与筛选按钮排版，且常出现在注入尝试里。
        var tags = MarketRules.NormalizeTags(new List<string?>
        {
            "a\u0007b",   // BEL
            "a\nb",       // 换行
            "a\tb",       // 制表（非空白类控制字符，trim 去不掉中间的）
            "ok",
        });
        Assert.Equal(new[] { "ok" }, tags);
    }

    [Fact]
    public void NormalizeTags_dedupes_case_insensitively_but_keeps_first_spelling()
    {
        var tags = MarketRules.NormalizeTags(new List<string?> { "Tools", "TOOLS", "tools", "其它" });
        Assert.Equal(new[] { "Tools", "其它" }, tags);
    }

    [Fact]
    public void NormalizeTags_caps_tag_count_per_plugin()
    {
        var raw = Enumerable.Range(1, 10).Select(i => $"标签{i}").ToList();
        var tags = MarketRules.NormalizeTags(raw);
        Assert.Equal(MarketRules.MaxTagsPerPlugin, tags.Count);
        // 保留的是先出现的那几个（不是随机的几个）。
        Assert.Equal(raw.Take(MarketRules.MaxTagsPerPlugin), tags);
    }

    [Fact]
    public void NormalizeTags_all_invalid_yields_empty()
    {
        var tags = MarketRules.NormalizeTags(new List<string?> { "", "   ", "\u0001", new string('长', 99) });
        Assert.Empty(tags);
    }

    [Fact]
    public void IsTagAcceptable_trims_are_valid_but_blank_is_not()
    {
        Assert.True(MarketRules.IsTagAcceptable(" 开发工具 "));
        Assert.False(MarketRules.IsTagAcceptable("   "));
        Assert.False(MarketRules.IsTagAcceptable(""));
        Assert.False(MarketRules.IsTagAcceptable(null));
    }

    [Fact]
    public void PrimaryTag_first_tag_wins_and_falls_back_to_untagged()
    {
        Assert.Equal("开发工具", MarketRules.PrimaryTag(new List<string> { "开发工具", "文本处理" }));
        Assert.Equal(MarketRules.UntaggedGroup, MarketRules.PrimaryTag(new List<string>()));
        Assert.Equal(MarketRules.UntaggedGroup, MarketRules.PrimaryTag(null));
    }

    [Fact]
    public void TagsSummary_ellipsizes_after_max()
    {
        var tags = new List<string> { "一", "二", "三", "四" };
        Assert.Equal("一 · 二 · 三 …", MarketRules.TagsSummary(tags));
        // max 小于总数时同样带省略号——省略号表达"还有没显示的"，与 max 取几无关。
        Assert.Equal("一 · 二 …", MarketRules.TagsSummary(tags, 2));
        Assert.Equal("一 · 二 · 三 · 四", MarketRules.TagsSummary(tags, 4));
        Assert.Equal("", MarketRules.TagsSummary(new List<string>()));
        Assert.Equal("", MarketRules.TagsSummary(null));
    }

    [Fact]
    public void MatchesTag_matches_any_of_the_plugins_tags()
    {
        var tags = new List<string> { "开发工具", "文本处理" };
        Assert.True(MarketRules.MatchesTag(tags, "开发工具"));
        // 任一标签命中即可：这是多标签插件能被多个分类检索到的前提。
        Assert.True(MarketRules.MatchesTag(tags, "文本处理"));
        Assert.True(MarketRules.MatchesTag(tags, "文本处理".ToUpperInvariant()));
        Assert.False(MarketRules.MatchesTag(tags, "游戏"));
        // null = 不按分类过滤。
        Assert.True(MarketRules.MatchesTag(tags, null));
        Assert.False(MarketRules.MatchesTag(null, "开发工具"));
    }
}

/// <summary>分类归组顺序（市场列表组头行的排布）。</summary>
public class MarketRulesGroupTests
{
    [Fact]
    public void GroupOrder_orders_by_count_then_name_and_sinks_untagged()
    {
        var order = MarketRules.GroupOrder(new List<string>
        {
            "文本处理", "开发工具", "开发工具", "文本处理", "开发工具", "未分类",
        });

        // 开发工具(3) > 文本处理(2)，未分类(1) 恒排最后。
        Assert.Equal(new[] { "开发工具", "文本处理", "未分类" }, order);
    }

    [Fact]
    public void GroupOrder_ties_break_by_name()
    {
        var order = MarketRules.GroupOrder(new List<string> { "效率", "办公", "效率", "办公" });
        Assert.Equal(new[] { "办公", "效率" }, order);
    }

    [Fact]
    public void GroupOrder_untagged_sinks_even_when_it_is_the_most_common()
    {
        var order = MarketRules.GroupOrder(new List<string>
        {
            "未分类", "未分类", "未分类", "工具",
        });
        Assert.Equal(new[] { "工具", "未分类" }, order);
    }

    [Fact]
    public void GroupOrder_is_case_insensitive_on_group_keys_but_stable_for_display()
    {
        // "tools" 与 "Tools" 是同一组（去重按忽略大小写），展示名保留先出现的那个。
        var order = MarketRules.GroupOrder(new List<string> { "tools", "Tools", "tools" });
        Assert.Single(order);
        Assert.Equal("tools", order[0]);
    }

    [Fact]
    public void GroupOrder_empty_and_null()
    {
        Assert.Empty(MarketRules.GroupOrder(new List<string>()));
        Assert.Empty(MarketRules.GroupOrder(null));
    }

    [Fact]
    public void IsUntagged_uses_the_constant_not_a_literal()
    {
        Assert.True(MarketRules.IsUntagged("未分类"));
        Assert.False(MarketRules.IsUntagged("工具"));
        Assert.False(MarketRules.IsUntagged(null));
    }
}

/// <summary>索引条目容错（规范 §9.5：跳过不完整的插件条目，其余照常展示）。</summary>
public class MarketRulesToleranceTests
{
    [Fact]
    public void Entry_without_id_or_latest_is_skipped()
    {
        Assert.False(MarketRules.IsPluginEntryUsable(null, "0.1.0"));
        Assert.False(MarketRules.IsPluginEntryUsable("", "0.1.0"));
        Assert.False(MarketRules.IsPluginEntryUsable("   ", "0.1.0"));
        Assert.False(MarketRules.IsPluginEntryUsable("com.spark.x", null));
        Assert.False(MarketRules.IsPluginEntryUsable("com.spark.x", ""));
        // 双缺失也拒绝（不是"有一个就行"）。
        Assert.False(MarketRules.IsPluginEntryUsable(null, null));
    }

    [Fact]
    public void Complete_entry_is_kept()
    {
        Assert.True(MarketRules.IsPluginEntryUsable("com.spark.hello", "0.1.0"));
    }

    [Theory]
    [InlineData(1, true)]
    [InlineData(2, false)]
    [InlineData(0, false)]
    [InlineData(-1, false)]
    public void Only_schema_one_is_supported(int schema, bool expected)
    {
        Assert.Equal(expected, MarketRules.IsSchemaSupported(schema));
    }
}

/// <summary>版本比较（market 的"可更新/可降级"判定，须与 host 的 cmp_version 语义一致）。</summary>
public class MarketRulesVersionTests
{
    [Theory]
    [InlineData("0.2.0", "0.1.0", 1)]
    [InlineData("0.1.0", "0.2.0", -1)]
    [InlineData("1.0.0", "1.0.0", 0)]
    public void Compares_semver_segments_numerically(string a, string b, int expectedSign)
    {
        Assert.Equal(expectedSign, Math.Sign(MarketRules.CompareVersion(a, b)));
    }

    [Fact]
    public void Missing_segments_are_padded_with_zero()
    {
        Assert.Equal(0, MarketRules.CompareVersion("1.0", "1.0.0"));
        Assert.True(MarketRules.CompareVersion("2.0", "2.0.1") < 0);
        Assert.True(MarketRules.CompareVersion("1.0", "0.9") > 0);
    }

    [Fact]
    public void Leading_v_prefix_is_ignored()
    {
        Assert.Equal(0, MarketRules.CompareVersion("v0.1.0", "0.1.0"));
        Assert.True(MarketRules.CompareVersion("v1.2.3", "v1.2.4") < 0);
        Assert.Equal(0, MarketRules.CompareVersion("V1.0.0", "1.0.0"));
    }

    [Fact]
    public void Trailing_non_digits_are_ignored_like_host()
    {
        // host 端 cmp_version 对 "0.1.0beta" 与 "0.1.0" 判等，这里保持一致。
        Assert.Equal(0, MarketRules.CompareVersion("0.1.0beta", "0.1.0"));
    }

    [Fact]
    public void Non_numeric_segment_falls_back_to_string_compare()
    {
        Assert.True(MarketRules.CompareVersion("alpha", "beta") < 0);
        Assert.True(MarketRules.CompareVersion("0.0.1", "nope") > 0);
        Assert.True(MarketRules.CompareVersion("nope", "0.0.1") < 0);
    }

    [Fact]
    public void Null_or_empty_is_the_smallest_value()
    {
        Assert.True(MarketRules.CompareVersion(null, "0.0.1") < 0);
        Assert.True(MarketRules.CompareVersion("", "0.0.1") < 0);
        Assert.True(MarketRules.CompareVersion("0.0.1", null) > 0);
        Assert.Equal(0, MarketRules.CompareVersion(null, null));
        Assert.Equal(0, MarketRules.CompareVersion("", ""));
    }

    [Fact]
    public void UpdateDirection_drives_the_card_button()
    {
        // 未安装：既不是更新也不是降级（按钮文案由 IsInstalled 决定）。
        Assert.Equal((false, false), MarketRules.UpdateDirection("0.2.0", null));
        Assert.Equal((false, false), MarketRules.UpdateDirection("0.2.0", ""));

        Assert.Equal((true, false), MarketRules.UpdateDirection("0.2.0", "0.1.0"));
        Assert.Equal((false, true), MarketRules.UpdateDirection("0.1.0", "0.2.0"));
        Assert.Equal((false, false), MarketRules.UpdateDirection("0.1.0", "0.1.0"));
    }

    // —— 以下为与 host cmp_version 逐条对齐的语义（这批断言最初是失败的，说明 C# 实现此前
    //    与 host 不一致，市场会把 `0.1.0beta`/四段版本号误判成"可更新"）——

    [Fact]
    public void Trailing_junk_in_a_segment_is_ignored_like_host()
    {
        // host: cmp_version("0.1.0beta", "0.1.0") == Equal（段只取连续数字前缀）。
        Assert.Equal(0, MarketRules.CompareVersion("0.1.0beta", "0.1.0"));
        Assert.True(MarketRules.CompareVersion("0.1.1beta", "0.1.0") > 0);
        Assert.True(MarketRules.CompareVersion("0.1.0beta", "0.1.1") < 0);
    }

    [Fact]
    public void Fourth_and_later_segments_are_ignored_like_host()
    {
        // host 只取 major/minor/patch 三段。
        Assert.Equal(0, MarketRules.CompareVersion("1.0.0.1", "1.0.0"));
        Assert.Equal(0, MarketRules.CompareVersion("1.0.0.99", "1.0.0"));
        Assert.True(MarketRules.CompareVersion("1.0.0.1", "1.0.1") < 0);
    }

    [Fact]
    public void Empty_segments_count_as_zero()
    {
        // "0..1" 的 minor 段为空 → 0，与 host 的 `num.parse::<u32>().unwrap_or(0)` 一致。
        Assert.Equal(0, MarketRules.CompareVersion("0..1", "0.0.1"));
        Assert.True(MarketRules.CompareVersion("0..1", "0.1.0") < 0);
    }

    [Fact]
    public void Huge_numeric_segments_saturate_instead_of_wrapping()
    {
        // host 用 u32 解析、超界当 0；C# 这边选择饱和到 int.MaxValue（保持单调），
        // 这是有意为之的唯一偏差，已写进实现注释。
        Assert.True(MarketRules.CompareVersion("99999999999999.0.0", "1.0.0") > 0);
        Assert.True(MarketRules.CompareVersion("1.0.0", "99999999999999.0.0") < 0);
    }

    [Fact]
    public void Non_numeric_versions_fall_back_to_ordinal_compare()
    {
        // host: 双方都解析不出数字段 → 字符串比较保证全序。
        Assert.True(MarketRules.CompareVersion("alpha", "beta") < 0);
        Assert.True(MarketRules.CompareVersion("beta", "alpha") > 0);
        Assert.Equal(0, MarketRules.CompareVersion("alpha", "alpha"));
        // 一方能解析就大：数字段优先于纯文本。
        Assert.True(MarketRules.CompareVersion("0.0.1", "nope") > 0);
        Assert.True(MarketRules.CompareVersion("nope", "0.0.1") < 0);
        Assert.True(MarketRules.CompareVersion("v1.0", "beta") > 0);
    }

    [Fact]
    public void Null_or_empty_version_parses_like_host()
    {
        // host: "" 的 trim 后无数字 → None；双方 None → 字符串比较（相等）。
        Assert.Equal(0, MarketRules.CompareVersion(null, null));
        Assert.Equal(0, MarketRules.CompareVersion("", ""));
        // 一方 None → 另一方大（不再走"空串最小"的旧特例，与 host 一致）。
        Assert.True(MarketRules.CompareVersion("0.0.1", null) > 0);
        Assert.True(MarketRules.CompareVersion(null, "0.0.1") < 0);
        Assert.True(MarketRules.CompareVersion("nope", null) > 0, "nope 与 null 都无数字 → 序数比较");
    }
}
