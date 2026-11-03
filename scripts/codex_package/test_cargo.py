#!/usr/bin/env python3

from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.cargo import source_binaries_for_target
from codex_package.targets import PACKAGE_VARIANTS
from codex_package.targets import TARGET_SPECS


class SourceBinariesForTargetTest(unittest.TestCase):
    def test_macos_package_with_prebuilt_entrypoint_builds_nothing(self) -> None:
        self.assertEqual(
            source_binaries_for_target(
                TARGET_SPECS["aarch64-apple-darwin"],
                PACKAGE_VARIANTS["codex"],
                build_entrypoint=False,
                build_exec_server=False,
                build_bwrap=False,
            ),
            [],
        )

    def test_linux_package_with_prebuilt_entrypoint_and_bwrap_builds_nothing(
        self,
    ) -> None:
        self.assertEqual(
            source_binaries_for_target(
                TARGET_SPECS["x86_64-unknown-linux-musl"],
                PACKAGE_VARIANTS["codex"],
                build_entrypoint=False,
                build_exec_server=False,
                build_bwrap=False,
            ),
            [],
        )

    def test_missing_exec_server_is_built_for_app_server(self) -> None:
        self.assertEqual(
            source_binaries_for_target(
                TARGET_SPECS["aarch64-apple-darwin"],
                PACKAGE_VARIANTS["codex-app-server"],
                build_entrypoint=False,
                build_exec_server=True,
                build_bwrap=False,
            ),
            ["codex-exec-server"],
        )


if __name__ == "__main__":
    unittest.main()
