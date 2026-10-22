import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import migrate_templates as migration


class TemplateMigrationTest(unittest.TestCase):
    def source(self, root):
        for name in migration.NAMES:
            (root / name).write_bytes(f"# {name}\r\n\r\n原文\r\n".encode())

    def result(self, code=0, value=None, error=""):
        return subprocess.CompletedProcess([], code, json.dumps(value), error)

    def test_plan_checks_every_target_and_never_writes_or_prints_content(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.source(root)
            def client(_client, args):
                self.assertEqual(args[0], "get")
                if args[-1].endswith("api-doc-template.md"):
                    return self.result(1, error="HTTP 404 Not Found")
                content = (root / args[-1].rsplit("/", 1)[-1]).read_bytes()
                return self.result(value={"content": content.decode(), "sha256": migration.digest(content)})
            with patch.object(migration, "invoke", side_effect=client) as invoke:
                report = migration.migrate(root, ["harness"], "shared")
            self.assertEqual(invoke.call_count, 3)
            self.assertEqual(report["status"], "planned")
            self.assertEqual([e["status"] for e in report["entries"]], ["unchanged", "missing", "unchanged"])
            self.assertNotIn("原文", json.dumps(report, ensure_ascii=False))

    def test_unsupported_service_and_different_targets_stop_before_any_write(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.source(root)
            inherited = (root / migration.NAMES[0]).read_bytes()
            for response in [self.result(1, error="HTTP 400 Bad Request: raw-sensitive-details"), self.result(value={"content": "# Different", "sha256": migration.digest(b"# Different")}), self.result(value={"namespace": "another-space", "content": inherited.decode(), "sha256": migration.digest(inherited)})]:
                with patch.object(migration, "invoke", return_value=response) as invoke:
                    report = migration.migrate(root, ["harness"], "shared", True)
                self.assertEqual(report["status"], "failed")
                self.assertEqual(report["saved"], [])
                self.assertTrue(all(call.args[1][0] == "get" for call in invoke.call_args_list))
                self.assertNotIn("raw-sensitive-details", json.dumps(report))

    def test_apply_preserves_bytes_and_exposes_partial_success_with_retry_key(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.source(root)
            writes = []
            def client(_client, args):
                if args[0] == "get":
                    return self.result(1, error="HTTP 404 Not Found")
                self.assertEqual(args[0], "--idempotency-key")
                path, staged = args[args.index("--path") + 1], Path(args[args.index("--file") + 1])
                content = staged.read_bytes()
                self.assertEqual(content, (root / path.rsplit("/", 1)[-1]).read_bytes())
                writes.append(path)
                if len(writes) == 2:
                    return self.result(1, error="connection reset")
                return self.result(value={"path": path, "sha256": migration.digest(content)})
            with patch.object(migration, "invoke", side_effect=client):
                report = migration.migrate(root, ["harness"], "shared", True)
            self.assertEqual(report["status"], "failed")
            self.assertEqual(len(report["saved"]), 1)
            self.assertEqual(len(writes), 2)
            self.assertEqual(report["pending_write"]["path"], writes[1])
            self.assertEqual(len(report["pending_write"]["idempotency_key"]), 32)

    def test_all_sources_are_checked_before_network(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.source(root)
            (root / migration.NAMES[-1]).write_bytes(b"invalid\x00template")
            with patch.object(migration, "invoke") as invoke:
                report = migration.migrate(root, ["harness"], "shared", True)
            self.assertEqual(report["status"], "failed")
            invoke.assert_not_called()


if __name__ == "__main__":
    unittest.main()
