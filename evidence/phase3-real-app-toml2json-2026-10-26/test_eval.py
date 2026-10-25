import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("evaluation", Path(__file__).with_name("run-eval.py"))
evaluation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evaluation)


class EvaluationChecks(unittest.TestCase):
    def setUp(self):
        self.spans = [
            {"name": "app_root", "scope": "toml2json", "span_id": "1" * 16,
             "trace_id": "a" * 32, "parent_span_id": "0" * 16},
            {"name": "outer", "scope": "clap_lex", "span_id": "2" * 16,
             "trace_id": "a" * 32, "parent_span_id": "1" * 16},
            {"name": "inner", "scope": "clap_lex", "span_id": "3" * 16,
             "trace_id": "a" * 32, "parent_span_id": "2" * 16},
        ]

    def test_trace_complete_chain_is_accepted(self):
        self.assertEqual(set(evaluation.verify_trace(self.spans, 2)), {"1" * 16, "2" * 16, "3" * 16})

    def test_trace_missing_parent_is_rejected(self):
        self.spans[2]["parent_span_id"] = "4" * 16
        with self.assertRaisesRegex(ValueError, "Missing parent"):
            evaluation.verify_trace(self.spans, 2)

    def test_trace_cross_trace_child_is_rejected(self):
        self.spans[2]["trace_id"] = "b" * 32
        with self.assertRaisesRegex(ValueError, "escaped"):
            evaluation.verify_trace(self.spans, 2)

    def test_trace_cycle_is_rejected(self):
        self.spans[1]["parent_span_id"] = "3" * 16
        with self.assertRaisesRegex(ValueError, "Cycle"):
            evaluation.verify_trace(self.spans, 2)

    def test_trace_duplicate_span_id_is_rejected(self):
        self.spans[2]["span_id"] = self.spans[1]["span_id"]
        with self.assertRaisesRegex(ValueError, "duplicate"):
            evaluation.verify_trace(self.spans, 2)

    def test_runtime_exact_warmup_and_measured_counts_are_accepted(self):
        output = "WARMUP_VERIFIED iters=1 spans=18 output_bytes=140\nBENCH_RESULT iters=10 spans=180 output_bytes=140 ns_per_iter=123.4"
        self.assertEqual(evaluation.verify_runtime(output, 10, 18, 140), 123.4)

    def test_runtime_missing_measured_span_is_rejected(self):
        output = "WARMUP_VERIFIED iters=1 spans=18 output_bytes=140\nBENCH_RESULT iters=10 spans=179 output_bytes=140 ns_per_iter=123.4"
        with self.assertRaisesRegex(ValueError, "BENCH_RESULT"):
            evaluation.verify_runtime(output, 10, 18, 140)

    def test_runtime_missing_warmup_span_is_rejected(self):
        output = "WARMUP_VERIFIED iters=1 spans=17 output_bytes=140\nBENCH_RESULT iters=10 spans=180 output_bytes=140 ns_per_iter=123.4"
        with self.assertRaisesRegex(ValueError, "WARMUP_VERIFIED"):
            evaluation.verify_runtime(output, 10, 18, 140)

    def test_runtime_baseline_unexpected_span_is_rejected(self):
        output = "WARMUP_VERIFIED iters=1 spans=0 output_bytes=140\nBENCH_RESULT iters=10 spans=1 output_bytes=140 ns_per_iter=123.4"
        with self.assertRaisesRegex(ValueError, "BENCH_RESULT"):
            evaluation.verify_runtime(output, 10, 0, 140)


if __name__ == "__main__":
    unittest.main()
