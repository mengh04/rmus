//! OpenAI 兼容 chat 调用 + 响应 JSON 提取。
//!
//! SDK http 走宿主 ureq 代理、无超时——对 LLM 长响应是特性不是缺陷，
//! 插件线程慢慢等就行。

use anyhow::{Context, Result, bail};
use rmus_plugin_sdk::http;

use crate::config::Config;

/// 一次对话补全。返回 `choices[0].message.content`。
pub fn chat(cfg: &Config, system: &str, user: &str, temperature: f32) -> Result<String> {
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": cfg.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "temperature": temperature,
        "max_tokens": 2400,
    })
    .to_string();
    let req = http::Request {
        url,
        method: "POST".into(),
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Authorization".into(), format!("Bearer {}", cfg.api_key)),
        ],
        body: Some(body),
    };
    let resp = http::Response::send(&req).context("连不上 LLM 端点")?;
    let status = resp.status();
    let bytes = resp.read_to_end().context("读 LLM 响应失败")?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if status != 200 {
        bail!("LLM 端点返回 {status}：{}", head(&text, 300));
    }
    let value: serde_json::Value =
        serde_json::from_str(&text).context("LLM 响应不是合法 JSON")?;
    value
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .context("LLM 响应里没有 choices[0].message.content")
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// 从模型输出里抠出 JSON 对象：剥 markdown 围栏 → 整体解析 →
/// 首个 `{` 到末个 `}` 兜底（模型总爱在 JSON 前后加话）。
pub fn extract_json(content: &str) -> Option<serde_json::Value> {
    let trimmed = content.trim();
    let inner = trimmed
        .strip_prefix("```")
        .and_then(|s| s.strip_suffix("```"))
        .map(|s| s.trim_start_matches("json").trim())
        .unwrap_or(trimmed);
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(inner) {
        return Some(v);
    }
    if let (Some(a), Some(b)) = (inner.find('{'), inner.rfind('}'))
        && a < b
    {
        return serde_json::from_str(&inner[a..=b]).ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_variants() {
        for case in [
            r#"{"a":1}"#,
            "```json\n{\"a\":1}\n```",
            "好的，这是结果：\n{\"a\":1}\n希望有帮助！",
            "前缀 {\"a\":{\"b\":2}} 后缀",
        ] {
            let v = extract_json(case).expect("应提取出 JSON");
            assert!(v.get("a").is_some());
        }
        assert!(extract_json("完全没有 JSON").is_none());
    }
}
