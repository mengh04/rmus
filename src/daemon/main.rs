//! rmusd：播放守护进程。
//!
//! 职责：Unix socket 上接受多个客户端连接（NDJSON 协议），把命令转交给
//! 引擎线程，把引擎事件广播给所有连接。目录扫描在本层完成（spawn_blocking），
//! 引擎只收扫描好的曲子列表。

use anyhow::{Context, Result, bail};
use clap::Parser;
use rmus::engine::{Engine, ToEngine};
use rmus::engine::queue::QueueItem;
use rmus::proto::{Reply, Request, RequestMsg, ServerMsg, TrackInfo};
use rmus::{library, scan, socket_path};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, oneshot};

#[cfg(target_os = "linux")]
mod mpris;

/// 库索引共享状态（读多写少；临界区只是内存 Vec，std 锁足够）
type Library = Arc<RwLock<Vec<QueueItem>>>;

/// 事件防抖窗口：批量拷贝会爆发大量 inotify 事件，静默 2 秒后才扫一次
const LIB_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(2);

/// 起后台线程监听库目录：文件变化后防抖、全量重建索引。
/// 专用阻塞线程（事件等待 + 扫描都是阻塞操作，不占 tokio worker）。
/// 静息态零轮询（inotify 推送）；监听失败只降级警告，手动 rescan 仍可用。
fn spawn_library_watcher(library: Library) -> Result<()> {
    let root = library::root();
    std::thread::Builder::new()
        .name("lib-watch".into())
        .spawn(move || {
            let (tx, rx) = std_mpsc::channel::<()>();
            use notify::Watcher;
            let mut watcher = match notify::recommended_watcher(move |_res| {
                // 事件内容不重要（增删改都导致重扫），只用来喂防抖
                let _ = tx.send(());
            }) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("[rmusd] 库监听不可用（rmus lib rescan 手动更新）: {e}");
                    return;
                }
            };
            if let Err(e) = watcher.watch(root.as_path(), notify::RecursiveMode::Recursive) {
                eprintln!("[rmusd] 监听 {} 失败: {e}", root.display());
                return;
            }
            loop {
                if rx.recv().is_err() {
                    break;
                }
                while rx.recv_timeout(LIB_DEBOUNCE).is_ok() {}
                let items = library::rescan();
                println!("[rmusd] 库已自动更新：{} 首", items.len());
                *library.write().unwrap() = items;
            }
        })
        .context("启动库监听线程失败")?;
    Ok(())
}

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

        // MPRIS：桌面媒体控制/媒体键/播放器小组件（可选，无 D-Bus 时降级警告）
        #[cfg(target_os = "linux")]
        match mpris::spawn(cmd_tx.clone(), events_tx.subscribe()).await {
            Ok(()) => println!("MPRIS: org.mpris.MediaPlayer2.rmus"),
            Err(e) => eprintln!("[rmusd] MPRIS 服务不可用（无桌面会话？）: {e:#}"),
        }

        // 库索引：载入缓存；缓存为空（首次使用）则后台全量扫描 ~/Music
        let library: Library = Arc::new(RwLock::new(library::load()));
        if library.read().unwrap().is_empty() {
            let lib = library.clone();
            tokio::spawn(async move {
                let items =
                    tokio::task::spawn_blocking(library::rescan).await.unwrap_or_default();
                println!("[rmusd] 库索引已建立：{} 首", items.len());
                *lib.write().unwrap() = items;
            });
        }
        // 实时监听库目录：新文件落地后自动更新索引（防抖 2s）
        spawn_library_watcher(library.clone())?;

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
                    tokio::spawn(handle_conn(
                        stream,
                        cmd_tx.clone(),
                        events_tx.subscribe(),
                        library.clone(),
                    ));
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
    library: Library,
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
        dispatch(msg, &cmd_tx, &out_tx, &library).await;
    }

    drop(out_tx);
    let _ = write_task.await;
    event_task.abort();
}

async fn dispatch(
    msg: RequestMsg,
    cmd_tx: &std_mpsc::Sender<ToEngine>,
    out_tx: &mpsc::UnboundedSender<String>,
    library: &Library,
) {
    let id = msg.id;

    // 库模糊搜索：查索引直接回（不进引擎）
    if let Request::Find { query } = msg.request {
        let tracks = {
            let lib = library.read().unwrap();
            library::matches(&lib, &query)
                .into_iter()
                .take(10)
                .enumerate()
                .map(|(i, item)| TrackInfo {
                    index: i,
                    path: item.path.clone(),
                    title: item.title.clone(),
                    artist: item.artist.clone(),
                    album: item.album.clone(),
                    duration_ms: item.duration_ms,
                })
                .collect()
        };
        let json = serde_json::to_string(&ServerMsg::Reply {
            id,
            result: Reply::Matches { tracks },
        })
        .unwrap_or_default();
        let _ = out_tx.send(json);
        return;
    }

    // 库模糊播放：最佳匹配转引擎插播
    if let Request::PlayMatch { query } = msg.request {
        let best = {
            let lib = library.read().unwrap();
            library::matches(&lib, &query).into_iter().next().cloned()
        };
        match best {
            Some(item) => {
                let (reply_tx, reply_rx) = oneshot::channel();
                if cmd_tx
                    .send(ToEngine::PlayNow {
                        id,
                        item,
                        reply: reply_tx,
                    })
                    .is_err()
                {
                    return;
                }
                send_reply(id, reply_rx.await, out_tx);
            }
            None => {
                let json = serde_json::to_string(&error_reply(
                    id,
                    format!("库中没有匹配 “{query}” 的曲子"),
                ))
                .unwrap_or_default();
                let _ = out_tx.send(json);
            }
        }
        return;
    }

    // 全量重建库索引（阻塞线程池扫描），完成后更新共享索引
    if matches!(msg.request, Request::LibRescan) {
        let lib = library.clone();
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            let items =
                tokio::task::spawn_blocking(library::rescan).await.unwrap_or_default();
            let added = items.len();
            *lib.write().unwrap() = items;
            let json = serde_json::to_string(&ServerMsg::Reply {
                id,
                result: Reply::Added { added },
            })
            .unwrap_or_default();
            let _ = out_tx.send(json);
        });
        return;
    }

    let replace = matches!(msg.request, Request::Open { .. });
    let peeking = matches!(msg.request, Request::Peek { .. });
    if let Request::Add { paths } | Request::Open { paths } | Request::Peek { paths } =
        msg.request
    {
        // 扫描可能耗时（大目录 + 逐文件读 tag），放阻塞线程池
        let cmd_tx = cmd_tx.clone();
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            let items = tokio::task::spawn_blocking(move || scan::collect(&paths))
                .await
                .unwrap_or_default();
            let (reply_tx, reply_rx) = oneshot::channel();
            let sent = if peeking {
                cmd_tx.send(ToEngine::PeekScanned {
                    id,
                    items,
                    reply: reply_tx,
                })
            } else if replace {
                cmd_tx.send(ToEngine::OpenScanned {
                    id,
                    items,
                    reply: reply_tx,
                })
            } else {
                cmd_tx.send(ToEngine::AddScanned {
                    id,
                    items,
                    reply: reply_tx,
                })
            };
            if sent.is_err() {
                return;
            }
            send_reply(id, reply_rx.await, &out_tx);
        });
        return;
    }

    if let Request::PlaylistAdd { name, paths } = msg.request {
        let cmd_tx = cmd_tx.clone();
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            let items = tokio::task::spawn_blocking(move || scan::collect(&paths))
                .await
                .unwrap_or_default();
            let (reply_tx, reply_rx) = oneshot::channel();
            let sent = cmd_tx.send(ToEngine::PlaylistAddScanned {
                id,
                name,
                items,
                reply: reply_tx,
            });
            if sent.is_err() {
                return;
            }
            send_reply(id, reply_rx.await, &out_tx);
        });
        return;
    }

    if matches!(msg.request, Request::Quit) {
        // 先删 socket：立即让后续连接失败，避免竞态（新客户端连上将死的
        // daemon / 新 daemon 误判"已在运行"）；再等响应写出后退出
        let _ = std::fs::remove_file(rmus::socket_path());
        if let Ok(json) = serde_json::to_string(&ServerMsg::Reply {
            id,
            result: rmus::proto::Reply::Ok,
        }) {
            let _ = out_tx.send(json);
        }
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
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
