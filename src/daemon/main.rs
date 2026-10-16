//! rmusd：播放守护进程。
//!
//! 职责：Unix socket 上接受多个客户端连接（NDJSON 协议），把命令转交给
//! 引擎线程，把引擎事件广播给所有连接。目录扫描在本层完成（spawn_blocking），
//! 引擎只收扫描好的曲子列表。

use anyhow::{Context, Result, bail};
use clap::Parser;
use rmus::engine::{Engine, ToEngine};
use rmus::proto::{Request, RequestMsg, ServerMsg};
use rmus::{scan, socket_path};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};

#[derive(Parser)]
#[command(name = "rmusd", version, about = "rmus 播放守护进程")]
struct Args {
    /// 监听的 socket 路径（默认 $XDG_RUNTIME_DIR/rmus.sock）
    #[arg(long)]
    socket: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let sock = args.socket.unwrap_or_else(socket_path);

    // 已有实例则退出（能连通说明活着；连不通是陈旧文件，删掉）
    if sock.exists() {
        if StdUnixStream::connect(&sock).is_ok() {
            bail!("rmusd 已在运行（{}）", sock.display());
        }
        std::fs::remove_file(&sock).context("删除陈旧 socket 文件失败")?;
    }

    let (cmd_tx, cmd_rx) = std_mpsc::channel::<ToEngine>();
    let (events_tx, _events_rx) = broadcast::channel::<rmus::proto::Event>(64);

    Engine::spawn(cmd_tx.clone(), cmd_rx, events_tx.clone())
        .context("启动播放引擎失败（没有音频设备？）")?;

    let runtime = tokio::runtime::Runtime::new().context("创建 tokio runtime 失败")?;
    runtime.block_on(async move {
        let listener = UnixListener::bind(&sock).with_context(|| {
            format!("监听 {} 失败", sock.display())
        })?;
        println!("rmusd 监听于 {}", sock.display());

        // 信号处理：退出时清理 socket 文件
        let sock_for_cleanup = sock.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = signal(SignalKind::terminate()).expect("注册 SIGTERM");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            let _ = std::fs::remove_file(&sock_for_cleanup);
            std::process::exit(0);
        });

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(handle_conn(stream, cmd_tx.clone(), events_tx.subscribe()));
                }
                Err(e) => eprintln!("[rmusd] 接受连接失败: {e}"),
            }
        }
    })
}

async fn handle_conn(
    stream: UnixStream,
    cmd_tx: std_mpsc::Sender<ToEngine>,
    mut events: broadcast::Receiver<rmus::proto::Event>,
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
        dispatch(msg, &cmd_tx, &out_tx).await;
    }

    drop(out_tx);
    let _ = write_task.await;
    event_task.abort();
}

async fn dispatch(
    msg: RequestMsg,
    cmd_tx: &std_mpsc::Sender<ToEngine>,
    out_tx: &mpsc::UnboundedSender<String>,
) {
    let id = msg.id;

    if let Request::Add { paths } = msg.request {
        // add 可能耗时（大目录 + 逐文件读 tag），放阻塞线程池
        let cmd_tx = cmd_tx.clone();
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            let items = tokio::task::spawn_blocking(move || scan::collect(&paths))
                .await
                .unwrap_or_default();
            let (reply_tx, reply_rx) = oneshot::channel();
            if cmd_tx
                .send(ToEngine::AddScanned {
                    id,
                    items,
                    reply: reply_tx,
                })
                .is_err()
            {
                return;
            }
            send_reply(id, reply_rx.await, &out_tx);
        });
        return;
    }

    if matches!(msg.request, Request::Quit) {
        // 先让响应写出去再退出
        if let Ok(json) = serde_json::to_string(&ServerMsg::Reply {
            id,
            result: rmus::proto::Reply::Ok,
        }) {
            let _ = out_tx.send(json);
        }
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let _ = std::fs::remove_file(rmus::socket_path());
            std::process::exit(0);
        });
        return;
    }

    let (reply_tx, reply_rx) = oneshot::channel();
    if cmd_tx
        .send(ToEngine::Request {
            id,
            request: msg.request,
            reply: reply_tx,
        })
        .is_err()
    {
        return;
    }
    send_reply(id, reply_rx.await, out_tx);
}

fn send_reply(
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

fn error_reply(id: u64, error: String) -> ServerMsg {
    ServerMsg::Error { id, error }
}
