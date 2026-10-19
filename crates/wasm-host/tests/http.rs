//! 网络/文件宿主函数测试：本地 TCP 假服务器 + net-test 插件。
//!
//! 前置：插件已编译到 wasm target——
//!
//! ```text
//! cargo build -p net-test-plugin --target wasm32-unknown-unknown
//! ```

use std::io::{Read, Write};
use std::net::TcpListener;

use serde_json::Value;
use wasm_host::Plugin;

/// 加载 net-test 插件（库目录指到临时目录）。
fn net_plugin(lib_dir: &std::path::Path) -> Plugin {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest)
        .join("../../target/wasm32-unknown-unknown/debug/net_test_plugin.wasm");
    let wasm = std::fs::read(&path).unwrap_or_else(|e| {
        panic!("读 {path:?} 失败（先 cargo build -p net-test-plugin --target wasm32-unknown-unknown）: {e}")
    });
    let mut plugin = Plugin::new(&wasm, Box::new(|_, _| {})).unwrap();
    plugin.set_lib_dir(lib_dir.to_path_buf());
    plugin
}

/// 本地假 HTTP 服务器（drop 返回的监听器即停止 accept）。
/// 路由：/text 200 正文；/missing 404；/redir 302→/text；/echo 回显请求头与 body。
fn spawn_server() -> (String, TcpListener) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let thread_listener = listener.try_clone().expect("clone listener");
    std::thread::spawn(move || {
        for stream in thread_listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                match stream.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        buf.push(byte[0]);
                        if buf.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let head = String::from_utf8_lossy(&buf);
            let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
            let body_of_post = if path == "/echo" {
                let len = head
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split(':').nth(1))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                let mut body = vec![0u8; len];
                if len > 0 {
                    let _ = stream.read_exact(&mut body);
                }
                let has_probe = head.to_ascii_lowercase().contains("x-probe:");
                format!("len={len} probe={has_probe} body={}", String::from_utf8_lossy(&body))
            } else {
                String::new()
            };
            let response = match path.as_str() {
                "/text" => {
                    "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello"
                        .as_bytes()
                        .to_vec()
                }
                "/missing" => {
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .as_bytes()
                        .to_vec()
                }
                "/redir" => {
                    "HTTP/1.1 302 Found\r\nLocation: /text\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .as_bytes()
                        .to_vec()
                }
                "/echo" => format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_of_post}",
                    body_of_post.len()
                )
                .into_bytes(),
                _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
            };
            let _ = stream.write_all(&response);
        }
    });
    (base, listener)
}

fn main() {}

/// 集成测试主体（#[test] 在文件底部；上面是共享设施）。
#[cfg(test)]
mod tests {
    use super::*;

    fn call(plugin: &mut Plugin, method: &str, base: &str) -> Value {
        plugin
            .call_method(method, &serde_json::json!({ "base": base }))
            .expect("call 失败")
            .expect("插件回 error")
    }

    #[test]
    fn http_各能力() {
        let tmp = std::env::temp_dir().join(format!("rmus-nettest-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let (base, _listener) = spawn_server();
        let mut plugin = net_plugin(&tmp);

        // GET + body
        let r = call(&mut plugin, "fetch", &base);
        assert_eq!(r["status"], 200);
        assert_eq!(r["body"], "hello");

        // 404 状态码透传（错误码语义由插件判定）
        let r = call(&mut plugin, "status", &base);
        assert_eq!(r["status"], 404);

        // 302 跟随后最终 URL
        let r = call(&mut plugin, "redirect", &base);
        assert_eq!(r["status"], 200);
        assert_eq!(r["url"], format!("{base}/text"));

        // POST form + 自定义 header
        let r = call(&mut plugin, "post", &base);
        assert_eq!(r["body"], "len=7 probe=true body=a=1&b=x");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn file_与库目录() {
        let tmp = std::env::temp_dir().join(format!("rmus-nettest-fs-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let (base, _listener) = spawn_server();
        let mut plugin = net_plugin(&tmp);

        let r = call(&mut plugin, "file", &base);
        assert_eq!(r["dir"], tmp.display().to_string(), "library_dir 经宿主注入");
        assert_eq!(r["existed"], true, "create + write 后存在");
        assert_eq!(r["renamed"], true, "rename 成功");
        assert_eq!(r["final_existed"], true, "rename 目标存在");
        assert_eq!(r["removed"], true, "remove 成功");
        assert_eq!(r["removed_gone"], true, "remove 后不存在");

        let r = call(&mut plugin, "libdir", &base);
        assert_eq!(r["dir"], tmp.display().to_string());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
