//! In-memory protocol tests: drive the dispatch loop over byte buffers.

use std::io::Cursor;
use std::path::PathBuf;

use gossamer_mcp::{ServerConfig, testing_run};
use gossamer_std::json::{self, Value};

fn drive_raw(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let config = ServerConfig {
        gos_exe: PathBuf::from("gos-not-used"),
    };
    testing_run(Cursor::new(input.as_bytes().to_vec()), &mut out, &config).unwrap();
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(String::from)
        .collect()
}

fn drive(input: &str) -> Vec<Value> {
    drive_raw(input)
        .iter()
        .map(|line| json::parse(line).unwrap())
        .collect()
}

fn req(id: i64, method: &str, params: &str) -> String {
    format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\",\"params\":{params}}}\n")
}

fn tool_text(reply: &Value) -> (String, bool) {
    let result = json::get(reply, "result").expect("tool result");
    let is_error = json::get(result, "isError")
        .and_then(json::as_bool)
        .unwrap();
    let content = json::get(result, "content")
        .and_then(json::as_array)
        .unwrap();
    let text = json::get(&content[0], "text")
        .and_then(json::as_str)
        .unwrap();
    (text.to_string(), is_error)
}

#[test]
fn initialize_reports_server_info_and_echoes_protocol_version() {
    let input = req(1, "initialize", "{\"protocolVersion\":\"2025-03-26\"}");
    let replies = drive(&input);
    assert_eq!(replies.len(), 1);
    let result = json::get(&replies[0], "result").unwrap();
    assert_eq!(
        json::get(result, "protocolVersion").and_then(json::as_str),
        Some("2025-03-26")
    );
    let info = json::get(result, "serverInfo").unwrap();
    assert_eq!(
        json::get(info, "name").and_then(json::as_str),
        Some("gos-mcp")
    );
    assert_eq!(json::get(&replies[0], "id").and_then(json::as_i64), Some(1));
}

#[test]
fn response_ids_are_wire_integers_not_floats() {
    let raw = drive_raw(&req(1, "ping", "{}"));
    assert!(raw[0].contains("\"id\":1"), "raw response was: {}", raw[0]);
    assert!(!raw[0].contains("1.0"), "raw response was: {}", raw[0]);
}

#[test]
fn ping_returns_empty_object_and_notifications_are_silent() {
    let mut input = String::new();
    input.push_str("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n");
    input.push_str(&req(2, "ping", "{}"));
    let replies = drive(&input);
    assert_eq!(replies.len(), 1);
    assert!(matches!(
        json::get(&replies[0], "result"),
        Some(Value::Object(m)) if m.is_empty()
    ));
}

#[test]
fn unknown_method_yields_method_not_found() {
    let replies = drive(&req(3, "bogus/method", "{}"));
    let error = json::get(&replies[0], "error").unwrap();
    assert_eq!(
        json::get(error, "code").and_then(json::as_i64),
        Some(-32601)
    );
}

#[test]
fn parse_error_line_yields_code_32700_and_loop_continues() {
    let mut input = "this is not json\n".to_string();
    input.push_str(&req(4, "ping", "{}"));
    let replies = drive(&input);
    assert_eq!(replies.len(), 2);
    let error = json::get(&replies[0], "error").unwrap();
    assert_eq!(
        json::get(error, "code").and_then(json::as_i64),
        Some(-32700)
    );
    assert!(json::get(&replies[1], "result").is_some());
}

#[test]
fn tools_list_names_every_tool_with_schemas() {
    let replies = drive(&req(5, "tools/list", "{}"));
    let result = json::get(&replies[0], "result").unwrap();
    let tools = json::get(result, "tools").and_then(json::as_array).unwrap();
    let names: Vec<&str> = tools
        .iter()
        .map(|t| json::get(t, "name").and_then(json::as_str).unwrap())
        .collect();
    for expected in [
        "check",
        "explain",
        "execute",
        "build",
        "test",
        "fmt",
        "doc",
        "hover",
        "definition",
        "references",
        "workspace_symbols",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }
    for tool in tools {
        assert!(
            json::get(tool, "description")
                .and_then(json::as_str)
                .is_some()
        );
        let schema = json::get(tool, "inputSchema").unwrap();
        assert_eq!(
            json::get(schema, "type").and_then(json::as_str),
            Some("object")
        );
    }
}

#[test]
fn tools_call_unknown_tool_is_invalid_params() {
    let replies = drive(&req(
        6,
        "tools/call",
        "{\"name\":\"nonesuch\",\"arguments\":{}}",
    ));
    let error = json::get(&replies[0], "error").unwrap();
    assert_eq!(
        json::get(error, "code").and_then(json::as_i64),
        Some(-32602)
    );
}

#[test]
fn exec_tool_reports_spawn_failure_as_tool_error() {
    // gos_exe points at a path that does not exist, so spawning fails
    // and the failure surfaces as isError content, not a crash.
    let replies = drive(&req(
        7,
        "tools/call",
        "{\"name\":\"explain\",\"arguments\":{\"code\":\"GT0001\"}}",
    ));
    let (text, is_error) = tool_text(&replies[0]);
    assert!(is_error, "text was: {text}");
    assert!(text.contains("gos-not-used"), "text was: {text}");
}

#[test]
fn exec_runner_captures_output_of_a_real_process() {
    // The test binary itself with libtest's --list flag: portable on
    // every CI platform without depending on a shell.
    let exe = std::env::current_exe().unwrap();
    let out = gossamer_mcp::testing_exec(&exe, &["--list".to_string()]).unwrap();
    assert_eq!(out.exit_code, Some(0));
    assert!(!out.timed_out);
    assert!(
        out.stdout
            .contains("exec_runner_captures_output_of_a_real_process")
    );
}

// One file per test: the nav tests run on parallel threads, and a
// shared fixture path would let one test's write race another's read.
fn nav_fixture(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("gos-mcp-nav-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.gos"));
    std::fs::write(
        &path,
        "fn double(x: i64) -> i64 { x * 2 }\nfn main() { println!(\"{}\", double(21)) }\n",
    )
    .unwrap();
    path.to_string_lossy().replace('\\', "\\\\")
}

#[test]
fn hover_reports_a_type_at_the_call_site() {
    let file = nav_fixture("hover");
    let params = format!(
        "{{\"name\":\"hover\",\"arguments\":{{\"file\":\"{file}\",\"line\":2,\"column\":29}}}}"
    );
    let replies = drive(&req(8, "tools/call", &params));
    let (text, is_error) = tool_text(&replies[0]);
    assert!(!is_error, "text was: {text}");
    assert!(text.contains("i64"), "hover text was: {text}");
}

#[test]
fn definition_points_at_the_declaration() {
    let file = nav_fixture("definition");
    let params = format!(
        "{{\"name\":\"definition\",\"arguments\":{{\"file\":\"{file}\",\"line\":2,\"column\":29}}}}"
    );
    let replies = drive(&req(9, "tools/call", &params));
    let (text, is_error) = tool_text(&replies[0]);
    assert!(!is_error, "text was: {text}");
    assert!(
        text.contains("definition.gos:1:"),
        "definition text was: {text}"
    );
}

#[test]
fn references_lists_call_sites() {
    let file = nav_fixture("references");
    let params = format!(
        "{{\"name\":\"references\",\"arguments\":{{\"file\":\"{file}\",\"line\":2,\"column\":29}}}}"
    );
    let replies = drive(&req(10, "tools/call", &params));
    let (text, is_error) = tool_text(&replies[0]);
    assert!(!is_error, "text was: {text}");
    assert!(
        text.contains("references.gos:2:"),
        "references text was: {text}"
    );
}

#[test]
fn skill_card_resource_lists_and_reads() {
    let mut input = req(11, "resources/list", "{}");
    input.push_str(&req(
        12,
        "resources/read",
        "{\"uri\":\"gossamer://skill-card\"}",
    ));
    let replies = drive(&input);
    let resources = json::get(json::get(&replies[0], "result").unwrap(), "resources")
        .and_then(json::as_array)
        .unwrap();
    assert_eq!(
        json::get(&resources[0], "uri").and_then(json::as_str),
        Some("gossamer://skill-card")
    );
    let contents = json::get(json::get(&replies[1], "result").unwrap(), "contents")
        .and_then(json::as_array)
        .unwrap();
    let text = json::get(&contents[0], "text")
        .and_then(json::as_str)
        .unwrap();
    assert!(text.contains("Gossamer"));
}

#[test]
fn unknown_resource_yields_resource_not_found() {
    let replies = drive(&req(
        13,
        "resources/read",
        "{\"uri\":\"gossamer://nonesuch\"}",
    ));
    let error = json::get(&replies[0], "error").unwrap();
    assert_eq!(
        json::get(error, "code").and_then(json::as_i64),
        Some(-32002)
    );
}

#[test]
fn skill_card_prompt_lists_and_renders() {
    let mut input = req(14, "prompts/list", "{}");
    input.push_str(&req(15, "prompts/get", "{\"name\":\"skill-card\"}"));
    let replies = drive(&input);
    let prompts = json::get(json::get(&replies[0], "result").unwrap(), "prompts")
        .and_then(json::as_array)
        .unwrap();
    assert_eq!(
        json::get(&prompts[0], "name").and_then(json::as_str),
        Some("skill-card")
    );
    let messages = json::get(json::get(&replies[1], "result").unwrap(), "messages")
        .and_then(json::as_array)
        .unwrap();
    assert_eq!(
        json::get(&messages[0], "role").and_then(json::as_str),
        Some("user")
    );
}
