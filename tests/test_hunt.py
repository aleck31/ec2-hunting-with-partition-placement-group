"""Checks that scripts/hunt.py ranks instances correctly and only calls real differences significant."""

import contextlib
import importlib.util
import io
import json
import random
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("hunt", Path(__file__).parent.parent / "scripts" / "hunt.py")
hunt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hunt)

CHRONY = "System time     : 0.000005000 seconds fast of NTP time\nRoot delay      : 0.000040000 seconds\nRoot dispersion : 0.000010000 seconds\n"


def write_run(root: Path, mode: str, offsets: dict, n: int = 400) -> Path:
    """One run dir; instance i's values are a shared per-slot load swing plus offsets[i] plus noise."""
    rng = random.Random(7)
    load = [rng.uniform(0, 300) for _ in range(n)]
    instances = {}
    for idx, (iid, offset) in enumerate(offsets.items()):
        d = root / iid
        d.mkdir(parents=True)
        (d / "meta.json").write_text(json.dumps({"instance_id": iid, "partition": str(idx + 1)}))
        (d / "chrony-after.txt").write_text(CHRONY)
        instances[str(idx)] = {"id": iid, "partition": idx + 1}
        if mode == "probe":
            rows = ["slot,target,warmup,status,offset_us,connect_us,syn_rtt_us"]
            rows += [f"{k},10.0.0.{k % 3},0,ok,0,0,{200 + load[k] + offset + rng.gauss(0, 5):.0f}" for k in range(n)]
        else:
            rows = ["conn,key,event_time_ms,recv_wall_us"]
            rows += [f"0,{k},0,{1_800_000_000_000_000 + k * 1000 + load[k] + offset + rng.gauss(0, 5):.0f}" for k in range(n)]
        (d / "samples.csv").write_text("\n".join(rows) + "\n")
    (root / "run.json").write_text(json.dumps({"mode": mode, "instances": instances}))
    return root


def analyse(run_dir: Path):
    run = hunt.load_run(run_dir)
    with contextlib.redirect_stdout(io.StringIO()):
        ranked = hunt.rank(run)
        verdict = hunt.verdict(run, ranked)
    return [r["inst"]["id"] for r in ranked], verdict


class RankingTest(unittest.TestCase):
    def test_tiny_lead_is_negligible(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, verdict = analyse(write_run(Path(tmp), "probe", {"i-a": 2, "i-b": 0, "i-c": 30}))
        self.assertIn("negligible", verdict)

    def test_faster_instance_wins_despite_shared_load_swings(self):
        with tempfile.TemporaryDirectory() as tmp:
            order, verdict = analyse(write_run(Path(tmp), "probe", {"i-a": 15, "i-b": 0, "i-c": 30}))
        self.assertEqual(order, ["i-b", "i-a", "i-c"])
        self.assertTrue(verdict.endswith("-> significant"), verdict)

    def test_equal_instances_are_not_significant(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, verdict = analyse(write_run(Path(tmp), "probe", {"i-a": 0, "i-b": 0, "i-c": 0}))
        self.assertTrue(verdict.endswith("NOT significant"), verdict)

    def test_stream_difference_within_clock_error_is_not_significant(self):
        # chrony bound is 5 + 40/2 + 10 = 35 µs per instance, so a 20 µs lead is within the combined 70 µs
        with tempfile.TemporaryDirectory() as tmp:
            order, verdict = analyse(write_run(Path(tmp), "stream", {"i-a": 20, "i-b": 0, "i-c": 40}))
        self.assertEqual(order[0], "i-b")
        self.assertTrue(verdict.endswith("NOT significant"), verdict)


class ProfileTest(unittest.TestCase):
    def setUp(self):
        self.profile = hunt.load_profile(str(Path(__file__).parent.parent / "targets" / "binance-usdm.toml"))
        self.eps = {ep["name"]: ep for ep in self.profile["endpoints"]}

    def test_endpoint_becomes_cex_ws_arguments(self):
        args = hunt.endpoint_args(self.eps["ws-api"])
        self.assertEqual(args[:2], ["--url", "wss://ws-fapi.binance.com/ws-fapi/v1"])
        self.assertIn("--id-field", args)
        self.assertEqual(hunt.endpoint_args(self.eps["market-stream"])[2:], ["--key", "u", "--event-time", "E"])

    def test_probe_targets_cover_every_endpoint_with_its_port(self):
        tcp = {"name": "fix", "protocol": "tcp", "host": "fix.example", "port": 9000}
        resolved = {"ws-fapi.binance.com": ["10.0.0.2"], "fapi.binance.com": ["10.0.0.1"], "fix.example": ["10.0.0.1"]}
        targets, labels = hunt.probe_targets(self.profile["endpoints"] + [tcp], resolved)
        self.assertEqual(targets, ["10.0.0.1:443", "10.0.0.1:9000", "10.0.0.2:443"])
        self.assertEqual(labels["10.0.0.1:9000"], "fix")
        self.assertEqual(labels["10.0.0.2:443"], "ws-api")


if __name__ == "__main__":
    unittest.main()
