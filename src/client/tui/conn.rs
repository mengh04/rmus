//! 与 rmusd 的连接封装：主线程用一条连接发请求，另一条专用连接由后台线程
//! 阻塞读 daemon 的事件推送，经 channel 交给主循环。
//!
//! 两条连接互不干扰：请求连接上的事件行由 `Client::request` 跳过，
//! 事件连接上只收 `wait_event`。

use crate::client::Client;
use crate::proto::Event;
use anyhow::Result;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

/// 后台线程交给主循环的消息。
pub enum Feed {
    /// daemon 推送的播放器事件
    Daemon(Event),
    /// 事件连接断开（daemon 退出/崩溃）
    Disconnected,
}

pub struct Daemon {
    client: Client,
    feed_rx: Receiver<Feed>,
}

impl Daemon {
    /// 连接 rmusd（默认连不上时自动拉起），并启动事件接收线程。
    pub fn connect(socket: Option<PathBuf>, no_spawn: bool) -> Result<Self> {
        let client = if no_spawn {
            Client::connect(socket.clone())?
        } else {
            Client::connect_or_spawn(socket.clone())?
        };
        // 主连接已保证 daemon 在跑，事件连接直接连即可
        let mut events_client = Client::connect(socket)?;

        let (tx, feed_rx) = channel();
        std::thread::Builder::new()
            .name("rmus-events".into())
            .spawn(move || {
                loop {
                    match events_client.wait_event() {
                        Ok(event) => {
                            if tx.send(Feed::Daemon(event)).is_err() {
                                break; // 主循环已退出
                            }
                        }
                        Err(_) => {
                            let _ = tx.send(Feed::Disconnected);
                            break;
                        }
                    }
                }
            })?;

        Ok(Self { client, feed_rx })
    }

    /// 发请求并返回 daemon 的 `result` 字段。
    pub fn request(&mut self, request: crate::proto::Request) -> Result<serde_json::Value> {
        self.client.request(request)
    }

    /// 测试/预览用：不连 daemon 的空壳，只为让 UI 能拿到状态。
    #[cfg(test)]
    pub fn detached() -> Self {
        let (client_end, _server_end) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let (_tx, feed_rx) = channel();
        Self {
            client: Client::from_stream(client_end),
            feed_rx,
        }
    }

    /// 主循环用：非阻塞地取走当前积压的所有推送。
    pub fn try_drain(&self) -> Vec<Feed> {
        let mut feeds = Vec::new();
        while let Ok(feed) = self.feed_rx.try_recv() {
            feeds.push(feed);
        }
        feeds
    }
}
