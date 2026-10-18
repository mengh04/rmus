//! rmus：客户端入口。无子命令进 TUI，带子命令做一次性控制。
//!
//! 命令行子命令的定义与分发在 [`cli`]；TUI 在 `rmus::client::tui`。

mod cli;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "rmus",
    version,
    about = "rmus 客户端：无参数进入 TUI，带子命令则一次性控制 rmusd"
)]
struct Cli {
    /// rmusd socket 路径（默认 $XDG_RUNTIME_DIR/rmus.sock）
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    /// 连不上时不自动拉起 rmusd
    #[arg(long, global = true)]
    no_spawn: bool,
    #[command(subcommand)]
    command: Option<Command>,
    /// 直接播放：rmus 文件或目录...（替换当前队列并从头播）
    paths: Vec<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// 添加文件/目录到队列尾部（目录递归扫描）
    Add { paths: Vec<PathBuf> },
    /// 播放（可选队列下标，从 0 开始）
    /// 播放：无参数 = 恢复播放；纯数字 = 队列下标（从 0）；其他词 = 库模糊搜索（多词 AND，如 `rmus play secret 茅野`）
    Play { index: Vec<String> },
    /// 暂停
    Pause,
    /// 播放/暂停切换
    Toggle,
    /// 停止（保留队列）
    Stop,
    /// 下一曲
    Next,
    /// 上一曲
    Prev,
    /// 跳转：绝对 "1:23" / "90"（秒）；相对 "+10" / "-10"（秒）
    Seek { target: String },
    /// 音量 0-100："65"、"+10"、"-10"
    Volume { target: String },
    /// 显示当前状态
    Status,
    /// 列出队列
    Queue,
    /// 清空队列并停止
    Clear,
    /// 删除队列中的曲子：无参数 = 删掉正在播放的这首（自动续播下一首）；数字 = 删第 index 首
    Remove { index: Option<usize> },
    /// 打乱队列（当前曲移到队首）
    Shuffle,
    /// 循环模式：无参数循环切换 off -> all -> one -> off；或直接指定
    Repeat { mode: Option<String> },
    /// 随机播放：无参数切换；或 on/off
    Random { mode: Option<String> },
    /// 命名队列（播放列表）管理
    #[command(subcommand)]
    Pl(Pl),
    /// 库操作
    #[command(subcommand)]
    Lib(Lib),
    /// 库模糊搜索，列出候选（`rmus find secret`）
    Find { query: String },
    /// 试听：保存当前状态，临时换成这些文件播放；播完自动恢复原队列与进度
    Peek { paths: Vec<PathBuf> },
    /// 结束试听，恢复进入试听前的队列与进度
    Back,
    /// 让 rmusd 退出
    Quit,
}

#[derive(Subcommand)]
enum Pl {
    /// 列出所有命名队列
    List,
    /// 创建空队列
    New { name: String },
    /// 把当前队列保存为命名队列（覆盖同名；当前队列即变为该命名队列）
    Save { name: String },
    /// 切换到命名队列并从头播放
    Play { name: String },
    /// 添加文件/目录到命名队列（不存在则创建；正播放该队列时立即生效）
    Add { name: String, paths: Vec<PathBuf> },
    /// 删除命名队列（正在播放的除外）
    Remove { name: String },
}

#[derive(Subcommand)]
enum Lib {
    /// 全量重建库索引（扫描 ~/Music）
    Rescan,
}

fn main() -> Result<()> {
    cli::run(Cli::parse())
}
