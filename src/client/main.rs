//! rmus：客户端。当前提供 CLI 子命令；无参数时的 TUI 是下一步。

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rmus::client::Client;
use rmus::proto::{PlayState, Request, StatusInfo, TrackInfo};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "rmus",
    version,
    about = "rmus 客户端：无参数进入 TUI（未实现），带子命令则一次性控制 rmusd"
)]
struct Cli {
    /// rmusd socket 路径（默认 $XDG_RUNTIME_DIR/rmus.sock）
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    /// 连不上时不自动拉起 rmusd
    #[arg(long, global = true)]
    no_spawn: bool,
    #[command(subcommand)]
    command: Option<Command>,
    /// 直接播放：rmus 文件或目录...（替换当前队列并从头播）
    paths: Vec<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// 添加文件/目录到队列尾部（目录递归扫描）
    Add { paths: Vec<PathBuf> },
    /// 播放（可选队列下标，从 0 开始）
    /// 播放：无参数 = 恢复播放；纯数字 = 队列下标（从 0）；其他词 = 库模糊搜索（多词 AND，如 `rmus play secret 茅野`）
    Play { index: Vec<String> },
    /// 暂停
    Pause,
    /// 播放/暂停切换
    Toggle,
    /// 停止（保留队列）
    Stop,
    /// 下一曲
    Next,
    /// 上一曲
    Prev,
    /// 跳转：绝对 "1:23" / "90"（秒）；相对 "+10" / "-10"（秒）
    Seek { target: String },
    /// 音量 0-100："65"、"+10"、"-10"
    Volume { target: String },
    /// 显示当前状态
    Status,
    /// 列出队列
    Queue,
    /// 清空队列并停止
    Clear,
    /// 删除队列中的曲子：无参数 = 删掉正在播放的这首（自动续播下一首）；数字 = 删第 index 首
    Remove { index: Option<usize> },
    /// 打乱队列（当前曲移到队首）
    Shuffle,
    /// 循环模式：无参数循环切换 off -> all -> one -> off；或直接指定
    Repeat { mode: Option<String> },
    /// 随机播放：无参数切换；或 on/off
    Random { mode: Option<String> },
    /// 命名队列（播放列表）管理
    #[command(subcommand)]
    Pl(Pl),
    /// 库操作
    #[command(subcommand)]
    Lib(Lib),
    /// 库模糊搜索，列出候选（`rmus find secret`）
    Find { query: String },
    /// 试听：保存当前状态，临时换成这些文件播放；播完自动恢复原队列与进度
    Peek { paths: Vec<PathBuf> },
    /// 结束试听，恢复进入试听前的队列与进度
    Back,
    /// 让 rmusd 退出
    Quit,
}

#[derive(Subcommand)]
enum Pl {
    /// 列出所有命名队列
    List,
    /// 创建空队列
    New { name: String },
    /// 把当前队列保存为命名队列（覆盖同名；当前队列即变为该命名队列）
    Save { name: String },
    /// 切换到命名队列并从头播放
    Play { name: String },
    /// 添加文件/目录到命名队列（不存在则创建；正播放该队列时立即生效）
    Add { name: String, paths: Vec<PathBuf> },
    /// 删除命名队列（正在播放的除外）
    Remove { name: String },
}

#[derive(Subcommand)]
enum Lib {
    /// 全量重建库索引（扫描 ~/Music）
    Rescan,
}

fn main() -> Result<()> {
    let mut cli = Cli::parse();
    // 相对路径以客户端 CWD 解释，统一转绝对路径再发给 daemon
    // （daemon 的 CWD 是它被拉起时的目录，直接转发相对路径会找错文件）
    cli.paths = absolutize(&cli.paths);
    if let Some(Command::Add { paths }) = cli.command.as_mut() {
        *paths = absolutize(paths);
    }
    if let Some(Command::Pl(Pl::Add { paths, .. })) = cli.command.as_mut() {
        *paths = absolutize(paths);
    }
    if let Some(Command::Peek { paths }) = cli.command.as_mut() {
        *paths = absolutize(paths);
    }

    let Some(command) = cli.command.as_ref() else {
        // 无子命令：带路径 = 直接播放（替换队列）；无路径 = TUI 占位摘要
        let mut client = connect(&cli)?;
        if !cli.paths.is_empty() {
            let value = client.request(Request::Open {
                paths: cli.paths.clone(),
            })?;
            print_status(&status_reply(value)?);
            return Ok(());
        }
        print_status(&status_of(&mut client)?);
        println!("\n(TUI 尚未实现，rmus --help 查看子命令)");
        return Ok(());
    };

    let mut client = connect(&cli)?;
    match command {
        Command::Add { paths } => {
            let value = client.request(Request::Add { paths: paths.clone() })?;
            let added = value.get("added").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("已添加 {added} 首");
        }
        Command::Lib(Lib::Rescan) => {
            let value = client.request(Request::LibRescan)?;
            let added = value.get("added").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("库索引已重建：{added} 首");
        }
        Command::Peek { paths } => {
            print_status(&status_reply(client.request(Request::Peek {
                paths: paths.clone(),
            })?)?);
        }
        Command::Back => {
            print_status(&status_reply(client.request(Request::Back)?)?);
        }
        Command::Play { index } => {
            let request = match index.as_slice() {
                [] => Request::Play { index: None },
                [one] => match one.parse::<usize>() {
                    Ok(i) => Request::Play { index: Some(i) },
                    Err(_) => Request::PlayMatch { query: one.clone() },
                },
                // 多词：拼回空格分隔的查询串（引擎侧再分词 AND 匹配）
                words => Request::PlayMatch {
                    query: words.join(" "),
                },
            };
            print_status(&status_reply(client.request(request)?)?);
        }
        Command::Find { query } => {
            let value = client.request(Request::Find {
                query: query.clone(),
            })?;
            let tracks: Vec<TrackInfo> = serde_json::from_value(
                value.get("tracks").cloned().unwrap_or_default(),
            )
            .context("解析搜索结果失败")?;
            if tracks.is_empty() {
                println!("没有匹配 “{query}” 的曲子");
                return Ok(());
            }
            for t in &tracks {
                println!(
                    "{:>2}. {} - {}  [{}]",
                    t.index + 1,
                    t.title,
                    t.artist,
                    t.album
                );
            }
        }
        Command::Pause => {
            print_status(&status_reply(client.request(Request::Pause)?)?);
        }
        Command::Toggle => {
            print_status(&status_reply(client.request(Request::TogglePlay)?)?);
        }
        Command::Stop => {
            client.request(Request::Stop)?;
            println!("已停止");
        }
        Command::Next => {
            print_status(&status_reply(client.request(Request::Next)?)?);
        }
        Command::Prev => {
            print_status(&status_reply(client.request(Request::Prev)?)?);
        }
        Command::Seek { target } => {
            print_status(&status_reply(client.request(parse_seek(target)?)?)?);
        }
        Command::Volume { target } => {
            client.request(parse_volume(target)?)?;
            print_status(&status_of(&mut client)?);
        }
        Command::Status => {
            print_status(&status_of(&mut client)?);
        }
        Command::Queue => {
            let status = status_of(&mut client)?;
            let value = client.request(Request::QueueList)?;
            let tracks: Vec<TrackInfo> = serde_json::from_value(
                value.get("tracks").cloned().unwrap_or_default(),
            )
            .context("解析队列失败")?;
            for track in &tracks {
                let marker = if Some(track.index) == status.current {
                    "▶"
                } else {
                    " "
                };
                let artist = if track.artist.is_empty() {
                    String::new()
                } else {
                    format!(" - {}", track.artist)
                };
                println!(
                    "{marker} {:>3} {}{} [{}]",
                    track.index,
                    track.title,
                    artist,
                    fmt_mmss(track.duration_ms)
                );
            }
        }
        Command::Clear => {
            client.request(Request::Clear)?;
            println!("队列已清空");
        }
        Command::Remove { index } => {
            client.request(Request::Remove { index: *index })?;
            match index {
                Some(i) => println!("已删除第 {i} 首"),
                None => println!("已删除当前曲"),
            }
        }
        Command::Shuffle => {
            client.request(Request::Shuffle)?;
            println!("队列已打乱");
        }
        Command::Repeat { mode } => {
            let value = client.request(Request::SetRepeat {
                mode: mode.as_deref().map(parse_repeat).transpose()?,
            })?;
            print_status(&status_reply(value)?);
        }
        Command::Random { mode } => {
            let on = match mode.as_deref() {
                None => None,
                Some("on") | Some("1") | Some("true") => Some(true),
                Some("off") | Some("0") | Some("false") => Some(false),
                Some(other) => bail!("random 参数应为 on/off，收到 {other}"),
            };
            let value = client.request(Request::SetRandom { on })?;
            print_status(&status_reply(value)?);
        }
        Command::Pl(pl) => handle_pl(&mut client, pl)?,
        Command::Quit => {
            client.request(Request::Quit)?;
            println!("rmusd 已退出");
        }
    }
    Ok(())
}

fn parse_repeat(mode: &str) -> Result<rmus::proto::RepeatMode> {
    match mode {
        "off" | "0" => Ok(rmus::proto::RepeatMode::Off),
        "all" | "on" | "1" => Ok(rmus::proto::RepeatMode::All),
        "one" | "single" => Ok(rmus::proto::RepeatMode::One),
        other => bail!("repeat 参数应为 off/all/one，收到 {other}"),
    }
}

fn handle_pl(client: &mut Client, pl: &Pl) -> Result<()> {
    match pl {
        Pl::List => {
            let value = client.request(Request::PlaylistsList)?;
            let names: Vec<String> = serde_json::from_value(
                value.get("names").cloned().unwrap_or_default(),
            )
            .context("解析队列列表失败")?;
            let current = value.get("current").and_then(|v| v.as_str());
            if names.is_empty() {
                println!("（没有命名队列，rmus pl save 名字 可保存当前队列）");
                return Ok(());
            }
            for name in names {
                let marker = if Some(name.as_str()) == current {
                    "▶"
                } else {
                    " "
                };
                println!("{marker} {name}");
            }
        }
        Pl::New { name } => {
            client.request(Request::PlaylistNew { name: name.clone() })?;
            println!("队列 {name} 已创建");
        }
        Pl::Save { name } => {
            client.request(Request::PlaylistSave { name: name.clone() })?;
            println!("当前队列已保存为 {name}");
        }
        Pl::Play { name } => {
            let value = client.request(Request::PlaylistPlay { name: name.clone() })?;
            print_status(&status_reply(value)?);
        }
        Pl::Add { name, paths } => {
            let value = client.request(Request::PlaylistAdd {
                name: name.clone(),
                paths: paths.clone(),
            })?;
            let added = value.get("added").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("已向队列 {name} 添加 {added} 首");
        }
        Pl::Remove { name } => {
            client.request(Request::PlaylistRemove { name: name.clone() })?;
            println!("队列 {name} 已删除");
        }
    }
    Ok(())
}

fn connect(cli: &Cli) -> Result<Client> {
    if cli.no_spawn {
        Client::connect(cli.socket.clone())
    } else {
        Client::connect_or_spawn(cli.socket.clone())
    }
}

/// 相对路径转绝对路径（以客户端 CWD 解释），不解析符号链接。
fn absolutize(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
        .collect()
}

fn status_of(client: &mut Client) -> Result<StatusInfo> {
    serde_json::from_value(client.request(Request::Status)?).context("解析状态失败")
}

fn status_reply(value: serde_json::Value) -> Result<StatusInfo> {
    serde_json::from_value(value).context("解析状态失败")
}

fn print_status(status: &StatusInfo) {
    let state = match status.state {
        PlayState::Playing => "▶ 播放中",
        PlayState::Paused => "‖ 已暂停",
        PlayState::Stopped => "■ 已停止",
    };
    let volume = (status.volume.clamp(0.0, 1.0) * 100.0).round() as u32;
    let repeat = match status.repeat {
        rmus::proto::RepeatMode::Off => "循环关",
        rmus::proto::RepeatMode::All => "列表循环",
        rmus::proto::RepeatMode::One => "单曲循环",
    };
    let random = if status.random { "随机" } else { "顺序" };
    let queue = status.queue_name.clone().unwrap_or_else(|| "临时".into());
    let peek = if status.peeking { "  ♪试听中(rmus back 恢复)" } else { "" };
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
fn parse_seek(target: &str) -> Result<Request> {
    let target = target.trim();
    if let Some(delta) = target.strip_prefix('+').or_else(|| target.strip_prefix('-')) {
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
    let secs: f64 = target.parse().context("跳转格式：秒数（90）、分秒（1:30）、相对（+10）")?;
    Ok(Request::Seek {
        position_ms: (secs * 1000.0).round() as u64,
    })
}

fn parse_volume(target: &str) -> Result<Request> {
    let target = target.trim();
    if let Some(delta) = target.strip_prefix('+').or_else(|| target.strip_prefix('-')) {
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

fn fmt_mmss(ms: u64) -> String {
    let secs = ms / 1000;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}
