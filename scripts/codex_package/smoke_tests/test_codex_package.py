"""Exercise each assembled package in the order its shipped files appear.

ROOT_OF_EXTRACTED_PACKAGE
├── bin
│   ├── codex                             # CLI package only
│   ├── codex-app-server                  # app-server package only
│   └── codex-exec-server
├── codex-package.json
├── codex-path
│   └── rg
└── codex-resources
    └── bwrap                             # Linux only

Debug symbols for all shipped binaries arrive in a separate companion archive.
Each package contains one entrypoint, not both codex and codex-app-server.
"""

import json
import subprocess
import tomllib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import pytest

from fixtures import SmokePackage


@pytest.mark.parametrize(
    ("arguments", "expected"),
    [
        pytest.param(("--help",), "Usage:", id="help"),
        pytest.param(("features", "list"), "memories", id="features"),
        pytest.param(("completion", "bash"), "codex", id="completion"),
    ],
)
def test_cli_public_commands(
    package: SmokePackage,
    arguments: tuple[str, ...],
    expected: str,
) -> None:
    """Packaged CLI remains usable for discovery, features, and completions."""
    output = package.run(*arguments).stdout
    assert expected in output


def test_cli_version_matches_release_sources(package: SmokePackage) -> None:
    """Verify the compiled banner against the sources used to package the release."""
    root = next(
        parent
        for parent in Path(__file__).resolve().parents
        if (parent / "codex-rs/Cargo.toml").is_file()
    )
    manifest = tomllib.loads((root / "codex-rs/Cargo.toml").read_text())
    version = manifest["workspace"]["package"]["version"]
    assert package.run("--version").stdout == f"codex-cli {version}\n"


@pytest.mark.parametrize("variant", ["cli_root", "app_server_root"])
def test_packaged_executor_initializes_and_enforces_sandbox(
    package: SmokePackage, variant: str
) -> None:
    """Each package re-enters its executor helpers and enforces filesystem/process policy."""
    root = getattr(package, variant)
    executor = root / "bin" / "codex-exec-server"
    directory = package.directory / f"{variant}-executor-sandbox"
    directory.mkdir()
    workspace = directory / "workspace"
    workspace.mkdir()
    home = directory / "home"
    home.mkdir()
    blocked = directory / "blocked.txt"
    blocked.write_text("unchanged")
    environment = {**package.environment, "CODEX_HOME": str(home)}
    sandbox = {
        "permissions": {
            "type": "managed",
            "file_system": {
                "type": "restricted",
                "entries": [
                    {
                        "path": {"type": "special", "value": {"kind": "root"}},
                        "access": "read",
                    },
                    {
                        "path": {"type": "path", "path": workspace.as_uri()},
                        "access": "write",
                    },
                ],
            },
            "network": "restricted",
        },
        "cwd": workspace.as_uri(),
    }
    version = (
        subprocess.check_output(
            [str(executor), "--version"], env=package.environment, text=True, timeout=45
        )
        .strip()
        .split()[-1]
    )
    assert version == package.run("--version").stdout.strip().split()[-1]
    with (
        ThreadPoolExecutor(max_workers=1) as reader,
        subprocess.Popen(
            [str(executor), "--listen", "stdio"],
            cwd=package.directory,
            env=environment,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        ) as process,
    ):
        assert process.stdin is not None and process.stdout is not None
        try:
            process.stdin.write(
                json.dumps(
                    {
                        "id": 1,
                        "method": "initialize",
                        "params": {"clientName": "package-smoke"},
                    }
                )
                + "\n"
            )
            process.stdin.flush()
            initialized = json.loads(
                reader.submit(process.stdout.readline).result(timeout=45)
            )
            assert initialized["id"] == 1
            assert initialized["result"]["sessionId"]
            reported_environment = initialized["result"]["environmentInfo"]
            metadata = json.loads((root / "codex-package.json").read_text())
            assert reported_environment["executorVersion"] == metadata["version"]
            process.stdin.write('{"method":"initialized","params":{}}\n')

            # Restricted writes always launch the filesystem helper, even when allowed.
            for request_id, path in [(2, workspace / "allowed.txt"), (3, blocked)]:
                process.stdin.write(
                    json.dumps(
                        {
                            "id": request_id,
                            "method": "fs/writeFile",
                            "params": {
                                "path": path.as_uri(),
                                "dataBase64": "b2s=",
                                "sandbox": sandbox,
                            },
                        }
                    )
                    + "\n"
                )
                process.stdin.flush()
                # The first helper can provision sandbox state before replying.
                response = json.loads(
                    reader.submit(process.stdout.readline).result(timeout=45)
                )
                if request_id == 2:
                    assert response == {"id": 2, "result": {}}, response
                    assert path.read_text() == "ok"
                else:
                    assert response["id"] == 3 and "error" in response, response
                    assert blocked.read_text() == "unchanged"

            command = [
                "/bin/sh",
                "-c",
                "printf process >process.txt; printf forbidden >../blocked.txt; exit 0",
            ]
            process.stdin.write(
                json.dumps(
                    {
                        "id": 4,
                        "method": "process/start",
                        "params": {
                            "processId": "package-sandbox",
                            "argv": command,
                            "cwd": workspace.as_uri(),
                            "env": environment,
                            "sandbox": sandbox,
                            "tty": False,
                            "pipeStdin": False,
                            "arg0": None,
                        },
                    }
                )
                + "\n"
            )
            process.stdin.flush()
            started = False
            exit_code = None
            notifications = []
            while not started or exit_code is None:
                response = json.loads(
                    reader.submit(process.stdout.readline).result(timeout=45)
                )
                notifications.append(response)
                if response.get("id") == 4:
                    expected_type = (
                        "macosSeatbelt"
                        if "apple-darwin" in package.target
                        else "linuxSeccomp"
                    )
                    assert response["result"]["sandboxType"] == expected_type, response
                    started = True
                if response.get("method") == "process/exited":
                    exit_code = response["params"]["exitCode"]
            assert exit_code == 0, notifications
            assert (workspace / "process.txt").is_file(), notifications
            assert (workspace / "process.txt").read_text().strip() == "process"
            assert blocked.read_text() == "unchanged"
            process.stdin.close()
            assert process.wait(timeout=45) == 0
        finally:
            if process.poll() is None:
                process.kill()


def test_packaged_executor_matches_subcommand_startup(
    package: SmokePackage,
    tmp_path: Path,
) -> None:
    """Both binaries reject the same invalid executor arguments and config."""
    home = tmp_path / "home"
    temporary = tmp_path / "tmp"
    home.mkdir()
    temporary.mkdir()
    (home / "config.toml").write_text("unknown_executor_option = true\n")
    # Release builds require the helper directory to live outside their temp root.
    environment = {
        **package.environment,
        "CODEX_HOME": str(home),
        "TMPDIR": str(temporary),
        "TEMP": str(temporary),
        "TMP": str(temporary),
    }
    executor = package.cli_root / "bin" / "codex-exec-server"
    for arguments in [
        ["--strict-config", "--listen", "stdio"],
        ["-c", "unknown_executor_option=true", "--strict-config", "--listen", "stdio"],
        ["--listen", "http://127.0.0.1:0"],
        ["--concurrent-requests", "0"],
        [
            "--aws-sigv4",
            "--remote",
            "https://example.openai.com",
            "--environment-id",
            "env-1",
        ],
        ["--remote-transport", "direct"],
        ["forward", "--connect", "ws://127.0.0.1:0"],
        [
            "-c",
            "unknown_executor_option=true",
            "forward",
            "-c",
            "analytics.enabled=false",
            "--strict-config",
            "--connect",
            "ws://127.0.0.1:0",
            "--remote",
            "https://example.openai.com",
            "--environment-id",
            "env-1",
        ],
    ]:
        outputs = []
        for command in ([str(package.cli), "exec-server"], [str(executor)]):
            result = subprocess.run(
                [*command, *arguments],
                cwd=tmp_path,
                env=environment,
                capture_output=True,
                text=True,
                timeout=45,
            )
            assert result.returncode != 0, arguments
            outputs.append(
                (
                    result.returncode,
                    result.stdout,
                    result.stderr.replace("codex exec-server", "codex-exec-server"),
                )
            )
        assert outputs[0] == outputs[1], arguments
