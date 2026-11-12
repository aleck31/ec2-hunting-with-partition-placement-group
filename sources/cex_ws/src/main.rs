mod common;
mod http;
mod probe;
mod stream;
mod ws;

use clap::{Parser, Subcommand};

/// Latency measurements for EC2 hunting; run all instances with the same --start-at so their samples pair up.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// TCP handshake RTT to each target (network only); the main ranking metric
    Probe(probe::ProbeArgs),
    /// WebSocket request/response round trips on established connections
    Ws(ws::WsArgs),
    /// HTTP request round trips on keep-alive connections (REST APIs)
    Http(http::HttpArgs),
    /// Arrival times of stream events, compared across instances
    Stream(stream::StreamArgs),
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let result = match Cli::parse().command {
        Command::Probe(args) => probe::run(args).await,
        Command::Ws(args) => ws::run(args).await,
        Command::Http(args) => http::run(args).await,
        Command::Stream(args) => stream::run(args).await,
    };
    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
