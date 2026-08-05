//! MCP request-dispatch loop.

use std::io::{BufRead, BufWriter, Write};

use gossamer_std::json::Value;

use crate::ServerConfig;
use crate::protocol::{field, field_str, obj, response_err, response_ok, s};
use crate::transport::{Incoming, Transport};

/// Latest MCP revision the server implements; also the fallback when
/// the client omits `protocolVersion`.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// The canonical skill card; `SKILL.md` at the repo root is the source
/// of truth, mirroring `gos skill-prompt`.
const SKILL_CARD: &str = include_str!("../../../SKILL.md");
const SKILL_CARD_URI: &str = "gossamer://skill-card";

/// Runs the dispatch loop over the supplied streams until EOF.
pub(crate) fn run<R: BufRead, W: Write>(
    reader: R,
    writer: W,
    config: &ServerConfig,
) -> std::io::Result<()> {
    let mut transport = Transport::new(reader, BufWriter::new(writer));
    let mut nav = crate::nav::NavSession::new();
    loop {
        let message = match transport.read_message()? {
            Incoming::Eof => return Ok(()),
            Incoming::ParseError => {
                transport.write_message(&response_err(Value::Null, -32700, "parse error"))?;
                continue;
            }
            Incoming::Message(value) => value,
        };
        let Some(method) = field_str(&message, "method") else {
            continue;
        };
        let id = field(&message, "id").clone();
        let params = field(&message, "params").clone();
        let is_notification = matches!(id, Value::Null);

        let reply = match method {
            "initialize" => Some(response_ok(id, initialize_result(&params))),
            "notifications/initialized" | "notifications/cancelled" => None,
            "ping" => Some(response_ok(id, obj(vec![]))),
            "tools/list" => Some(response_ok(id, crate::tools::list())),
            "tools/call" => Some(crate::tools::call(id, &params, config, &mut nav)),
            "resources/list" => Some(response_ok(id, resources_list())),
            "resources/read" => Some(resources_read(id, &params)),
            "prompts/list" => Some(response_ok(id, prompts_list())),
            "prompts/get" => Some(prompts_get(id, &params)),
            _ if is_notification => None,
            other => Some(response_err(
                id,
                -32601,
                &format!("method not found: {other}"),
            )),
        };
        if let Some(reply) = reply {
            transport.write_message(&reply)?;
        }
    }
}

fn initialize_result(params: &Value) -> Value {
    let version = field_str(params, "protocolVersion").unwrap_or(PROTOCOL_VERSION);
    obj(vec![
        ("protocolVersion", s(version)),
        (
            "capabilities",
            obj(vec![
                ("tools", obj(vec![])),
                ("resources", obj(vec![])),
                ("prompts", obj(vec![])),
            ]),
        ),
        (
            "serverInfo",
            obj(vec![
                ("name", s("gos-mcp")),
                ("version", s(env!("CARGO_PKG_VERSION"))),
            ]),
        ),
        (
            "instructions",
            s(
                "Gossamer toolchain server. Run `check` before `execute`; read the \
               gossamer://skill-card resource (or the skill-card prompt) to learn \
               idiomatic Gossamer before writing .gos code. Prefer receiver methods \
               and metadata fields already returned by standard library records over \
               redundant module calls. Prefer dedicated collection contracts: \
               Stack for LIFO-only values, Queue for FIFO-only values, \
               MinHeap or MaxHeap for priority queues, and Deque only when \
               both ends matter.",
            ),
        ),
    ])
}

fn resources_list() -> Value {
    obj(vec![(
        "resources",
        Value::Array(vec![obj(vec![
            ("uri", s(SKILL_CARD_URI)),
            ("name", s("Gossamer skill card")),
            (
                "description",
                s("Self-contained idiomatic-Gossamer reference for coding agents."),
            ),
            ("mimeType", s("text/markdown")),
        ])]),
    )])
}

fn resources_read(id: Value, params: &Value) -> Value {
    match field_str(params, "uri") {
        Some(SKILL_CARD_URI) => response_ok(
            id,
            obj(vec![(
                "contents",
                Value::Array(vec![obj(vec![
                    ("uri", s(SKILL_CARD_URI)),
                    ("mimeType", s("text/markdown")),
                    ("text", s(SKILL_CARD)),
                ])]),
            )]),
        ),
        other => response_err(
            id,
            -32002,
            &format!("unknown resource: {}", other.unwrap_or("<missing uri>")),
        ),
    }
}

fn prompts_list() -> Value {
    obj(vec![(
        "prompts",
        Value::Array(vec![obj(vec![
            ("name", s("skill-card")),
            (
                "description",
                s("Teach the model idiomatic Gossamer in one step."),
            ),
        ])]),
    )])
}

fn prompts_get(id: Value, params: &Value) -> Value {
    match field_str(params, "name") {
        Some("skill-card") => response_ok(
            id,
            obj(vec![
                ("description", s("The Gossamer skill card.")),
                (
                    "messages",
                    Value::Array(vec![obj(vec![
                        ("role", s("user")),
                        (
                            "content",
                            obj(vec![("type", s("text")), ("text", s(SKILL_CARD))]),
                        ),
                    ])]),
                ),
            ]),
        ),
        other => response_err(
            id,
            -32602,
            &format!("unknown prompt: {}", other.unwrap_or("<missing name>")),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::SKILL_CARD;

    #[test]
    fn skill_card_teaches_explicit_imports_and_direct_metadata_access() {
        assert!(SKILL_CARD.contains("Import every standard library module you use"));
        assert!(SKILL_CARD.contains("entry.is_symlink"));
        assert!(SKILL_CARD.contains("fs::is_symlink(&entry.path)"));
        assert!(SKILL_CARD.contains("Calls never create `&mut` implicitly"));
    }

    #[test]
    fn skill_card_teaches_collection_literal_spellings() {
        for literal in ["`[]`", "`#[]`", "`{}`", "`#{}`", "`T::from([1,2,3])`"] {
            assert!(
                SKILL_CARD.contains(literal),
                "skill card should document {literal}"
            );
        }
        // The retired bracket spellings must not read as live syntax.
        for retired in ["`^[]`", "`_[]`", "`<[]`", "`[]>`"] {
            assert!(
                !SKILL_CARD.contains(retired),
                "skill card still presents the removed literal {retired}"
            );
        }
        assert!(SKILL_CARD.contains("Stack"));
        assert!(SKILL_CARD.contains("LIFO-only argument contract"));
        assert!(SKILL_CARD.contains("Queue"));
        assert!(SKILL_CARD.contains("FIFO-only behavior"));
        assert!(SKILL_CARD.contains("MinHeap` / `MaxHeap` for explicit priority order"));
        assert!(
            SKILL_CARD.contains("bare `[v; N]` is a syntax error"),
            "skill card should teach that a repeat literal is a fixed array"
        );
        assert!(
            SKILL_CARD.contains("`%i Tuple` documents"),
            "skill card should document the Tuple type"
        );
    }
}
