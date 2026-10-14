import datetime as dt
import importlib.util
import json
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("sample", Path(__file__).with_name("sample.py"))
sample = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sample)


class MonitorTests(unittest.TestCase):
    def test_no_matching_journal_logs_are_healthy_but_command_errors_remain_errors(self):
        with patch.object(sample.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", "")):
            self.assertEqual(sample.command(["journalctl"], allow_no_matches=True), "")
        with patch.object(sample.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", "permission denied")):
            with self.assertRaises(subprocess.CalledProcessError):
                sample.command(["journalctl"], allow_no_matches=True)

    def test_proc_cpu_uses_real_tick_frequency_and_handles_process_name(self):
        tail = ["0"] * 22
        tail[0], tail[11], tail[12], tail[19] = "S", "250", "50", "2000"
        actual = sample.process_stat("123 (a (weird) name) " + " ".join(tail), 250)
        self.assertEqual(actual, {"state": "S", "cpu_seconds": 1.2, "start_ticks": 2000})

    def test_interval_cpu_rejects_restart_boot_change_and_missing_history(self):
        current = {"pid": 7, "start_ticks": 10, "cpu_seconds": 1.6}
        previous = {"boot_id": "a", "process": {**current, "cpu_seconds": 1.0}}
        self.assertEqual(sample.cpu_percent(current, previous, "a", 60), 1.0)
        self.assertIsNone(sample.cpu_percent(current, previous, "b", 60))
        self.assertIsNone(sample.cpu_percent({**current, "pid": 8}, previous, "a", 60))
        self.assertIsNone(sample.cpu_percent({**current, "start_ticks": 11}, previous, "a", 60))
        self.assertIsNone(sample.cpu_percent(current, None, "a", 60))

    def test_resource_alerts_require_persistence_and_clear_on_recovery(self):
        item = {"systemd": {"ActiveState": "active", "LimitNOFILE": 100},
                "process": {"rss_bytes": 11 * 1024 * 1024, "threads": 3, "fds": 10,
                            "cpu_percent_one_core": 0}, "boot_id": "a"}
        previous = None
        for _ in range(4):
            streaks, alerts = sample.evaluate(item, previous, {"rss_mib": 10})
            self.assertEqual(alerts, [])
            previous = {**item, "streaks": streaks}
        streaks, alerts = sample.evaluate(item, previous, {"rss_mib": 10})
        self.assertEqual(alerts, ["rss_budget"])
        recovered = {**item, "process": {**item["process"], "rss_bytes": 8 * 1024 * 1024}}
        self.assertEqual(sample.evaluate(recovered, {**item, "streaks": streaks}, {"rss_mib": 10}), ({}, []))

    def test_down_and_restart_are_immediate_signals(self):
        item = {"systemd": {"ActiveState": "failed"}, "boot_id": "a"}
        self.assertIn("service_unavailable", sample.evaluate(item, None, {"rss_mib": 10})[1])
        old = {"systemd": {"NRestarts": 0}, "process": {"pid": 1, "start_ticks": 10}, "boot_id": "a"}
        current = {"systemd": {"ActiveState": "active", "NRestarts": 1},
                   "process": {"pid": 2, "start_ticks": 20}, "boot_id": "a"}
        self.assertEqual(set(sample.evaluate(current, old, {"rss_mib": 10})[1]),
                         {"process_restarted", "automatic_restart"})

    def test_retention_and_rotation_keep_recent_samples_with_fixed_disk_bound(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            (directory / "samples-2026-10-01.jsonl").write_text("old")
            current = directory / "samples-2026-10-13.jsonl"
            current.write_text("x" * 495)
            with patch.object(sample, "MAX_FILE_BYTES", 500):
                sample.append_sample(directory, {"one": 1}, dt.date(2026, 10, 13))
            self.assertFalse((directory / "samples-2026-10-01.jsonl").exists())
            self.assertEqual(current.with_suffix(".jsonl.1").stat().st_size, 495)
            self.assertEqual(json.loads(current.read_text()), {"one": 1})

    def test_journal_summary_omits_message_content(self):
        events = [{"PRIORITY": "3", "MESSAGE": "secret"}, {"PRIORITY": "4"}]
        with patch.object(sample, "command", side_effect=["\n".join(map(json.dumps, events)), ""]) as cmd:
            result = sample.journal_sample("rustfrp-test-client.service", 1, 2)
        self.assertEqual(result, {"warning_or_higher": 2, "error_or_higher": 1, "application_warning_or_error": 0, "application_errors": 0, "capped": False})
        self.assertNotIn("secret", json.dumps(result))
        self.assertIn("--output-fields=PRIORITY,__REALTIME_TIMESTAMP", cmd.call_args_list[0][0][0])

    def test_application_stderr_errors_are_seen_even_with_info_journal_priority(self):
        events = [{"PRIORITY": "6", "MESSAGE": "[E] secret error", "__CURSOR": "1"}]
        with patch.object(sample, "command", side_effect=["", json.dumps(events[0])]):
            result = sample.journal_sample("rustfrp-test-client.service", 1, 2)
        self.assertEqual(result["application_errors"], 1)
        self.assertEqual(result["application_warning_or_error"], 1)
        self.assertNotIn("secret", json.dumps(result))


if __name__ == "__main__":
    unittest.main()
