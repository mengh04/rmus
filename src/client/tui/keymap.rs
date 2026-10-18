//! 键位表：Normal 模式的所有按键在此定义（单一事实来源）。
//!
//! - 改键 / 加键：改 [`resolve`] 的 match + [`HELP`] 里的说明即可
//! - 帮助浮层直接从 [`HELP`] 生成，不会和实际键位对不上
//!
//! 输入模式与搜索面板的按键很少，直接写在 `app.rs`。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Normal 模式下用户动作的抽象；`App::execute` 负责落实。
///
/// 拓展：加变体 -> `resolve` 绑键 -> `execute` 加分支 -> 需要展示就在 `ui/` 加渲染。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Help,
    /// 切到下一个/上一个 tab
    NextTab,
    PrevTab,
    /// 直接跳到第 n 个 tab（从 1 数）
    GotoTab(usize),
    /// 移动选中项（作用于当前活动列表：搜索结果 > 队列/播放列表）
    Up,
    Down,
    Top,
    Bottom,
    HalfUp,
    HalfDown,
    /// Enter：播放选中项（队列曲目 / 播放列表 / 搜索结果）
    PlaySelected,
    /// 播放/暂停切换
    PlayToggle,
    Next,
    Prev,
    Stop,
    /// 从队列删除选中曲目
    RemoveSelected,
    /// 删除正在播放的曲目
    RemoveCurrent,
    /// 队列内把选中曲上移 / 下移一位
    MoveUp,
    MoveDown,
    /// 把选中曲目加到队列尾部（音乐库视图；不打断播放）
    AddSelected,
    /// 重命名：当前队列（队列视图）或选中的播放列表
    RenameSelected,
    /// 清空队列
    ClearQueue,
    /// 打乱队列
    Shuffle,
    /// 结束试听，恢复原队列
    Back,
    /// 循环模式 off -> all -> one
    CycleRepeat,
    /// 随机播放开关
    ToggleRandom,
    VolumeUp,
    VolumeDown,
    /// 打开搜索面板（telescope 式：实时过滤，Enter 插播）
    Search,
    /// 打开 `:` 命令面板（telescope 式：过滤、代入、执行）
    Command,
    /// 把当前队列保存为命名播放列表（输入名字）
    SavePlaylist,
    /// 全量重建库索引
    Rescan,
    /// Esc：关闭帮助/搜索结果浮层
    Close,
}

/// 按键 -> 动作。返回 None 表示该键未绑定（忽略）。
pub fn resolve(key: KeyEvent) -> Option<Action> {
    use Action::*;
    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => Some(Quit),
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => Some(HalfDown),
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => Some(HalfUp),
        (KeyModifiers::SHIFT, KeyCode::Char(c)) => match c {
            // 大写字母键（SHIFT+char），vim 风格的"重动作"
            'G' => Some(Bottom),
            'B' => Some(Back),
            'D' => Some(RemoveCurrent),
            'C' => Some(ClearQueue),
            'S' => Some(Shuffle),
            'R' => Some(Rescan),
            'J' => Some(MoveDown),
            'K' => Some(MoveUp),
            _ => None,
        },
        // 不带修饰键的普通键
        (m, KeyCode::Char(c)) if m.is_empty() => match c {
            'q' => Some(Quit),
            '?' => Some(Help),
            'j' => Some(Down),
            'k' => Some(Up),
            'g' => Some(Top),
            'h' => Some(PrevTab),
            'l' => Some(NextTab),
            ' ' => Some(PlayToggle),
            'n' => Some(Next),
            'p' => Some(Prev),
            's' => Some(Stop),
            'd' => Some(RemoveSelected),
            'x' => Some(RemoveCurrent),
            'a' => Some(AddSelected),
            'c' => Some(RenameSelected),
            'r' => Some(CycleRepeat),
            'z' => Some(ToggleRandom),
            '+' | '=' => Some(VolumeUp),
            '-' => Some(VolumeDown),
            '/' => Some(Search),
            ':' => Some(Command),
            'w' => Some(SavePlaylist),
            _ => None,
        },
        (KeyModifiers::NONE, KeyCode::Enter) => Some(PlaySelected),
        (KeyModifiers::NONE, KeyCode::Esc) => Some(Close),
        (KeyModifiers::NONE, KeyCode::Tab) => Some(NextTab),
        (KeyModifiers::NONE, KeyCode::BackTab) => Some(PrevTab),
        (KeyModifiers::NONE, KeyCode::Up) => Some(Up),
        (KeyModifiers::NONE, KeyCode::Down) => Some(Down),
        (KeyModifiers::NONE, KeyCode::PageUp) => Some(HalfUp),
        (KeyModifiers::NONE, KeyCode::PageDown) => Some(HalfDown),
        (KeyModifiers::NONE, KeyCode::Home) => Some(Top),
        (KeyModifiers::NONE, KeyCode::End) => Some(Bottom),
        _ => None,
    }
}

/// 数字键 1..=9 -> tab 下标；单独拆出来避免和普通字符键混在一起。
pub fn resolve_digit(c: char, tab_count: usize) -> Option<Action> {
    let n = c.to_digit(10)? as usize;
    (n >= 1 && n <= tab_count).then_some(Action::GotoTab(n - 1))
}

/// 帮助浮层内容：(按键, 说明)。新加键位记得同步这里。
pub const HELP: &[(&str, &str)] = &[
    ("j / k / ↑↓", "上下移动"),
    ("g / G", "跳到顶部 / 底部"),
    ("C-d / C-u", "向下 / 向上翻半页"),
    ("Enter", "播放选中项"),
    ("空格", "播放 / 暂停"),
    ("n / p", "下一曲 / 上一曲"),
    ("s", "停止"),
    ("d / D / x", "删除：选中曲 / 正在播放的曲"),
    ("J / K", "队列内上移 / 下移选中曲"),
    ("a", "把选中曲加入队列（音乐库 / 搜索里 Tab 同效）"),
    ("c", "重命名：当前队列 / 选中的播放列表"),
    ("C", "清空队列"),
    ("S", "打乱队列"),
    ("r", "循环模式 关→列表→单曲"),
    ("z", "随机播放开关"),
    ("+ / -", "音量加减"),
    ("/", "搜索库（拼音/首字母，实时过滤）"),
    (":", "命令面板（过滤，Tab 代入，↑↓ 历史）"),
    ("w", "当前队列存为播放列表"),
    ("B", "结束试听，恢复原队列"),
    ("R", "重建库索引"),
    ("Tab / 数字", "切换 队列/音乐库/播放列表"),
    ("Esc", "关闭浮层"),
    ("? / q", "帮助 / 退出"),
];
