//! UI 插件示例：组件表（component kit）的完整演示。
//!
//! 三项演示列表（`Up/Down/j/k` 导航，`Enter` 确认）+ 进度条（空格步进）
//! + 标题/键位提示行——组件全部来自 `rmus_plugin_sdk::ui`。

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use rmus_plugin_sdk::{declare_plugin_manifest, event, print_line, ui, ui::style};

declare_plugin_manifest! {
    name: "hello", version: "0.3.0", ui: true,
}

static SELECTED: AtomicU32 = AtomicU32::new(0);
static DOWNLOADED: AtomicU64 = AtomicU64::new(0);

const DEMO_ITEMS: [&str; 4] = ["群青 - YOASOBI", "晴天 - 周杰伦", "晚风 - 王菲", "Amsterdam - 冷玩"];

fn draw(cols: i32) {
    let selected = SELECTED.load(Ordering::Relaxed) as usize;
    let downloaded = DOWNLOADED.load(Ordering::Relaxed);
    let width = cols.max(20) as usize;

    // 标题行：组件 header
    ui::header("hello 组件演示", "组件表 v0");
    ui::set_selected(1 + selected);

    // 列表：组件 list_row_custom（前缀 = 序号）
    for (i, item) in DEMO_ITEMS.iter().enumerate() {
        ui::list_row_custom(&format!("{}. ", i + 1), item, i == selected);
    }

    // 进度条：组件 progress（空格步进 25%）
    ui::progress(downloaded, 100, "下载", width);

    // 富文本行：手动拼 spans（样式令牌 → 主题色）
    ui::print_line(&[
        ui::span(style::FAINT, "状态: "),
        ui::span(
            if downloaded >= 100 { style::ACCENT } else { style::BASE },
            if downloaded >= 100 { "完成 ✓" } else { "进行中" },
        ),
        ui::span(style::FAINT, format!("（列宽 {width}）")),
    ]);

    // 键位提示行：组件 hints
    ui::hints(&[("↑↓/jk", "导航"), ("Enter", "确认"), ("空格", "步进")]);

    // 兼容旧 API 的纯文本行
    print_line("（以上由 rmus_plugin_sdk::ui 组件表渲染）");
}

#[unsafe(no_mangle)]
pub extern "C" fn load() {}

/// 返回非 0 表示状态变了、需要重绘。
#[unsafe(no_mangle)]
pub extern "C" fn update(ev: i32) -> i32 {
    use event::{kind, KIND_CHAR, KIND_DOWN, KIND_ENTER, KIND_SPACE, KIND_UP};
    match kind(ev) {
        KIND_UP => {
            let cur = SELECTED.load(Ordering::Relaxed);
            SELECTED.store(cur.saturating_sub(1), Ordering::Relaxed);
            1
        }
        KIND_DOWN => {
            let cur = SELECTED.load(Ordering::Relaxed);
            if (cur as usize) < DEMO_ITEMS.len() - 1 {
                SELECTED.store(cur + 1, Ordering::Relaxed);
            }
            1
        }
        KIND_SPACE => {
            // 空格步进假进度
            let cur = DOWNLOADED.load(Ordering::Relaxed);
            DOWNLOADED.store((cur + 25).min(100), Ordering::Relaxed);
            1
        }
        KIND_ENTER => 1, // 确认：重绘（v0 无副作用）
        KIND_CHAR => {
            // j/k 导航（vim 习惯）
            if event::char_of(ev) == Some('j') {
                return update(event::encode(KIND_DOWN, 0));
            }
            if event::char_of(ev) == Some('k') {
                return update(event::encode(KIND_UP, 0));
            }
            0
        }
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn render(_rows: i32, cols: i32) {
    draw(cols);
}
