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
    /// 调整队列顺序：把第 from 首移到第 to 的位置（下标从 0 开始）
    Move { from: usize, to: usize },
    /// 打乱队列（当前曲移到队首）
    Shuffle,
    /// 循环模式：无参数循环切换 off -> all -> one -> off；或直接指定
    Repeat { mode: Option<String> },
    /// 随机播放：无参数切换；或 on/off
    Random { mode: Option<String> },
    /// 播放列表管理
    #[command(subcommand)]
    Pl(Pl),
    /// 库操作
    #[command(subcommand)]
    Lib(Lib),
    /// 库模糊搜索，列出候选（`rmus find secret`）
    Find { query: String },
    /// 在线搜索/下载（走 rmusd 的插件，如 rmus-dl；产物自动进 ~/Music）
    Dl {
        /// 搜索关键词（多词自动拼接）
        query: Vec<String>,
        /// 下载第几个结果（从 1 数，逗号分隔，如 `-g 1,3`）；缺省只列出结果
        #[arg(short, long)]
        get: Option<String>,
    },
    /// 试听：保存当前状态，临时换成这些文件播放；播完自动恢复原队列与进度
    Peek { paths: Vec<PathBuf> },
    /// 结束试听，恢复进入试听前的队列与进度
    Back,
    /// 让 rmusd 退出
    Quit,
}

#[derive(Subcommand)]
enum Pl {
    /// 列出所有播放列表
    List,
    /// 创建空播放列表
    New { name: String },
    /// 把当前队列保存为播放列表（覆盖同名；当前队列即变为该播放列表）
    Save { name: String },
    /// 播放列表复制进队列并从头播放（之后互不影响）
    Play { name: String },
    /// 添加文件/目录到播放列表（不存在则创建；不影响正在播放的队列）
    Add { name: String, paths: Vec<PathBuf> },
    /// 删除播放列表（不影响正在播放的队列）
    Remove { name: String },
    /// 重命名播放列表（正在播的队列来源标签跟随改名）
    Rename { from: String, to: String },
}

#[derive(Subcommand)]
enum Lib {
    /// 全量重建库索引（扫描 ~/Music）
    Rescan,
}

fn main() -> Result<()> {
    cli::run(Cli::parse())
}
