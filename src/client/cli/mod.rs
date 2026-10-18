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
use serde_json::json;

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
        Command::Dl { query, get } => handle_dl(&mut client, &query.join(" "), get.as_deref())?,
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
    }
    Ok(())
}

/// `rmus dl`：在线搜索 / 下载。挑第一个提供 search+download 的插件。
fn handle_dl(
    client: &mut rmus::client::Client,
    query: &str,
    get: Option<&str>,
) -> Result<()> {
    if query.is_empty() {
        bail!("用法：rmus dl <关键词> [-g 1,3]");
    }
    let value = client.request(Request::PluginList)?;
    let plugins: Vec<serde_json::Value> = value
        .get("plugins")
        .cloned()
        .unwrap_or_default()
        .as_array()
        .cloned()
        .unwrap_or_default();
    let usable = plugins.iter().find(|p| {
        let methods = p.get("methods").and_then(|m| m.as_array()).cloned().unwrap_or_default();
        methods.iter().any(|m| m.get("name").and_then(|n| n.as_str()) == Some("search"))
            && methods.iter().any(|m| m.get("name").and_then(|n| n.as_str()) == Some("download"))
    });
    let Some(plugin) = usable.and_then(|p| p.get("name").and_then(|n| n.as_str()).map(String::from))
    else {
        bail!("未发现可用的在线搜索插件（把 rmus-dl 装入插件目录后重启 rmusd）");
    };

    let value = client.request(Request::PluginCall {
        plugin: plugin.clone(),
        method: "search".into(),
        params: json!({"query": query}),
    })?;
    let songs = value
        .get("songs")
        .cloned()
        .unwrap_or_default()
        .as_array()
        .cloned()
        .unwrap_or_default();
    if songs.is_empty() {
        println!("没有匹配 “{query}” 的结果");
        return Ok(());
    }
    for (i, song) in songs.iter().enumerate() {
        let title = song.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let artist = song.get("artist").and_then(|v| v.as_str()).unwrap_or("");
        let album = song.get("album").and_then(|v| v.as_str()).unwrap_or("");
        let source = song.get("source").and_then(|v| v.as_str()).unwrap_or("?");
        let duration = song
            .get("duration_ms")
            .and_then(|v| v.as_u64())
            .filter(|ms| *ms > 0)
            .map(fmt_mmss)
            .unwrap_or_else(|| "--:--".into());
        // 品质：格式 + 体积估计（下载前已知的那部分）
        let format = song.get("format").and_then(|v| v.as_str());
        let size = song.get("size_bytes").and_then(|v| v.as_u64());
        let quality = match (format, size) {
            (Some(f), Some(bytes)) => format!(" {f} {}M", (bytes + 512 * 1024) / (1024 * 1024)),
            (Some(f), None) => format!(" {f}"),
            (None, _) => String::new(),
        };
        println!(
            "{:>3}. [{source}{quality}] {title} - {artist}  {album}  {duration}",
            i + 1
        );
    }

    let Some(get) = get else {
        println!("（加 -g 序号 下载，如 -g 1,3；产物自动进入 ~/Music）");
        return Ok(());
    };
    for index in get.split(',') {
        let Ok(index) = index.trim().parse::<usize>() else {
            bail!("序号应为数字：{get}");
        };
        if index == 0 || index > songs.len() {
            bail!("序号 {index} 超出范围（1-{}）", songs.len());
        }
        let song = &songs[index - 1];
        let title = song.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let value = client.request(Request::PluginCall {
            plugin: plugin.clone(),
            method: "download".into(),
            params: json!({"song": song}),
        })?;
        let call = value.get("call").and_then(|v| v.as_u64()).unwrap_or(0);
        println!("任务 #{call} 已提交：“{title}”（rmusd 后台下载，完成后自动入库）");
    }
    Ok(())
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
