//! End-to-end MCP protocol test: drives `gos mcp` over real pipes.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use gossamer_std::json::{self, Value};

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: i64,
}

impl McpClient {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gos"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn gos mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        let init = client.request(
            "initialize",
            "{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{}}",
        );
        assert!(json::get(&init, "result").is_some());
        client
    }

    fn request(&mut self, method: &str, params: &str) -> Value {
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"{method}\",\"params\":{params}}}",
            self.next_id
        )
        .unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        json::parse(line.trim()).unwrap()
    }

    fn call_tool(&mut self, name: &str, arguments: &str) -> String {
        let reply = self.request(
            "tools/call",
            &format!("{{\"name\":\"{name}\",\"arguments\":{arguments}}}"),
        );
        let result = json::get(&reply, "result").expect("tool result");
        let content = json::get(result, "content")
            .and_then(json::as_array)
            .unwrap();
        json::get(&content[0], "text")
            .and_then(json::as_str)
            .unwrap()
            .to_string()
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn json_path(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "\\\\")
}

#[test]
fn check_run_and_timeout_work_end_to_end() {
    let dir = std::env::temp_dir().join(format!("gos-mcp-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let mut client = McpClient::start();

    // A type error surfaces through `check` as a structured diagnostic.
    let bad = dir.join("bad.gos");
    std::fs::write(&bad, "fn main() { let x: i64 = \"nope\" }\n").unwrap();
    let text = client.call_tool("check", &format!("{{\"file\":\"{}\"}}", json_path(&bad)));
    assert!(text.contains("GT"), "check output was: {text}");

    // A clean program runs and its stdout comes back.
    let ok = dir.join("ok.gos");
    std::fs::write(&ok, "fn main() { println!(\"mcp says {}\", 21 * 2) }\n").unwrap();
    let text = client.call_tool("run", &format!("{{\"file\":\"{}\"}}", json_path(&ok)));
    assert!(text.contains("exit code: 0"), "run output was: {text}");
    assert!(text.contains("mcp says 42"), "run output was: {text}");

    // An infinite loop is killed at the timeout instead of hanging.
    let spin = dir.join("spin.gos");
    std::fs::write(&spin, "fn main() { loop { } }\n").unwrap();
    let text = client.call_tool(
        "run",
        &format!("{{\"file\":\"{}\",\"timeout_ms\":500}}", json_path(&spin)),
    );
    assert!(text.contains("timed out"), "timeout output was: {text}");

    // The skill card resource reads back.
    let reply = client.request("resources/read", "{\"uri\":\"gossamer://skill-card\"}");
    let contents = json::get(json::get(&reply, "result").unwrap(), "contents")
        .and_then(json::as_array)
        .unwrap();
    assert!(
        json::get(&contents[0], "text")
            .and_then(json::as_str)
            .unwrap()
            .contains("Gossamer")
    );
}
