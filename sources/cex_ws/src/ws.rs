use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, client_async_tls_with_config, tungstenite::protocol::Message,
};
use url::Url;

use crate::common::{self, Grid, Result, RunArgs};

pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const DEFAULT_REQUEST: &str =
    r#"{"id": "{id}","method": "depth","params": {"symbol": "BTCUSDT","limit": 5}}"#;
const CONNECT_ATTEMPTS: u32 = 3;
/// Idle time before each connection's first request, to drain the previous connection and reconnect
const GAP: Duration = Duration::from_secs(2);

#[derive(clap::Args)]
pub struct WsArgs {
    /// Target IP; TCP goes straight there while TLS SNI and Host come from --url
    target_ip: IpAddr,
    /// WebSocket API URL, e.g. wss://ws-fapi.binance.com/ws-fapi/v1
    #[arg(long)]
    url: Url,
    /// JSON request; {id} is replaced per request and must come back as the response's string "id"
    #[arg(long, default_value = DEFAULT_REQUEST)]
    request: String,
    /// Measured responses in total, split evenly over the connections
    #[arg(long, default_value_t = 600)]
    samples: u64,
    /// Sequential connections; each may land on a different backend behind the target IP
    #[arg(long, default_value_t = 3)]
    connections: u64,
    /// Responses discarded at the start of each connection
    #[arg(long, default_value_t = 5)]
    warmup: u64,
    #[arg(long, default_value_t = 100)]
    interval_ms: u64,
    #[command(flatten)]
    run: RunArgs,
}

/// Slot plan shared by all instances: each connection gets `gap` idle slots, then `warmup` and `per_conn` request slots.
#[derive(Clone, Copy)]
struct Layout {
    gap: u64,
    warmup: u64,
    per_conn: u64,
    connections: u64,
}

impl Layout {
    fn period(&self) -> u64 {
        self.gap + self.warmup + self.per_conn
    }

    /// First and one-past-last request slot of connection `c`
    fn requests(&self, c: u64) -> (u64, u64) {
        (c * self.period() + self.gap, (c + 1) * self.period())
    }

    fn is_warmup(&self, slot: u64) -> bool {
        slot % self.period() < self.gap + self.warmup
    }
}

struct Sample {
    slot: u64,
    conn: u64,
    warmup: bool,
    status: &'static str,
    offset_us: i64,
    rtt_us: u64,
    srtt_us: u64,
}

struct ConnStat {
    tcp_connect_us: u64,
    retrans: u32,
}

pub async fn run(args: WsArgs) -> Result<()> {
    if args.url.host_str().is_none() || args.url.port_or_known_default().is_none() {
        return Err("--url needs a host and a ws:// or wss:// scheme".into());
    }
    if !args.request.contains("{id}") {
        return Err("--request must contain {id}".into());
    }
    if args.samples == 0 || args.connections == 0 || args.interval_ms == 0 {
        return Err("--samples, --connections and --interval-ms must be positive".into());
    }
    let layout = Layout {
        gap: (GAP.as_millis() as u64).div_ceil(args.interval_ms).max(1),
        warmup: args.warmup,
        per_conn: args.samples.div_ceil(args.connections),
        connections: args.connections,
    };
    let grid = Grid::new(args.run.start_at, Duration::from_millis(args.interval_ms));
    let addr = SocketAddr::new(args.target_ip, args.url.port_or_known_default().unwrap());

    let mut samples = Vec::new();
    let mut conns = Vec::new();
    for c in 0..layout.connections {
        let (first, end) = layout.requests(c);
        grid.sleep_until(c * layout.period()).await;
        let sample = |slot, status, offset_us, rtt_us, srtt_us| Sample {
            slot,
            conn: c,
            warmup: layout.is_warmup(slot),
            status,
            offset_us,
            rtt_us,
            srtt_us,
        };
        let (mut ws, tcp_connect) = match connect(&args.url, addr).await {
            Ok(connected) => connected,
            Err(e) => {
                eprintln!("connection {c}: {e}");
                samples.extend((first..end).map(|slot| sample(slot, "noconn", 0, 0, 0)));
                continue;
            }
        };

        let mut pending: HashMap<String, (u64, Instant, i64)> = HashMap::new();
        let mut retrans = 0;
        let mut next = first;
        // Wait for late responses into the next gap; reconnecting takes far less than the other half
        let drain_until = grid.deadline(end) + GAP / 2;
        loop {
            if next >= end && pending.is_empty() {
                break;
            }
            tokio::select! {
                msg = ws.next() => {
                    // Taken before JSON parsing so the round trip covers only network and exchange time
                    let recv = Instant::now();
                    match msg {
                        Some(Ok(Message::Text(data))) => {
                            let id = serde_json::from_str::<Value>(&data).ok().and_then(|j| j["id"].as_str().map(str::to_owned));
                            let Some((slot, sent, offset)) = id.and_then(|id| pending.remove(&id)) else { continue };
                            let info = common::tcp_info(fd(&ws));
                            retrans = info.as_ref().map_or(retrans, |i| i.total_retrans);
                            let srtt = info.map_or(0, |i| i.rtt_us as u64);
                            samples.push(sample(slot, "ok", offset, (recv - sent).as_micros() as u64, srtt));
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            if let Err(e) = ws.send(Message::Pong(payload)).await {
                                eprintln!("connection {c}: pong failed: {e}");
                                break;
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => { eprintln!("connection {c}: {e}"); break; }
                        None => { eprintln!("connection {c}: closed by peer"); break; }
                    }
                }
                on_time = grid.wait(next), if next < end => {
                    let slot = next;
                    next += 1;
                    if !on_time {
                        samples.push(sample(slot, "late", 0, 0, 0));
                        continue;
                    }
                    let offset = grid.offset_us(slot);
                    let id = slot.to_string();
                    let sent = Instant::now();
                    if let Err(e) = ws.send(Message::Text(args.request.replace("{id}", &id).into())).await {
                        eprintln!("connection {c}: send failed: {e}");
                        break;
                    }
                    pending.insert(id, (slot, sent, offset));
                }
                _ = tokio::time::sleep_until(drain_until.into()), if next >= end => break,
            }
        }
        samples.extend(
            pending
                .into_values()
                .map(|(slot, _, offset)| sample(slot, "timeout", offset, 0, 0)),
        );
        samples.extend((next..end).map(|slot| sample(slot, "noconn", 0, 0, 0)));
        conns.push(ConnStat {
            tcp_connect_us: tcp_connect.as_micros() as u64,
            retrans,
        });
        let _ = ws.close(None).await;
    }
    samples.sort_by_key(|s| s.slot);

    let ok = |s: &&Sample| s.status == "ok" && !s.warmup;
    println!("App round trip in µs (request write to response frame, before JSON parsing):");
    println!(
        "all: {}",
        common::stats(
            &samples
                .iter()
                .filter(ok)
                .map(|s| s.rtt_us)
                .collect::<Vec<_>>()
        )
    );
    for (c, conn) in conns.iter().enumerate() {
        let rtt: Vec<u64> = samples
            .iter()
            .filter(ok)
            .filter(|s| s.conn == c as u64)
            .map(|s| s.rtt_us)
            .collect();
        println!(
            "connection {c}: {} tcp_connect={} retrans={}",
            common::stats(&rtt),
            conn.tcp_connect_us,
            conn.retrans
        );
    }
    let srtt: Vec<u64> = samples
        .iter()
        .filter(ok)
        .map(|s| s.srtt_us)
        .filter(|&v| v > 0)
        .collect();
    println!(
        "TCP srtt (includes exchange time, the ACK rides on the response): {}",
        common::stats(&srtt)
    );
    for status in ["timeout", "late", "noconn"] {
        let n = samples.iter().filter(|s| s.status == status).count();
        if n > 0 {
            println!("{status}: {n}");
        }
    }

    if let Some(path) = &args.run.out {
        let rows = samples.iter().map(|s| {
            format!(
                "{},{},{},{},{},{},{}",
                s.slot, s.conn, s.warmup as u8, s.status, s.offset_us, s.rtt_us, s.srtt_us
            )
        });
        common::write_csv(
            path,
            "slot,conn,warmup,status,offset_us,rtt_us,srtt_us",
            rows,
        )?;
    }
    Ok(())
}

/// Connects straight to the target IP with retries; TLS SNI and the Host header still come from the URL.
pub async fn connect(url: &Url, addr: SocketAddr) -> Result<(Ws, Duration)> {
    let mut backoff = Duration::from_millis(500);
    for attempt in 1.. {
        let start = Instant::now();
        let result = async {
            let tcp = TcpStream::connect(addr).await?;
            let tcp_connect = start.elapsed();
            tcp.set_nodelay(true)?;
            let (ws, _) = client_async_tls_with_config(url.as_str(), tcp, None, None).await?;
            Ok::<_, Box<dyn std::error::Error>>((ws, tcp_connect))
        }
        .await;
        match result {
            Ok(connected) => return Ok(connected),
            Err(e) if attempt < CONNECT_ATTEMPTS => {
                eprintln!(
                    "connect to {addr} failed (attempt {attempt}): {e}; retrying in {} ms",
                    backoff.as_millis()
                );
                tokio::time::sleep(backoff).await;
                backoff *= 2;
            }
            Err(e) => {
                return Err(format!(
                    "connect to {addr} failed after {CONNECT_ATTEMPTS} attempts: {e}"
                )
                .into());
            }
        }
    }
    unreachable!()
}

pub fn fd(ws: &Ws) -> std::os::fd::RawFd {
    match ws.get_ref() {
        MaybeTlsStream::Rustls(tls) => tls.get_ref().0.as_raw_fd(),
        MaybeTlsStream::Plain(tcp) => tcp.as_raw_fd(),
        _ => -1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_gives_each_connection_a_gap_then_warmup_then_samples() {
        let l = Layout {
            gap: 2,
            warmup: 1,
            per_conn: 3,
            connections: 2,
        };
        assert_eq!(l.requests(0), (2, 6));
        assert_eq!(l.requests(1), (8, 12));
        let warm: Vec<u64> = (0..12).filter(|&s| l.is_warmup(s)).collect();
        assert_eq!(warm, [0, 1, 2, 6, 7, 8]);
    }
}
