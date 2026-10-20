//! rmusd：播放守护进程。
//!
//! 职责：Unix socket 上接受多个客户端连接（NDJSON 协议），把命令转交给
//! 引擎线程，把引擎事件广播给所有连接。目录扫描在本层完成（spawn_blocking），
//! 引擎只收扫描好的曲子列表。
//!
//! 子模块：
//! - [`conn`]：单条连接（读写任务、事件转发）
//! - [`dispatch`]：请求分发（库查询就地回，扫描丢阻塞线程池，其余进引擎）
//! - [`plugin_host`]：插件宿主（发现、会话、调用转发）
//! - [`reply`]：回复/错误行编码
//! - [`watch`]：库目录监听
//! - [`mpris`]：Linux 桌面媒体集成

mod conn;
mod dispatch;
mod mpris;
mod plugin_host;
mod reply;
mod watch;

use anyhow::{Context, Result, bail};
use clap::Parser;
use rmus::engine::{Engine, ToEngine};
use rmus::library::{self, LibEntry};
use rmus::socket_path;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, RwLock};
use tokio::net::UnixListener;
use tokio::sync::broadcast;

use conn::handle_conn;
use watch::spawn_library_watcher;

/// 库索引共享状态（读多写少；临界区只是内存 Vec，std 锁足够）
pub(crate) type Library = Arc<RwLock<Vec<LibEntry>>>;

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
        let listener =
            UnixListener::bind(&sock).with_context(|| format!("监听 {} 失败", sock.display()))?;
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
                let items = tokio::task::spawn_blocking(library::rescan)
                    .await
                    .unwrap_or_default();
                println!("[rmusd] 库索引已建立：{} 首", items.len());
                *lib.write().unwrap() = items;
            });
        }
        // 实时监听库目录：新文件落地后自动更新索引（防抖 2s）
        spawn_library_watcher(library.clone())?;

        // 插件宿主：扫描插件目录、握手、按需托管会话
        let plugins = plugin_host::PluginHost::discover(events_tx.clone(), cmd_tx.clone()).await;
        if plugins.is_empty() {
            println!("[rmusd] 未发现插件（可选装，不影响播放）");
        }

        // 信号处理：退出时清理 socket 文件与插件子进程
        let sock_for_cleanup = sock.clone();
        let plugins_for_signal = plugins.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut term = signal(SignalKind::terminate()).expect("注册 SIGTERM");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            let _ = std::fs::remove_file(&sock_for_cleanup);
            plugins_for_signal.shutdown();
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
                        plugins.clone(),
                    ));
                }
                Err(e) => eprintln!("[rmusd] 接受连接失败: {e}"),
            }
        }
    })
}
