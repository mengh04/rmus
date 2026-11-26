//! 请求分发：库查询就地回、扫描丢阻塞线程池、其余转交引擎。

use crate::Library;
use crate::plugin_host::PluginHost;
use crate::reply::{error_reply, send_reply};
use rmus::engine::ToEngine;
use rmus::library;
use rmus::proto::{Reply, Request, RequestMsg, ServerMsg, TrackInfo};
use rmus::scan;
use std::sync::mpsc as std_mpsc;
use tokio::sync::{mpsc, oneshot};

pub(crate) async fn dispatch(
    msg: RequestMsg,
    cmd_tx: &std_mpsc::Sender<ToEngine>,
    out_tx: &mpsc::UnboundedSender<String>,
    library: &Library,
    plugins: &PluginHost,
) {
    let id = msg.id;

    // 插件调用：宿主层就地处理（不进引擎）。长任务立即回任务号，
    // 进度/结果走 Event::Plugin 广播。
    if let Request::PluginCall {
        plugin,
        method,
        params,
    } = msg.request
    {
        let host = plugins.clone();
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            let json = match host.call(&plugin, &method, params).await {
                Ok(value) => serde_json::to_string(&ServerMsg::Reply {
                    id,
                    result: Reply::Plugin(value),
                })
                .unwrap_or_default(),
                Err(error) => serde_json::to_string(&error_reply(id, error)).unwrap_or_default(),
            };
            let _ = out_tx.send(json);
        });
        return;
    }

    if let Request::PluginCancel { plugin, call } = msg.request {
        let host = plugins.clone();
        let out_tx = out_tx.clone();
        tokio::spawn(async move {
            let json = match host.cancel(&plugin, call).await {
                Ok(value) => serde_json::to_string(&ServerMsg::Reply {
                    id,
                    result: Reply::Plugin(value),
                })
                .unwrap_or_default(),
                Err(error) => serde_json::to_string(&error_reply(id, error)).unwrap_or_default(),
            };
            let _ = out_tx.send(json);
        });
        return;
    }

    if matches!(msg.request, Request::PluginList) {
        let json = serde_json::to_string(&ServerMsg::Reply {
            id,
            result: Reply::Plugin(plugins.list()),
        })
        .unwrap_or_default();
        let _ = out_tx.send(json);
        return;
    }

    // UI 插件：按键注入 / 视图刷新。投递即回（视图经 Event::PluginView 推送）。
    if let Request::PluginKey {
        plugin,
        key,
        rows,
        cols,
    } = msg.request
    {
        let result = plugins.send_ui_key(&plugin, key, rows, cols, false);
        let json = serde_json::to_string(&ui_ack(id, result)).unwrap_or_default();
        let _ = out_tx.send(json);
        return;
    }

    if let Request::PluginViewReq { plugin, rows, cols } = msg.request {
        let result = plugins.send_ui_key(&plugin, 0, rows, cols, true);
        let json = serde_json::to_string(&ui_ack(id, result)).unwrap_or_default();
        let _ = out_tx.send(json);
        return;
    }

    if let Request::PluginData { plugin, data } = msg.request {
        let result = plugins.send_ui_data(&plugin, &data);
        let json = serde_json::to_string(&ui_ack(id, result)).unwrap_or_default();
        let _ = out_tx.send(json);
        return;
    }

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
                    recommended: false,
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

    // 整库列表：查索引直接回（不进引擎）
    if matches!(msg.request, Request::LibList) {
        let tracks = {
            let lib = library.read().unwrap();
            lib.iter()
                .enumerate()
                .map(|(i, entry)| TrackInfo {
                    index: i,
                    path: entry.item.path.clone(),
                    title: entry.item.title.clone(),
                    artist: entry.item.artist.clone(),
                    album: entry.item.album.clone(),
                    duration_ms: entry.item.duration_ms,
                    recommended: false,
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

    // 创建种子电台：按 path / query 解析种子曲，均省略则用当前正在播放的曲目
    if let Request::RadioStart { path, query } = msg.request {
        if let Some(path) = path {
            // 优先查库索引（秒级、标签完整），未命中再扫描文件
            let from_lib = {
                let lib = library.read().unwrap();
                lib.iter()
                    .find(|e| e.item.path == path)
                    .map(|e| e.item.clone())
            };
            let cmd_tx = cmd_tx.clone();
            let out_tx = out_tx.clone();
            tokio::spawn(async move {
                let item = match from_lib {
                    Some(it) => Some(it),
                    None => tokio::task::spawn_blocking(move || {
                        scan::collect(&[path]).into_iter().next()
                    })
                    .await
                    .ok()
                    .flatten(),
                };
                match item {
                    Some(seed) => {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        if cmd_tx
                            .send(ToEngine::StartRadio {
                                id,
                                seed,
                                reply: reply_tx,
                            })
                            .is_err()
                        {
                            return;
                        }
                        send_reply(id, reply_rx.await, &out_tx);
                    }
                    None => {
                        let json = serde_json::to_string(&error_reply(
                            id,
                            "找不到可用的种子音频文件".into(),
                        ))
                        .unwrap_or_default();
                        let _ = out_tx.send(json);
                    }
                }
            });
            return;
        }
        if let Some(query) = query.map(|q| q.trim().to_string()).filter(|q| !q.is_empty()) {
            let best = {
                let lib = library.read().unwrap();
                library::matches(&lib, &query).into_iter().next().cloned()
            };
            match best {
                Some(seed) => {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    if cmd_tx
                        .send(ToEngine::StartRadio {
                            id,
                            seed,
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
        let (reply_tx, reply_rx) = oneshot::channel();
        if cmd_tx
            .send(ToEngine::StartRadioCurrent {
                id,
                reply: reply_tx,
            })
            .is_err()
        {
            return;
        }
        send_reply(id, reply_rx.await, out_tx);
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
            let items = tokio::task::spawn_blocking(library::rescan)
                .await
                .unwrap_or_default();
            let added = items.len();
            *lib.write().unwrap() = items;
            let json = serde_json::to_string(&ServerMsg::Reply {
                id,
                result: Reply::Added { added, skipped: 0 },
            })
            .unwrap_or_default();
            let _ = out_tx.send(json);
        });
        return;
    }

    let replace = matches!(msg.request, Request::Open { .. });
    let peeking = matches!(msg.request, Request::Peek { .. });
    let insert_next = matches!(msg.request, Request::InsertNext { .. });
    if let Request::Add { paths }
    | Request::InsertNext { paths }
    | Request::Open { paths }
    | Request::Peek { paths } = msg.request
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
            } else if insert_next {
                cmd_tx.send(ToEngine::InsertNextScanned {
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

    if let Request::DeleteFile { path } = msg.request {
        let cmd_tx = cmd_tx.clone();
        let out_tx = out_tx.clone();
        let library = library.clone();
        tokio::spawn(async move {
            let (reply_tx, reply_rx) = oneshot::channel();
            if cmd_tx
                .send(ToEngine::PurgePath {
                    path: path.clone(),
                    reply: reply_tx,
                })
                .is_err()
            {
                return;
            }
            let purged = match reply_rx.await {
                Ok(Ok(item)) => item,
                Ok(Err(e)) => {
                    let json = serde_json::to_string(&error_reply(id, e)).unwrap_or_default();
                    let _ = out_tx.send(json);
                    return;
                }
                Err(_) => {
                    let json = serde_json::to_string(&error_reply(id, "引擎无响应".into()))
                        .unwrap_or_default();
                    let _ = out_tx.send(json);
                    return;
                }
            };

            let target_path = purged
                .as_ref()
                .map(|i| i.path.clone())
                .or(path)
                .unwrap_or_default();

            // 补齐 title/artist（库视图删未入队曲目时从库索引取）
            let (title, artist) = match purged {
                Some(item) => (item.title, item.artist),
                None => {
                    let lib = library.read().unwrap();
                    lib.iter()
                        .find(|e| e.item.path == target_path)
                        .map(|e| (e.item.title.clone(), e.item.artist.clone()))
                        .unwrap_or_else(|| {
                            let stem = target_path
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| target_path.display().to_string());
                            (stem, String::new())
                        })
                }
            };

            let del_path = target_path.clone();
            let remove_res = tokio::task::spawn_blocking(move || std::fs::remove_file(&del_path))
                .await
                .unwrap_or_else(|e| Err(std::io::Error::other(e.to_string())));
            if let Err(e) = remove_res
                && e.kind() != std::io::ErrorKind::NotFound
            {
                let json = serde_json::to_string(&error_reply(
                    id,
                    format!("删除文件失败（{}）：{e}", target_path.display()),
                ))
                .unwrap_or_default();
                let _ = out_tx.send(json);
                return;
            }

            // 同步更新内存库索引并落盘（无需等 watch.rs 2s 防抖）
            {
                let mut lib = library.write().unwrap();
                lib.retain(|e| e.item.path != target_path);
                let snapshot = lib.clone();
                drop(lib);
                tokio::task::spawn_blocking(move || library::save(&snapshot));
            }

            // 永久写入推荐拉黑名单（自动推荐 / rmus-fm / rmus-dj 共用）
            let (bt, ba) = (title.clone(), artist.clone());
            tokio::task::spawn_blocking(move || {
                rmus_plugin_sdk::blocklist::add(&rmus::plugin::data_root(), &bt, &ba);
            });

            let json = serde_json::to_string(&ServerMsg::Reply {
                id,
                result: Reply::Deleted {
                    path: target_path,
                    title,
                    artist,
                },
            })
            .unwrap_or_default();
            let _ = out_tx.send(json);
        });
        return;
    }

    if matches!(msg.request, Request::Quit) {
        // 先删 socket：立即让后续连接失败，避免竞态（新客户端连上将死的
        // daemon / 新 daemon 误判"已在运行"）；顺手终止插件子进程；
        // 再等响应写出后退出
        let _ = std::fs::remove_file(rmus::socket_path());
        plugins.shutdown();
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

/// UI 插件按键/视图请求的应答：投递成功回 Ok，失败回 error。
fn ui_ack(id: u64, result: Result<(), String>) -> ServerMsg {
    match result {
        Ok(()) => ServerMsg::Reply {
            id,
            result: Reply::Ok,
        },
        Err(error) => error_reply(id, error),
    }
}
