//! 插件契约：内建原生插件的清单结构与数据目录约定。
//!
//! 所有插件已作为 workspace 子 crate 原生集成进 `rmusd`（不再使用 wasm 或
//! 外部子进程），但对外仍保留统一的 manifest / method / tab / cli 元数据
//! 契约，TUI 与 CLI 无需关心具体模块实现。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// daemon 数据根（`~/.local/share/rmus`，尊重 XDG_DATA_HOME）：插件私有
/// 数据（播客订阅/下载、DJ/FM 状态与配置）的落点。与库根分离——库根下的
/// 音频文件会被音乐库扫描收编。
pub fn data_root() -> PathBuf {
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".local").join("share"))
                .unwrap_or_default()
        });
    data_home.join("rmus")
}

/// 一个插件对外宣告的能力清单。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// 插件名（客户端用它寻址，如 `"rmus-dl"`）；限 `[a-z0-9-]`
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub methods: Vec<PluginMethod>,
    /// UI 插件：提供 `update` / `render` 画布通道
    #[serde(default)]
    pub ui: bool,
    /// 注册的 TUI tab 标题（如 `"在线"` / `"播客"` / `"FM"` / `"DJ"`）
    #[serde(default)]
    pub tab: Option<String>,
    /// 客户端命令行入口名（如 `"dl"` 对应 `rmus -dl …`）
    #[serde(default)]
    pub cli: Option<String>,
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
        if let Some(cli) = &self.cli
            && (cli.is_empty() || !cli.chars().all(ok))
        {
            return Err(format!("非法命令行入口名 “{cli}”"));
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

/// 短调用超时（网络类操作，放宽到比慢接口重试总和更长）。
pub const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
        assert_eq!(m.method_kind("nope"), None);
        let m2: PluginManifest =
            serde_json::from_str(r#"{"name":"x","version":"1","methods":[{"name":"a"}]}"#).unwrap();
        assert_eq!(m2.method_kind("a"), Some(MethodKind::Call));
        assert_eq!(m2.cli, None);
        let m3: PluginManifest = serde_json::from_str(
            r#"{"name":"dl","version":"1","cli":"dl","methods":[{"name":"cli"}]}"#,
        )
        .unwrap();
        assert_eq!(m3.cli.as_deref(), Some("dl"));
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
        let bad_cli: PluginManifest = serde_json::from_str(
            r#"{"name":"dl","version":"1","cli":"下载","methods":[]}"#,
        )
        .unwrap();
        assert!(bad_cli.validate().is_err());
    }
}
