# Target profiles

A profile in `targets/` describes one exchange: its endpoints and how to measure each. `targets/binance-usdm.toml` is a worked example; copy it for another exchange.

```toml
name = "my-exchange"

[weights]                    # roles in the combined score
order = 0.5
md = 0.5

[[endpoints]]
name = "ws-api"              # used by --endpoint and in reports
protocol = "ws"              # tcp | ws | http | stream
role = "order"               # order | md; default md for streams, order otherwise
url = "wss://ws.example.com/v1"
request = '{"id":"{id}","op":"ping"}'
id_field = "id"

[[endpoints]]
name = "md"
protocol = "stream"
url = "wss://stream.example.com/ws"
subscribe = '{"op":"subscribe","args":["book.BTCUSDT"]}'
key_field = "seq"            # same value on every instance for the same event
event_time_field = "ts"

[[endpoints]]
name = "rest"
protocol = "http"
url = "https://api.example.com/v1/time"
method = "GET"               # optional: body, headers = ["Name: value"]

[[endpoints]]
name = "fix"
protocol = "tcp"             # handshake RTT only
host = "fix.example.com"
port = 9000
```

Any endpoint may add `args = [...]`, extra `cex_latency` options. `hunt.py run --profile P` probes the TCP handshake to the IPs of all endpoints, each on its own port; `--endpoint NAME` runs that endpoint's own protocol. Without a profile, give the mode and the `cex_latency` arguments yourself: `hunt.py run probe --hosts ws-fapi.binance.com` or `hunt.py run ws -- <ip> --url wss://...`.

Before adding an exchange, check that its endpoints are not behind a CDN or proxying load balancer ([who answers the SYN](measurement.md#what-the-numbers-mean)) and that the request methods are public.
