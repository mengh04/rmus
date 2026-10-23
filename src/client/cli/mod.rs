//! 子命令分发：把 `rmus <命令>` 翻译成一次 daemon 请求并打印结果。
//!
//! 相对路径统一转成绝对路径再发：daemon 的 CWD 是它被拉起时的目录，
//! 直接转发相对路径会找错文件。

mod format;
mod playlist;
pub(crate) mod plugin_cli;

use crate::{Cli, Command, Lib, Pl};
use anyhow::{Context, Result, bail};
use format::{
    connect, fmt_mmss, parse_repeat, parse_seek, parse_volume, print_status, status_of,
    status_reply,
};
use playlist::handle_pl;
use rmus::proto::{Request, StatusInfo, TrackInfo};

/// 入口：先规整路径，再按子命令分发；无子命令时进 TUI。
pub(crate) fn run(mut cli: Cli) -> Result<()> {
    normalize(&mut cli);

    let Some(command) = cli.command.as_ref() else {
        // 无子命令：带路径 = 直接播放（替换队列）；无路径 = 进入 TUI
        if !cli.paths.is_empty() {
            let mut client = connect(&cli)?;
            let value = client.request(Request::Open {
                paths: cli.paths.clone(),
            })?;
            print_status(&status_reply(value)?);
            return Ok(());
        }
        return rmus::client::tui::run(cli.socket.clone(), cli.no_spawn);
    };

    let mut client = connect(&cli)?;
    match command {
        Command::Add { paths } => {
            let value = client.request(Request::Add {
                paths: paths.clone(),
            })?;
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
            let tracks: Vec<TrackInfo> =
                serde_json::from_value(value.get("tracks").cloned().unwrap_or_default())
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
            let tracks: Vec<TrackInfo> =
                serde_json::from_value(value.get("tracks").cloned().unwrap_or_default())
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
        Command::Move { from, to } => {
            client.request(Request::Move {
                from: *from,
                to: *to,
            })?;
            println!("已把第 {from} 首移到第 {to}");
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
        Command::Restart => {
            restart(&cli)?;
        }
        Command::Plugin { entry, args } => plugin_cli::run(&cli, entry, args)?,
        Command::Plugins => {
            // restart 已退出读循环，这里重连一次
            let mut client = connect(&cli)?;
            let value = client.request(Request::PluginList)?;
            let plugins = value
                .get("plugins")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if plugins.is_empty() {
                println!("未发现插件（插件目录见 docs/plugins.md）");
            }
            for p in &plugins {
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                let version = p.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                let ui = p.get("ui").and_then(|v| v.as_bool()).unwrap_or(false);
                let methods: Vec<&str> = p
                    .get("methods")
                    .and_then(|v| v.as_array())
                    .map(|list| {
                        list.iter()
                            .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                            .collect()
                    })
                    .unwrap_or_default();
                let kind = if ui { " [UI]" } else { "" };
                let entry = p
                    .get("cli")
                    .and_then(|v| v.as_str())
                    .map(|e| format!(" [-{e}]"))
                    .unwrap_or_default();
                println!("{name} {version}{kind}{entry}  {}", methods.join(", "));
            }
        }
    }
    Ok(())
}

/// `rmus restart`：抓播放现场 → 停旧 daemon → 拉起新 daemon → 恢复播放。
/// 注意读循环会在 Quit 响应后结束，所以这里不走外层的 `client`。
fn restart(cli: &Cli) -> Result<()> {
    use std::time::Duration;

    let socket = cli.socket.clone().unwrap_or_else(rmus::socket_path);

    // 停前抓播放现场（队列 + 当前进度/音量/模式）；抓不到就只重启不恢复
    let snapshot: Option<(Vec<TrackInfo>, StatusInfo)> =
        rmus::client::Client::connect(cli.socket.clone()).ok().and_then(|mut c| {
            let tracks: Vec<TrackInfo> = serde_json::from_value(
                c.request(Request::QueueList).ok()?.get("tracks").cloned()?,
            )
            .ok()?;
            let status: StatusInfo = serde_json::from_value(c.request(Request::Status).ok()?).ok()?;
            Some((tracks, status))
        });

    // 旧 daemon 在跑就请它退出（连不上 = 没在跑，直接拉起）
    if rmus::client::Client::connect(cli.socket.clone()).is_ok() {
        let _ = rmus::client::Client::connect(cli.socket.clone())
            .and_then(|mut c| c.request(Request::Quit));
        println!("已请求旧 rmusd 退出…");
    }
    // 等 socket 不可连（进程退出；socket 文件可能残留，新 daemon 会清理）
    for _ in 0..30 {
        if rmus::client::Client::connect(cli.socket.clone()).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if cli.no_spawn {
        bail!("--no-spawn：旧 rmusd 已退出，跳过自动拉起");
    }
    rmus::client::start_daemon(&socket).context("拉起新 rmusd 失败")?;

    // 等新 daemon 就绪（socket 可连且能应答），随后恢复播放现场
    for _ in 0..50 {
        if let Ok(mut client) = rmus::client::Client::connect(cli.socket.clone()) {
            client
                .request(Request::Status)
                .context("新 rmusd 无响应")?;
            println!("rmusd 已重启（{}）", socket.display());

            // 恢复现场：重建队列 → 音量/循环 → 跳回原曲目 → 恢复随机（最后，
            // 免得 Open/Play 被随机干扰）→ 进度拉回原位置。
            if let Some((tracks, status)) = snapshot
                && !tracks.is_empty()
            {
                client.request(Request::Open {
                    paths: tracks.iter().map(|t| t.path.clone()).collect(),
                })?;
                client.request(Request::SetVolume {
                    value: status.volume,
                })?;
                client.request(Request::SetRepeat {
                    mode: Some(status.repeat),
                })?;
                if let Some(index) = status.current {
                    client.request(Request::Play { index: Some(index) })?;
                }
                client.request(Request::SetRandom { on: Some(status.random) })?;
                if status.position_ms > 0 {
                    client.request(Request::Seek {
                        position_ms: status.position_ms,
                    })?;
                }
                println!(
                    "已恢复播放：{}（{} 首队列，进度 {}）",
                    status
                        .track
                        .as_ref()
                        .map(|t| t.title.as_str())
                        .unwrap_or("未知曲目"),
                    tracks.len(),
                    fmt_mmss(status.position_ms),
                );
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("rmusd 重启超时（10s 内未就绪）")
}

/// 相对路径转绝对路径（以客户端 CWD 解释），不解析符号链接。
fn normalize(cli: &mut Cli) {    cli.paths = absolutize(&cli.paths);
    if let Some(Command::Add { paths }) = cli.command.as_mut() {
        *paths = absolutize(paths);
    }
    if let Some(Command::Pl(Pl::Add { paths, .. })) = cli.command.as_mut() {
        *paths = absolutize(paths);
    }
    if let Some(Command::Peek { paths }) = cli.command.as_mut() {
        *paths = absolutize(paths);
    }
}

fn absolutize(paths: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    paths
        .iter()
        .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
        .collect()
}
