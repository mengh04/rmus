//! `:` 命令执行：解析结果 -> daemon 请求 / 本地状态。

use super::super::cmd::{Cmd, SeekTarget, VolumeTarget};
use super::App;
use crate::proto::Request;

impl App {
    pub(super) fn run_command(&mut self, command: Cmd) {
        match command {
            Cmd::QuitTui => self.should_quit = true,
            Cmd::KillDaemon => {
                let _ = self.daemon.request(Request::Quit);
                self.should_quit = true;
            }
            Cmd::Play(index) => self.simple(Request::Play { index }),
            Cmd::Pause => self.simple(Request::Pause),
            Cmd::Seek(target) => match target {
                SeekTarget::Abs(position_ms) => self.simple(Request::Seek { position_ms }),
                SeekTarget::Rel(delta_ms) => self.simple(Request::SeekRelative { delta_ms }),
            },
            Cmd::Volume(target) => match target {
                VolumeTarget::Set(value) => self.simple(Request::SetVolume { value }),
                VolumeTarget::Adjust(delta) => self.simple(Request::AdjustVolume { delta }),
            },
            Cmd::Repeat(mode) => self.simple(Request::SetRepeat { mode }),
            Cmd::Random(on) => self.simple(Request::SetRandom { on }),
            Cmd::Add(path) => self.load_paths(|paths| Request::Add { paths }, path, "已加入队列"),
            Cmd::Open(path) => {
                self.load_paths(|paths| Request::Open { paths }, path, "已替换队列并播放")
            }
            Cmd::Peek(path) => {
                self.load_paths(|paths| Request::Peek { paths }, path, "试听中，B 结束恢复")
            }
            Cmd::PlayMatch(query) => {
                // 先查一次拿到最佳匹配的名字，用于插播提示（匹配逻辑同一份）
                let best = self
                    .daemon
                    .request(Request::Find {
                        query: query.clone(),
                    })
                    .ok()
                    .and_then(|value| super::state::parse_tracks(&value).into_iter().next());
                if let Err(e) = self.daemon.request(Request::PlayMatch { query }) {
                    self.notify(format!("{e}"), true);
                } else if let Some(track) = best {
                    self.notify(format!("插播 {} - {}", track.title, track.artist), false);
                }
                self.need_status = true;
                self.need_queue = true;
            }
            Cmd::PlaylistNew(name) => {
                self.simple(Request::PlaylistNew { name: name.clone() });
                self.need_playlists = true;
                self.notify(format!("已创建播放列表 “{name}”"), false);
            }
            Cmd::PlaylistPlay(name) => {
                self.simple(Request::PlaylistPlay { name });
                self.need_playlists = true;
                self.need_queue = true;
            }
            Cmd::PlaylistRemove(name) => {
                self.simple(Request::PlaylistRemove { name: name.clone() });
                self.need_playlists = true;
                self.notify(format!("已删除播放列表 “{name}”"), false);
            }
            Cmd::PlaylistAdd { name, path } => {
                self.simple(Request::PlaylistAdd {
                    name: name.clone(),
                    paths: vec![path],
                });
                self.need_playlists = true;
                self.notify(format!("已加入播放列表 “{name}”"), false);
            }
            Cmd::Rescan => self.rescan_library(),
            Cmd::Help => self.help_open = true,
        }
    }

    /// add / open / peek 共用：路径扫描在 daemon 层，回复后刷新。
    fn load_paths(
        &mut self,
        build: fn(Vec<std::path::PathBuf>) -> Request,
        path: std::path::PathBuf,
        done: &str,
    ) {
        if let Err(e) = self.daemon.request(build(vec![path])) {
            self.notify(format!("{e}"), true);
        } else {
            self.notify(done, false);
        }
        self.need_status = true;
        self.need_queue = true;
    }
}
