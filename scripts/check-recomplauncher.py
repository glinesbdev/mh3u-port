#!/usr/bin/env python3
"""Validate recomplauncher.json against the RecompLauncher schema.

The schema is https://recomplauncher.com/schema/recomplauncher.json (the copy in the
RecompLauncher repo's docs/recomplauncher.schema.json is the same document). Set
RECOMPLAUNCHER_SCHEMA to a local file to skip the download.
"""

import json
import os
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
INSTANCE = ROOT / "recomplauncher.json"
URL = "https://recomplauncher.com/schema/recomplauncher.json"


def schema():
    path = os.environ.get("RECOMPLAUNCHER_SCHEMA")
    if path:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    # The site rejects Python's default User-Agent.
    request = urllib.request.Request(URL, headers={"User-Agent": "mh3u-port-schema-check"})
    with urllib.request.urlopen(request, timeout=60) as resp:
        return json.load(resp)


def main() -> int:
    try:
        import jsonschema
    except ImportError:
        print("jsonschema is not installed (pip install jsonschema)", file=sys.stderr)
        return 2
    instance = json.loads(INSTANCE.read_text(encoding="utf-8"))
    jsonschema.Draft7Validator(schema()).validate(instance)
    if instance.get("format") != 1:
        print("format must be 1", file=sys.stderr)
        return 1
    if instance.get("repo") != "glinesbdev/mh3u-port":
        print("repo must be glinesbdev/mh3u-port", file=sys.stderr)
        return 1
    print(f"valid: {INSTANCE}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as exc:
        print(f"recomplauncher.json: {exc}", file=sys.stderr)
        sys.exit(1)
