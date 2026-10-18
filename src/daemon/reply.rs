//! 出站回复编码：引擎结果 / 错误行 -> NDJSON 字符串。

use rmus::proto::ServerMsg;
use tokio::sync::{mpsc, oneshot};

pub(crate) fn send_reply(
    id: u64,
    result: Result<Result<rmus::proto::Reply, String>, oneshot::error::RecvError>,
    out_tx: &mpsc::UnboundedSender<String>,
) {
    let server_msg = match result {
        Ok(Ok(reply)) => ServerMsg::Reply { id, result: reply },
        Ok(Err(error)) => error_reply(id, error),
        Err(_) => error_reply(id, "引擎无响应".into()),
    };
    if let Ok(json) = serde_json::to_string(&server_msg) {
        let _ = out_tx.send(json);
    }
}

pub(crate) fn error_reply(id: u64, error: String) -> ServerMsg {
    ServerMsg::Error { id, error }
}
