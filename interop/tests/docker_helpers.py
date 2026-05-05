"""Helpers that shell out to the docker CLI for chaos + log-scrape tests.

These helpers gracefully skip the calling test when the docker CLI is not
available or the named container cannot be resolved. That keeps the suite
runnable in pure-unit mode (`INTEROP_OFFLINE=1`) and from environments
where the test-runner container does not have the docker socket mounted.
"""

import os
import shutil
import subprocess
import time

import pytest

RIPPLED_CONTAINERS = [
    name for name in os.environ.get("RIPPLED_CONTAINERS", "").split(",") if name
]
RXRPL_CONTAINERS = [
    name for name in os.environ.get("RXRPL_CONTAINERS", "").split(",") if name
]


def docker_available() -> bool:
    """True if the docker CLI is on PATH and the daemon answers `docker ps`."""
    if shutil.which("docker") is None:
        return False
    try:
        subprocess.run(
            ["docker", "ps"],
            check=True,
            capture_output=True,
            timeout=5,
        )
        return True
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError):
        return False


def require_docker():
    """Skip the calling test if the docker CLI is unreachable."""
    if not docker_available():
        pytest.skip(
            "docker CLI not available (mount /var/run/docker.sock into the "
            "test-runner or run on the host)"
        )


def container_logs(name: str, tail: int = 1000) -> str:
    """Return combined stdout+stderr of a container, or skip on error."""
    require_docker()
    try:
        result = subprocess.run(
            ["docker", "logs", "--tail", str(tail), name],
            check=True,
            capture_output=True,
            timeout=15,
        )
    except subprocess.CalledProcessError as exc:
        pytest.skip(
            f"`docker logs {name}` failed (container missing?): "
            f"{exc.stderr.decode(errors='replace')[:200]}"
        )
    return (result.stdout + result.stderr).decode(errors="replace")


def stop_container(name: str, timeout: int = 5):
    require_docker()
    subprocess.run(
        ["docker", "stop", "-t", str(timeout), name],
        check=True,
        capture_output=True,
        timeout=timeout + 10,
    )


def start_container(name: str):
    require_docker()
    subprocess.run(
        ["docker", "start", name],
        check=True,
        capture_output=True,
        timeout=15,
    )


def wait_until_running(name: str, timeout: int = 30):
    """Poll `docker inspect` until the container reports state=running."""
    require_docker()
    deadline = time.time() + timeout
    while time.time() < deadline:
        result = subprocess.run(
            ["docker", "inspect", "-f", "{{.State.Running}}", name],
            capture_output=True,
            timeout=5,
        )
        if result.returncode == 0 and result.stdout.strip() == b"true":
            return
        time.sleep(1)
    pytest.fail(f"container {name} did not enter running state within {timeout}s")
