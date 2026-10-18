//! 单条客户端连接：出站写任务、事件转发、读循环。

use crate::Library;
use crate::dispatch::dispatch;
use crate::plugin_host::PluginHost;
use crate::reply::error_reply;
use rmus::engine::ToEngine;
use rmus::proto::{RequestMsg, ServerMsg};
use std::sync::mpsc as std_mpsc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc};

pub(crate) async fn handle_conn(
    stream: UnixStream,
    cmd_tx: std_mpsc::Sender<ToEngine>,
    mut events: broadcast::Receiver<rmus::proto::Event>,
    library: Library,
    plugins: PluginHost,
) {
    let (read_half, write_half) = stream.into_split();

    // 出站队列：请求响应与事件都走这里，由单独的写任务串行写出
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let write_task = tokio::spawn(async move {
        let mut writer = write_half;
        while let Some(line) = out_rx.recv().await {
            if writer.write_all(line.as_bytes()).await.is_err()
                || writer.write_all(b"\n").await.is_err()
            {
                break;
            }
            let _ = writer.flush().await;
        }
    });

    // 事件转发：广播 -> 本连接出站队列
    let event_out = out_tx.clone();
    let event_task = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    if let Ok(json) = serde_json::to_string(&ServerMsg::Event(event)) {
                        // 对端关闭（send 失败）则结束
                        if event_out.send(json).is_err() {
                            break;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // 读循环：解析每行请求并等待引擎响应
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // 对端关闭
            Ok(_) => {}
            Err(_) => break,
        }
        let msg: RequestMsg = match serde_json::from_str(line.trim_end()) {
            Ok(msg) => msg,
            Err(e) => {
                let json = serde_json::to_string(&error_reply(0, format!("请求格式错误: {e}")))
                    .unwrap_or_default();
                let _ = out_tx.send(json);
                continue;
            }
        };
        dispatch(msg, &cmd_tx, &out_tx, &library, &plugins).await;
    }

    drop(out_tx);
    let _ = write_task.await;
    event_task.abort();
}
