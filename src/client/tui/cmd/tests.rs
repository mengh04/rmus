//! 命令表与解析的单元测试。

use super::parse::{Cmd, SeekTarget, VolumeTarget, parse};
use super::{SPECS, filter};
use crate::proto::RepeatMode;
use std::path::PathBuf;

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
    assert_eq!(
        parse("volume -5"),
        Ok(Cmd::Volume(VolumeTarget::Adjust(-0.05)))
    );
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
    assert_eq!(
        parse("pm 晚风 王菲"),
        Ok(Cmd::PlayMatch("晚风 王菲".into()))
    );
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
fn 过滤() {
    // 空查询 = 全部，保持 SPECS 顺序
    assert_eq!(filter("").len(), SPECS.len());
    assert_eq!(filter("seek")[0].names[0], "seek");
    // 名称包含与描述命中
    assert!(filter("random").iter().any(|s| s.names.contains(&"rand")));
    assert!(filter("播放").iter().any(|s| s.names.contains(&"play")));
    assert!(filter("zzz").is_empty());
    // 只看第一个词：带参数的行仍按命令名过滤
    assert_eq!(filter("seek 30")[0].names[0], "seek");
}

fn home_join(rel: &str) -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("测试环境应有 HOME")
        .join(rel)
}
