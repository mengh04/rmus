//! MPRIS（org.mpris.MediaPlayer2.rmus）服务端。
//!
//! 桌面环境/媒体键/playerctl 通过 D-Bus 控制 rmusd：
//! - MPRIS 方法 -> ToEngine 命令（等待引擎回执再返回）
//! - 引擎事件 -> 缓存最新状态 + properties_changed / Seeked 信号
//!
//! 状态缓存：事件桥任务维护一份 StatusInfo 快照（Position 事件就地更新
//! position，其余事件拉一次全量），MPRIS 属性查询全部读缓存，不查引擎。
//!
//! 子模块：
//! - [`art`]：封面临时文件（MPRIS 只能给 URL）
//! - [`metadata`]：从 tag 取封面/歌词，拼 Metadata
//! - [`events`]：引擎事件 -> 属性变更 / Seeked 信号
//! - [`root`] / [`player`]：两组 D-Bus 接口实现

mod art;
mod events;
mod metadata;
mod player;
mod root;

use art::ArtCache;
use events::handle_event;
use mpris_server::Server;
use rmus::engine::ToEngine;
use rmus::proto::{Event, Request, StatusInfo};
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use tokio::sync::{RwLock, broadcast, oneshot};
use zbus::fdo;

/// Position 事件间隔的公差：超过它视为用户 seek（发 Seeked 信号）
const SEEK_DETECT_MS: i64 = 2000;

struct Shared {
    status: StatusInfo,
    /// 上一次 Position 事件的值，用于检测 seek 跳变
    last_position: Option<u64>,
    /// 当前曲封面的 file:// URL
    art_url: Option<String>,
    /// 当前曲歌词（内嵌 tag 原文，可能是 LRC）
    lyrics: Option<String>,
    /// 封面临时文件管理
    art: ArtCache,
}

pub struct Mpris {
    cmd_tx: std_mpsc::Sender<ToEngine>,
    shared: Arc<RwLock<Shared>>,
}

/// 启动 MPRIS 服务与事件桥。失败（如无 D-Bus 会话）由调用方决定是否致命。
/// 启动 MPRIS 服务与事件桥。失败（如无 D-Bus 会话）由调用方决定是否致命。
pub async fn spawn(
    cmd_tx: std_mpsc::Sender<ToEngine>,
    mut events: broadcast::Receiver<Event>,
) -> anyhow::Result<()> {
    ArtCache::cleanup_orphans();
    let shared = Arc::new(RwLock::new(Shared {
        status: fetch_status(&cmd_tx).await.unwrap_or_default(),
        last_position: None,
        art_url: None,
        lyrics: None,
        art: ArtCache::new(),
    }));

    let server = Server::new(
        "rmus",
        Mpris {
            cmd_tx: cmd_tx.clone(),
            shared: shared.clone(),
        },
    )
    .await?;

    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => handle_event(&server, &shared, &cmd_tx, event).await,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    Ok(())
}

pub(super) async fn fetch_status(cmd_tx: &std_mpsc::Sender<ToEngine>) -> Option<StatusInfo> {
    let (reply_tx, reply_rx) = oneshot::channel();
    cmd_tx
        .send(ToEngine::Request {
            id: 0,
            request: Request::Status,
            reply: reply_tx,
        })
        .ok()?;
    match reply_rx.await {
        Ok(Ok(rmus::proto::Reply::Status(status))) => Some(*status),
        _ => None,
    }
}

impl Mpris {
    async fn request(&self, request: Request) -> fdo::Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(ToEngine::Request {
                id: 0,
                request,
                reply: reply_tx,
            })
            .map_err(|_| fdo::Error::Failed("引擎不可用".into()))?;
        match reply_rx.await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(fdo::Error::Failed(e)),
            Err(_) => Err(fdo::Error::Failed("引擎无响应".into())),
        }
    }

    async fn status(&self) -> StatusInfo {
        self.shared.read().await.status.clone()
    }
}
