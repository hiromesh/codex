#!/usr/bin/env python3

from pathlib import Path
import io
import tarfile
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.archive import write_archive, resolve_zstd_command
from codex_package.layout import build_package_dir
from codex_package.layout import validate_package_dir
from codex_package.targets import PACKAGE_VARIANTS
from codex_package.targets import PackageInputs
from codex_package.targets import TARGET_SPECS
from codex_package.targets import default_target


class PackageLayoutTest(unittest.TestCase):
    def test_strip_policy_preserves_inputs_and_runs_packaged_executables(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / "input-executable"
            # System executables may already be stripped. Compile with debug
            # information so a no-op strip cannot pass this regression test.
            subprocess.run(
                ["cc", "-g", "-O0", "-x", "c", "-", "-o", str(source)],
                input=r"""#include <stdio.h>
int main(void) {
    fputs("package-ok", stdout);
    return 0;
}
""",
                text=True,
                check=True,
                capture_output=True,
            )
            original = source.read_bytes()
            target = default_target()
            for mode in ("auto", "all", "none"):
                with self.subTest(mode=mode):
                    package = root / mode
                    command = [
                        sys.executable,
                        str(
                            Path(__file__).resolve().parents[1]
                            / "build_codex_package.py"
                        ),
                        "--target",
                        target,
                        "--cargo-profile",
                        "release",
                        "--package-dir",
                        str(package),
                        "--strip",
                        mode,
                    ]
                    flags = [
                        "--entrypoint-bin",
                        "--exec-server-bin",
                        "--rg-bin",
                    ]
                    if "linux" in target:
                        flags.append("--bwrap-bin")
                    for flag in flags:
                        command.extend((flag, str(source)))
                    if mode != "all":
                        # Preserved prebuilt inputs must not even need a strip tool.
                        command.extend(("--strip-tool", str(root / "absent-strip")))
                    subprocess.run(command, check=True, capture_output=True)
                    for name in ("codex", "codex-exec-server"):
                        executable = package / "bin" / name
                        self.assertEqual(
                            subprocess.check_output([str(executable)]),
                            b"package-ok",
                        )
                        if mode != "all":
                            self.assertEqual(executable.read_bytes(), original)
                        else:
                            self.assertLess(executable.stat().st_size, len(original))
                    self.assertEqual(source.read_bytes(), original)
                    self.assertEqual((package / "codex-path/rg").read_bytes(), original)
                    if "linux" in target:
                        self.assertEqual(
                            (package / "codex-resources/bwrap").read_bytes(), original
                        )

    def test_macos_package_preserves_prebuilt_resource_binaries(self) -> None:
        for variant_name in ("codex", "codex-app-server"):
            for target in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
                with self.subTest(variant=variant_name, target=target):
                    with tempfile.TemporaryDirectory() as temp_dir:
                        root = Path(temp_dir)
                        package_dir = root / "package"
                        package_dir.mkdir()
                        rg_bin = touch_executable(root / "signed-rg")
                        rg_bin.write_bytes(b"signed ripgrep binary")
                        variant = PACKAGE_VARIANTS[variant_name]
                        spec = TARGET_SPECS[target]
                        inputs = PackageInputs(
                            entrypoint_bin=touch_executable(
                                root / variant.executable_stem
                            ),
                            exec_server_bin=rg_bin,
                            rg_bin=rg_bin,
                            bwrap_bin=None,
                        )

                        build_package_dir(package_dir, "1.2.3", variant, spec, inputs)
                        validate_package_dir(package_dir, variant, spec)
                        for suffix in (".tar.gz", ".tar.zst"):
                            archive_path = root / f"package{suffix}"
                            write_archive(package_dir, archive_path, force=False)
                            data = (
                                subprocess.check_output(
                                    [
                                        *resolve_zstd_command(),
                                        "-d",
                                        "--stdout",
                                        str(archive_path),
                                    ]
                                )
                                if suffix == ".tar.zst"
                                else archive_path.read_bytes()
                            )
                            with tarfile.open(
                                fileobj=io.BytesIO(data), mode="r:*"
                            ) as archive:
                                member = archive.getmember("bin/codex-exec-server")
                                self.assertTrue(member.mode & 0o111)
                                self.assertEqual(
                                    archive.extractfile(member).read(),
                                    rg_bin.read_bytes(),
                                )

                        self.assertEqual(
                            {
                                "rg": (package_dir / "codex-path" / "rg").read_bytes(),
                                "executor": (
                                    package_dir / "bin/codex-exec-server"
                                ).read_bytes(),
                            },
                            {
                                "rg": b"signed ripgrep binary",
                                "executor": b"signed ripgrep binary",
                            },
                        )

    def test_app_server_package_includes_exec_server_beside_entrypoint(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            package_dir = root / "package"
            package_dir.mkdir()
            inputs = PackageInputs(
                entrypoint_bin=touch_executable(root / "codex-app-server"),
                exec_server_bin=touch_executable(root / "codex-exec-server"),
                rg_bin=touch_executable(root / "rg"),
                bwrap_bin=touch_executable(root / "bwrap"),
            )

            build_package_dir(
                package_dir,
                "1.2.3",
                PACKAGE_VARIANTS["codex-app-server"],
                TARGET_SPECS["x86_64-unknown-linux-musl"],
                inputs,
            )
            validate_package_dir(
                package_dir,
                PACKAGE_VARIANTS["codex-app-server"],
                TARGET_SPECS["x86_64-unknown-linux-musl"],
            )

            self.assertTrue((package_dir / "bin" / "codex-exec-server").is_file())


def touch_executable(path: Path) -> Path:
    path.touch(mode=0o755)
    return path


if __name__ == "__main__":
    unittest.main()
