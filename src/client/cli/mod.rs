//! 子命令分发：把 `rmus <命令>` 翻译成一次 daemon 请求并打印结果。
//!
//! 相对路径统一转成绝对路径再发：daemon 的 CWD 是它被拉起时的目录，
//! 直接转发相对路径会找错文件。

mod format;
mod playlist;

use crate::{Cli, Command, Lib, Pl};
use anyhow::{Context, Result, bail};
use format::{
    connect, fmt_mmss, parse_repeat, parse_seek, parse_volume, print_status, status_of,
    status_reply,
};
use playlist::handle_pl;
use rmus::proto::{Request, TrackInfo};

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

/// 相对路径转绝对路径（以客户端 CWD 解释），不解析符号链接。
fn normalize(cli: &mut Cli) {
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
}

fn absolutize(paths: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    paths
        .iter()
        .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
        .collect()
}
