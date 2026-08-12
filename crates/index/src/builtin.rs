//! 内置系统命令（学习 utools：命令属核心而非插件）。
//!
//! 纯数据 + 匹配逻辑，不做任何 Win32 调用（执行在 host 的 builtins 模块）。
//! 搜索支持中文名 / 别名 / 拼音缩写（如 `jt` → 截图）；不可逆操作带确认文案，
//! host 首次收到 invoke 时返回 Confirm，UI 弹窗确认后再以 `confirm` action 执行。

use spark_core::{Action, Candidate, Source};

/// 一条内置命令的静态描述。
pub struct BuiltinSpec {
    /// 候选 id，形如 `builtin.lock`（host invoke 路由与 UI 回传都靠它）。
    pub id: &'static str,
    pub title: &'static str,
    pub subtitle: &'static str,
    /// 别名 / 拼音缩写，前缀匹配。
    pub aliases: &'static [&'static str],
    /// 不可逆操作：非空时回车不直接执行，先弹确认框（文案即确认框内容）。
    pub confirm: Option<&'static str>,
    /// 图标来源：System32 下 exe/msc/cpl 的绝对路径（UI 提取文件图标）；
    /// None 时回退 Fluent 字形 / 首字母。stock 图标类命令（回收站等）由 UI 侧映射。
    pub icon: Option<&'static str>,
}

const ALL: &[BuiltinSpec] = &[
    // ===== 电源与会话 =====
    BuiltinSpec {
        id: "builtin.lock",
        title: "锁屏",
        subtitle: "锁定当前会话",
        aliases: &["锁定", "lock", "sd"],
        confirm: None,
        icon: None, // UI 侧 SIID_LOCK
    },
    BuiltinSpec {
        id: "builtin.shutdown",
        title: "关机",
        subtitle: "关闭计算机",
        aliases: &["关闭计算机", "shutdown", "poweroff", "gj"],
        confirm: Some("确认关机？"),
        icon: None,
    },
    BuiltinSpec {
        id: "builtin.reboot",
        title: "重启",
        subtitle: "重新启动计算机",
        aliases: &["重新启动", "restart", "cq"],
        confirm: Some("确认重启？"),
        icon: None,
    },
    BuiltinSpec {
        id: "builtin.logoff",
        title: "注销",
        subtitle: "注销当前用户",
        aliases: &["退出登录", "logout", "zx"],
        confirm: Some("确认注销当前用户？"),
        icon: None,
    },
    BuiltinSpec {
        id: "builtin.sleep",
        title: "睡眠",
        subtitle: "让计算机进入睡眠状态",
        aliases: &["待机", "sleep", "sm"],
        confirm: None,
        icon: None,
    },
    // ===== 回收站 =====
    BuiltinSpec {
        id: "builtin.empty_recycle_bin",
        title: "清空回收站",
        subtitle: "永久删除回收站中的所有项目",
        aliases: &["recycle", "qkhsz"],
        confirm: Some("确认清空回收站？回收站中的文件将被永久删除。"),
        icon: None, // UI 侧 SIID_RECYCLERFULL
    },
    BuiltinSpec {
        id: "builtin.recycle_bin",
        title: "回收站",
        subtitle: "打开回收站",
        aliases: &["打开回收站", "recycle bin", "trash", "hsz"],
        confirm: None,
        icon: None, // UI 侧 SIID_RECYCLER
    },
    // ===== 系统工具 =====
    BuiltinSpec {
        id: "builtin.screenshot",
        title: "截图",
        subtitle: "打开系统截图工具",
        aliases: &["屏幕截图", "截屏", "screenshot", "jt"],
        confirm: None,
        icon: None, // UI 侧探测 snippingtool
    },
    BuiltinSpec {
        id: "builtin.settings",
        title: "设置",
        subtitle: "打开 Windows 设置",
        aliases: &["系统设置", "windows 设置", "sz"],
        confirm: None,
        icon: None, // UI 侧提取 Win11 设置图标（ImmersiveControlPanel\SystemSettings.exe）
    },
    BuiltinSpec {
        id: "builtin.explorer",
        title: "文件资源管理",
        subtitle: "打开文件资源管理器",
        aliases: &["资源管理器", "文件管理器", "explorer"],
        confirm: None,
        icon: None, // UI 侧 explorer.exe
    },
    BuiltinSpec {
        id: "builtin.remote_desktop",
        title: "远程桌面连接",
        subtitle: "打开远程桌面连接（mstsc）",
        aliases: &["远程桌面", "mstsc", "rdp", "yc"],
        confirm: None,
        icon: None, // UI 侧 mstsc.exe
    },
    BuiltinSpec {
        id: "builtin.regedit",
        title: "注册表编辑器",
        subtitle: "系统注册表编辑工具",
        aliases: &["注册表", "regedit", "zcbj"],
        confirm: None,
        icon: Some(r"C:\Windows\regedit.exe"), // Win11 起位于系统根目录
    },
    BuiltinSpec {
        id: "builtin.msinfo",
        title: "系统信息",
        subtitle: "查看系统硬件与软件信息",
        aliases: &["msinfo32", "xtxx"],
        confirm: None,
        icon: Some(r"C:\Windows\System32\msinfo32.exe"),
    },
    BuiltinSpec {
        id: "builtin.sysprops",
        title: "系统属性",
        subtitle: "查看系统版本与高级设置",
        aliases: &["高级系统设置", "sysdm", "xtsx"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.env_vars",
        title: "环境变量",
        subtitle: "编辑系统与用户环境变量",
        aliases: &["系统环境变量", "用户环境变量", "env", "hjbl"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.device_manager",
        title: "设备管理器",
        subtitle: "查看和管理硬件设备",
        aliases: &["设备管理", "devmgmt", "sbgl"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.disk_management",
        title: "磁盘管理",
        subtitle: "分区与磁盘管理",
        aliases: &["diskmgmt", "cpg"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.computer_management",
        title: "计算机管理",
        subtitle: "系统管理工具集合",
        aliases: &["compmgmt", "jsjgl"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.services",
        title: "服务",
        subtitle: "管理系统服务",
        aliases: &["服务管理", "services", "fwgl"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.event_viewer",
        title: "事件查看器",
        subtitle: "查看系统与应用日志",
        aliases: &["事件查看", "eventvwr", "sjckq"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.task_manager",
        title: "任务管理器",
        subtitle: "查看进程与性能",
        aliases: &["taskmgr", "rwgl"],
        confirm: None,
        icon: Some(r"C:\Windows\System32\Taskmgr.exe"),
    },
    BuiltinSpec {
        id: "builtin.task_scheduler",
        title: "任务计划程序",
        subtitle: "创建和管理计划任务",
        aliases: &["taskschd", "rwjh"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.resource_monitor",
        title: "资源监视器",
        subtitle: "实时监控资源占用",
        aliases: &["resmon", "zyjsq"],
        confirm: None,
        icon: Some(r"C:\Windows\System32\resmon.exe"),
    },
    BuiltinSpec {
        id: "builtin.performance_monitor",
        title: "性能监视器",
        subtitle: "性能计数器与日志",
        aliases: &["perfmon", "xnjs"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.secpol",
        title: "本地安全策略",
        subtitle: "配置本地安全策略",
        aliases: &["secpol", "bdaq"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.gpedit",
        title: "组策略编辑器",
        subtitle: "编辑组策略（专业版及以上）",
        aliases: &["组策略", "gpedit"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.msconfig",
        title: "系统配置",
        subtitle: "启动项与系统引导配置",
        aliases: &["msconfig", "xtpz"],
        confirm: None,
        icon: Some(r"C:\Windows\System32\MSConfig.exe"),
    },
    BuiltinSpec {
        id: "builtin.shared_folders",
        title: "共享文件夹",
        subtitle: "管理共享与当前会话",
        aliases: &["fsmgmt", "gxwjj"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.users_groups",
        title: "本地用户和组",
        subtitle: "管理用户账户与组",
        aliases: &["lusrmgr", "bdyhhz"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    // ===== 控制面板 / 设置页 =====
    BuiltinSpec {
        id: "builtin.control_panel",
        title: "控制面板",
        subtitle: "经典系统设置入口",
        aliases: &["control", "kzmb"],
        confirm: None,
        icon: Some(r"C:\Windows\System32\control.exe"),
    },
    BuiltinSpec {
        id: "builtin.programs_features",
        title: "程序和功能",
        subtitle: "卸载或更改程序",
        aliases: &["卸载程序", "appwiz"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.network_connections",
        title: "网络连接",
        subtitle: "管理网络适配器",
        aliases: &["ncpa", "wllj"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.sound",
        title: "声音设置",
        subtitle: "扬声器与录音设备",
        aliases: &["声音", "mmsys"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.power_options",
        title: "电源选项",
        subtitle: "电源计划与睡眠设置",
        aliases: &["powercfg", "dyxx"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.display_settings",
        title: "显示设置",
        subtitle: "分辨率与显示器设置",
        aliases: &["分辨率", "显示器", "xssz"],
        confirm: None,
        icon: Some(r"C:\Windows\ImmersiveControlPanel\SystemSettings.exe"),
    },
    BuiltinSpec {
        id: "builtin.date_time",
        title: "日期和时间",
        subtitle: "系统日期、时间与时区",
        aliases: &["timedate", "rqhsj"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.mouse",
        title: "鼠标",
        subtitle: "鼠标指针与按键设置",
        aliases: &["鼠标设置", "main"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.region",
        title: "区域设置",
        subtitle: "格式、语言与位置",
        aliases: &["区域", "intl"],
        confirm: None,
        icon: None, // 字形绘制（BuiltinIcon）
    },
    BuiltinSpec {
        id: "builtin.fonts",
        title: "字体",
        subtitle: "查看和管理系统字体",
        aliases: &["fonts", "zt"],
        confirm: None,
        icon: None,
    },
    // ===== 常用应用 =====
    BuiltinSpec {
        id: "builtin.cmd",
        title: "命令提示符",
        subtitle: "Windows 命令行",
        aliases: &["cmd", "命令行"],
        confirm: None,
        icon: Some(r"C:\Windows\System32\cmd.exe"),
    },
    BuiltinSpec {
        id: "builtin.powershell",
        title: "Windows PowerShell",
        subtitle: "命令行脚本环境",
        aliases: &["powershell", "ps"],
        confirm: None,
        icon: Some(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
    },
    BuiltinSpec {
        id: "builtin.calc",
        title: "计算器",
        subtitle: "标准与科学计算",
        aliases: &["calc", "jsq"],
        confirm: None,
        icon: None, // Win11 商店版，UI 侧按 WindowsApps 探测
    },
    BuiltinSpec {
        id: "builtin.notepad",
        title: "记事本",
        subtitle: "轻量文本编辑器",
        aliases: &["notepad", "jsb"],
        confirm: None,
        icon: Some(r"C:\Windows\System32\notepad.exe"),
    },
    BuiltinSpec {
        id: "builtin.paint",
        title: "画图",
        subtitle: "简单图像绘制工具",
        aliases: &["mspaint", "paint"],
        confirm: None,
        icon: None, // Win11 商店版，UI 侧按 WindowsApps 前缀探测
    },
    BuiltinSpec {
        id: "builtin.magnifier",
        title: "放大镜",
        subtitle: "屏幕放大工具",
        aliases: &["magnify", "fdj"],
        confirm: None,
        icon: Some(r"C:\Windows\System32\Magnify.exe"),
    },
    BuiltinSpec {
        id: "builtin.on_screen_keyboard",
        title: "屏幕键盘",
        subtitle: "虚拟键盘输入",
        aliases: &["osk", "pmjp"],
        confirm: None,
        icon: Some(r"C:\Windows\System32\osk.exe"),
    },
    // ===== 网络信息 =====
    BuiltinSpec {
        id: "builtin.lan_ip",
        title: "内网IP",
        subtitle: "查看本机局域网 IPv4 地址（回车复制）",
        aliases: &["局域网ip", "本地ip", "内网地址", "ip"],
        confirm: None,
        icon: None,
    },
    BuiltinSpec {
        id: "builtin.public_ip",
        title: "公网IP",
        subtitle: "查看本机公网出口 IPv4 地址（回车复制）",
        aliases: &["外网ip", "出口ip", "wan ip", "外网地址", "gwip"],
        confirm: None,
        icon: None,
    },
];

pub fn all() -> &'static [BuiltinSpec] {
    ALL
}

/// 按候选 id 查命令（host invoke 路由用）。
pub fn find(id: &str) -> Option<&'static BuiltinSpec> {
    ALL.iter().find(|s| s.id == id)
}

/// 给 UI 设置页展示的命令清单（wire 结构，host.get_builtins 返回）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct BuiltinInfo {
    pub id: &'static str,
    pub title: &'static str,
    pub subtitle: &'static str,
    pub aliases: &'static [&'static str],
    /// 是否不可逆操作（执行前会弹确认框）
    pub confirm: bool,
    /// 图标来源路径（UI 提取用；None 时 UI 回退字形）
    pub icon: Option<&'static str>,
}

pub fn infos() -> Vec<BuiltinInfo> {
    ALL.iter()
        .map(|s| BuiltinInfo {
            id: s.id,
            title: s.title,
            subtitle: s.subtitle,
            aliases: s.aliases,
            confirm: s.confirm.is_some(),
            icon: s.icon,
        })
        .collect()
}

/// 非空查询下的 builtin 命中：标题包含或别名前缀匹配。
pub fn candidates(q: &str) -> Vec<Candidate> {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<Candidate> = Vec::new();
    for spec in ALL {
        let title = spec.title.to_lowercase();
        let matched = title.contains(&q)
            || spec
                .aliases
                .iter()
                .any(|a| a.to_lowercase().starts_with(&q));
        if !matched {
            continue;
        }
        let mut c = Candidate {
            id: spec.id.into(),
            title: spec.title.into(),
            subtitle: Some(spec.subtitle.into()),
            target: None,
            icon: spec.icon.map(|p| p.to_string()),
            score: 0.9,
            source: Source::Builtin,
            actions: vec![Action::open_default()],
            plugin_id: None,
        };
        // 打分风格对齐 memory.rs：精确 > 前缀 > 包含 > 别名
        if title == q {
            c.score += 0.35;
        } else if title.starts_with(&q) {
            c.score += 0.25;
        } else if title.contains(&q) {
            c.score += 0.12;
        } else {
            c.score += 0.08;
        }
        hits.push(c);
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(q: &str) -> Vec<String> {
        candidates(q).into_iter().map(|c| c.id).collect()
    }

    #[test]
    fn matches_chinese_title() {
        assert!(ids("截图").contains(&"builtin.screenshot".to_string()));
        assert!(ids("锁").contains(&"builtin.lock".to_string()));
    }

    #[test]
    fn matches_alias_and_pinyin_abbrev() {
        // 拼音缩写
        assert!(ids("jt").contains(&"builtin.screenshot".to_string()));
        // 中文别名
        assert!(ids("回收站").contains(&"builtin.empty_recycle_bin".to_string()));
        // 英文别名前缀
        assert!(ids("rest").contains(&"builtin.reboot".to_string()));
        // shut / shutdown 都能命中关机（shutdown 别名前缀匹配）
        assert!(ids("shut").contains(&"builtin.shutdown".to_string()));
        assert!(ids("shutdown").contains(&"builtin.shutdown".to_string()));
        // 远程桌面：中文别名与 mstsc
        assert!(ids("远程桌面").contains(&"builtin.remote_desktop".to_string()));
        assert!(ids("mstsc").contains(&"builtin.remote_desktop".to_string()));
        // 回收站与清空回收站是两个独立命令，都可通过"回收站"搜到
        assert!(ids("回收站").contains(&"builtin.recycle_bin".to_string()));
        assert!(ids("清空回收站").contains(&"builtin.empty_recycle_bin".to_string()));
        // 新增系统工具：注册表 / 服务 / 计算器
        assert!(ids("注册表").contains(&"builtin.regedit".to_string()));
        assert!(ids("服务").contains(&"builtin.services".to_string()));
        assert!(ids("calc").contains(&"builtin.calc".to_string()));
    }

    #[test]
    fn empty_query_returns_nothing() {
        assert!(candidates("").is_empty());
        assert!(candidates("  ").is_empty());
    }

    #[test]
    fn ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for spec in all() {
            assert!(seen.insert(spec.id), "duplicate builtin id: {}", spec.id);
        }
    }

    #[test]
    fn destructive_commands_have_confirm_text() {
        for id in [
            "builtin.shutdown",
            "builtin.reboot",
            "builtin.logoff",
            "builtin.empty_recycle_bin",
        ] {
            assert!(find(id).unwrap().confirm.is_some(), "{id} should confirm");
        }
        assert!(find("builtin.lock").unwrap().confirm.is_none());
    }

    #[test]
    fn builtin_candidates_are_launchable_shaped() {
        let hits = candidates("关机");
        assert_eq!(hits.len(), 1);
        let c = &hits[0];
        assert_eq!(c.source, Source::Builtin);
        assert!(c.target.is_none());
        assert_eq!(c.actions.len(), 1);
        assert!(c.actions[0].is_default);
    }

    #[test]
    fn system_tool_candidates_carry_icon() {
        // 有图标路径的命令：候选 icon 应带上（UI 提取文件图标用）
        let regedit = candidates("注册表")
            .into_iter()
            .find(|c| c.id == "builtin.regedit");
        assert!(regedit.is_some());
        assert!(regedit.unwrap().icon.unwrap().ends_with("regedit.exe"));
    }

    #[test]
    fn infos_mirror_command_table() {
        let infos = infos();
        assert_eq!(infos.len(), all().len(), "清单与命令表一一对应");
        // 不可逆操作标记
        let shutdown = infos.iter().find(|i| i.id == "builtin.shutdown").unwrap();
        assert!(shutdown.confirm);
        assert!(shutdown.aliases.contains(&"gj"));
        let lock = infos.iter().find(|i| i.id == "builtin.lock").unwrap();
        assert!(!lock.confirm);
        // serde 可序列化（wire 结构）
        let json = serde_json::to_value(&infos).unwrap();
        assert_eq!(json.as_array().unwrap().len(), infos.len());
    }
}
