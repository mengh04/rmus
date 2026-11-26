//! fm 配置（`data_dir/fm/config.json`）：行为参数，没有密钥——
//! 免费公开 API（Deezer），开箱即用。Last.fm 接入后 key 也放这。

use anyhow::{Context, Result, bail};
use rmus_plugin_sdk::{data_dir, fs};

use serde::{Deserialize, Serialize};

fn default_playlist() -> String {
    "FM 推荐".into()
}
fn default_count() -> usize {
    4
}
fn default_flac_only() -> bool {
    true
}
fn default_auto() -> bool {
    true
}
fn default_interval_hours() -> u64 {
    6
}
fn default_auto_queue() -> bool {
    false
}
fn default_chart() -> bool {
    true
}
fn default_artist_top() -> bool {
    true
}
/// 曲库高频歌手里最多取多少位去 Deezer 拉热门曲。
fn default_artist_depth() -> usize {
    6
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 入库目标播放列表（引擎 PlaylistAdd 不存在即建）
    #[serde(default = "default_playlist")]
    pub playlist: String,
    /// 每轮入库数量上限
    #[serde(default = "default_count")]
    pub count: usize,
    /// 与 rmus-dl 一致：只要无损
    #[serde(default = "default_flac_only")]
    pub flac_only: bool,
    /// 常驻 worker：rmus 运行期间到点自动跑轮
    #[serde(default = "default_auto")]
    pub auto: bool,
    /// 自动轮间隔（小时，1..=168）
    #[serde(default = "default_interval_hours")]
    pub interval_hours: u64,
    /// 自动轮的新歌除进播放列表外，也直接加到当前队列尾
    #[serde(default = "default_auto_queue")]
    pub auto_queue: bool,
    /// 启用 Deezer 全球榜单候选
    #[serde(default = "default_chart")]
    pub chart: bool,
    /// 启用「曲库高频歌手热门曲」候选
    #[serde(default = "default_artist_top")]
    pub artist_top: bool,
    /// 歌手热门候选最多探测多少位歌手（1..=20）
    #[serde(default = "default_artist_depth")]
    pub artist_depth: usize,
}

impl Default for Config {
    fn default() -> Self {
        serde_json::from_str("{}").unwrap()
    }
}

/// 插件数据目录：`<data_dir>/fm/`（墓碑/日志/排班都在这）。
pub fn fm_dir() -> String {
    format!("{}/fm", data_dir().trim_end_matches('/'))
}

pub fn config_path() -> String {
    format!("{}/config.json", fm_dir())
}

pub fn read_text(path: &str) -> Option<String> {
    use std::io::Read as _;
    if !fs::exists(path) {
        return None;
    }
    fs::File::open(path).ok().and_then(|mut f| {
        let mut buf = String::new();
        f.read_to_string(&mut buf).ok().map(|_| buf)
    })
}

pub fn write_text(path: &str, text: &str) -> Result<()> {
    let mut f = fs::File::create(path).with_context(|| format!("创建 {path} 失败"))?;
    f.write_all(text.as_bytes()).with_context(|| format!("写入 {path} 失败"))?;
    Ok(())
}

/// 读配置：文件缺失/损坏都回落默认（零配置可用是本插件的卖点）。
pub fn load() -> Result<Config> {
    let Some(text) = read_text(&config_path()) else {
        return Ok(Config::default());
    };
    serde_json::from_str(&text).with_context(|| format!("解析 {} 失败", config_path()))
}

pub fn save(cfg: &Config) -> Result<()> {
    // SDK 写模式隐式建父目录，直接写即可
    write_text(&config_path(), &serde_json::to_string_pretty(cfg)?)
}

/// CLI/UI `--set k=v`：白名单 + 范围校验，防止把配置写坏。
pub fn set(cfg: &mut Config, key: &str, value: &str) -> Result<()> {
    match key {
        "playlist" => cfg.playlist = value.trim().to_string(),
        "count" => {
            let n: usize = value.trim().parse().context("count 须为整数")?;
            if !(1..=20).contains(&n) {
                bail!("count 须在 1..=20");
            }
            cfg.count = n;
        }
        "flac_only" | "auto" | "auto_queue" | "chart" | "artist_top" => {
            let b = value.trim().parse::<bool>().context("须为 true/false")?;
            match key {
                "flac_only" => cfg.flac_only = b,
                "auto" => cfg.auto = b,
                "auto_queue" => cfg.auto_queue = b,
                "chart" => cfg.chart = b,
                _ => cfg.artist_top = b,
            }
        }
        "interval_hours" => {
            let n: u64 = value.trim().parse().context("interval_hours 须为整数")?;
            if !(1..=168).contains(&n) {
                bail!("interval_hours 须在 1..=168");
            }
            cfg.interval_hours = n;
        }
        "artist_depth" => {
            let n: usize = value.trim().parse().context("artist_depth 须为整数")?;
            if !(1..=20).contains(&n) {
                bail!("artist_depth 须在 1..=20");
            }
            cfg.artist_depth = n;
        }
        _ => bail!("未知配置项 {key}（可设：playlist/count/flac_only/auto/interval_hours/auto_queue/chart/artist_top/artist_depth）"),
    }
    Ok(())
}
