//! `App` 持有的界面状态类型：视图、模式、浮层、消息。

use crate::proto::TrackInfo;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// 主界面视图。核心 tab 恒在；插件 tab 由 manifest 的 `tab` 声明注册，
/// 随插件清单动态增减（无插件注册时不出现）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tab {
    Queue,
    Library,
    Playlists,
    /// 核心面板：未注册 tab 的 UI 插件画布（demo / 面板型）+ 插件清单
    Plugins,
    /// 插件注册的 tab（标题即 manifest 的 `tab` 声明）
    Plugin(PluginTab),
}

/// 一个插件注册的 tab 位：内容与标题都来自该插件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginTab {
    /// 内容提供者（按键/数据/视图请求的寻址目标）
    pub plugin: String,
    /// tab 栏显示的标题（manifest 声明）
    pub title: String,
}

impl Tab {
    /// tab 栏的运行时形状，完全是插件清单的投影：
    /// 核心 3 个（队列/音乐库/播放列表）+ 每个注册了 tab 的 UI 插件
    /// 一项（按发现顺序）+「插件」面板（存在未注册 tab 的 UI 插件时）。
    pub fn visible(plugins: &[PluginInfo]) -> Vec<Tab> {
        let mut tabs = vec![Tab::Queue, Tab::Library, Tab::Playlists];
        for p in plugins {
            if p.ui && let Some(title) = &p.tab {
                tabs.push(Tab::Plugin(PluginTab {
                    plugin: p.name.clone(),
                    title: title.clone(),
                }));
            }
        }
        if plugins.iter().any(|p| p.ui && p.tab.is_none()) {
            tabs.push(Tab::Plugins);
        }
        tabs
    }

    pub fn title(&self) -> String {
        match self {
            Tab::Queue => "队列".into(),
            Tab::Library => "音乐库".into(),
            Tab::Playlists => "播放列表".into(),
            Tab::Plugins => "插件".into(),
            Tab::Plugin(pt) => pt.title.clone(),
        }
    }

    pub fn next(&self, visible: &[Tab]) -> Tab {
        let i = visible.iter().position(|t| t == self).unwrap_or(0);
        visible[(i + 1) % visible.len()].clone()
    }

    pub fn prev(&self, visible: &[Tab]) -> Tab {
        let i = visible.iter().position(|t| t == self).unwrap_or(0);
        let n = visible.len();
        visible[(i + n - 1) % n].clone()
    }
}

/// 输入模式的用途（vim 命令行：`:w` 存播放列表）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputPurpose {
    SavePlaylist,
    /// 重命名播放列表（携带旧名，确认时发 PlaylistRename）
    RenamePlaylist(String),
    /// 新建空播放列表（播放列表 tab 按 a）
    NewPlaylist,
    /// 插件搜索关键词（携带内容插件名；确认时发 PluginData submit）
    PluginSearch(String),
}

impl InputPurpose {
    /// 底部命令行显示的前缀。
    pub fn prefix(&self) -> &'static str {
        match self {
            InputPurpose::SavePlaylist => ":w ",
            InputPurpose::RenamePlaylist(_) => ":rename ",
            InputPurpose::NewPlaylist => "新建 ",
            InputPurpose::PluginSearch(_) => "/ ",
        }
    }
}

/// 一个插件画布在 TUI 侧的本地状态（视图缓存 + 导航），按插件名分槽。
#[derive(Debug, Default)]
pub struct PluginUi {
    /// 插件最近一次推送的视图行
    pub view: Vec<crate::proto::PluginLine>,
    /// 选中行（插件经 `rmus_set_selected` 声明的画布行号；TUI 用它铺
    /// 选中底色）
    pub selected: usize,
    /// 当前列表语义（插件经 `rmus_set_list` 声明）：顶栏计数与底栏
    /// `n/N` 只认它。旧插件不声明 = None（不显示，也不拿画布行数冒充）。
    pub list: Option<crate::proto::PluginList>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// UI 插件按键直通：除 Esc 外的按键全部转给插件（插件 tab 按 `i` 进入）
    Plugin,
    /// Telescope 式搜索面板：输入与实时结果一体（`search::handle_key` 处理）
    Search,
    /// Telescope 式命令面板（`palette::handle_key` 处理）
    Command,
    /// 候选列表：输入模糊过滤，Enter 确认（`picker::handle_key` 处理）
    Picker,
    Input(InputPurpose),
}

/// 搜索面板内的子模式：字母键要么进关键词，要么当动作（vim normal）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// 正在输入关键词（默认）
    #[default]
    Input,
    /// vim normal：j/k 移动，o/a/gf… 直接触发动作，`i` 回输入
    Normal,
}

/// 搜索面板状态；`None` = 未打开。与 [`Mode::Search`] 同生共死。
pub struct SearchState {
    pub query: String,
    pub results: Vec<TrackInfo>,
    pub selected: usize,
    /// 面板内子模式；Esc 在输入态进 normal，normal 态再 Esc 才关面板
    pub mode: SearchMode,
    /// normal 态按过 `g`：等 gg（跳顶）/ gf（音乐库定位）
    pub pending_g: bool,
}

/// 已发现插件的信息（`plugin list`）。methods 形状即协议的
/// `[{name,kind}]`，直接复用契约类型。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// UI 插件（wasm）：宿主提供按键/渲染通道
    #[serde(default)]
    pub ui: bool,
    /// 插件注册的 TUI tab 标题（manifest `tab` 声明）；None = 不占 tab 位
    #[serde(default)]
    pub tab: Option<String>,
    #[serde(default)]
    pub methods: Vec<crate::plugin::PluginMethod>,
}

impl PluginInfo {
    /// 在线 tab 约定：插件要同时提供 search 与 download。
    pub fn supports_online(&self) -> bool {
        let names: Vec<&str> = self.methods.iter().map(|m| m.name.as_str()).collect();
        names.contains(&"search") && names.contains(&"download")
    }
}

/// `:` 命令面板状态；`None` = 未打开。与 [`Mode::Command`] 同生共死。
/// 命令列表由 `cmd::filter(&input)` 现算，不存这里。
pub struct CmdPalette {
    pub input: String,
    pub selected: usize,
    /// 上次回车的解析错误（面板内展示；输入变化即清除）
    pub error: Option<String>,
}

/// 候选列表（telescope 式）：输入即模糊过滤，Enter 确认选中项。
/// 通用组件：label 展示、value 是确认时交给动作的载荷（如播放列表名）。
#[derive(Clone)]
pub struct CandidateItem {
    pub label: String,
    pub value: String,
}

#[derive(Clone)]
pub struct CandidatePicker {
    /// 浮层标题，如 "加入播放列表（3 首）"
    pub title: String,
    pub input: String,
    /// 全量候选（确认时输入若不匹配任何 label，按新名字处理）
    pub items: Vec<CandidateItem>,
    pub selected: usize,
}

impl CandidatePicker {
    /// 输入过滤后的候选下标（得分降序，同分保持原序）。
    pub fn filtered(&self) -> Vec<usize> {
        let query = self.input.trim();
        let mut scored: Vec<(i64, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| fuzzy_score(&item.label, query).map(|s| (s, i)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    /// 当前高亮的候选（过滤后越界时夹紧）。
    pub fn picked(&self) -> Option<&CandidateItem> {
        let filtered = self.filtered();
        filtered
            .get(self.selected.min(filtered.len().saturating_sub(1)))
            .and_then(|i| self.items.get(*i))
    }
}

/// 候选列表的模糊打分：完全相等 > 前缀 > 包含 > 子序列；空查询全中。
fn fuzzy_score(label: &str, query: &str) -> Option<i64> {
    let label = label.to_lowercase();
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    if label == query {
        Some(1000)
    } else if label.starts_with(&query) {
        Some(100)
    } else if label.contains(&query) {
        Some(50)
    } else if query.chars().all(|q| label.chars().any(|l| l == q)) {
        Some(10)
    } else {
        None
    }
}

/// 底部状态行的临时消息。
pub struct Message {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

impl Message {
    /// 普通提示 5 秒；错误多留一会儿，够看清原因（多数错误来自一次操作，
    /// 错过了只能靠 daemon 日志，而它可能是 /dev/null）。
    const TTL: Duration = Duration::from_secs(5);
    const ERROR_TTL: Duration = Duration::from_secs(15);

    pub fn alive(&self) -> bool {
        self.at.elapsed() < if self.error { Self::ERROR_TTL } else { Self::TTL }
    }
}

/// 协议回复里取 `tracks` 数组。
pub(super) fn parse_tracks(value: &serde_json::Value) -> Vec<TrackInfo> {
    serde_json::from_value(value.get("tracks").cloned().unwrap_or_default()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(name: &str, ui: bool, tab: Option<&str>) -> PluginInfo {
        serde_json::from_value(serde_json::json!({
            "name": name, "version": "0.0.0", "ui": ui, "tab": tab,
        }))
        .unwrap()
    }

    #[test]
    fn tab_bar_follows_plugin_registrations() {
        // 无 UI 插件：只有核心 3 个（连「插件」面板都不出现）
        let tabs = Tab::visible(&[]);
        assert_eq!(tabs.len(), 3);
        assert!(!tabs.iter().any(|t| matches!(t, Tab::Plugin(_) | Tab::Plugins)));

        // 只有注册了 tab 的插件：仍然没有「插件」面板
        let tabs = Tab::visible(&[plugin("rmus-dl", true, Some("在线"))]);
        assert_eq!(tabs.len(), 4);
        assert!(!tabs.contains(&Tab::Plugins));

        // rmus-dl（注册「在线」）+ hello（未注册）+ echo（注册「演示」）：
        // 按清单顺序插入插件 tab，标题用声明的字符串
        let plugins = vec![
            plugin("rmus-dl", true, Some("在线")),
            plugin("hello", true, None),
            plugin("echo", true, Some("演示")),
        ];
        let tabs = Tab::visible(&plugins);
        assert_eq!(tabs.len(), 6);
        assert_eq!(tabs[3], Tab::Plugin(PluginTab {
            plugin: "rmus-dl".into(), title: "在线".into(),
        }));
        assert_eq!(tabs[4], Tab::Plugin(PluginTab {
            plugin: "echo".into(), title: "演示".into(),
        }));
        assert_eq!(tabs[5], Tab::Plugins);
        // tab 标题直接来自插件声明
        assert_eq!(tabs[3].title(), "在线");
        assert_eq!(tabs[4].title(), "演示");

        // 非法形态：声明了 tab 但不是 UI 插件——不占 tab 位，
        // 也不触发「插件」面板（它不是合法的 UI 插件）
        let plugins = vec![plugin("fake", false, Some("幽灵"))];
        assert_eq!(Tab::visible(&plugins).len(), 3);
    }

    #[test]
    fn tab_cycle_skips_unregistered() {
        // 无 UI 插件时 next/prev 在核心 3 个里循环
        let tabs = Tab::visible(&[]);
        assert_eq!(tabs[0].next(&tabs), Tab::Library);
        assert_eq!(tabs[0].prev(&tabs), Tab::Playlists);
    }
}

#[cfg(test)]
mod plugin_ui_tests {
    use crate::client::tui::app::App;
    use crate::client::tui::conn::Daemon;
    use crate::proto::PluginLine;

    fn line() -> PluginLine {
        serde_json::from_value(serde_json::json!({
            "spans": [{"text": "行", "style": "base"}],
        }))
        .unwrap()
    }

    #[test]
    fn 选中行以插件声明为准_未声明沿用并夹紧() {
        let mut app = App::new(Daemon::detached());
        // 插件声明选中画布行 3
        app.apply_plugin_view("p", vec![line(); 8], Some(3), None);
        assert_eq!(app.plugin_uis["p"].selected, 3);

        // 列表语义独立于画布行号：300 集时底栏应显示 n/300
        let list = crate::proto::PluginList {
            total: 300,
            index: 42,
        };
        app.apply_plugin_view("p", vec![line(); 8], Some(3), Some(list));
        assert_eq!(app.plugin_uis["p"].list, Some(list));

        // 未声明：沿用旧值（输入态等不画列表的帧）
        app.apply_plugin_view("p", vec![line(); 8], None, None);
        assert_eq!(app.plugin_uis["p"].selected, 3);
        assert_eq!(app.plugin_uis["p"].list, Some(list));

        // 视图缩短：夹紧到末行
        app.apply_plugin_view("p", vec![line()], None, None);
        assert_eq!(app.plugin_uis["p"].selected, 0);
    }
}
