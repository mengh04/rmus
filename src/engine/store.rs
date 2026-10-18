//! 引擎状态持久化：播放列表 + 当前队列（会话恢复）+ 播放模式 + 音量。
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
    /// 播放列表（持久命名集合）。旧版字段名叫 named，alias 兼容读取旧状态文件
    #[serde(default, alias = "named")]
    pub playlists: BTreeMap<String, Vec<QueueItem>>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 旧字段名named兼容读取() {
        let state: PersistedState =
            serde_json::from_str(r#"{"named": {"摇滚": [], "流行": []}}"#).unwrap();
        assert!(state.playlists.contains_key("摇滚"));
        assert!(state.playlists.contains_key("流行"));
    }

    #[test]
    fn 新字段名playlists直接读取() {
        let state: PersistedState = serde_json::from_str(r#"{"playlists": {"摇滚": []}}"#).unwrap();
        assert!(state.playlists.contains_key("摇滚"));
    }

    #[test]
    fn 落盘用新字段名且可回读() {
        let state = PersistedState {
            playlists: BTreeMap::from([("摇滚".into(), Vec::new())]),
            ..Default::default()
        };
        let text = serde_json::to_string(&state).unwrap();
        assert!(text.contains("\"playlists\""));
        assert!(!text.contains("\"named\""));
        let round: PersistedState = serde_json::from_str(&text).unwrap();
        assert!(round.playlists.contains_key("摇滚"));
    }
}
