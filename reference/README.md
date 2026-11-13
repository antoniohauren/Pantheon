# Independent reference checks

Third-party implementations that Pantheon's output is checked against
(docs/05-testing.md, "Independent reference"). Nothing here ships or builds
with the workspace.

## ZArchive: upstream reader (Proteus)

| File | Provenance |
| --- | --- |
| `zarchive-reader.cpp` | Carried verbatim from pkg-tools `tests/reference/zarchive-reader.cpp` at `20e3c3a` (written by the pkg-tools author in commit `f4219cd`). SHA-256 `acfedf8cfb941d6ce1bbb047820707d98d7aff302a1bd16a196f0d080b195747`. |
| `check-zarchive.py` | Adapted from pkg-tools `tests/reference/check-zarchive.py` at `20e3c3a`: builds `proteus`/`hades` instead of `pkg-tools`, and adds the `--pkg`/`--update` conversion mode. |

The harness links the **unmodified** upstream reader from
[ZArchive](https://github.com/Exzap/ZArchive) at commit
`965b66c8d67b6b7e30fd63b3b75aa91a99ff303b` (MIT No Attribution, © 2022 Exzap),
the revision pkg-tools verified against and the selected shadPS4 pins. The
upstream sources are cloned at test time and never vendored; the script refuses
another revision or local edits to `src/` and `include/`.

Requires a C++17 compiler and the system libzstd headers (test-only):

```sh
git clone https://github.com/Exzap/ZArchive.git /path/to/zarchive
git -C /path/to/zarchive checkout 965b66c8d67b6b7e30fd63b3b75aa91a99ff303b
python3 reference/check-zarchive.py /path/to/zarchive
python3 reference/check-zarchive.py /path/to/zarchive --large
python3 reference/check-zarchive.py /path/to/zarchive --pkg GAME.pkg --update UPDATE.pkg
```

The base run packs a generated tree (empty files and folders, 65,535/65,536/
65,537-byte and multi-record payloads, UTF-8 and 127-byte names) and has the
upstream reader compare enumeration, sizes, every byte, case-insensitive
lookup and random reads. It also checks the footer SHA-256, that raw and
compressed blocks both occur, and that a 128-byte name is refused with
nothing published. `--large` adds a sparse 4 GiB + 65,537-byte file. `--pkg`
converts a local PS4 package (and its `--update`) and compares each archive
with `hades extract` of the same package. No game data is committed; reports
print counts only.
