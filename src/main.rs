mod mcp;
mod serial;

use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

fn main() {
    if let Some(message) = parse_help() {
        eprintln!("{message}");
        return;
    }

    let mut hub = serial::SerialHub::new();
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!("stdin read failed: {error}");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(message) => mcp::handle_message(message, &mut hub),
            Err(_) => Some(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {"code": -32700, "message": "parse error"}
            })),
        };
        if let Some(response) = response
            && (serde_json::to_writer(&mut stdout, &response).is_err()
                || stdout.write_all(b"\n").is_err()
                || stdout.flush().is_err())
        {
            break;
        }
    }
}

fn parse_help() -> Option<String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return None;
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        Some(
            "serial-mcp: Windows x64 MCP stdio server for serial/UART consoles.\n\
             Usage: serial-mcp.exe\n\n\
             Configure this binary in Codex as an MCP stdio server; it must not be run interactively.\n"
                .to_string(),
        )
    } else {
        Some(format!("unknown argument: {}", args.join(" ")))
    }
}
