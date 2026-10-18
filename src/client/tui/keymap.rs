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
    /// 队列选中曲在音乐库里定位（`gd`，goto definition）。
    /// 由 App 的 `g` 前缀拦截，不经 `resolve` 单键派发。
    LocateInLibrary,
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
    /// 相对跳转（毫秒，负数快退）
    Seek(i64),
    /// 撤销上一次队列改动
    Undo,
    /// 重做被撤销的队列改动
    Redo,
    /// 把 yank 寄存器里的曲子插到队列光标后（vim p）
    PutAfter,
    /// 插到队列光标前（vim P）
    PutBefore,
    /// 标记/取消标记选中行（间隔选段；有标记时 d / y 收割）
    MarkToggle,
    /// 清空当前视图的所有标记
    MarkClear,
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

/// `[` / `]` 每次快退 / 快进的步长。
pub const SEEK_STEP_MS: i64 = 10_000;

/// 按键 -> 动作。返回 None 表示该键未绑定（忽略）。
///
/// 前缀键 `g` / `d` / `y`（gg gd gt gT / dd dj dk dG dgg / yy）由
/// `App::handle_key` 的 operator-pending 状态机拦截，这里收不到；
/// 数字键切视图，同样在 App::handle_key 里先行处理。
pub fn resolve(key: KeyEvent) -> Option<Action> {
    use Action::*;
    match (key.modifiers, key.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => Some(Quit),
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => Some(HalfDown),
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => Some(HalfUp),
        (KeyModifiers::CONTROL, KeyCode::Char('r')) => Some(Redo),
        (KeyModifiers::SHIFT, KeyCode::Char(c)) => match c {
            // 大写字母键（SHIFT+char），vim 风格的"重动作"
            'G' => Some(Bottom),
            'M' => Some(MarkClear),
            'B' => Some(Back),
            'D' => Some(RemoveCurrent),
            'C' => Some(ClearQueue),
            'S' => Some(Shuffle),
            'R' => Some(Rescan),
            'J' => Some(MoveDown),
            'K' => Some(MoveUp),
            // vim N：上一个匹配 ≈ 上一曲
            'N' => Some(Prev),
            // vim P：put 到队列光标前
            'P' => Some(PutBefore),
            _ => None,
        },
        // 不带修饰键的普通键
        (m, KeyCode::Char(c)) if m.is_empty() => match c {
            'q' => Some(Quit),
            '?' => Some(Help),
            'j' => Some(Down),
            'k' => Some(Up),
            '0' => Some(Top),
            'h' => Some(PrevTab),
            'l' => Some(NextTab),
            ' ' => Some(PlayToggle),
            'n' => Some(Next),
            's' => Some(Stop),
            // 队列 / 播放列表视图里 `d` 由 App 拦成前缀键（dd），到不了这
            'd' => Some(RemoveSelected),
            'x' => Some(RemoveCurrent),
            'a' => Some(AddSelected),
            'c' => Some(RenameSelected),
            'r' => Some(CycleRepeat),
            'z' => Some(ToggleRandom),
            '+' | '=' => Some(VolumeUp),
            '-' => Some(VolumeDown),
            '[' => Some(Seek(-SEEK_STEP_MS)),
            ']' => Some(Seek(SEEK_STEP_MS)),
            'u' => Some(Undo),
            // ranger 式间隔标记：m 逐个标，d / y 收割
            'm' => Some(MarkToggle),
            // vim p：put 到队列光标后
            'p' => Some(PutAfter),
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
    ("0 / gg / G", "跳到顶部 / 底部"),
    ("数字 1-4 / gt / gT", "切换视图（2gt = 第 2 个）"),
    ("gd", "队列选中曲在音乐库定位"),
    ("C-d / C-u", "向下 / 向上翻半页"),
    ("Enter", "播放选中项"),
    ("空格", "播放 / 暂停"),
    ("n / N", "下一曲 / 上一曲"),
    ("s", "停止"),
    ("dd", "剪切选中曲进寄存器（队列 / 播放列表）"),
    ("dj / dk / dG / dgg", "队列范围剪切：向下 / 向上 / 到底 / 到顶"),
    ("v", "visual 选段：d 剪切 / y 复制 / o 对调 / Esc 退出"),
    ("m / M", "间隔标记当前行 · 清空标记（标记后 d/y 收割）"),
    ("D / x", "删除正在播放的曲"),
    ("d / Enter / x", "在线视图：下载 / 下载并试听 / 取消下载"),
    ("yy", "复制选中曲到寄存器（队列 / 音乐库）"),
    ("p / P", "粘贴到队列光标后 / 前（dd 剪切的也能贴）"),
    ("J / K", "队列内上移 / 下移选中曲"),
    ("a", "把选中曲加入队列（音乐库 / 搜索里 Tab 同效）"),
    ("c", "重命名：当前队列 / 选中的播放列表"),
    ("C", "清空队列"),
    ("S", "打乱队列"),
    ("u / C-r", "撤销 / 重做队列改动"),
    ("r", "循环模式 关→列表→单曲"),
    ("z", "随机播放开关"),
    ("+ / -", "音量加减"),
    ("[ / ]", "快退 / 快进 10 秒"),
    ("/", "搜索：库（拼音/首字母）；在线视图搜在线"),
    ("/ 面板内", "Esc 进 normal：j/k 移动 · o 下一首 · a 入队 · gf 定位"),
    ("/ 面板内", "i 回输入 · gg/G 首/尾 · C-u 清空 · C-w 删词 · q 关"),
    (":", "命令面板（过滤，Tab 代入，↑↓ 历史）"),
    ("w", "当前队列存为播放列表"),
    ("B", "结束试听，恢复原队列"),
    ("R", "重建库索引"),
    ("gt / gT / Tab", "切换视图（2gt = 第 2 个）"),
    ("Esc", "关闭浮层"),
    ("? / q", "帮助 / 退出"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn 方括号是快退快进() {
        assert_eq!(resolve(plain('[')), Some(Action::Seek(-SEEK_STEP_MS)));
        assert_eq!(resolve(plain(']')), Some(Action::Seek(SEEK_STEP_MS)));
    }

    #[test]
    fn 撤销重做与粘贴() {
        assert_eq!(resolve(plain('u')), Some(Action::Undo));
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(resolve(ctrl_r), Some(Action::Redo));
        assert_eq!(resolve(plain('p')), Some(Action::PutAfter));
        let shift_p = KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT);
        assert_eq!(resolve(shift_p), Some(Action::PutBefore));
    }

    #[test]
    fn 大写n是上一曲_p不再是() {
        let shift_n = KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT);
        assert_eq!(resolve(shift_n), Some(Action::Prev));
        assert_eq!(resolve(plain('p')), Some(Action::PutAfter));
    }

    #[test]
    fn g_不再是单键跳顶_交给gg前缀() {
        assert_eq!(resolve(plain('g')), None);
    }
}
