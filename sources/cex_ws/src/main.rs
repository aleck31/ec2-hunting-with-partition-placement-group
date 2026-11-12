use std::collections::HashMap;
use std::env;
use std::net::{IpAddr, SocketAddr};
use std::process;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, client_async_tls_with_config, tungstenite::protocol::Message,
};
use url::Url;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const WARMUP: usize = 5;
const CONNECT_ATTEMPTS: u32 = 5;
const DEFAULT_REQUEST: &str =
    r#"{"id": "{id}","method": "depth","params": {"symbol": "BTCUSDT","limit": 5}}"#;
const USAGE: &str = "Usage: cex_ws <target_ip> --url <wss://host/path> [--request <json containing {id}>] [--samples N] [--interval-ms N]";

struct Args {
    target_ip: IpAddr,
    url: Url,
    request: String,
    samples: usize,
    interval: Duration,
}

struct TcpInfo {
    rtt_us: u32,
    rttvar_us: u32,
    total_retrans: u32,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = parse_args();
    println!(
        "[{}] Program started, target ip: {}, url: {}",
        timestamp_us(),
        args.target_ip,
        args.url
    );

    let (mut ws, tcp_connect) = connect(&args).await;
    println!(
        "[{}, {}] Connected, TCP connect took {}µs",
        timestamp_us(),
        args.target_ip,
        tcp_connect.as_micros()
    );

    let mut interval = tokio::time::interval(args.interval);
    let mut pending: HashMap<String, Instant> = HashMap::new();
    let mut seq: u64 = 0;
    let mut warmed_up = 0;
    let mut app_rtt: Vec<u64> = Vec::with_capacity(args.samples);
    let mut tcp_rtt: Vec<u64> = Vec::with_capacity(args.samples);

    loop {
        tokio::select! {
            msg = ws.next() => {
                // Taken before JSON parsing so the latency covers only network and exchange time
                let recv = Instant::now();
                match msg {
                    Some(Ok(Message::Text(data))) => {
                        let id = serde_json::from_str::<Value>(&data).ok().and_then(|j| j["id"].as_str().map(str::to_owned));
                        let Some(sent) = id.and_then(|id| pending.remove(&id)) else { continue };
                        let latency_us = (recv - sent).as_micros() as u64;
                        if warmed_up < WARMUP {
                            warmed_up += 1;
                            println!("[{},{}] Warming up {}/{}, response latency: {}µs", timestamp_us(), args.target_ip, warmed_up, WARMUP, latency_us);
                            continue;
                        }
                        app_rtt.push(latency_us);
                        let tcp = tcp_info(ws.get_ref());
                        if let Some(t) = &tcp {
                            tcp_rtt.push(t.rtt_us as u64);
                        }
                        let n = app_rtt.len();
                        let avg = app_rtt.iter().sum::<u64>() as f64 / n as f64;
                        println!("[{},{},{}] response latency: {}µs, min: {}µs, max: {}µs, average: {:.2}µs, tcp srtt: {}, message: {}",
                            timestamp_us(), args.target_ip, n, latency_us, app_rtt.iter().min().unwrap(), app_rtt.iter().max().unwrap(), avg,
                            tcp.as_ref().map_or("n/a".to_string(), |t| format!("{}µs (rttvar {}µs)", t.rtt_us, t.rttvar_us)), data);

                        if n == args.samples {
                            println!("[{}] Latency statistics after {} samples:", timestamp_us(), n);
                            print_stats("App round trip", app_rtt);
                            if !tcp_rtt.is_empty() {
                                print_stats("TCP smoothed RTT (kernel)", tcp_rtt);
                            }
                            if let Some(t) = tcp {
                                println!("TCP retransmits: {}", t.total_retrans);
                            }
                            process::exit(0);
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if let Err(e) = ws.send(Message::Pong(payload)).await {
                            fail(&format!("Failed to send pong: {e}"));
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => fail(&format!("WebSocket error: {e}")),
                    None => fail("WebSocket closed by peer"),
                }
            }
            _ = interval.tick() => {
                seq += 1;
                let id = seq.to_string();
                let req = args.request.replace("{id}", &id);
                let sent = Instant::now();
                if let Err(e) = ws.send(Message::Text(req.into())).await {
                    fail(&format!("Failed to send request: {e}"));
                }
                println!("[{},{},{}] request {} sent, write took {}µs", timestamp_us(), args.target_ip, app_rtt.len(), id, sent.elapsed().as_micros());
                pending.insert(id, sent);
            }
        }
    }
}

fn parse_args() -> Args {
    let argv: Vec<String> = env::args().skip(1).collect();
    let mut target_ip = None;
    let mut url = None;
    let mut request = DEFAULT_REQUEST.to_string();
    let mut samples: usize = 600;
    let mut interval_ms: u64 = 1000;

    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        if let Some(flag) = arg.strip_prefix("--") {
            let Some(value) = argv.get(i + 1) else {
                usage(&format!("--{flag} needs a value"))
            };
            match flag {
                "url" => {
                    url = Some(
                        Url::parse(value).unwrap_or_else(|e| usage(&format!("invalid --url: {e}"))),
                    )
                }
                "request" => request = value.clone(),
                "samples" => samples = value.parse().unwrap_or_else(|_| usage("invalid --samples")),
                "interval-ms" => {
                    interval_ms = value
                        .parse()
                        .unwrap_or_else(|_| usage("invalid --interval-ms"))
                }
                _ => usage(&format!("unknown option --{flag}")),
            }
            i += 2;
        } else if target_ip.is_none() {
            target_ip = Some(
                arg.parse()
                    .unwrap_or_else(|_| usage(&format!("invalid target_ip: {arg}"))),
            );
            i += 1;
        } else {
            usage(&format!("unexpected argument: {arg}"));
        }
    }

    let Some(target_ip) = target_ip else {
        usage("missing target_ip")
    };
    let Some(url) = url else {
        usage("missing --url")
    };
    if url.host_str().is_none() || url.port_or_known_default().is_none() {
        usage("--url needs a host and a ws:// or wss:// scheme");
    }
    if !request.contains("{id}") {
        usage("--request must contain {id}");
    }
    if samples == 0 || interval_ms == 0 {
        usage("--samples and --interval-ms must be positive");
    }
    Args {
        target_ip,
        url,
        request,
        samples,
        interval: Duration::from_millis(interval_ms),
    }
}

/// Connects straight to the target IP; TLS SNI and the Host header still come from the URL.
async fn connect(args: &Args) -> (Ws, Duration) {
    let addr = SocketAddr::new(args.target_ip, args.url.port_or_known_default().unwrap());
    let mut backoff = Duration::from_secs(1);
    for attempt in 1..=CONNECT_ATTEMPTS {
        match try_connect(&args.url, addr).await {
            Ok(connected) => return connected,
            Err(e) if attempt < CONNECT_ATTEMPTS => {
                eprintln!(
                    "[{}] Connect attempt {attempt} to {addr} failed: {e}; retrying in {}s",
                    timestamp_us(),
                    backoff.as_secs()
                );
                tokio::time::sleep(backoff).await;
                backoff *= 2;
            }
            Err(e) => fail(&format!(
                "Connect to {addr} failed after {CONNECT_ATTEMPTS} attempts: {e}"
            )),
        }
    }
    unreachable!()
}

async fn try_connect(
    url: &Url,
    addr: SocketAddr,
) -> Result<(Ws, Duration), Box<dyn std::error::Error>> {
    let start = Instant::now();
    let tcp = TcpStream::connect(addr).await?;
    let tcp_connect = start.elapsed();
    tcp.set_nodelay(true)?;
    let (ws, _) = client_async_tls_with_config(url.as_str(), tcp, None, None).await?;
    Ok((ws, tcp_connect))
}

#[cfg(target_os = "linux")]
fn tcp_info(stream: &MaybeTlsStream<TcpStream>) -> Option<TcpInfo> {
    use std::os::fd::AsRawFd;
    let fd = match stream {
        MaybeTlsStream::Rustls(tls) => tls.get_ref().0.as_raw_fd(),
        MaybeTlsStream::Plain(tcp) => tcp.as_raw_fd(),
        _ => return None,
    };
    // SAFETY: tcp_info is plain old data and getsockopt writes at most `len` bytes into it
    let mut info: libc::tcp_info = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::tcp_info>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_INFO,
            (&raw mut info).cast(),
            &mut len,
        )
    };
    (rc == 0).then_some(TcpInfo {
        rtt_us: info.tcpi_rtt,
        rttvar_us: info.tcpi_rttvar,
        total_retrans: info.tcpi_total_retrans,
    })
}

#[cfg(not(target_os = "linux"))]
fn tcp_info(_stream: &MaybeTlsStream<TcpStream>) -> Option<TcpInfo> {
    None
}

fn print_stats(name: &str, mut values: Vec<u64>) {
    values.sort_unstable();
    let n = values.len();
    println!("{name}:");
    println!("min: {}µs", values[0]);
    for (label, p) in [
        ("p50", 0.50),
        ("p75", 0.75),
        ("p90", 0.90),
        ("p99", 0.99),
        ("p99.9", 0.999),
    ] {
        // Skip percentiles with fewer than 5 samples above them; they would just echo the max
        if n as f64 * (1.0 - p) >= 5.0 {
            println!(
                "{label}: {}µs",
                values[((n - 1) as f64 * p).round() as usize]
            );
        }
    }
    println!("max: {}µs", values[n - 1]);
}

fn usage(msg: &str) -> ! {
    eprintln!("Error: {msg}\n{USAGE}");
    process::exit(2);
}

fn fail(msg: &str) -> ! {
    eprintln!("[{}] {msg}", timestamp_us());
    process::exit(1);
}

fn timestamp_us() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_micros()
}
