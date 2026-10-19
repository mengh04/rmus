//! 插件契约：rmusd 与外部插件进程之间的发现规则与线协议。
//!
//! 设计取向：**进程外插件**（LSP 风格的 stdio 子进程），不在进程内加载
//! 动态库——Rust 无稳定 ABI，且下载器这类网络插件崩溃/挂起时不应拖垮
//! 播放守护进程。rmus 自身对插件能力零知识：它只知道协议，不知道
//! "下载"是什么。
//!
//! # 发现
//!
//! 依次扫描（先到先得，重名时前者胜）：
//! 1. `$RMUS_PLUGIN_DIR`（`:` 分隔多个目录，开发/测试用）
//! 2. `$XDG_CONFIG_HOME/rmus/plugins`（默认 `~/.config/rmus/plugins`）
//! 3. `$XDG_DATA_HOME/rmus/plugins`（默认 `~/.local/share/rmus/plugins`）
//! 4. `$XDG_DATA_DIRS` 下每项的 `rmus/plugins`
//!
//! 目录里的每个**可执行文件**都是一个插件候选。宿主以 `--rmus-manifest`
//! 调用它（5s 超时），插件应向 stdout 打一行 manifest JSON：
//!
//! ```json
//! {"name":"dl","version":"0.1.0","methods":[{"name":"search","kind":"call"},
//!  {"name":"download","kind":"task"},{"name":"cancel","kind":"call"}]}
//! ```
//!
//! - `kind: "call"`：短调用，宿主等待响应（30s 超时），如搜索
//! - `kind: "task"`：长任务，宿主立即向客户端返回任务号 `{"call":N}`，
//!   进度与结果经事件推送；插件子进程按需注入环境变量：
//!   `RMUS_LIBRARY_DIR`（库根目录，任务产物应写入这里）
//!
//! # 会话
//!
//! 首次调用时宿主以 `--rmus-plugin` 拉起长驻子进程，stdin/stdout 逐行
//! NDJSON（风格与 [`crate::proto`] 一致）：
//!
//! ```text
//! 宿主 → 插件:  {"id":1,"method":"search","params":{"query":"晴天"}}
//! 插件 → 宿主:  {"id":1,"result":{"songs":[...]}}
//! 插件 → 宿主:  {"id":2,"error":"..."}
//! 插件 → 宿主:  {"event":"progress","call":3,"data":{"received":1024,"total":4096}}
//! 宿主 → 插件:  {"id":9,"method":"cancel","params":{"call":3}}
//! ```
//!
//! 约束：插件 stdout 只能输出协议行（进度/日志走 stderr）；每个请求必须
//! 恰好一个响应（task 完成时也要回，宿主把它包装成 done 事件广播）；
//! 取消是被取消的那个请求自己回 error（宿主随后广播 failed）。
//!
//! 会话生命周期：懒启动、空闲（无 pending 且无活动）10 分钟回收、
//! daemon 退出时 kill。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 一个插件对外宣告的能力清单（`--rmus-manifest` 的解析结果）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// 插件名（客户端用它寻址，如 `"dl"`）；限 `[a-z0-9-]`
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub methods: Vec<PluginMethod>,
    /// UI 插件：wasm 实现，宿主提供按键注入（PluginKey）与视图推送
    /// （Event::PluginView）通道；methods 为空
    #[serde(default)]
    pub ui: bool,
    /// 插件注册的 TUI tab 标题（如 `"在线"`）；None = 不占 tab 位
    /// （只在「插件」清单里出现）。tab 栏随插件装卸动态增减。
    #[serde(default)]
    pub tab: Option<String>,
}

impl PluginManifest {
    /// 某方法的类别；未宣告的方法按短调用处理。
    pub fn method_kind(&self, method: &str) -> Option<MethodKind> {
        self.methods
            .iter()
            .find(|m| m.name == method)
            .map(|m| m.kind)
    }

    /// manifest 结构合法性：名字字符集 + 方法名非空。
    pub fn validate(&self) -> Result<(), String> {
        let ok = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
        if self.name.is_empty() || !self.name.chars().all(ok) {
            return Err(format!("非法插件名 “{}”", self.name));
        }
        if self.methods.iter().any(|m| m.name.is_empty()) {
            return Err("方法名不能为空".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MethodKind {
    /// 短调用：宿主等待响应（有超时）
    #[default]
    Call,
    /// 长任务：宿主立即返回任务号，结果经事件推送
    Task,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMethod {
    pub name: String,
    #[serde(default)]
    pub kind: MethodKind,
}

/// 握手超时（`--rmus-manifest` 必须在此时间内输出一行 JSON）。
pub const MANIFEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// 短调用超时（网络类操作，放宽到比慢接口重试总和更长）。
pub const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// 会话空闲回收阈值。
pub const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// 插件 → 宿主 的一行响应（带 `id`）。
#[derive(Debug, Deserialize)]
pub struct PluginReply {
    pub id: u64,
    #[serde(default)]
    pub result: serde_json::Value,
    #[serde(default)]
    pub error: Option<String>,
}

/// 插件 → 宿主 的一行通知（带 `event`）。
#[derive(Debug, Deserialize)]
pub struct PluginNotify {
    pub event: String,
    #[serde(default)]
    pub call: Option<u64>,
    #[serde(default)]
    pub data: serde_json::Value,
}

/// 插件候选目录（去重后的有序列表；顺序即优先级）。
pub fn plugin_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !dirs.contains(&p) {
            dirs.push(p);
        }
    };
    // 开发覆盖：RMUS_PLUGIN_DIR 用 `:` 分隔多个目录
    if let Some(list) = std::env::var_os("RMUS_PLUGIN_DIR") {
        for dir in std::env::split_paths(&list) {
            push(dir);
        }
    }
    let xdg_or = |var: &str, fallback: fn(&std::ffi::OsStr) -> PathBuf| {
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| fallback(&h))
                    .unwrap_or_default()
            })
    };
    let data_home = xdg_or("XDG_DATA_HOME", |h| {
        PathBuf::from(h).join(".local").join("share")
    });
    if !data_home.as_os_str().is_empty() {
        push(data_home.join("rmus").join("plugins"));
    }
    let config_home = xdg_or("XDG_CONFIG_HOME", |h| PathBuf::from(h).join(".config"));
    if !config_home.as_os_str().is_empty() {
        push(config_home.join("rmus").join("plugins"));
    }
    if let Some(list) = std::env::var_os("XDG_DATA_DIRS") {
        for dir in std::env::split_paths(&list) {
            push(dir.join("rmus").join("plugins"));
        }
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_解析与方法查找() {
        let text = r#"{"name":"dl","version":"0.1.0","methods":[
            {"name":"search","kind":"call"},
            {"name":"download","kind":"task"}]}"#;
        let m: PluginManifest = serde_json::from_str(text).unwrap();
        assert_eq!(m.name, "dl");
        assert_eq!(m.method_kind("search"), Some(MethodKind::Call));
        assert_eq!(m.method_kind("download"), Some(MethodKind::Task));
        // 未宣告 / kind 缺省按 call
        assert_eq!(m.method_kind("nope"), None);
        let m2: PluginManifest =
            serde_json::from_str(r#"{"name":"x","version":"1","methods":[{"name":"a"}]}"#).unwrap();
        assert_eq!(m2.method_kind("a"), Some(MethodKind::Call));
    }

    #[test]
    fn manifest_校验() {
        let m: PluginManifest = serde_json::from_str(
            r#"{"name":"dl","version":"1","methods":[{"name":"search"}]}"#,
        )
        .unwrap();
        assert!(m.validate().is_ok());
        let bad: PluginManifest = serde_json::from_str(
            r#"{"name":"Bad Name","version":"1","methods":[{"name":"search"}]}"#,
        )
        .unwrap();
        assert!(bad.validate().is_err());
    }
}
