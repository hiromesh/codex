#!/usr/bin/env python3

from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.cargo import build_source_binaries
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
                build_code_mode_host=False,
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
                build_code_mode_host=False,
                build_exec_server=False,
                build_bwrap=False,
            ),
            [],
        )

    def test_windows_package_with_prebuilt_binaries_builds_nothing(self) -> None:
        self.assertEqual(
            source_binaries_for_target(
                TARGET_SPECS["x86_64-pc-windows-msvc"],
                PACKAGE_VARIANTS["codex"],
                build_entrypoint=False,
                build_code_mode_host=False,
                build_exec_server=False,
                build_bwrap=False,
            ),
            [],
        )

    def test_missing_code_mode_host_is_built_for_app_server(self) -> None:
        self.assertEqual(
            source_binaries_for_target(
                TARGET_SPECS["aarch64-apple-darwin"],
                PACKAGE_VARIANTS["codex-app-server"],
                build_entrypoint=False,
                build_code_mode_host=True,
                build_exec_server=True,
                build_bwrap=False,
            ),
            ["codex-code-mode-host", "codex-exec-server"],
        )

    def test_build_uses_prebuilt_windows_binaries_without_running_cargo(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            entrypoint = touch_file(root / "codex.exe")
            code_mode_host = touch_file(root / "codex-code-mode-host.exe")

            outputs = build_source_binaries(
                TARGET_SPECS["x86_64-pc-windows-msvc"],
                PACKAGE_VARIANTS["codex"],
                cargo=str(root / "cargo-that-should-not-run"),
                profile="release",
                entrypoint_bin=entrypoint,
                code_mode_host_bin=code_mode_host,
                exec_server_bin=code_mode_host,
                bwrap_bin=None,
            )

        self.assertEqual(outputs.entrypoint_bin, entrypoint)
        self.assertEqual(outputs.code_mode_host_bin, code_mode_host)


def touch_file(path: Path) -> Path:
    path.write_text("", encoding="utf-8")
    return path.resolve()


if __name__ == "__main__":
    unittest.main()
