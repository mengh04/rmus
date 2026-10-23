//! 插件命令行入口：`rmus -<入口名> <参数…>`（如 `rmus -dl 郁可唯`）。
//!
//! 插件 manifest 的 `cli` 字段宣告入口名；客户端按名找到插件后，把参数
//! 打包 `{"args":[…]}` 调它的 `cli` 方法（task 调用）：进度事件渲染到
//! stderr，完结包 `{"event":"done"|"failed",…}` 渲染到 stdout。`--json`
//! 是客户端旗标（拦下不转发）：结果原样打印；结果含 `failed` 项时以非
//! 零码退出（已下载的照常落盘）。Ctrl+C 不转发取消：daemon 侧任务照常
//! 跑完，文件不丢。

use crate::Cli;
use anyhow::Result;
use rmus::proto::{Event, Request};
use std::io::IsTerminal;

use super::format::connect;

/// 入口名 → 插件 → `cli` 方法调用 → 事件渲染循环。
pub(crate) fn run(cli: &Cli, entry: &str, args: &[String]) -> Result<()> {
    // --json 是客户端旗标：拦下不转发，改为原样打印结果 JSON
    let as_json = args.iter().any(|a| a == "--json");
    let args: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--json")
        .cloned()
        .collect();

    let mut client = connect(cli)?;

    // 按入口名找插件（PluginList 带 manifest 的 cli 字段）
    let list = client.request(Request::PluginList)?;
    let mut found = None;
    let mut registered = Vec::new();
    for p in list
        .get("plugins")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let Some(name) = p.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        match p.get("cli").and_then(|v| v.as_str()) {
            Some(e) if e == entry => found = Some(name.to_owned()),
            Some(e) => registered.push(e.to_owned()),
            _ => {}
        }
    }
    let Some(plugin) = found else {
        anyhow::bail!(
            "没有 -{entry} 入口{}",
            if registered.is_empty() {
                "：没有插件注册命令行入口".to_owned()
            } else {
                format!(
                    "；已注册：{}",
                    registered
                        .iter()
                        .map(|e| format!("-{e}"))
                        .collect::<Vec<_>>()
                        .join("、")
                )
            }
        );
    };

    let reply = client.request(Request::PluginCall {
        plugin: plugin.clone(),
        method: "cli".into(),
        params: serde_json::json!({ "args": args }),
    })?;

    // task 调用：回包 {"call":N}，之后等 Event::Plugin 推进；
    // call 类回包即结果（兼容把 cli 宣告成短调用的插件）
    let Some(call) = reply.get("call").and_then(|v| v.as_u64()) else {
        return finish(&reply, as_json);
    };
    loop {
        let Event::Plugin {
            plugin: name,
            call: c,
            data,
        } = client.wait_event()?
        else {
            continue;
        };
        if name != plugin || c != Some(call) {
            continue; // 其他插件 / 其他任务的事件
        }
        match data.get("event").and_then(|v| v.as_str()) {
            Some("done") => {
                clear_line();
                return finish(
                    data.get("result").unwrap_or(&serde_json::Value::Null),
                    as_json,
                );
            }
            Some("failed") => {
                clear_line();
                eprintln!(
                    "{plugin}：{}",
                    data.get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("未知错误")
                );
                std::process::exit(1);
            }
            _ => render_progress(&data),
        }
    }
}

/// 打印最终结果；`--json` 原样输出，否则按结果形状人话渲染
/// （usage / 搜索列表 / 下载成败清单）。有 failed 项时非零码退出。
fn finish(result: &serde_json::Value, as_json: bool) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(result)?);
    } else if let Some(usage) = result.get("usage").and_then(|v| v.as_str()) {
        println!("{usage}");
    } else if let Some(songs) = result.get("songs").and_then(|v| v.as_array()) {
        let query = result.get("query").and_then(|v| v.as_str()).unwrap_or("");
        println!("“{query}” 搜索结果（`rmus -dl --pick 编号` 下载）：");
        for (i, s) in songs.iter().enumerate() {
            println!(
                "{:>2}. {} - {}［{}，{}］",
                i + 1,
                get(s, "title"),
                get(s, "artist"),
                get(s, "format"),
                size(s.get("size_bytes")),
            );
        }
    } else {
        let downloaded = result.get("downloaded").and_then(|v| v.as_array());
        let failed = result.get("failed").and_then(|v| v.as_array());
        for d in downloaded.into_iter().flatten() {
            println!(
                "✓ {} - {} → {}",
                get(d, "title"),
                get(d, "artist"),
                get(d, "path")
            );
        }
        for f in failed.into_iter().flatten() {
            println!(
                "✗ {} - {}：{}",
                get(f, "title"),
                get(f, "artist"),
                get(f, "error")
            );
        }
        let ok = downloaded.map_or(0, |a| a.len());
        let bad = failed.map_or(0, |a| a.len());
        if ok + bad > 0 {
            println!("完成：成功 {ok} 首，失败 {bad} 首");
        }
    }
    if result
        .get("failed")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty())
    {
        std::process::exit(1);
    }
    Ok(())
}

/// 进度事件渲染到 stderr：TTY 上 \r 原地刷新；管道下只在换阶段时打
/// 一行、跳过字节级进度（结果只走 stdout，保持管道干净）。
fn render_progress(data: &serde_json::Value) {
    let tty = std::io::stderr().is_terminal();
    if let (Some(received), Some(total)) = (
        data.get("received").and_then(|v| v.as_u64()),
        data.get("total").and_then(|v| v.as_u64()),
    ) {
        if !tty {
            return;
        }
        let pct = (received * 100).checked_div(total).unwrap_or(0);
        eprint!(
            "\r\x1b[K  {pct:>3}%  {:.1}/{:.1} MB",
            received as f64 / 1_048_576.0,
            total as f64 / 1_048_576.0
        );
        return;
    }
    match data.get("phase").and_then(|v| v.as_str()) {
        Some("download") => {
            let line = format!("  下载 {} - {} …", get(data, "title"), get(data, "artist"));
            if tty {
                eprint!("\r\x1b[K{line}")
            } else {
                eprintln!("{line}")
            }
        }
        Some("metadata") => {
            if tty {
                eprint!("\r\x1b[K  写元数据 …");
            } else {
                eprintln!("  写元数据 …");
            }
        }
        _ => {}
    }
}

fn clear_line() {
    if std::io::stderr().is_terminal() {
        eprint!("\r\x1b[K");
    }
}

fn get<'a>(v: &'a serde_json::Value, key: &str) -> &'a str {
    v.get(key).and_then(|v| v.as_str()).unwrap_or("?")
}

fn size(v: Option<&serde_json::Value>) -> String {
    match v.and_then(|v| v.as_u64()) {
        Some(n) => format!("{:.1} MB", n as f64 / 1_048_576.0),
        None => "未知大小".into(),
    }
}
