//! 在线 tab 的业务逻辑：搜索（后台线程）、下载、取消、事件落地。
//!
//! 搜索是慢调用，走 [`Daemon::spawn_request`] 后台线程，回执由
//! `pump_events` 统一收（`AsyncReply::OnlineSearch`，generation 防过期覆盖）。
//! 下载是长任务：请求立刻返回任务号，进度/结果经 `Event::Plugin`
//! 推送，在 [`App::apply_plugin_event`] 落到 `downloads` 表。

use super::state::{AsyncReply, DownloadState, DownloadStatus, OnlineSong, OnlineState};
use super::App;
use crate::proto::Request;
use std::path::PathBuf;

impl App {
    /// 确认搜索输入后调用：发后台请求，结果回来自动落表。
    pub(super) fn run_online_search(&mut self, raw: &str) {
        let query = raw.trim().to_string();
        if query.is_empty() {
            return;
        }
        let Some(plugin) = self.online_plugin.clone() else {
            self.notify("在线搜索不可用：没有提供 search/download 的插件", true);
            return;
        };
        self.search_generation += 1;
        self.notify(format!("搜索 “{query}”…"), false);
        let rx = self.daemon.spawn_request(Request::PluginCall {
            plugin: plugin.name.clone(),
            method: "search".into(),
            params: serde_json::json!({"query": query}),
        });
        let tx = self.replies_tx.clone();
        let generation = self.search_generation;
        std::thread::spawn(move || {
            let result = rx.recv().unwrap_or_else(|_| Err(anyhow::anyhow!("后台请求中断")));
            let _ = tx.send(AsyncReply::OnlineSearch {
                generation,
                query,
                result,
            });
        });
    }

    /// 搜索回执落地（只认最新一代；旧请求的结果直接丢）。
    pub(super) fn apply_online_search(
        &mut self,
        query: &str,
        result: anyhow::Result<serde_json::Value>,
    ) {
        match result {
            Ok(value) => {
                let songs: Vec<OnlineSong> =
                    serde_json::from_value(value.get("songs").cloned().unwrap_or_default())
                        .unwrap_or_default();
                let count = songs.len();
                self.online = Some(OnlineState {
                    query: query.to_string(),
                    songs,
                    selected: 0,
                });
                self.scroll_online = 0;
                if count == 0 {
                    self.notify(format!("“{query}” 没有搜索结果"), true);
                } else {
                    self.notify(format!("“{query}” 找到 {count} 首"), false);
                }
            }
            Err(e) => self.notify(format!("搜索失败：{e}"), true),
        }
    }

    /// d：下载选中结果（完成后入库）；peek = 完成后自动试听（Enter）。
    pub(super) fn download_selected(&mut self, peek: bool) {
        let Some(plugin) = self.online_plugin.clone() else {
            self.notify("在线下载不可用：没有提供 search/download 的插件", true);
            return;
        };
        let Some(song) = self
            .online
            .as_ref()
            .and_then(|o| o.songs.get(o.selected))
            .cloned()
        else {
            return;
        };
        // 同一首不许重复排队（下载中的才算；已完成再下交给插件报"已存在"）
        if self.downloads.values().any(|d| {
            d.status == DownloadStatus::Running
                && d.song.source == song.source
                && d.song.song_id == song.song_id
        }) {
            self.notify("这首歌已经在下载了", true);
            return;
        }
        let params = serde_json::json!({"song": song});
        match self.daemon.request(Request::PluginCall {
            plugin: plugin.name.clone(),
            method: "download".into(),
            params,
        }) {
            Ok(value) => {
                let call = value.get("call").and_then(|v| v.as_u64()).unwrap_or(0);
                self.downloads.insert(
                    call,
                    DownloadState {
                        song,
                        received: 0,
                        total: None,
                        status: DownloadStatus::Running,
                        peek,
                    },
                );
            }
            Err(e) => self.notify(format!("{e}"), true),
        }
    }

    /// x：取消选中曲目在途的下载。
    pub(super) fn cancel_selected_download(&mut self) {
        let Some(plugin) = self.online_plugin.clone() else {
            return;
        };
        let call = self
            .online
            .as_ref()
            .and_then(|o| o.songs.get(o.selected))
            .and_then(|song| {
                self.downloads.iter().find_map(|(call, d)| {
                    (d.status == DownloadStatus::Running
                        && d.song.source == song.source
                        && d.song.song_id == song.song_id)
                        .then_some(*call)
                })
            });
        let Some(call) = call else {
            return;
        };
        if let Err(e) = self.daemon.request(Request::PluginCancel {
            plugin: plugin.name,
            call,
        }) {
            self.notify(format!("{e}"), true);
        }
    }

    /// `Event::Plugin` 落地：更新下载进度 / 完成态（试听在这里触发）。
    pub(super) fn apply_plugin_event(
        &mut self,
        plugin: &str,
        call: Option<u64>,
        data: &serde_json::Value,
    ) {
        if self.online_plugin.as_ref().is_none_or(|p| p.name != plugin) {
            return;
        }
        let Some(call) = call else { return };

        // 进度通知：{"received":N,"total":M}
        if let Some(received) = data.get("received").and_then(|v| v.as_u64())
            && let Some(download) = self.downloads.get_mut(&call)
            && download.status == DownloadStatus::Running
        {
            download.received = received;
            if let Some(total) = data.get("total").and_then(|v| v.as_u64()) {
                download.total = Some(total);
            }
            return;
        }

        // 任务完结（宿主包装）：{"event":"done","result":{...}} / failed
        match data.get("event").and_then(|v| v.as_str()) {
            Some("done") => {
                let Some(download) = self.downloads.get_mut(&call) else {
                    return;
                };
                download.status = DownloadStatus::Done;
                self.need_library = true;
                let peek = download.peek;
                let title = download.song.title.clone();
                let path = data
                    .pointer("/result/path")
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from);
                self.notify(format!("“{title}” 下载完成，已入库"), false);
                if peek
                    && let Some(path) = path
                {
                    self.simple(Request::Peek { paths: vec![path] });
                }
            }
            Some("failed") => {
                let (title, error) = match self.downloads.get_mut(&call) {
                    Some(download) => {
                        let error = data
                            .get("error")
                            .and_then(|v| v.as_str())
                            .unwrap_or("未知错误")
                            .to_string();
                        download.status = DownloadStatus::Failed(error.clone());
                        (download.song.title.clone(), error)
                    }
                    None => return,
                };
                self.notify(format!("“{title}” 下载失败：{error}"), true);
            }
            _ => {}
        }
    }
}
