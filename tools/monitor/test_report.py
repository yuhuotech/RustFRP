import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("report", Path(__file__).with_name("report.py"))
report = importlib.util.module_from_spec(spec)
spec.loader.exec_module(report)


class ReportTests(unittest.TestCase):
    def test_report_handles_rotation_time_order_and_real_cpu_windows(self):
        def row(ts, cpu, rss):
            return {"timestamp": ts, "interval_seconds": 60,
                    "services": {"agent": {"systemd": {"ActiveState": "active"},
                        "process": {"pid": 1, "rss_bytes": rss * 1048576, "threads": 3,
                                    "fds": 10, "cpu_percent_one_core": cpu}}}}
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp)
            (p / "samples-2026-10-13.jsonl").write_text(json.dumps(row(950, 2, 8)) + "\npartial")
            (p / "samples-2026-10-13.jsonl.1").write_text(json.dumps(row(890, None, 7)) + "\n")
            with patch.object(report.time, "time", return_value=1000):
                result = report.summarize(p)
        self.assertEqual(result["samples"], 2)
        self.assertEqual(result["observed_seconds"], 60)
        self.assertEqual(result["invalid_lines"], 1)
        agent = result["services"]["agent"]
        self.assertEqual((agent["first_rss_mib"], agent["last_rss_mib"]), (7, 8))
        self.assertEqual(agent["cpu_mean_percent_one_core"], 2)
        self.assertEqual(agent["cpu_window_seconds"], 60)
        self.assertEqual(agent["rss_max_mib"], 8)


if __name__ == "__main__":
    unittest.main()
