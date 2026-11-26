#!/usr/bin/env python3
"""Offline tests for scripts/bench/bench.py (docs/88 C.1). No CLI, no model:
fixtures only. Run: python3 scripts/bench/test_bench.py

Fixtures (scripts/bench/fixtures/):
  * rollout-bare-01, rollout-baaz-fix-01: Stage 17's task-01 Codex rollouts
    (bare 0.144.6; Baaz with the model-at-start fix, 0.153.2), trimmed to
    their session_meta and token_count events;
  * results-s17-codex.jsonl: the 10 bare (Stage 17 full run) and 10 Baaz
    (model-at-start fix run) Codex rows, re-derived from every rollout with
    `with_rollout`. docs/88 §1 quotes their sums: 744,667 and 802,517.
"""
import json, os, sys, tempfile, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import bench  # noqa: E402

FIX = os.path.join(HERE, "fixtures")
RESULTS = os.path.join(FIX, "results-s17-codex.jsonl")


def rows():
    return [json.loads(l) for l in open(RESULTS)]


class Totals(unittest.TestCase):
    def test_fixture_reproduces_the_plan_totals(self):
        rs = rows()
        self.assertEqual(sum(r["tokens_in"] for r in rs if r["mode"] == "bare"), 744_667)
        self.assertEqual(sum(r["tokens_in"] for r in rs if r["mode"] == "baaz"), 802_517)
        self.assertEqual(sum(len(r["requests"]) for r in rs if r["mode"] == "bare"), 59)
        self.assertEqual(sum(len(r["requests"]) for r in rs if r["mode"] == "baaz"), 60)
        self.assertEqual(sum(r["uncached"] for r in rs if r["mode"] == "bare"), 73_691)
        self.assertEqual(sum(r["uncached"] for r in rs if r["mode"] == "baaz"), 57_173)

    def test_report_prints_per_finished_and_paired_numbers(self):
        text = bench.report(RESULTS)
        self.assertIn("| codex | bare | s17 | 10 | 10 | 74,466 | 7,369 |", text)
        self.assertIn("| codex | baaz | s17 | 10 | 10 | 80,251 | 5,717 |", text)
        self.assertIn("## Paired", text)
        # Mean paired difference = (802,517 - 744,667) / 10.
        self.assertIn("+5,785 ±", text)

    def test_rollout_parse_matches_the_results_row(self):
        for name, mode, version in (("bare-01", "bare", "0.144.6"), ("baaz-fix-01", "baaz", "0.153.2")):
            m = bench.rollout_metrics(os.path.join(FIX, f"rollout-{name}"))
            row = [r for r in rows() if r["task"] == "01-fix-add" and r["mode"] == mode][0]
            self.assertEqual(m["cli_versions"], [version])
            self.assertEqual(m["requests"], row["requests"])
            self.assertEqual(m["model_switch"], 0)
        bare = bench.with_rollout({"tokens_in": 0, "tokens_out": 0, "cached": 0}, os.path.join(FIX, "rollout-bare-01"))
        self.assertEqual(bare["first_in"], bare["requests"][0][0])
        self.assertEqual(bare["uncached"], bare["tokens_in"] - bare["cached"])


class Versions(unittest.TestCase):
    def test_a_version_mismatch_is_refused(self):
        # The Stage 17 rows crossed versions (0.144.6 bare, 0.153.2 Baaz).
        self.assertEqual(len(bench.version_mismatches(rows())), 10)
        checks = dict((name, ok) for name, ok, _ in bench.gate(rows(), "codex", "s17"))
        self.assertFalse(checks["0 version mismatches"])
        self.assertIn("arm `s17`: FAIL", bench.report(RESULTS))

    def test_one_version_passes_the_version_check(self):
        rs = [dict(r, cli_versions=["0.153.2"]) for r in rows()]
        self.assertEqual(bench.version_mismatches(rs), [])
        # A run whose rollout names no version is a mismatch too.
        rs[0]["cli_versions"] = []
        self.assertEqual(len(bench.version_mismatches(rs)), 1)


class Gate(unittest.TestCase):
    def test_the_s17_fix_rows_fail_the_token_checks(self):
        checks = {name: ok for name, ok, _ in bench.gate(rows(), "codex", "s17")}
        self.assertFalse(checks["input tokens per finished task: Baaz <= bare"])
        self.assertTrue(checks["uncached tokens: Baaz <= bare"])
        self.assertTrue(checks["finished: Baaz >= bare"])
        self.assertFalse(checks["first request: Baaz < bare on every run"])
        self.assertTrue(checks["0 <model_switch> blocks"])

    def test_a_cheaper_baaz_passes(self):
        rs = []
        for r in rows():
            r = dict(r, cli_versions=["0.153.2"])
            if r["mode"] == "baaz":
                bare = [b for b in rows() if b["task"] == r["task"] and b["mode"] == "bare"][0]
                r.update(tokens_in=bare["tokens_in"] - 100, cached=bare["cached"], first_in=bare["first_in"] - 1)
                r["uncached"] = r["tokens_in"] - r["cached"]
            rs.append(r)
        self.assertTrue(all(ok for _, ok, _ in bench.gate(rs, "codex", "s17")))

    def test_a_model_switch_block_fails(self):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "sessions"))
            with open(os.path.join(d, "sessions", "r.jsonl"), "w") as f:
                f.write(json.dumps({"type": "response_item", "payload": {"type": "message", "role": "developer",
                                    "content": [{"type": "input_text", "text": "<model_switch>\nnew model</model_switch>"}]}}) + "\n")
            self.assertEqual(bench.rollout_metrics(d)["model_switch"], 1)


class Homes(unittest.TestCase):
    def owner(self, d):
        owner = os.path.join(d, "owner")
        os.makedirs(os.path.join(owner, "skills", "mine"))
        os.makedirs(os.path.join(owner, "plugins", "cache", "p"))
        open(os.path.join(owner, "auth.json"), "w").write("{}")
        open(os.path.join(owner, "AGENTS.md"), "w").write("be terse\n")
        open(os.path.join(owner, "config.toml"), "w").write(
            'model = "x"\nnotify = [\n  "say",\n  "hi",\n]\n[plugins."p"]\nenabled = true\nnotify = "kept: inside a table"\n')
        return owner

    def test_clean_bare_home_holds_only_the_auth_link(self):
        with tempfile.TemporaryDirectory() as d:
            owner, home = self.owner(d), os.path.join(d, "bare")
            bench.seed_codex_home(home, "clean", baaz=False, owner=owner)
            self.assertEqual(os.listdir(home), ["auth.json"])
            self.assertTrue(os.path.islink(os.path.join(home, "auth.json")))

    def test_clean_baaz_home_blocks_every_owner_entry(self):
        with tempfile.TemporaryDirectory() as d:
            owner, home = self.owner(d), os.path.join(d, "state", "codex-home")
            bench.seed_codex_home(home, "clean", baaz=True, owner=owner)
            self.assertEqual(sorted(os.listdir(home)), sorted(bench.CODEX_LINKED))
            for name in bench.CODEX_LINKED:
                path = os.path.join(home, name)
                self.assertTrue(os.path.islink(path))
                # Present to Baaz's upkeep (lstat), absent to Codex (stat).
                self.assertEqual(os.path.exists(path), name == "auth.json")

    def test_installed_bare_home_copies_the_owner_set_minus_notify(self):
        with tempfile.TemporaryDirectory() as d:
            owner, home = self.owner(d), os.path.join(d, "bare")
            bench.seed_codex_home(home, "installed", baaz=False, owner=owner)
            self.assertTrue(os.path.isdir(os.path.join(home, "skills", "mine")))
            self.assertTrue(os.path.isdir(os.path.join(home, "plugins", "cache", "p")))
            self.assertFalse(os.path.islink(os.path.join(home, "skills")))
            config = open(os.path.join(home, "config.toml")).read()
            self.assertNotIn('"say"', config)
            self.assertIn('model = "x"', config)
            self.assertIn("kept: inside a table", config)

    def test_installed_baaz_home_is_left_to_production(self):
        with tempfile.TemporaryDirectory() as d:
            owner, home = self.owner(d), os.path.join(d, "state", "codex-home")
            bench.seed_codex_home(home, "installed", baaz=True, owner=owner)
            self.assertEqual(os.listdir(home), ["auth.json"])


class Keys(unittest.TestCase):
    def test_old_rows_and_repeats_have_distinct_keys(self):
        old = {"task": "01", "provider": "codex", "mode": "bare"}
        self.assertEqual(bench.run_key(old), ("01", "codex", "bare", "base", "-", 1))
        self.assertNotEqual(bench.run_key(dict(old, arm="clean", repeat=2)), bench.run_key(dict(old, arm="clean", repeat=1)))


if __name__ == "__main__":
    unittest.main()
