//! MCP tool table: schemas, listing, and call dispatch.

use std::time::Duration;

use gossamer_std::json::{self, Value};

use crate::ServerConfig;
use crate::exec::{self, ExecOutcome};
use crate::nav::NavSession;
use crate::protocol::{field, field_str, obj, response_err, response_ok, s};

/// Default bound on `execute` / `build` / `test` subprocess time.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// One argument in a tool's input schema.
struct Arg {
    name: &'static str,
    ty: &'static str,
    description: &'static str,
    required: bool,
}

struct Tool {
    name: &'static str,
    description: &'static str,
    args: &'static [Arg],
}

const TIMEOUT_ARG: Arg = Arg {
    name: "timeout_ms",
    ty: "integer",
    description: "Kill the process after this many milliseconds (default 120000).",
    required: false,
};

const POSITION_ARGS: &[Arg] = &[
    Arg {
        name: "file",
        ty: "string",
        description: "Path to a Gossamer source file.",
        required: true,
    },
    Arg {
        name: "line",
        ty: "integer",
        description: "1-based line number.",
        required: true,
    },
    Arg {
        name: "column",
        ty: "integer",
        description: "1-based column number.",
        required: true,
    },
];

const TOOLS: &[Tool] = &[
    Tool {
        name: "check",
        description: "Parse + resolve + typecheck a Gossamer file or project. Returns one \
                      JSON object per diagnostic (stable schema). Empty output means no \
                      findings.",
        args: &[Arg {
            name: "file",
            ty: "string",
            description: "A .gos file or directory; defaults to the project's src/.",
            required: false,
        }],
    },
    Tool {
        name: "explain",
        description: "Long-form explanation for a Gossamer diagnostic code (GT0017, \
                      GL0001, ...).",
        args: &[Arg {
            name: "code",
            ty: "string",
            description: "The diagnostic code to look up.",
            required: true,
        }],
    },
    Tool {
        name: "execute",
        description: "Execute a Gossamer program on the bytecode VM (with JIT). Returns \
                      exit code, stdout, and stderr.",
        args: &[
            Arg {
                name: "file",
                ty: "string",
                description: "Entry source file, with any filename extension, or project directory.",
                required: true,
            },
            Arg {
                name: "args",
                ty: "array",
                description: "Arguments forwarded to the program.",
                required: false,
            },
            TIMEOUT_ARG,
        ],
    },
    Tool {
        name: "build",
        description: "Compile a Gossamer program to a native executable via LLVM.",
        args: &[
            Arg {
                name: "file",
                ty: "string",
                description: "Entry .gos file; defaults to the project's entry.",
                required: false,
            },
            Arg {
                name: "release",
                ty: "boolean",
                description: "Full LLVM -O3 pipeline instead of the fast unoptimised build.",
                required: false,
            },
            TIMEOUT_ARG,
        ],
    },
    Tool {
        name: "test",
        description: "Run #[test] functions. Returns the test report.",
        args: &[
            Arg {
                name: "path",
                ty: "string",
                description: "A .gos file or project directory; defaults to the project.",
                required: false,
            },
            Arg {
                name: "filter",
                ty: "string",
                description: "Only run tests whose name contains this substring.",
                required: false,
            },
            TIMEOUT_ARG,
        ],
    },
    Tool {
        name: "fmt",
        description: "Format a Gossamer source file in place, or report formatting drift \
                      with check=true.",
        args: &[
            Arg {
                name: "file",
                ty: "string",
                description: "Path to a .gos source file.",
                required: true,
            },
            Arg {
                name: "check",
                ty: "boolean",
                description: "Report instead of rewriting; nonzero exit when unformatted.",
                required: false,
            },
        ],
    },
    Tool {
        name: "doc",
        description: "Item listing plus doc comments for a Gossamer source file.",
        args: &[Arg {
            name: "file",
            ty: "string",
            description: "Path to a .gos source file.",
            required: true,
        }],
    },
    Tool {
        name: "hover",
        description: "Type and docs for the symbol at a source position.",
        args: POSITION_ARGS,
    },
    Tool {
        name: "definition",
        description: "Location of the declaring item for the symbol at a source position.",
        args: POSITION_ARGS,
    },
    Tool {
        name: "references",
        description: "Every reference to the symbol at a source position.",
        args: POSITION_ARGS,
    },
    Tool {
        name: "workspace_symbols",
        description: "Search item declarations by name across a workspace.",
        args: &[
            Arg {
                name: "query",
                ty: "string",
                description: "Substring to match against item names.",
                required: true,
            },
            Arg {
                name: "root",
                ty: "string",
                description: "Workspace root directory (default: current directory).",
                required: false,
            },
        ],
    },
];

/// `tools/list` result.
pub(crate) fn list() -> Value {
    let tools = TOOLS.iter().map(tool_value).collect();
    obj(vec![("tools", Value::Array(tools))])
}

fn tool_value(tool: &Tool) -> Value {
    let props = tool
        .args
        .iter()
        .map(|a| {
            (
                a.name.to_string(),
                obj(vec![("type", s(a.ty)), ("description", s(a.description))]),
            )
        })
        .collect();
    let required = tool
        .args
        .iter()
        .filter(|a| a.required)
        .map(|a| s(a.name))
        .collect();
    obj(vec![
        ("name", s(tool.name)),
        ("description", s(tool.description)),
        (
            "inputSchema",
            obj(vec![
                ("type", s("object")),
                ("properties", Value::Object(props)),
                ("required", Value::Array(required)),
            ]),
        ),
    ])
}

/// `tools/call` dispatch: returns a complete JSON-RPC response.
pub(crate) fn call(
    id: Value,
    params: &Value,
    config: &ServerConfig,
    nav: &mut NavSession,
) -> Value {
    let Some(name) = field_str(params, "name") else {
        return response_err(id, -32602, "tools/call requires a `name`");
    };
    let args = field(params, "arguments");
    let outcome = match name {
        "check" => exec_tool(config, check_args(args), args),
        "explain" => match field_str(args, "code") {
            Some(code) => exec_tool(config, vec!["explain".into(), code.into()], args),
            None => Err("`code` is required".to_string()),
        },
        "execute" => match field_str(args, "file") {
            Some(file) => {
                let mut command = vec![file.to_string()];
                if let Some(extra) = json::as_array(field(args, "args")) {
                    command.extend(extra.iter().filter_map(json::as_str).map(String::from));
                }
                exec_tool(config, command, args)
            }
            None => Err("`file` is required".to_string()),
        },
        "build" => {
            let mut command = vec!["build".to_string()];
            if json::as_bool(field(args, "release")) == Some(true) {
                command.push("--release".to_string());
            }
            if let Some(file) = field_str(args, "file") {
                command.push(file.to_string());
            }
            exec_tool(config, command, args)
        }
        "test" => {
            let mut command = vec!["test".to_string()];
            if let Some(path) = field_str(args, "path") {
                command.push(path.to_string());
            }
            if let Some(filter) = field_str(args, "filter") {
                command.push("--run".to_string());
                command.push(filter.to_string());
            }
            exec_tool(config, command, args)
        }
        "fmt" => match field_str(args, "file") {
            Some(file) => {
                let mut command = vec!["fmt".to_string()];
                if json::as_bool(field(args, "check")) == Some(true) {
                    command.push("--check".to_string());
                }
                command.push(file.to_string());
                exec_tool(config, command, args)
            }
            None => Err("`file` is required".to_string()),
        },
        "doc" => match field_str(args, "file") {
            Some(file) => exec_tool(config, vec!["doc".into(), file.into()], args),
            None => Err("`file` is required".to_string()),
        },
        "hover" | "definition" | "references" => nav.position_tool(name, args),
        "workspace_symbols" => nav.workspace_symbols(args),
        other => return response_err(id, -32602, &format!("unknown tool: {other}")),
    };
    match outcome {
        Ok(result) => response_ok(id, result),
        Err(message) => response_ok(id, text_result(&message, true)),
    }
}

fn check_args(args: &Value) -> Vec<String> {
    let mut command = vec![
        "check".to_string(),
        "--message-format".to_string(),
        "json".to_string(),
    ];
    if let Some(file) = field_str(args, "file") {
        command.push(file.to_string());
    }
    command
}

fn timeout_from(args: &Value) -> Duration {
    let ms = json::as_i64(field(args, "timeout_ms"))
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    Duration::from_millis(ms)
}

fn exec_tool(config: &ServerConfig, command: Vec<String>, args: &Value) -> Result<Value, String> {
    let outcome = exec::run_gos(&config.gos_exe, &command, timeout_from(args))?;
    Ok(exec_result(&outcome))
}

fn exec_result(outcome: &ExecOutcome) -> Value {
    let status = match (outcome.timed_out, outcome.exit_code) {
        (true, _) => "timed out (killed)".to_string(),
        (false, Some(code)) => format!("exit code: {code}"),
        (false, None) => "killed by signal".to_string(),
    };
    let text = format!(
        "{status}\n\n--- stdout ---\n{}\n--- stderr ---\n{}",
        outcome.stdout, outcome.stderr
    );
    text_result(&text, outcome.timed_out)
}

/// Wraps `text` as MCP tool-call content.
pub(crate) fn text_result(text: &str, is_error: bool) -> Value {
    obj(vec![
        (
            "content",
            Value::Array(vec![obj(vec![("type", s("text")), ("text", s(text))])]),
        ),
        ("isError", Value::Bool(is_error)),
    ])
}
