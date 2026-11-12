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

def write_run(root: Path, mode: str, offsets: dict, n: int = 400, noise: float = 5) -> Path:
    """One run dir; instance i's values are a shared per-slot load swing plus offsets[i] plus noise."""
    rng = random.Random(7)
    load = [rng.uniform(0, 300) for _ in range(n)]
    instances = {}
    for idx, (iid, offset) in enumerate(offsets.items()):
        d = root / iid
        d.mkdir(parents=True)
        (d / "meta.json").write_text(json.dumps({"instance_id": iid, "partition": str(idx + 1)}))
        instances[str(idx)] = {"id": iid, "partition": idx + 1}
        rows = ["slot,target,warmup,status,offset_us,connect_us,syn_rtt_us"]
        rows += [f"{k},10.0.0.{k % 3},0,ok,0,0,{200 + load[k] + offset + rng.gauss(0, noise):.0f}" for k in range(n)]
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
    def test_faster_instance_wins_despite_shared_load_swings(self):
        with tempfile.TemporaryDirectory() as tmp:
            order, verdict = analyse(write_run(Path(tmp), "probe", {"i-a": 15, "i-b": 0, "i-c": 30}))
        self.assertEqual(order, ["i-b", "i-a", "i-c"])
        self.assertTrue(verdict.endswith("-> significant"), verdict)

    def test_equal_instances_are_not_significant(self):
        with tempfile.TemporaryDirectory() as tmp:
            _, verdict = analyse(write_run(Path(tmp), "probe", {"i-a": 0, "i-b": 0, "i-c": 0}))
        self.assertIn("NOT significant (no lead", verdict)



class RoleTest(unittest.TestCase):
    def test_roles_rank_separately_and_weights_pick_the_host(self):
        # i-a is 20 µs closer to the order-entry IP, i-b 20 µs closer to the market-data IP
        rng = random.Random(5)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            instances = {}
            for idx, (iid, order_off, md_off) in enumerate([("i-a", 0, 20), ("i-b", 20, 0), ("i-c", 30, 30)]):
                d = root / iid
                d.mkdir()
                (d / "meta.json").write_text(json.dumps({"instance_id": iid, "partition": str(idx + 1)}))
                rows = ["slot,target,warmup,status,offset_us,connect_us,syn_rtt_us"]
                for k in range(600):
                    off, target = (order_off, "10.0.0.1:443") if k % 2 else (md_off, "10.0.0.2:443")
                    rows.append(f"{k},{target},0,ok,0,0,{250 + off + rng.gauss(0, 5):.0f}")
                (d / "samples.csv").write_text("\n".join(rows) + "\n")
                instances[str(idx)] = {"id": iid, "partition": idx + 1}
            (root / "run.json").write_text(json.dumps({"mode": "probe", "instances": instances,
                                                       "target_roles": {"10.0.0.1:443": "order", "10.0.0.2:443": "md"}}))
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                hunt.report_roles(hunt.load_run(root), {"order": 1, "md": 0}, 5.0)
        text = out.getvalue()
        self.assertIn("order  weight 1.00: 0(", text)
        self.assertIn("md     weight 0.00: 1(", text)
        self.assertIn("rank correlation md vs order: +0.50", text)
        self.assertIn("keep_instances = [0]", text)



class ProfileTest(unittest.TestCase):
    def setUp(self):
        self.profile = hunt.load_profile(str(Path(__file__).parent.parent / "targets" / "binance-usdm.toml"))

    def test_probe_targets_cover_every_endpoint_with_its_port(self):
        tcp = {"name": "fix", "protocol": "tcp", "host": "fix.example", "port": 9000}
        resolved = {"ws-fapi.binance.com": ["10.0.0.2"], "fapi.binance.com": ["10.0.0.1"], "fix.example": ["10.0.0.1"]}
        targets, labels, roles = hunt.probe_targets(self.profile["endpoints"] + [tcp], resolved)
        self.assertEqual(targets, ["10.0.0.1:443", "10.0.0.1:9000", "10.0.0.2:443"])
        self.assertEqual(labels["10.0.0.1:9000"], "fix")
        self.assertEqual(labels["10.0.0.2:443"], "ws-api")
        self.assertEqual(roles, {"10.0.0.1:443": "order", "10.0.0.1:9000": "order", "10.0.0.2:443": "order"})


class DrawsTest(unittest.TestCase):
    def test_instances_in_one_partition_count_as_one_draw(self):
        run = {"insts": [{"partition": p} for p in ("1", "1", "2", "-")]}
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            hunt.report_draws(run)
        self.assertIn("placement draws: 3 (2 partitions + 1 outside the group", out.getvalue())
        self.assertIn("(29 draws for 95%)", out.getvalue())


if __name__ == "__main__":
    unittest.main()
