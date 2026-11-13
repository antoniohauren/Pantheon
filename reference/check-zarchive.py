#!/usr/bin/env python3
"""Independent check of proteus output with the unmodified pinned upstream reader.

Usage: check-zarchive.py ZARCHIVE_CHECKOUT [--large] [--pkg GAME.pkg [--update UPDATE.pkg]]...

Builds `reference/zarchive-reader.cpp` against the upstream ZArchive reader at
the pinned revision, packs a generated tree with `proteus pack`, and has the
upstream reader compare enumeration, sizes, every byte, case-insensitive
lookup and random reads. It also checks the footer SHA-256, that both raw and
compressed blocks occur, and that a 128-byte name is refused. `--large` adds a
sparse 4 GiB + 65,537-byte file. Each `--pkg` input is extracted with
`hades extract` and converted with `proteus convert` (with the `--update`
that follows it, if any), and the reader compares each archive with the
corresponding extracted tree. Reports print counts, never names.
"""
import hashlib
import pathlib
import random
import struct
import subprocess
import sys
import tempfile

PIN = "965b66c8d67b6b7e30fd63b3b75aa91a99ff303b"
FOOTER = 144
RECORD = 40
BLOCK = 65536

reference = pathlib.Path(sys.argv[1]).resolve()
options = sys.argv[2:]
packages = []  # (base, update or None)
for i, option in enumerate(options):
    if option == "--pkg":
        packages.append((options[i + 1], None))
    elif option == "--update":
        packages[-1] = (packages[-1][0], options[i + 1])
repo = pathlib.Path(__file__).resolve().parents[1]
head = subprocess.check_output(["git", "-C", str(reference), "rev-parse", "HEAD"], text=True)
assert head.strip() == PIN, "upstream checkout is not at the pinned revision"
assert not subprocess.check_output(["git", "-C", str(reference), "diff", "HEAD", "--", "src", "include"]), \
    "upstream reader sources are modified"
subprocess.run(["cargo", "build", "--quiet", "--release", "-p", "pantheon-cli",
                "--bin", "proteus", "--bin", "hades"], cwd=repo, check=True)
proteus = repo / "target/release/proteus"
hades = repo / "target/release/hades"

with tempfile.TemporaryDirectory(prefix="pantheon-zarchive-check-", dir=repo / "target") as temporary:
    work = pathlib.Path(temporary)
    oracle = work / "reader"
    subprocess.run(["c++", "-std=c++17", "-O2", "-I" + str(reference / "include"),
                    str(repo / "reference/zarchive-reader.cpp"),
                    str(reference / "src/zarchivereader.cpp"), "-lzstd", "-o", str(oracle)], check=True)

    tree = work / "tree"
    (tree / "Nested" / "empty-dir").mkdir(parents=True)
    rng = random.Random(42)
    for i, size in enumerate([0, 1, 65535, 65536, 65537, 19 * 65536 + 97]):
        (tree / "Nested" / f"noise-{i}").write_bytes(rng.randbytes(size))
        (tree / f"repeat-{i}").write_bytes(b"abc123" * (size // 6) + b"x" * (size % 6))
    for name in ["a", "AA", "Z", "é", "Ω", "n" * 127]:
        (tree / name).write_bytes(b"name test")
    if "--large" in options:
        with (tree / "Large").open("wb") as large:
            large.truncate(2**32 + 65537)
    archive = work / "test.zar"
    subprocess.run([str(proteus), "pack", str(tree), str(archive)], check=True)

    data = bytearray(archive.read_bytes())
    footer = len(data) - FOOTER
    digest = bytes(data[footer + 96:footer + 128])
    data[footer + 96:footer + 128] = bytes(32)
    assert hashlib.sha256(data).digest() == digest, "footer SHA-256"
    sections = struct.unpack_from(">12Q", data, footer)
    index, length = sections[2:4]
    sizes = [n + 1 for record in range(index, index + length, RECORD)
             for n in struct.unpack_from(">16H", data, record + 8)]
    assert BLOCK in sizes and any(1 < n < BLOCK for n in sizes), "raw and compressed blocks"
    del data
    subprocess.run([str(oracle), str(archive), str(tree)], check=True)
    print("pack: footer SHA-256, raw/compressed blocks and UTF-8 names verified")

    (tree / ("n" * 128)).write_bytes(b"unsupported name")
    rejected = work / "rejected.zar"
    result = subprocess.run([str(proteus), "pack", str(tree), str(rejected)], capture_output=True)
    assert result.returncode != 0 and not rejected.exists(), "128-byte name must be refused"
    print("pack: 128-byte name refused, nothing published")

    def compare(label, archive, package):
        extracted = work / f"{label}-extracted"
        subprocess.run([str(hades), "extract", package, str(extracted)], check=True, capture_output=True)
        result = subprocess.run([str(oracle), str(archive), str(extracted)], capture_output=True, text=True)
        print(f"{label}: {result.stdout.strip() or result.stderr.strip()}")
        assert result.returncode == 0, f"{label}: upstream reader comparison failed"

    for number, (base, update) in enumerate(packages, start=1):
        converted = work / f"converted-{number}.zar"
        command = [str(proteus), "convert", base, str(converted)]
        if update:
            command += ["--update", update]
        subprocess.run(command, check=True, capture_output=True)
        compare(f"pkg {number} base", converted, base)
        if update:
            compare(f"pkg {number} update", work / f"converted-{number}-UPDATE.zar", update)
