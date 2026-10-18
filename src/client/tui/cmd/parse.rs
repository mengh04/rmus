//! `:命令 参数` -> [`Cmd`]。
//!
//! 只做解析，不碰 daemon：路径原样返回（`~` 展开），执行在 `app::command`。

use crate::proto::RepeatMode;
use std::path::PathBuf;

/// 解析结果；执行在 `app::command::App::run_command`。
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    QuitTui,
    KillDaemon,
    /// 播放第 n 曲（0 起）；None = 恢复/从头
    Play(Option<usize>),
    Pause,
    Seek(SeekTarget),
    Volume(VolumeTarget),
    /// None = 循环切换
    Repeat(Option<RepeatMode>),
    /// None = 随机切换
    Random(Option<bool>),
    Add(PathBuf),
    Open(PathBuf),
    Peek(PathBuf),
    PlayMatch(String),
    PlaylistNew(String),
    PlaylistPlay(String),
    PlaylistRemove(String),
    PlaylistAdd {
        name: String,
        path: PathBuf,
    },
    Rescan,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekTarget {
    /// 绝对位置（毫秒）
    Abs(u64),
    /// 相对位移（毫秒，可负）
    Rel(i64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VolumeTarget {
    /// 绝对值 0.0..=1.0
    Set(f32),
    Adjust(f32),
}

/// `line` -> [`Cmd`]；Err 为给用户看的错误消息。
pub fn parse(line: &str) -> Result<Cmd, String> {
    let (name, rest) = split_ws(line.trim());
    match name {
        "" => Err("空命令".into()),
        "q" | "quit" => match rest {
            "" => Ok(Cmd::QuitTui),
            "!" => Ok(Cmd::KillDaemon),
            _ => Err(":q 不接参数（结束 daemon 用 :q!）".into()),
        },
        "q!" | "quit!" => Ok(Cmd::KillDaemon),
        "play" => match rest.parse::<usize>() {
            Err(_) if rest.is_empty() => Ok(Cmd::Play(None)),
            Ok(0) => Err("曲目从 1 数起".into()),
            Ok(n) => Ok(Cmd::Play(Some(n - 1))),
            Err(_) => Err(format!(":play 参数应为曲号，收到 “{rest}”")),
        },
        "pause" => argless(rest, Cmd::Pause),
        "seek" => parse_time(rest).map(Cmd::Seek),
        "vol" | "volume" => parse_volume(rest).map(Cmd::Volume),
        "repeat" => Ok(Cmd::Repeat(match rest {
            "" => None,
            "off" | "关" => Some(RepeatMode::Off),
            "all" | "列表" => Some(RepeatMode::All),
            "one" | "单曲" => Some(RepeatMode::One),
            _ => return Err(":repeat 参数应为 off | all | one".into()),
        })),
        "rand" | "random" => Ok(Cmd::Random(match rest {
            "" => None,
            "on" | "开" => Some(true),
            "off" | "关" => Some(false),
            _ => return Err(":rand 参数应为 on | off".into()),
        })),
        "add" => path_arg(rest).map(Cmd::Add),
        "open" => path_arg(rest).map(Cmd::Open),
        "peek" => path_arg(rest).map(Cmd::Peek),
        "pm" | "playmatch" if !rest.is_empty() => Ok(Cmd::PlayMatch(rest.into())),
        "pm" | "playmatch" => Err(":pm 需要搜索词".into()),
        "pl" | "playlist" => parse_playlist(rest),
        "rescan" => argless(rest, Cmd::Rescan),
        "help" | "h" => argless(rest, Cmd::Help),
        _ => Err(format!("未知命令 “{name}”（:help 查看全部）")),
    }
}

fn split_ws(line: &str) -> (&str, &str) {
    match line.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest.trim()),
        None => (line, ""),
    }
}

fn argless(rest: &str, cmd: Cmd) -> Result<Cmd, String> {
    if rest.is_empty() {
        Ok(cmd)
    } else {
        Err("此命令不接参数".into())
    }
}

/// 整行剩余部分作为一个路径（可含空格），`~` 展开。
fn path_arg(rest: &str) -> Result<PathBuf, String> {
    if rest.is_empty() {
        return Err("缺少路径".into());
    }
    Ok(expand_tilde(rest))
}

fn expand_tilde(path: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(dir) = home()
    {
        return dir.join(rest);
    }
    if path == "~"
        && let Some(dir) = home()
    {
        return dir;
    }
    PathBuf::from(path)
}

/// `:pl <子命令> …`：new | play | add | del。
fn parse_playlist(rest: &str) -> Result<Cmd, String> {
    let (sub, rest) = split_ws(rest);
    match sub {
        "new" => Ok(Cmd::PlaylistNew(name_arg(rest, ":pl new <名字>")?)),
        "play" => Ok(Cmd::PlaylistPlay(name_arg(rest, ":pl play <名字>")?)),
        "del" | "rm" => Ok(Cmd::PlaylistRemove(name_arg(rest, ":pl del <名字>")?)),
        "add" => {
            let (name, path) = rest
                .split_once(char::is_whitespace)
                .ok_or(":pl add <名字> <路径>")?;
            Ok(Cmd::PlaylistAdd {
                name: name.into(),
                path: expand_tilde(path.trim()),
            })
        }
        "" => Err("用法：:pl new | play | add | del …".into()),
        _ => Err(format!("未知子命令 “{sub}”（new | play | add | del）")),
    }
}

fn name_arg(rest: &str, usage: &str) -> Result<String, String> {
    if rest.is_empty() {
        Err(usage.into())
    } else {
        Ok(rest.into())
    }
}

/// `90` / `1:23` / `1:02:03` 绝对秒；`+30` / `-1:00` 相对秒。
fn parse_time(s: &str) -> Result<SeekTarget, String> {
    if let Some(rest) = s.strip_prefix('+') {
        return Ok(SeekTarget::Rel(parse_hms(rest)? as i64 * 1000));
    }
    if let Some(rest) = s.strip_prefix('-') {
        return Ok(SeekTarget::Rel(-(parse_hms(rest)? as i64) * 1000));
    }
    Ok(SeekTarget::Abs(parse_hms(s)? * 1000))
}

/// 时:分:秒 -> 秒；分、秒段须 < 60。
fn parse_hms(s: &str) -> Result<u64, String> {
    let mut total = 0u64;
    for (i, part) in s.split(':').enumerate() {
        if i > 2 {
            return Err(format!("时间参数 “{s}” 最多 时:分:秒"));
        }
        let n: u64 = part
            .parse()
            .map_err(|_| format!("时间参数 “{s}” 应为 秒 或 分:秒"))?;
        if i > 0 && n >= 60 {
            return Err(format!("时间 “{s}” 的分/秒段应 < 60"));
        }
        total = total * 60 + n;
    }
    Ok(total)
}

/// `80` 绝对百分比；`+10` / `-10` 相对。
fn parse_volume(s: &str) -> Result<VolumeTarget, String> {
    if let Some(rest) = s.strip_prefix('+') {
        return Ok(VolumeTarget::Adjust(parse_percent(rest)?));
    }
    if let Some(rest) = s.strip_prefix('-') {
        return Ok(VolumeTarget::Adjust(-parse_percent(rest)?));
    }
    let n: f32 = s
        .parse()
        .map_err(|_| format!("音量参数 “{s}” 应为 0-100 或 ±N"))?;
    if !(0.0..=100.0).contains(&n) {
        return Err("音量应在 0-100".into());
    }
    Ok(VolumeTarget::Set(n / 100.0))
}

fn parse_percent(s: &str) -> Result<f32, String> {
    s.parse::<f32>()
        .map(|n| n / 100.0)
        .map_err(|_| format!("音量参数 “{s}” 不是数字"))
}
