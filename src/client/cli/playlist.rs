//! `rmus pl ...`：播放列表管理。

use crate::Pl;
use anyhow::{Context, Result};
use rmus::client::Client;
use rmus::proto::Request;

use super::format::{print_status, status_reply};

pub(crate) fn handle_pl(client: &mut Client, pl: &Pl) -> Result<()> {
    match pl {
        Pl::List => {
            let value = client.request(Request::PlaylistsList)?;
            let names: Vec<String> =
                serde_json::from_value(value.get("names").cloned().unwrap_or_default())
                    .context("解析播放列表失败")?;
            let current = value.get("current").and_then(|v| v.as_str());
            let modified = value
                .get("modified")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if names.is_empty() {
                println!("（没有播放列表，rmus pl save 名字 可把当前队列存为播放列表）");
                return Ok(());
            }
            for name in names {
                let marker = if Some(name.as_str()) == current {
                    if modified { "*" } else { "▶" }
                } else {
                    " "
                };
                println!("{marker} {name}");
            }
            if modified {
                println!("（* = 正在播的队列已偏离这张列表，`rmus pl save 名字 --force` 才会回写）");
            }
        }
        Pl::New { name } => {
            client.request(Request::PlaylistNew { name: name.clone() })?;
            println!("播放列表 {name} 已创建");
        }
        Pl::Save { name, force } => {
            client.request(Request::PlaylistSave {
                name: name.clone(),
                overwrite: *force,
            })?;
            if *force {
                println!("当前队列已覆盖保存到播放列表 {name}");
            } else {
                println!("当前队列已另存为播放列表 {name}");
            }
        }
        Pl::Play { name } => {
            let value = client.request(Request::PlaylistPlay { name: name.clone() })?;
            print_status(&status_reply(value)?);
        }
        Pl::Add { name, paths } => {
            let value = client.request(Request::PlaylistAdd {
                name: name.clone(),
                paths: paths.clone(),
            })?;
            let added = value.get("added").and_then(|v| v.as_u64()).unwrap_or(0);
            let skipped = value.get("skipped").and_then(|v| v.as_u64()).unwrap_or(0);
            if skipped > 0 {
                println!("已向播放列表 {name} 添加 {added} 首（{skipped} 首已在列表里，跳过）");
            } else {
                println!("已向播放列表 {name} 添加 {added} 首");
            }
        }
        Pl::Remove { name } => {
            client.request(Request::PlaylistRemove { name: name.clone() })?;
            println!("播放列表 {name} 已删除");
        }
        Pl::Rename { from, to } => {
            client.request(Request::PlaylistRename {
                from: from.clone(),
                to: to.clone(),
            })?;
            println!("播放列表 {from} 已重命名为 {to}");
        }
    }
    Ok(())
}
