//! 引擎状态持久化：命名队列 + 当前队列 + 播放模式 + 音量。
//!
//! 落盘 $XDG_STATE_HOME/rmus/state.json，队列/模式/音量变更时同步写
//! （变更频率是用户操作级，写盘 ~ms 级，不值得引入异步落盘）。
//! 播放位置不恢复：重启后 play 从当前曲开头播。

use crate::engine::queue::QueueItem;
use crate::proto::RepeatMode;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct PersistedState {
    pub volume: Option<f64>,
    pub repeat: Option<RepeatMode>,
    pub random: Option<bool>,
    pub queue_name: Option<String>,
    pub current_index: Option<usize>,
    #[serde(default)]
    pub current_queue: Vec<QueueItem>,
    #[serde(default)]
    pub named: BTreeMap<String, Vec<QueueItem>>,
}

pub fn state_file() -> PathBuf {
    crate::state_dir().join("state.json")
}

pub fn load() -> PersistedState {
    let Ok(text) = std::fs::read_to_string(state_file()) else {
        return PersistedState::default();
    };
    match serde_json::from_str(&text) {
        Ok(state) => state,
        Err(e) => {
            eprintln!("[rmusd] 状态文件损坏，忽略: {e}");
            PersistedState::default()
        }
    }
}

pub fn save(state: &PersistedState) {
    let path = state_file();
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_err() {
            eprintln!("[rmusd] 无法创建状态目录 {}", dir.display());
            return;
        }
    let text = match serde_json::to_string_pretty(state) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[rmusd] 序列化状态失败: {e}");
            return;
        }
    };
    // 先写临时文件再原子替换，避免写一半崩溃损坏状态
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        eprintln!("[rmusd] 状态落盘失败: {}", path.display());
    }
}
