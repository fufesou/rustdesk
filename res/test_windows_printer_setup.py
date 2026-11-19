"""Check an unpacked Windows Flutter build from a non-elevated shell."""

import ctypes
from pathlib import Path
import subprocess
import sys


EXIT_SUCCESS = 0
EXIT_FAILURE = 1
COMMAND_TIMEOUT_SECONDS = 15


def main():
    if sys.platform != "win32" or ctypes.windll.shell32.IsUserAnAdmin():
        raise RuntimeError("Run this test on Windows without administrator privileges")
    executable = Path(sys.argv[1]).resolve(strict=True)
    for command, expected in [
        ("--version", EXIT_SUCCESS),
        ("--install-remote-printer", EXIT_FAILURE),
        ("--uninstall-remote-printer", EXIT_FAILURE),
    ]:
        result = subprocess.run(
            [str(executable), command],
            capture_output=True,
            text=True,
            timeout=COMMAND_TIMEOUT_SECONDS,
        )
        assert result.returncode == expected, (
            f"{command}: expected exit {expected}, got {result.returncode}\n"
            f"{result.stdout}\n{result.stderr}"
        )
        print(f"{command}: exit {result.returncode}")


if __name__ == "__main__":
    main()
