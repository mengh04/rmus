//! `:` 命令面板：命令表、解析、Tab 补全。执行在 `app::App::run_command`。
//!
//! 设计取向：
//! - 含空格的路径不用引号——`add` / `open` / `peek` / `pl add` 把"剩余整行"
//!   当作一个路径（`~` 展开），命令行写起来最省事
//! - 时间 `90` / `1:23` 为绝对秒，`+30` / `-30` 为相对秒
//! - 音量 `80` 为绝对百分比，`+10` / `-10` 为相对

use crate::proto::RepeatMode;
use std::path::PathBuf;

/// 一条命令的说明（`:help` 展示 + Tab 补全；`names[0]` 是规范名）。
pub struct Spec {
    pub names: &'static [&'static str],
    pub desc: &'static str,
}

pub const SPECS: &[Spec] = &[
    Spec {
        names: &["q", "quit"],
        desc: "退出 TUI（q! 结束 rmusd）",
    },
    Spec {
        names: &["play"],
        desc: "播放第 n 曲（缺省 = 恢复/从头）",
    },
    Spec {
        names: &["pause"],
        desc: "暂停",
    },
    Spec {
        names: &["seek"],
        desc: "跳转：90 / 1:23 绝对，+30 / -30 相对（秒）",
    },
    Spec {
        names: &["vol", "volume"],
        desc: "音量：80 绝对%，+10 / -10 相对",
    },
    Spec {
        names: &["repeat"],
        desc: "循环 off | all | one（缺省切换）",
    },
    Spec {
        names: &["rand", "random"],
        desc: "随机 on | off（缺省切换）",
    },
    Spec {
        names: &["add"],
        desc: "加入队列（整行为一个路径）",
    },
    Spec {
        names: &["open"],
        desc: "替换队列并从头播放",
    },
    Spec {
        names: &["peek"],
        desc: "试听（B 结束恢复原队列）",
    },
    Spec {
        names: &["pm", "playmatch"],
        desc: "库模糊插播",
    },
    Spec {
        names: &["pl", "playlist"],
        desc: "pl new | play | add | del 名字 [路径]",
    },
    Spec {
        names: &["rescan"],
        desc: "重建库索引",
    },
    Spec {
        names: &["help", "h"],
        desc: "本帮助",
    },
];

/// 解析结果；执行在 `app::App::run_command`。
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
    PlaylistAdd { name: String, path: PathBuf },
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

/// Tab 补全：所有以 `prefix` 开头的命令名（含别名）。
pub fn complete(prefix: &str) -> Vec<&'static str> {
    SPECS
        .iter()
        .flat_map(|spec| spec.names.iter())
        .filter(|name| name.starts_with(prefix))
        .copied()
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析_基础() {
        assert_eq!(parse("q"), Ok(Cmd::QuitTui));
        assert_eq!(parse("q!"), Ok(Cmd::KillDaemon));
        assert_eq!(parse("quit !"), Ok(Cmd::KillDaemon));
        assert_eq!(parse("play"), Ok(Cmd::Play(None)));
        assert_eq!(parse("play 3"), Ok(Cmd::Play(Some(2))));
        assert_eq!(parse("  pause  "), Ok(Cmd::Pause));
        assert_eq!(parse("repeat all"), Ok(Cmd::Repeat(Some(RepeatMode::All))));
        assert_eq!(parse("repeat"), Ok(Cmd::Repeat(None)));
        assert_eq!(parse("rand off"), Ok(Cmd::Random(Some(false))));
        assert_eq!(parse("rescan"), Ok(Cmd::Rescan));
        assert_eq!(parse("h"), Ok(Cmd::Help));
    }

    #[test]
    fn 解析_seek() {
        assert_eq!(parse("seek 90"), Ok(Cmd::Seek(SeekTarget::Abs(90_000))));
        assert_eq!(parse("seek 1:23"), Ok(Cmd::Seek(SeekTarget::Abs(83_000))));
        assert_eq!(
            parse("seek 1:02:03"),
            Ok(Cmd::Seek(SeekTarget::Abs(3_723_000)))
        );
        assert_eq!(parse("seek +30"), Ok(Cmd::Seek(SeekTarget::Rel(30_000))));
        assert_eq!(parse("seek -1:00"), Ok(Cmd::Seek(SeekTarget::Rel(-60_000))));
        assert!(parse("seek 1:99").is_err());
        assert!(parse("seek abc").is_err());
    }

    #[test]
    fn 解析_音量() {
        assert_eq!(parse("vol 80"), Ok(Cmd::Volume(VolumeTarget::Set(0.8))));
        assert_eq!(parse("vol +10"), Ok(Cmd::Volume(VolumeTarget::Adjust(0.1))));
        assert_eq!(parse("volume -5"), Ok(Cmd::Volume(VolumeTarget::Adjust(-0.05))));
        assert!(parse("vol 200").is_err());
        assert!(parse("vol x").is_err());
    }

    #[test]
    fn 解析_路径与播放列表() {
        // 整行为一个路径，空格不用引号
        assert_eq!(
            parse("add ~/Music/My Album/01 song.flac"),
            Ok(Cmd::Add(home_join("Music/My Album/01 song.flac")))
        );
        assert_eq!(parse("pm 晚风 王菲"), Ok(Cmd::PlayMatch("晚风 王菲".into())));
        assert_eq!(
            parse("pl new 我喜欢的"),
            Ok(Cmd::PlaylistNew("我喜欢的".into()))
        );
        assert_eq!(
            parse("pl play 我喜欢的"),
            Ok(Cmd::PlaylistPlay("我喜欢的".into()))
        );
        assert_eq!(
            parse("pl del 我喜欢的"),
            Ok(Cmd::PlaylistRemove("我喜欢的".into()))
        );
        assert_eq!(
            parse("pl add 歌单 ~/x.flac"),
            Ok(Cmd::PlaylistAdd {
                name: "歌单".into(),
                path: home_join("x.flac"),
            })
        );
    }

    #[test]
    fn 解析_错误() {
        assert!(parse("bogus").is_err());
        assert!(parse("play 0").is_err());
        assert!(parse("play x").is_err());
        assert!(parse("add").is_err());
        assert!(parse("pm").is_err());
        assert!(parse("pl").is_err());
        assert!(parse("pl add 名字").is_err());
        assert!(parse("repeat 抖腿").is_err());
    }

    #[test]
    fn 补全() {
        let v = complete("v");
        assert!(v.contains(&"vol") && v.contains(&"volume"));
        assert!(complete("pl").contains(&"playlist"));
        assert!(complete("zzz").is_empty());
    }

    fn home_join(rel: &str) -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("测试环境应有 HOME")
            .join(rel)
    }
}
