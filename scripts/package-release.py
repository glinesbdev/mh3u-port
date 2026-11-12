#!/usr/bin/env python3
"""Build a RecompLauncher release archive.

The archive is flat: mh3u-tools (or mh3u-tools.exe), recomplauncher.json and LICENSE
at the top, and nothing else. A settings file shipped here would replace the player's
on every update, and the translated game (game.dll / libgame.so) is derived from their
dump, so neither is packed.

    python3 scripts/package-release.py linux  target/release/mh3u-tools      0.1.0 dist
    python3 scripts/package-release.py windows target/release/mh3u-tools.exe 0.1.0 dist
    python3 scripts/package-release.py self-check
"""

import io
import stat
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
JSON_NAME = "recomplauncher.json"
LICENSE_NAME = "LICENSE"
# Names that must never ride along. An update unpacks over the install, so a settings
# file in the archive would replace the player's.
FORBIDDEN = ("bindings.json", "settings.toml", "settings.ini", "settings.json", "wishlist.json")


def version_of(text: str) -> str:
    version = text[1:] if text.startswith("v") else text
    if not version or any(c in version for c in "/\\"):
        raise SystemExit(f"version {text!r} is not a single path segment")
    return version


def archive_name(system: str, version: str) -> str:
    if system == "linux":
        return f"mh3u-port-v{version}-Linux-x64.tar.gz"
    if system == "windows":
        return f"mh3u-port-v{version}-Windows-x64.zip"
    raise SystemExit("system is linux or windows")


def binary_name(system: str) -> str:
    return "mh3u-tools.exe" if system == "windows" else "mh3u-tools"


def members_ok(names: list[str], system: str) -> None:
    allowed = {binary_name(system), JSON_NAME, LICENSE_NAME}
    if set(names) != allowed:
        raise SystemExit(f"archive members {sorted(names)} are not {sorted(allowed)}")
    for name in names:
        if "/" in name or "\\" in name or name.startswith("."):
            raise SystemExit(f"{name} is not at the top of the archive")
        lower = name.lower()
        if any(bad in lower for bad in FORBIDDEN) or lower.startswith("save"):
            raise SystemExit(f"refusing to ship {name}: an update would replace the player's copy")


def pack(system: str, binary: Path, version: str, out_dir: Path) -> Path:
    if not binary.is_file():
        raise SystemExit(f"not a file: {binary}")
    out_dir.mkdir(parents=True, exist_ok=True)
    dest = out_dir / archive_name(system, version)
    name = binary_name(system)
    manifest = ROOT / JSON_NAME
    license_file = ROOT / LICENSE_NAME
    if not manifest.is_file() or not license_file.is_file():
        raise SystemExit("recomplauncher.json and LICENSE must sit at the repo root")
    if system == "linux":
        with tarfile.open(dest, "w:gz") as tar:
            for src, arc in ((binary, name), (manifest, JSON_NAME), (license_file, LICENSE_NAME)):
                info = tar.gettarinfo(src, arcname=arc)
                info.uid = info.gid = 0
                info.uname = info.gname = ""
                if arc == name:
                    info.mode = stat.S_IRWXU | stat.S_IRGRP | stat.S_IXGRP | stat.S_IROTH | stat.S_IXOTH
                with src.open("rb") as fh:
                    tar.addfile(info, fh)
        with tarfile.open(dest, "r:gz") as tar:
            members_ok(tar.getnames(), system)
    else:
        with zipfile.ZipFile(dest, "w", compression=zipfile.ZIP_DEFLATED) as zf:
            for src, arc in ((binary, name), (manifest, JSON_NAME), (license_file, LICENSE_NAME)):
                zf.write(src, arcname=arc)
        with zipfile.ZipFile(dest) as zf:
            members_ok(zf.namelist(), system)
    print(dest)
    return dest


def self_check() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        for system, filename in (("linux", "mh3u-tools"), ("windows", "mh3u-tools.exe")):
            binary = root / filename
            binary.write_bytes(b"not a real build\n")
            binary.chmod(0o755)
            dest = pack(system, binary, "0.0.0-test", root / "dist")
            data = dest.read_bytes()
            if system == "linux":
                with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tar:
                    members_ok(tar.getnames(), system)
                    got = tar.extractfile("recomplauncher.json")
                    blob = b"" if got is None else got.read()
            else:
                with zipfile.ZipFile(io.BytesIO(data)) as zf:
                    members_ok(zf.namelist(), system)
                    blob = zf.read("recomplauncher.json")
            if b'"format": 1' not in blob or b"glinesbdev/mh3u-port" not in blob:
                raise SystemExit("recomplauncher.json in the archive is not this repo's file")
    print("package layout ok")


def main() -> int:
    if len(sys.argv) == 2 and sys.argv[1] == "self-check":
        self_check()
        return 0
    if len(sys.argv) != 5:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    system, binary, version, out_dir = sys.argv[1:]
    pack(system, Path(binary), version_of(version), Path(out_dir))
    return 0


if __name__ == "__main__":
    sys.exit(main())
