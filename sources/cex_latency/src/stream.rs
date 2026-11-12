use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::protocol::Message;
use url::Url;

use crate::common::{self, Grid, Result, RunArgs};
use crate::ws;

/// Idle seconds before each connection's recording window, used to connect and subscribe
const GAP_S: u64 = 2;

#[derive(clap::Args)]
pub struct StreamArgs {
    /// Target IP; TCP goes straight there while TLS SNI and Host come from --url
    target_ip: IpAddr,
    /// Stream URL, e.g. wss://fstream.binance.com/ws/btcusdt@bookTicker
    #[arg(long)]
    url: Url,
    /// Message sent after connecting, e.g. a SUBSCRIBE request
    #[arg(long)]
    subscribe: Option<String>,
    /// JSON field that identifies the same event on every instance (Binance bookTicker: u, aggTrade: a)
    #[arg(long, default_value = "u")]
    key: String,
    /// JSON field with the exchange event time in ms, recorded for reference
    #[arg(long, default_value = "E")]
    event_time: String,
    /// Recording window per connection
    #[arg(long, default_value_t = 30)]
    seconds: u64,
    /// Sequential connections; each may land on a different stream backend
    #[arg(long, default_value_t = 2)]
    connections: u64,
    #[command(flatten)]
    run: RunArgs,
}

struct Arrival {
    conn: u64,
    key: String,
    event_time_ms: u64,
    recv_wall_us: u64,
}

/// Records when each stream event arrives on this host's wall clock, next to the exchange's event time.
pub async fn run(args: StreamArgs) -> Result<()> {
    if args.url.host_str().is_none() || args.url.port_or_known_default().is_none() {
        return Err("--url needs a host and a ws:// or wss:// scheme".into());
    }
    if args.seconds == 0 || args.connections == 0 {
        return Err("--seconds and --connections must be positive".into());
    }
    let grid = Grid::new(args.run.start_at, Duration::from_secs(1));
    let period = GAP_S + args.seconds;
    let addr = SocketAddr::new(args.target_ip, args.url.port_or_known_default().unwrap());

    let mut arrivals = Vec::new();
    for c in 0..args.connections {
        grid.sleep_until(c * period).await;
        let (mut ws, tcp_connect) = match ws::connect(&args.url, addr).await {
            Ok(connected) => connected,
            Err(e) => {
                eprintln!("connection {c}: {e}");
                continue;
            }
        };
        if let Some(msg) = &args.subscribe {
            ws.send(Message::Text(msg.clone().into())).await?;
        }
        let (from, until) = (
            grid.deadline(c * period + GAP_S),
            grid.deadline((c + 1) * period),
        );
        let before = arrivals.len();
        loop {
            tokio::select! {
                msg = ws.next() => {
                    // Wall clock to line up with the exchange's event time; too coarse to compare hosts
                    let recv_wall_us = common::wall_us();
                    match msg {
                        Some(Ok(Message::Text(data))) => {
                            if std::time::Instant::now() < from {
                                continue;
                            }
                            let Ok(json) = serde_json::from_str::<Value>(&data) else { continue };
                            let Some(key) = field(&json, &args.key).map(as_key) else { continue };
                            let event_time_ms = field(&json, &args.event_time).and_then(Value::as_u64).unwrap_or(0);
                            arrivals.push(Arrival { conn: c, key, event_time_ms, recv_wall_us });
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            if ws.send(Message::Pong(payload)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => { eprintln!("connection {c}: {e}"); break; }
                        None => { eprintln!("connection {c}: closed by peer"); break; }
                    }
                }
                _ = tokio::time::sleep_until(until.into()) => break,
            }
        }
        let got = &arrivals[before..];
        // Exchange event times are ms and include the clock offset, so this is only a sanity check
        // Signed: clock offset and ms-truncated event times can put the receipt before the event
        let lag: Vec<i64> = got
            .iter()
            .filter(|a| a.event_time_ms > 0)
            .map(|a| a.recv_wall_us as i64 - (a.event_time_ms * 1000) as i64)
            .collect();
        println!(
            "connection {c}: {} events ({:.0}/s) tcp_connect={}µs, receive minus event time in µs: {}",
            got.len(),
            got.len() as f64 / args.seconds as f64,
            tcp_connect.as_micros(),
            common::stats(&lag)
        );
        let _ = ws.close(None).await;
    }

    if let Some(path) = &args.run.out {
        let rows = arrivals.iter().map(|a| {
            format!(
                "{},{},{},{}",
                a.conn, a.key, a.event_time_ms, a.recv_wall_us
            )
        });
        common::write_csv(path, "conn,key,event_time_ms,recv_wall_us", rows)?;
    }
    Ok(())
}

/// Looks up a field at the top level or inside a combined stream's "data" object
fn field<'a>(json: &'a Value, name: &str) -> Option<&'a Value> {
    json.get(name)
        .or_else(|| json.get("data").and_then(|d| d.get(name)))
}

fn as_key(v: &Value) -> String {
    match v {
        Value::String(s) => s.replace(',', ";"),
        other => other.to_string(),
    }
}
