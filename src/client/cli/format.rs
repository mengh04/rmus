//! 输出与参数解析：状态行、队列行、时间/音量/循环参数。
//!
//! 都是给终端人看的，和 TUI 的渲染无关。

use crate::Cli;
use anyhow::{Context, Result, bail};
use rmus::client::Client;
use rmus::proto::{PlayState, RepeatMode, Request, StatusInfo};

/// 建立连接；`--no-spawn` 时不自动拉起 rmusd。
pub(crate) fn connect(cli: &Cli) -> Result<Client> {
    if cli.no_spawn {
        Client::connect(cli.socket.clone())
    } else {
        Client::connect_or_spawn(cli.socket.clone())
    }
}

pub(crate) fn status_of(client: &mut Client) -> Result<StatusInfo> {
    serde_json::from_value(client.request(Request::Status)?).context("解析状态失败")
}

pub(crate) fn status_reply(value: serde_json::Value) -> Result<StatusInfo> {
    serde_json::from_value(value).context("解析状态失败")
}

pub(crate) fn print_status(status: &StatusInfo) {
    let state = match status.state {
        PlayState::Playing => "▶ 播放中",
        PlayState::Paused => "‖ 已暂停",
        PlayState::Stopped => "■ 已停止",
    };
    let volume = (status.volume.clamp(0.0, 1.0) * 100.0).round() as u32;
    let repeat = match status.repeat {
        RepeatMode::Off => "循环关",
        RepeatMode::All => "列表循环",
        RepeatMode::One => "单曲循环",
    };
    let random = if status.random { "随机" } else { "顺序" };
    let queue = status.queue_name.clone().unwrap_or_else(|| "临时".into());
    let peek = if status.peeking {
        "  ♪试听中(rmus back 恢复)"
    } else {
        ""
    };
    match &status.track {
        Some(track) => {
            println!("{state}  {} - {}", track.title, track.artist);
            println!(
                "  {} / {}   音量 {volume}%   {repeat}/{random}   队列[{queue}] {}/{}{peek}",
                fmt_mmss(status.position_ms),
                fmt_mmss(status.duration_ms),
                status.current.map(|i| i + 1).unwrap_or(0),
                status.queue_len
            );
        }
        None => {
            println!(
                "{state}   音量 {volume}%   {repeat}/{random}   队列[{queue}] {} 首",
                status.queue_len
            );
        }
    }
}

/// "2:35" -> 绝对；"90"（秒）-> 绝对；"+10"/"-10"（秒）-> 相对
pub(crate) fn parse_seek(target: &str) -> Result<Request> {
    let target = target.trim();
    if let Some(delta) = target
        .strip_prefix('+')
        .or_else(|| target.strip_prefix('-'))
    {
        let sign = if target.starts_with('-') { -1 } else { 1 };
        let secs: f64 = delta.parse().context("相对跳转格式：+10 / -10（秒）")?;
        return Ok(Request::SeekRelative {
            delta_ms: (secs * 1000.0).round() as i64 * sign,
        });
    }
    if target.contains(':') {
        let mut total_secs = 0u64;
        for part in target.split(':') {
            let n: u64 = part
                .parse()
                .with_context(|| format!("时间格式错误：{target}（应为 1:23 或 1:02:03）"))?;
            total_secs = total_secs * 60 + n;
        }
        return Ok(Request::Seek {
            position_ms: total_secs * 1000,
        });
    }
    let secs: f64 = target
        .parse()
        .context("跳转格式：秒数（90）、分秒（1:30）、相对（+10）")?;
    Ok(Request::Seek {
        position_ms: (secs * 1000.0).round() as u64,
    })
}

pub(crate) fn parse_volume(target: &str) -> Result<Request> {
    let target = target.trim();
    if let Some(delta) = target
        .strip_prefix('+')
        .or_else(|| target.strip_prefix('-'))
    {
        let sign = if target.starts_with('-') { -1.0 } else { 1.0 };
        let n: f64 = delta.parse().context("音量格式：65 / +10 / -10")?;
        return Ok(Request::AdjustVolume {
            delta: (n / 100.0) as f32 * sign,
        });
    }
    let n: f64 = target.parse().context("音量格式：65 / +10 / -10")?;
    if !(0.0..=100.0).contains(&n) {
        bail!("音量范围 0-100");
    }
    Ok(Request::SetVolume {
        value: (n / 100.0) as f32,
    })
}

pub(crate) fn parse_repeat(mode: &str) -> Result<RepeatMode> {
    match mode {
        "off" | "0" => Ok(RepeatMode::Off),
        "all" | "on" | "1" => Ok(RepeatMode::All),
        "one" | "single" => Ok(RepeatMode::One),
        other => bail!("repeat 参数应为 off/all/one，收到 {other}"),
    }
}

pub(crate) fn fmt_mmss(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}
