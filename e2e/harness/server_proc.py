"""Binary discovery, process lifecycle, and health polling for nsac-server."""

from __future__ import annotations

from pathlib import Path
import socket
import subprocess
import tempfile
import time
from urllib.error import URLError
from urllib.request import urlopen
import json


def find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def resolve_server_binary(
    repo_root: Path,
    explicit_bin: Path | None = None,
    *,
    no_build: bool = False,
) -> Path:
    if explicit_bin is not None:
        if not explicit_bin.is_file():
            raise FileNotFoundError(f"Explicit nsac-server binary not found: {explicit_bin}")
        return explicit_bin

    candidates = [
        repo_root / "target" / "debug" / "nsac-server",
        repo_root / "target" / "release" / "nsac-server",
    ]
    for candidate in candidates:
        if candidate.is_file():
            return candidate

    if no_build:
        raise FileNotFoundError(
            "nsac-server binary not found in target/debug or target/release and --no-build set"
        )

    proc = subprocess.run(
        ["cargo", "build", "--bin", "nsac-server"],
        cwd=str(repo_root),
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"cargo build --bin nsac-server failed (rc={proc.returncode}):\n"
            f"{proc.stderr[-3000:]}"
        )

    debug_bin = repo_root / "target" / "debug" / "nsac-server"
    if not debug_bin.is_file():
        raise FileNotFoundError(f"Expected binary after build at {debug_bin}")
    return debug_bin


class ServerProcess:
    """Manages spawning, health polling, log capture, and clean shutdown of nsac-server."""

    def __init__(
        self,
        binary_path: Path,
        env_vars: dict[str, str],
        http_bind: str,
        *,
        cwd: Path | None = None,
    ) -> None:
        self.binary_path = binary_path
        self.env_vars = dict(env_vars)
        self.http_bind = http_bind
        self.base_url = f"http://{http_bind}"
        self.cwd = cwd or binary_path.parent
        self._proc: subprocess.Popen[bytes] | None = None
        self._log_file = tempfile.NamedTemporaryFile(
            prefix="nsac_server_e2e_", suffix=".log", delete=True
        )

    def start(self, health_timeout_secs: float = 25.0) -> "ServerProcess":
        if self._proc is not None:
            return self
        env = dict(self.env_vars)
        self._proc = subprocess.Popen(
            [str(self.binary_path)],
            cwd=str(self.cwd),
            env=env,
            stdout=self._log_file,
            stderr=subprocess.STDOUT,
        )
        self._wait_for_health(timeout=health_timeout_secs)
        return self

    def _wait_for_health(self, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        health_url = f"{self.base_url}/api/v1/health"
        last_err = ""
        while time.monotonic() < deadline:
            if self._proc is not None and self._proc.poll() is not None:
                raise RuntimeError(
                    f"nsac-server exited prematurely with code {self._proc.returncode}.\n"
                    f"--- server logs ---\n{self.logs_tail(100)}"
                )
            try:
                with urlopen(health_url, timeout=1.0) as resp:
                    if resp.status == 200:
                        data = json.loads(resp.read().decode("utf-8"))
                        if isinstance(data, dict) and data.get("success") is True:
                            return
            except (URLError, OSError, ValueError) as exc:
                last_err = str(exc)
            time.sleep(0.1)
        raise TimeoutError(
            f"Timed out waiting for {health_url} ({last_err}).\n"
            f"--- server logs ---\n{self.logs_tail(100)}"
        )

    def stop(self) -> None:
        if self._proc is not None:
            if self._proc.poll() is None:
                self._proc.terminate()
                try:
                    self._proc.wait(timeout=5.0)
                except subprocess.TimeoutExpired:
                    self._proc.kill()
                    self._proc.wait(timeout=3.0)
            self._proc = None

    def close_logs(self) -> None:
        try:
            self._log_file.close()
        except Exception:
            pass

    def logs_tail(self, lines: int = 80) -> str:
        try:
            self._log_file.flush()
            with open(self._log_file.name, "r", encoding="utf-8", errors="replace") as fh:
                all_lines = fh.readlines()
            return "".join(all_lines[-lines:])
        except Exception as exc:
            return f"<unable to read server log: {exc}>"
