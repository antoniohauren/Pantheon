# Phase 1 spike: `pantheon-fs`

Status: Linux verified (local runs, D35). macOS and Windows are **unverified**:
no macOS or Windows target is installed on the build machine, so that code has
not been compiled, let alone run.

## Publication path per filesystem

`OutputFile::create_new` probes the filesystem while the scratch file is still
empty: first a hard link to a second scratch name, then a no-replace rename to
it, and if both fail with a "not supported" error, fallback 2 (D28, direct
`create_new` on the final name). Re-run the probe on any filesystem with:

```sh
TMPDIR=/path/on/that/fs cargo test -p pantheon-fs probe_publication_path -- --ignored --nocapture
```

| Filesystem (Linux 7.2, local) | Mount | File publication | Tree no-replace rename (P15) |
| --- | --- | --- | --- |
| tmpfs (scratch/temp dir) | `/tmp` | hard link | `renameat2(RENAME_NOREPLACE)` works |
| tmpfs | `/dev/shm`, `/run/user/1000` | hard link | works |
| ext4 (repository) | `/` | hard link | works |

Not tested:

- **FAT32/exFAT**: mounting an image needs root here, and the one vfat mount
  (`/boot`) is not writable by the user. From kernel behaviour (not measured):
  Linux vfat/exfat refuse `link` with `EPERM`, so they should take fallback 1;
  both implement `RENAME_NOREPLACE`. This is a claim to check, not evidence.
- FUSE (`/run/user/1000/doc`, portal) is not writable at its root.
- Network shares. No local evidence that any filesystem reaches fallback 2.

Hard-link-less and no-replace-rename-less filesystems are covered by injected
errors in the unit tests (`file::tests::hard_link_less_filesystem_falls_back_to_rename`,
`file::tests::neither_link_nor_rename_writes_directly_and_reports_incomplete`,
`tree::tests::no_replace_rename_unsupported_creates_directly_with_marker`).

Fallback errnos (Unix): `EPERM`, `EOPNOTSUPP`/`ENOTSUP`, `EXDEV`, `ENOSYS`, plus
`EINVAL` for the rename only (Linux's answer for an unsupported `renameat2` flag).
Windows (unverified): `ERROR_INVALID_FUNCTION`, `ERROR_NOT_SUPPORTED`,
`ERROR_NOT_SAME_DEVICE`. `EEXIST`/`ERROR_ALREADY_EXISTS` is always `UnsafeOutput`.

## Does `rustix` `renameat_with` cover both flags?

Yes, checked in the `rustix 1.1.5` source:

- `renameat_with` is available on `linux_kernel`, `apple` and `redox`.
- Linux: `RenameFlags::NOREPLACE` = `RENAME_NOREPLACE`, issued as the raw
  `renameat2` syscall (linux_raw backend).
- macOS: `RenameFlags::NOREPLACE` = `RENAME_EXCL`, issued through
  `renameatx_np` (looked up weakly). On macOS < 10.12, which lacks it, any
  non-empty flags return `ENOSYS`, which `pantheon-fs` treats as "unsupported"
  and so takes the fallback. Note it is `renameatx_np`, the descriptor-relative
  sibling of the `renamex_np` named in P15; the flag is the same.

## Is `std` enough on Windows? (reasoned, unverified)

For the no-follow opens, mostly yes: `OpenOptionsExt::custom_flags` with
`FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS` opens the object
itself (including directories), and `MetadataExt::file_attributes` exposes
`FILE_ATTRIBUTE_REPARSE_POINT`, so junctions and symlinks are rejected after the
open. `std::fs::hard_link` covers publication. What `std` cannot do:

- a no-replace rename (`std::fs::rename` replaces): one `MoveFileExW` call
  through `windows-sys` (D22);
- a handle-relative open, so every check is by path and re-done per operation;
  the race window documented in foundation §2 remains;
- a stable file index (`MetadataExt::file_index` is unstable), so
  `SourceSnapshot` uses the creation time in its place on Windows. This is
  weaker than inode identity; fixing it needs `GetFileInformationByHandle`,
  which would be a third `unsafe` call and so a decision-log entry.

`windows-sys` features: `Win32_Storage_FileSystem`, `Win32_Security_Cryptography`.
`Win32_Foundation` (`NTSTATUS`, `BOOL`) is not listed: it comes in through the
`Win32` parent feature.

## Windows `Access::OwnerOnly`

Not implemented. `OutputFile::create_new` and `OutputTree::create_file` return
`Error::Unsupported` for it on Windows rather than writing a file others can
read. An owner-only ACL needs security-descriptor APIs; that is a separate spike.
