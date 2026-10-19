//! UI 插件（wasm）的客户端侧：视图缓存、按键直通、视图请求。
//!
//! 数据流：本模块发 `Request::PluginKey` / `Request::PluginViewReq` →
//! daemon 转 wasm 实例线程 → 插件 update/render → daemon 以
//! `Event::PluginView` 推回 → `apply_plugin_view` 按插件名写入分槽缓存，
//! 渲染层按当前 tab 取对应插件的槽位画。
//!
//! 寻址一律用插件名（tab 注册只是把插件放进 tab 栏，交互全走名字）。

use super::{App, Mode, Tab};
use crate::proto::{plugin_key, Request};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl App {
    /// 请求某个插件画布的当前视图（切进对应 tab / 终端 resize 时）。
    pub(super) fn request_view(&mut self, plugin: &str) {
        let (cols, rows) = self.viewport;
        self.simple(Request::PluginViewReq {
            plugin: plugin.to_string(),
            rows,
            cols,
        });
    }

    /// 当前 tab 的内容插件：按键/数据/视图请求都发给它。
    pub(super) fn send_tab_key(&mut self, kind: i32, payload: i32) {
        use crate::proto::plugin_key;
        let Some(plugin) = self.plugin_of_tab(&self.tab) else {
            return;
        };
        let (cols, rows) = self.viewport;
        self.simple(Request::PluginKey {
            plugin,
            key: plugin_key::encode(kind, payload),
            rows,
            cols,
        });
    }

    /// 把搜索关键词交给某个插件（worker 搜索，结果经 PluginView 回）。
    pub(super) fn submit_search(&mut self, plugin: &str, query: &str) {
        self.simple(Request::PluginData {
            plugin: plugin.to_string(),
            data: serde_json::json!({ "submit": query }),
        });
    }

    /// `Event::PluginView` 落地：按插件名写入分槽缓存。选中行以插件
    /// 声明为准（`rmus_set_selected`，画布行号）；未声明时沿用旧值并
    /// 夹紧。未在 tab 栏的插件也照收（缓存无害，插件卸载前可能推最后一帧）。
    pub(super) fn apply_plugin_view(
        &mut self,
        plugin: &str,
        lines: Vec<crate::proto::PluginLine>,
        selected: Option<usize>,
    ) {
        let ui = self.plugin_uis.entry(plugin.to_string()).or_default();
        ui.view = lines;
        let last = ui.view.len().saturating_sub(1);
        ui.selected = selected.unwrap_or(ui.selected.min(last)).min(last);
    }

    /// 切换 UI 插件的按键映射（v0：Enter/空格/上下/可见字符）。
    fn map_key(key: KeyEvent) -> Option<i32> {
        use plugin_key::{encode, KIND_CHAR, KIND_DOWN, KIND_ENTER, KIND_SPACE, KIND_UP};
        match (key.modifiers, key.code) {
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Enter) => {
                Some(encode(KIND_ENTER, 0))
            }
            (KeyModifiers::NONE, KeyCode::Char(' ')) => Some(encode(KIND_SPACE, 0)),
            (KeyModifiers::NONE, KeyCode::Up) => Some(encode(KIND_UP, 0)),
            (KeyModifiers::NONE, KeyCode::Down) => Some(encode(KIND_DOWN, 0)),
            (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
                Some(encode(KIND_CHAR, c as i32))
            }
            _ => None,
        }
    }

    /// `Mode::Plugin`：按键直通。除 Esc（退出直通）外全部转给**当前 tab
    /// 的插件**；未映射的键直接吞掉（直通模式下不触发全局动作）。
    pub(super) fn handle_plugin_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.mode = Mode::Normal;
            return;
        }
        let Some(key) = Self::map_key(key) else {
            return;
        };
        let Some(plugin) = self.plugin_of_tab(&self.tab) else {
            return;
        };
        let (cols, rows) = self.viewport;
        self.simple(Request::PluginKey { plugin, key, rows, cols });
    }

    /// 当前 tab 是否是插件画布（注册 tab 或「插件」面板）。
    pub(super) fn tab_is_plugin_canvas(&self) -> bool {
        matches!(self.tab, Tab::Plugin(_) | Tab::Plugins)
    }
}
