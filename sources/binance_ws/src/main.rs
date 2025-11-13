use std::time::{Instant, SystemTime, UNIX_EPOCH};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message, MaybeTlsStream};
use url::Url;
use tokio::sync::mpsc;
use std::collections::HashMap;
use serde_json::Value;
use std::env;
use std::process;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        println!("Usage: {} <target_ip>", args[0]);
        return;
    }
    let target_ip = args[1].clone();

    let url = Url::parse("wss://ws-fapi.binance.com/ws-fapi/v1").unwrap();
    let app_start = Instant::now();

    println!("[{}] Program started, target ip: {}", timestamp_us(), target_ip);

    let mut ws_stream = loop {
        let (mut stream, _) = connect_async(url.as_str())
            .await
            .expect("Failed to connect");

        let underlying = stream.get_ref();
        let peer_ip = match underlying {
            MaybeTlsStream::Rustls(tls) => tls.get_ref().0.peer_addr().unwrap().ip().to_string(),
            MaybeTlsStream::Plain(tcp) => tcp.peer_addr().unwrap().ip().to_string(),
            _ => "Unknown".to_string(),
        };

        if peer_ip == target_ip {
            break stream;
        } else {
            println!("[{}, {}] Connected to wrong IP, disconnecting...", timestamp_us(), peer_ip);
            stream.close(None).await.expect("Failed to close");
            println!("[{}] Reconnecting...", timestamp_us());
        }
    };

    let underlying = ws_stream.get_ref();
    let peer_ip = match underlying {
        MaybeTlsStream::Rustls(tls) => tls.get_ref().0.peer_addr().unwrap().ip().to_string(),
        MaybeTlsStream::Plain(tcp) => tcp.peer_addr().unwrap().ip().to_string(),
        _ => "Unknown".to_string(),
    };

    let connected = Instant::now();
    println!("[{}, {}] Connected to Binance Futures WebSocket, taken {}µs", timestamp_us(), peer_ip, (connected - app_start).as_micros());

    let (send_tx, send_rx) = mpsc::channel::<(String, Instant, Message)>(32);

    tokio::spawn(async move {
        let mut send_rx = send_rx;
        let mut min_latency = u128::MAX;
        let mut max_latency = 0u128;
        let mut sum_latency = 0u128;
        let mut count = 0u64;
        let mut warm_up_count = 0u64;
        let mut send_times: HashMap<String, Instant> = HashMap::new();
        let mut latencies: Vec<u128> = Vec::new();
        loop {
            tokio::select! {
                Some(msg) = ws_stream.next() => {
                    match msg {
                        Ok(Message::Text(data)) => {
                            if let Ok(json) = serde_json::from_str::<Value>(&data) {
                                if let Some(id) = json["id"].as_str() {
                                    if let Some(send_time) = send_times.remove(id) {
                                        if warm_up_count > 5 {
                                            let latency_us = send_time.elapsed().as_micros();

                                            // Update min latency
                                            if latency_us < min_latency {
                                                min_latency = latency_us;
                                            }

                                            // Update max latency
                                            if latency_us > max_latency {
                                                max_latency = latency_us;
                                            }

                                            // Update running sum and count for average
                                            sum_latency += latency_us;
                                            count += 1;
                                            let avg = if count > 0 {
                                                sum_latency as f64 / count as f64
                                            } else {
                                                0.0
                                            };
                                            // TODO: send result to observability platform for timeseries vistualization, work around to calculate locally
                                            latencies.push(latency_us);
                                            println!("[{},{},{}] response latency: {}µs, min: {}µs, max: {}µs, average: {:.2}µs, message: {}",
                                                timestamp_us(), peer_ip, count, latency_us, min_latency, max_latency, avg, json);

                                            if count == 600 {
                                                latencies.sort();
                                                let n = latencies.len();
                                                let p50 = latencies[(0.50 * (n - 1) as f64).round() as usize];
                                                let p75 = latencies[(0.75 * (n - 1) as f64).round() as usize];
                                                let p90 = latencies[(0.90 * (n - 1) as f64).round() as usize];
                                                let p99 = latencies[(0.99 * (n - 1) as f64).round() as usize];
                                                let p999 = latencies[(0.999 * (n - 1) as f64).round() as usize];
                                                let min = latencies[0];
                                                let max = latencies[n - 1];

                                                println!("[{}] Latency statistics after 600 samples:", timestamp_us());
                                                println!("min: {}µs", min);
                                                println!("p50: {}µs", p50);
                                                println!("p75: {}µs", p75);
                                                println!("p90: {}µs", p90);
                                                println!("p99: {}µs", p99);
                                                println!("p99.9: {}µs", p999);
                                                println!("max: {}µs", max);

                                                process::exit(0);
                                            }
                                        } else {
                                            println!("[{},{},{}] Warming up, response message: {}", timestamp_us(), peer_ip, count, json);
                                            warm_up_count += 1;
                                        }
                                    }
                                }
                            }
                        }
                        Ok(Message::Ping(data)) => {
                            ws_stream.send(Message::Pong(data)).await.expect("Failed to send pong");
                        }
                        Ok(Message::Pong(_)) => {
                            // Ignore or log unsolicited pong
                        }
                        Ok(_) => {
                            // Handle other messages if needed
                        }
                        Err(e) => {
                            println!("[{},{},{}] WebSocket error: {}", timestamp_us(), peer_ip, count, e);
                            break;
                        }
                    }
                }
                Some((id, req_start, to_send)) = send_rx.recv() => {
                    let send_start = Instant::now();
                    ws_stream.send(to_send).await.expect("Failed to send message");
                    println!("[{},{},{}] send -> acknowledged by kernel/NIC latency {}µs", timestamp_us(), peer_ip, count, send_start.elapsed().as_micros());
                    send_times.insert(id, req_start);
                }
                else => break,
            }
        }
    });

    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));

    loop {
        interval.tick().await;

        let id = timestamp_us().to_string();

        let req = format!(r#"{{"id": "{}","method": "depth","params": {{"symbol": "BTCUSDT","limit": 5}}}}"#, id);

        let start = Instant::now();

        println!("[{}] request message: {}", timestamp_us(), &req);

        send_tx.send((id.clone(), start, Message::Text(req.into()))).await.expect("Failed to queue send");
    }
}

fn timestamp_us() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_micros()
}