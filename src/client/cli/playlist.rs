//! `rmus pl ...`：命名队列管理。

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
                    .context("解析队列列表失败")?;
            let current = value.get("current").and_then(|v| v.as_str());
            if names.is_empty() {
                println!("（没有命名队列，rmus pl save 名字 可保存当前队列）");
                return Ok(());
            }
            for name in names {
                let marker = if Some(name.as_str()) == current {
                    "▶"
                } else {
                    " "
                };
                println!("{marker} {name}");
            }
        }
        Pl::New { name } => {
            client.request(Request::PlaylistNew { name: name.clone() })?;
            println!("队列 {name} 已创建");
        }
        Pl::Save { name } => {
            client.request(Request::PlaylistSave { name: name.clone() })?;
            println!("当前队列已保存为 {name}");
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
            println!("已向队列 {name} 添加 {added} 首");
        }
        Pl::Remove { name } => {
            client.request(Request::PlaylistRemove { name: name.clone() })?;
            println!("队列 {name} 已删除");
        }
        Pl::Rename { from, to } => {
            client.request(Request::PlaylistRename {
                from: from.clone(),
                to: to.clone(),
            })?;
            println!("队列 {from} 已重命名为 {to}");
        }
    }
    Ok(())
}
