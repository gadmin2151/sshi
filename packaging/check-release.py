#!/usr/bin/env python3
"""Check exported sources and package contents without printing matched credentials."""
import argparse
import io
from pathlib import Path
import re
import subprocess
import sys
import tarfile

SENSITIVE_FILE = re.compile(
    r"(?:^|/)(?:\.env(?:\..*)?|relay\.env|[^/]*\.(?:pem|token|key|p12|pfx|ppk)|"
    r"(?:known_hosts|authorized_keys)(?:\..*)?|id_(?:rsa|ecdsa|ed25519)(?:\.pub)?|"
    r"(?:connections|profiles|known-hosts|invites-[^/]+)\.toml|[^/]*\.sshi-backup\.json|"
    r"\.ssh|\.sshi|\.aws|\.codex|\.agents)(?:/|$)", re.I
)
PATTERNS = [
    re.compile(rb"iroh_[a-f0-9]{32,}", re.I),
    re.compile(rb"-----BEGIN (?:[A-Z ]+ )?PRIVATE KEY-----\s+[A-Za-z0-9+/]{35,}"),
    re.compile(rb"(?:gh[pousr]_[A-Za-z0-9]{32,}|github_pat_[A-Za-z0-9_]{35,})"),
    re.compile(rb"(?:ssh-ed25519|ssh-rsa|ecdsa-sha2-[^ ]{4,12}) AAAA[A-Za-z0-9+/]{30,}"),
    re.compile(rb"(?im)^\s*(?:export\s+)?IROH_RELAY_ACCESS_TOKEN\s*=\s*[\"']?[a-zA-Z0-9_-]{20,}"),
]


def check(name, content):
    name = name.removeprefix("./")
    if (name != ".env.example" and SENSITIVE_FILE.search(name)) or any(p.search(content) for p in PATTERNS):
        raise ValueError(f"Review sensitive material in {name}")


def read(*args):
    return subprocess.check_output(args)


def rpm_contents(path):
    data = read("rpm2cpio", path)
    offset = 0
    while data[offset:offset + 6] in (b"070701", b"070702"):
        fields = [int(data[i:i + 8], 16) for i in range(offset + 6, offset + 110, 8)]
        size, namesize = fields[6], fields[11]
        start = offset + 110
        name = data[start:start + namesize - 1].decode()
        if name == "TRAILER!!!":
            return
        start = (start + namesize + 3) & ~3
        check(name, data[start:start + size])
        offset = (start + size + 3) & ~3
    raise ValueError(f"Invalid RPM payload in {path}")


def package(path):
    if path.suffix == ".deb":
        for section in ("--ctrl-tarfile", "--fsys-tarfile"):
            with tarfile.open(fileobj=io.BytesIO(read("dpkg-deb", section, path))) as archive:
                for entry in archive:
                    check(entry.name, archive.extractfile(entry).read() if entry.isfile() else b"")
    elif path.suffix == ".rpm":
        rpm_contents(path)
    elif path.name.endswith(".pkg.tar.zst"):
        with tarfile.open(fileobj=io.BytesIO(read("zstd", "-dc", "--", path))) as archive:
            for entry in archive:
                check(entry.name, archive.extractfile(entry).read() if entry.isfile() else b"")
    elif path.suffix == ".pkg":
        if sys.platform == "darwin":
            import tempfile
            with tempfile.TemporaryDirectory() as folder:
                dest = Path(folder) / "expanded"
                subprocess.run(["pkgutil", "--expand-full", path, dest], check=True)
                for file in dest.rglob("*"):
                    if file.is_file() and not file.is_symlink():
                        check(file.relative_to(dest).as_posix(), file.read_bytes())
    elif path.suffix != ".exe" and path.name != "SHA256SUMS":
        raise ValueError(f"Unexpected release artifact {path.name}")
    check(path.name, path.read_bytes())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", action="store_true", help="inspect all Git index blobs")
    parser.add_argument("--dist", type=Path, help="inspect a directory containing release assets")
    args = parser.parse_args()
    if args.source:
        files = read("git", "ls-files", "-z").decode().split("\0")
        for name in filter(None, files):
            check(name, read("git", "show", ":" + name))
    if args.dist:
        for file in sorted(args.dist.iterdir()):
            package(file)
    print("Checked release inputs for private keys, relay credentials and local profiles.")


if __name__ == "__main__":
    main()
