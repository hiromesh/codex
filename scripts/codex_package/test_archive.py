#!/usr/bin/env python3

from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.archive import resolve_zstd_command


class ResolveZstdCommandTest(unittest.TestCase):
    def test_prefers_zstd_from_path(self) -> None:
        def which(name: str) -> str | None:
            return {"zstd": "/usr/bin/zstd"}.get(name)

        self.assertEqual(resolve_zstd_command(which=which), ["/usr/bin/zstd"])

    def test_errors_when_zstd_is_not_available(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "zstd is required"):
            resolve_zstd_command(which=lambda _name: None)


if __name__ == "__main__":
    unittest.main()
