//! dj 配置（`data_dir/dj/config.json`）：OpenAI 兼容端点 + 行为参数。
//!
//! 生成器-过滤器架构里 LLM 只负责提名，端点/key/模型这些信任锚点必须
//! 是用户显式配置的——不经 LLM 之手，也不猜默认端点。

use anyhow::{Context, Result, bail};
use rmus_plugin_sdk::{data_dir, fs};


use serde::{Deserialize, Serialize};

fn default_playlist() -> String {
    "DJ 推荐".into()
}
fn default_count() -> usize {
    5
}
fn default_flac_only() -> bool {
    true
}
fn default_auto() -> bool {
    true
}
fn default_interval_hours() -> u64 {
    4
}
fn default_auto_queue() -> bool {
    false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// OpenAI 兼容端点根（如 `https://api.openai.com/v1`，
    /// 拼上 `/chat/completions` 发请求）
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 入库目标播放列表（引擎 PlaylistAdd 不存在即建）
    #[serde(default = "default_playlist")]
    pub playlist: String,
    /// 每轮提名数量
    #[serde(default = "default_count")]
    pub count: usize,
    /// 与 rmus-dl 一致：只要无损
    #[serde(default = "default_flac_only")]
    pub flac_only: bool,
    /// 常驻 worker：rmus 运行期间到点自动跑轮（TUI 打开时生效）
    #[serde(default = "default_auto")]
    pub auto: bool,
    /// 自动轮间隔（小时，1..=168）
    #[serde(default = "default_interval_hours")]
    pub interval_hours: u64,
    /// 自动轮的新歌除进播放列表外，也直接加到当前队列尾（马上能听到）
    #[serde(default = "default_auto_queue")]
    pub auto_queue: bool,
}

impl Config {
    /// 三要素齐了才算配置完成（playlist/count/flac_only 有默认值）。
    pub fn is_configured(&self) -> bool {
        !(self.base_url.trim().is_empty()
            || self.api_key.trim().is_empty()
            || self.model.trim().is_empty())
    }
}

/// 插件数据目录：`<data_dir>/dj/`（画像/墓碑/日志都在这）。
pub fn dj_dir() -> String {
    format!("{}/dj", data_dir().trim_end_matches('/'))
}

pub fn config_path() -> String {
    format!("{}/config.json", dj_dir())
}

pub fn read_text(path: &str) -> Option<String> {
    use std::io::Read as _;
    if !fs::exists(path) {
        return None;
    }
    let mut file = fs::File::open(path).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;
    Some(buf)
}

pub fn write_text(path: &str, text: &str) -> Result<()> {
    let mut file = fs::File::create(path).with_context(|| format!("创建 {path} 失败"))?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("写 {path} 失败"))?;
    Ok(())
}

/// 读配置；没建过就报错并给出建法（配置是一切的前提）。
pub fn load() -> Result<Config> {
    let path = config_path();
    let Some(text) = read_text(&path) else {
        bail!(
            "还没有配置（{path}）。先跑：rmus -dj --set base_url=<端点> --set api_key=<密钥> --set model=<模型>"
        );
    };
    serde_json::from_str(&text).with_context(|| format!("配置文件 {path} 不是合法 JSON"))
}

pub fn save(cfg: &Config) -> Result<()> {
    write_text(&config_path(), &serde_json::to_string_pretty(cfg)?)
}

/// `--set key=value`；只认白名单里的键，值做基本校验。
pub fn set(cfg: &mut Config, key: &str, value: &str) -> Result<()> {
    match key {
        "base_url" => {
            let v = value.trim().trim_end_matches('/');
            if !(v.starts_with("http://") || v.starts_with("https://")) {
                bail!("base_url 要以 http:// 或 https:// 开头");
            }
            cfg.base_url = v.to_string();
        }
        "api_key" => {
            if value.trim().is_empty() {
                bail!("api_key 不能为空");
            }
            cfg.api_key = value.trim().to_string();
        }
        "model" => {
            if value.trim().is_empty() {
                bail!("model 不能为空");
            }
            cfg.model = value.trim().to_string();
        }
        "playlist" => cfg.playlist = value.trim().to_string(),
        "count" => {
            let n: usize = value
                .trim()
                .parse()
                .with_context(|| format!("count 需要正整数，得到 {value:?}"))?;
            if n == 0 || n > 20 {
                bail!("count 取值 1..=20");
            }
            cfg.count = n;
        }
        "flac_only" => {
            cfg.flac_only = value.trim() == "true";
        }
        "auto" => {
            cfg.auto = value.trim() == "true";
        }
        "interval_hours" => {
            let n: u64 = value
                .trim()
                .parse()
                .with_context(|| format!("interval_hours 需要正整数，得到 {value:?}"))?;
            if n == 0 || n > 168 {
                bail!("interval_hours 取值 1..=168（小时）");
            }
            cfg.interval_hours = n;
        }
        "auto_queue" => {
            cfg.auto_queue = value.trim() == "true";
        }
        other => bail!("未知配置项 {other}（可设：base_url/api_key/model/playlist/count/flac_only/auto/interval_hours/auto_queue）"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fill_and_set_validates() {
        let mut cfg: Config = serde_json::from_str(
            r#"{"base_url":"https://x/v1","api_key":"k","model":"m"}"#,
        )
        .unwrap();
        assert!(cfg.is_configured());
        assert_eq!(cfg.playlist, "DJ 推荐");
        assert_eq!(cfg.count, 5);
        assert!(cfg.flac_only);

        set(&mut cfg, "base_url", "https://y/v1/").unwrap();
        assert_eq!(cfg.base_url, "https://y/v1");
        set(&mut cfg, "count", "8").unwrap();
        assert_eq!(cfg.count, 8);
        assert!(set(&mut cfg, "count", "0").is_err());
        assert!(set(&mut cfg, "base_url", "ftp://x").is_err());
        assert!(set(&mut cfg, "nope", "1").is_err());

        let broken: Result<Config> =
            serde_json::from_str(r#"{"base_url":"https://x/v1"}"#).map_err(|e| e.into());
        // 缺 api_key/model 的 JSON 能解析（字段是必有，解析直接失败）
        assert!(broken.is_err());
    }
}
