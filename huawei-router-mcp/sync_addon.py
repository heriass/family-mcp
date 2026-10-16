"""Refresh the standalone HA build context from the canonical server files."""
from pathlib import Path
import shutil

if __name__ == "__main__":
    root = Path(__file__).resolve().parent
    for name in ("server.py", "router_parser.py", "requirements.txt"):
        shutil.copyfile(root / name, root / "addon" / name)
