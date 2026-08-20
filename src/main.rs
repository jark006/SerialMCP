mod http_transport;
mod mcp;
mod modem;
mod serial;

use std::{net::SocketAddr, process::ExitCode};

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8765";

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("serial-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let Some(bind_address) = parse_args(std::env::args().skip(1))? else {
        println!("{}", help_text());
        return Ok(());
    };
    let listener = tokio::net::TcpListener::bind(bind_address)
        .await
        .map_err(|error| format!("failed to bind {bind_address}: {error}"))?;
    let local_address = listener
        .local_addr()
        .map_err(|error| format!("failed to read listener address: {error}"))?;
    eprintln!(
        "serial-mcp listening on http://{local_address}{}",
        http_transport::MCP_ENDPOINT
    );
    if !local_address.ip().is_loopback() {
        eprintln!("warning: non-loopback binding has no authentication; restrict network access");
    }

    axum::serve(listener, http_transport::router())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("HTTP server failed: {error}"))
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Option<SocketAddr>, String> {
    let mut args = args.into_iter();
    let mut bind_address = DEFAULT_BIND_ADDRESS.to_string();
    let mut bind_was_set = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(None),
            "--bind" => {
                if bind_was_set {
                    return Err("--bind may only be specified once".to_string());
                }
                bind_address = args
                    .next()
                    .ok_or_else(|| "--bind requires an IP:PORT value".to_string())?;
                bind_was_set = true;
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }

    bind_address
        .parse()
        .map(Some)
        .map_err(|error| format!("invalid --bind address '{bind_address}': {error}"))
}

fn help_text() -> &'static str {
    "serial-mcp: Windows x64 MCP Streamable HTTP server for serial/UART consoles.\n\
     Usage: serial-mcp.exe [--bind <IP:PORT>]\n\n\
     Options:\n\
       --bind <IP:PORT>  Listen address (default: 127.0.0.1:8765)\n\
       -h, --help        Show this help\n\n\
     MCP endpoint: http://127.0.0.1:8765/mcp"
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("failed to install Ctrl+C handler: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bind_address_is_loopback() {
        let address = parse_args(Vec::<String>::new()).unwrap().unwrap();
        assert_eq!(address, "127.0.0.1:8765".parse().unwrap());
    }

    #[test]
    fn bind_argument_is_parsed() {
        let address = parse_args(["--bind".to_string(), "127.0.0.1:9000".to_string()])
            .unwrap()
            .unwrap();
        assert_eq!(address, "127.0.0.1:9000".parse().unwrap());
    }

    #[test]
    fn invalid_arguments_are_rejected() {
        assert!(parse_args(["--port".to_string()]).is_err());
        assert!(parse_args(["--bind".to_string()]).is_err());
        assert!(parse_args(["--bind".to_string(), "localhost:9000".to_string()]).is_err());
    }
}
