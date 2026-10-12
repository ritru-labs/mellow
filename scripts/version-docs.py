#!/usr/bin/env python3
"""Keep rendered release labels tied to Cargo.toml; run --write when versioning."""
import argparse
from pathlib import Path
import re

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--write", action="store_true")
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
version = re.search(r'^version = "([^" ]+)"', (root / "Cargo.toml").read_text(), re.M).group(1)
labels = {
    "README.md": [(r"(\*\*Version )[0-9]+\.[0-9]+\.[0-9]+(, beta\.\*\*)", rf"\g<1>{version}\g<2>")],
    "docs/install.md": [(r"(MELLOW_VERSION=|/releases/download/v|mellow[_ -])[0-9]+\.[0-9]+\.[0-9]+", rf"\g<1>{version}")],
    "docs/getting-started.md": [(r"(describe \*\*)[0-9]+\.[0-9]+\.[0-9]+( beta\*\*)", rf"\g<1>{version}\g<2>")],
    "docs/index.md": [
        (r"(MELLOW · DOCUMENTATION · )[0-9]+\.[0-9]+\.[0-9]+( BETA)", rf"\g<1>{version}\g<2>"),
        (r"(\| Capability in )[0-9]+\.[0-9]+\.[0-9]+( \|)", rf"\g<1>{version}\g<2>"),
    ],
}
errors = []
for name, patterns in labels.items():
    path = root / name
    old = path.read_text()
    new = old
    for pattern, replacement in patterns:
        new, count = re.subn(pattern, replacement, new)
        if count < 1 or (name != "docs/install.md" and count != 1):
            errors.append(f"{name}: missing or ambiguous version label matching {pattern}")
    if old != new:
        if args.write:
            path.write_text(new)
        else:
            errors.append(f"{name}: release labels differ from Cargo.toml {version}; run python3 scripts/version-docs.py --write")
if errors:
    raise SystemExit("\n".join(errors))
print(f"Release documentation labels agree with Cargo.toml {version}")
