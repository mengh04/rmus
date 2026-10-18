//! `:` 命令面板：命令表、解析、面板过滤。
//!
//! 设计取向：
//! - 含空格的路径不用引号——`add` / `open` / `peek` / `pl add` 把"剩余整行"
//!   当作一个路径（`~` 展开），命令行写起来最省事
//! - 时间 `90` / `1:23` 为绝对秒，`+30` / `-30` 为相对秒
//! - 音量 `80` 为绝对百分比，`+10` / `-10` 为相对
//!
//! 分工：[`parse`] 只做"一行文本 -> [`Cmd`]"；本文件放给用户看的命令表，
//! 以及面板的模糊过滤；执行在 `app::command`。

mod parse;

#[cfg(test)]
mod tests;

pub use parse::{Cmd, SeekTarget, VolumeTarget, parse};

/// 一条命令的说明（`:help` 展示 + 面板过滤；`names[0]` 是规范名）。
pub struct Spec {
    pub names: &'static [&'static str],
    pub desc: &'static str,
}

pub const SPECS: &[Spec] = &[
    Spec {
        names: &["q", "quit"],
        desc: "退出 TUI（q! 结束 rmusd）",
    },
    Spec {
        names: &["play"],
        desc: "播放第 n 曲（缺省 = 恢复/从头）",
    },
    Spec {
        names: &["pause"],
        desc: "暂停",
    },
    Spec {
        names: &["seek"],
        desc: "跳转：90 / 1:23 绝对，+30 / -30 相对",
    },
    Spec {
        names: &["vol", "volume"],
        desc: "音量：80 绝对%，+10 / -10 相对",
    },
    Spec {
        names: &["repeat"],
        desc: "循环 off | all | one（缺省切换）",
    },
    Spec {
        names: &["rand", "random"],
        desc: "随机 on | off（缺省切换）",
    },
    Spec {
        names: &["add"],
        desc: "加入队列（整行为一个路径）",
    },
    Spec {
        names: &["open"],
        desc: "替换队列并从头播放",
    },
    Spec {
        names: &["peek"],
        desc: "试听（B 结束恢复原队列）",
    },
    Spec {
        names: &["pm", "playmatch"],
        desc: "库模糊插播",
    },
    Spec {
        names: &["pl", "playlist"],
        desc: "pl new | play | add | del 名字 [路径]",
    },
    Spec {
        names: &["rescan"],
        desc: "重建库索引",
    },
    Spec {
        names: &["help", "h"],
        desc: "本帮助",
    },
];

/// 按查询过滤命令（只看第一个词）：名称前缀 > 名称包含 > 描述包含，
/// 同档保持 SPECS 顺序。空查询自然返回全部。
pub fn filter(query: &str) -> Vec<&'static Spec> {
    let q = query.split_whitespace().next().unwrap_or("").to_lowercase();
    let mut scored: Vec<(u8, usize, &Spec)> = SPECS
        .iter()
        .enumerate()
        .filter_map(|(i, spec)| {
            let name_hit = spec
                .names
                .iter()
                .filter_map(|name| {
                    let name = name.to_lowercase();
                    if name.contains(&q) {
                        Some(if name == q {
                            3
                        } else if name.starts_with(&q) {
                            2
                        } else {
                            1
                        })
                    } else {
                        None
                    }
                })
                .max();
            let desc_hit = spec.desc.to_lowercase().contains(&q).then_some(0);
            Some((name_hit.or(desc_hit)?, i, spec))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, spec)| spec).collect()
}
