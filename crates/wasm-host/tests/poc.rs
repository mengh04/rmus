//! POC 集成测试：hello 插件（组件表演示）的 load → update → render 往返。
//!
//! 前置：插件已编译到 wasm target——
//!
//! ```text
//! cargo build -p hello-plugin --target wasm32-unknown-unknown
//! ```

use wasm_host::{Plugin, RenderLine, event};

/// workspace 共享 target 目录下的插件产物。
fn hello_wasm() -> Vec<u8> {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest)
        .join("../../target/wasm32-unknown-unknown/debug/hello_plugin.wasm");
    std::fs::read(&path).unwrap_or_else(|e| panic!("读 {path:?} 失败（先 cargo build -p hello-plugin --target wasm32-unknown-unknown）: {e}"))
}

fn plugin() -> Plugin {
    Plugin::new(&hello_wasm(), Box::new(|_, _| {})).unwrap()
}

/// 断言一行的 spans（style/text 成对，text 支持前缀匹配）。
fn assert_line(line: &RenderLine, expect: &[(&str, &str)]) {
    let got: Vec<(&str, &str)> =
        line.spans.iter().map(|s| (s.style.as_str(), s.text.as_str())).collect();
    assert_eq!(got, expect, "行内容不符: {line:?}");
}

/// 拼接一行的全部文本。
fn line_text(line: &RenderLine) -> String {
    line.spans.iter().map(|s| s.text.as_str()).collect()
}

#[test]
fn manifest_可读取() {
    let mut p = plugin();
    assert_eq!(
        p.manifest().unwrap(),
        r#"{"name":"hello","version":"0.3.0","ui":true,"methods":[]}"#
    );
}

#[test]
fn render_输出组件表() {
    let mut p = plugin();
    let lines = p.render(24, 80).unwrap();
    // header + 4 列表 + 进度条 + 状态行 + hints + 兼容文本行
    assert_eq!(lines.len(), 9);

    // 标题行：header 组件（strong 标题 + faint 提示）
    assert_line(
        &lines[0],
        &[("strong", "hello 组件演示"), ("faint", "  ·  组件表 v0")],
    );

    // 列表：选中行 accent 前缀 + strong 文本；未选中 faint 前缀 + base 文本
    assert_line(
        &lines[1],
        &[("accent", "▸ 1. "), ("strong", "群青 - YOASOBI")],
    );
    assert_line(
        &lines[2],
        &[("faint", "  2. "), ("base", "晴天 - 周杰伦")],
    );

    // 进度条初始 0%（bar + 百分比三段）
    let progress_text = line_text(&lines[5]);
    assert!(progress_text.contains("下载"), "进度条标签缺失: {progress_text}");
    assert!(progress_text.contains("0%"), "进度百分比缺失: {progress_text}");

    // 键位提示行存在
    let hints_text = line_text(&lines[7]);
    assert!(hints_text.contains("导航") && hints_text.contains("步进"));
}

#[test]
fn 导航_选中行移动() {
    let mut p = plugin();
    assert!(p.update(event::encode(event::KIND_DOWN, 0)).unwrap());
    let lines = p.render(24, 80).unwrap();
    // 第 1 行不再选中（faint 前缀），第 2 行选中
    assert!(lines[1].spans[0].style == "faint");
    assert!(lines[2].spans[0].style == "accent");

    // 字符键 j = DOWN
    assert!(p.update(event::encode(event::KIND_CHAR, 'j' as i32)).unwrap());
    let lines = p.render(24, 80).unwrap();
    assert!(lines[3].spans[0].style == "accent");

    // UP 回退
    assert!(p.update(event::encode(event::KIND_UP, 0)).unwrap());
    let lines = p.render(24, 80).unwrap();
    assert!(lines[2].spans[0].style == "accent");
    // 插件声明的选中行同步给宿主（TUI 铺选中底色依据）
    assert_eq!(p.take_selected(), Some(2));
    // 列表语义（总数, 下标）也同步给宿主（顶栏计数/底栏统计依据）
    assert_eq!(p.take_list(), Some((4, 1)));
    // render 重新声明，值跟着走
    assert!(p.update(event::encode(event::KIND_CHAR, 'j' as i32)).unwrap());
    p.render(24, 80).unwrap();
    assert_eq!(p.take_selected(), Some(3));
    assert_eq!(p.take_list(), Some((4, 2)));
}

#[test]
fn 空格_进度步进到完成() {
    let mut p = plugin();
    for _ in 0..3 {
        assert!(p.update(event::encode(event::KIND_SPACE, 0)).unwrap());
    }
    let lines = p.render(24, 80).unwrap();
    let progress_text = line_text(&lines[5]);
    assert!(progress_text.contains("75%"), "75% 处: {progress_text}");
    let status_text = line_text(&lines[6]);
    assert!(status_text.contains("进行中"), "75% 处状态行: {status_text}");

    // 第 4 步到 100%：状态行变"完成 ✓"
    assert!(p.update(event::encode(event::KIND_SPACE, 0)).unwrap());
    let lines = p.render(24, 80).unwrap();
    let progress_text = line_text(&lines[5]);
    assert!(progress_text.contains("100%"), "100% 处: {progress_text}");
    let status_text = line_text(&lines[6]);
    assert!(status_text.contains("完成 ✓"), "100% 处状态行: {status_text}");
}

#[test]
fn lifecycle_可重入() {
    let mut p = plugin();
    assert!(p.update(event::encode(event::KIND_DOWN, 0)).unwrap());
    p.reload().unwrap();
    // reload 不重置插件静态状态，只验证 load 可安全二次调用
    let lines = p.render(24, 80).unwrap();
    assert!(lines[2].spans[0].style == "accent");
}

#[test]
#[should_panic(expected = "render 区域为空")]
fn 空区域拒绝渲染() {
    let mut p = plugin();
    p.render(0, 80).unwrap();
}
