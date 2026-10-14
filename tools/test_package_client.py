import tempfile
import unittest
import locale
import os
import sys
from pathlib import Path
from unittest.mock import patch

import package_client as packaging


class ClientOutputEncoding(unittest.TestCase):
    def test_utf8_rules_survive_a_windows_legacy_code_page(self):
        text = '{"instructions":"配置设备空间"}'
        raw = text.encode("utf-8")
        with self.assertRaises(UnicodeDecodeError):
            raw.decode("cp1252")
        with patch.object(locale, "getencoding", return_value="cp1252"):
            result = packaging.run_client(
                [sys.executable, "-c", f"import sys; sys.stdout.buffer.write({raw!r})"],
                os.environ.copy(),
            )
        self.assertEqual(result, text)


class AssetValidation(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.output = Path(self.temp.name)
        self.names = sorted(
            packaging.archive_name(packaging.version(), name)
            for name in packaging.TARGETS
        )
        for name in self.names:
            archive = self.output / name
            archive.write_bytes(name.encode())
            archive.with_name(name + ".sha256").write_text(
                f"{packaging.digest(archive)}  {name}\n", encoding="utf-8"
            )

    def test_complete_set_has_one_matching_checksum_per_platform(self):
        packaging.finalize(self.output)
        expected = "".join(
            f"{packaging.digest(self.output / name)}  {name}\n"
            for name in self.names
        )
        self.assertEqual((self.output / "SHA256SUMS").read_text(), expected)
        # Retrying collection is safe; it does not include SHA256SUMS in itself.
        packaging.finalize(self.output)
        self.assertEqual((self.output / "SHA256SUMS").read_text(), expected)

    def test_missing_platform_prevents_publication(self):
        (self.output / self.names[0]).unlink()
        with self.assertRaisesRegex(ValueError, "incomplete"):
            packaging.finalize(self.output)
        self.assertFalse((self.output / "SHA256SUMS").exists())

    def test_unexpected_file_prevents_publication(self):
        (self.output / "private-config.json").write_text("{}")
        with self.assertRaisesRegex(ValueError, "unexpected"):
            packaging.finalize(self.output)

    def test_corrupt_transfer_prevents_publication(self):
        (self.output / self.names[0]).write_bytes(b"modified")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            packaging.finalize(self.output)
        self.assertFalse((self.output / "SHA256SUMS").exists())

    def test_target_label_mismatch_prevents_packaging(self):
        with self.assertRaisesRegex(ValueError, "target/platform mismatch"):
            packaging.package("x86_64-unknown-linux-gnu", "macos-x64", self.output)


if __name__ == "__main__":
    unittest.main()
