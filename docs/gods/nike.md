# Nike: trophies

> Goddess of victory. Nike shows the trophies a game offers.

## Scope (parity)

- Read trophy sets straight from loaded PS3/PS4 packages, with no extracted tree.
- Per trophy: ID (the real ID, never assumed contiguous), name, description, grade,
  hidden flag, group ID and icon. Hidden text is shown (spoilers), and the UI says so.
- Language: English, then the default language, then the first available.
- PS4 release ESFM decryption uses IDs from `sce_sys/npbind.dat` when available.
  Otherwise it tries `NPWR00000_00`–`NPWR99999_00` against one encrypted XML
  block. Other suffixes require a matching binding.
- These are **definitions, not earned status**. No profile or console data is read.
- An unreadable set reports an error without hiding the others. The CLI exits
  `10` (`Partial`) if some sets fail, and with the error's own code if all fail
  ([frontends §6.5](../04-frontends.md#65-exit-codes)). A package with no trophy archive gives an empty list.
- Limits: TRP ≤ 64 MiB each and ≤ 128 MiB combined, XML/icons ≤ 4 MiB each,
  ≤ 4,096 trophies per set.

## Library

```rust
// pantheon-trp: archive + config parsing + ESFM decryption (shared by PS3/PS4)
// nike
pub fn read(pkg: &mut Package, ctx: &Ctx) -> Result<Vec<SetResult>, Error>;  // SetResult = Result<TrophySet, SetError>
```

The ID brute-force runs on the worker thread, checks `Cancel` every 1,000
candidates, and reports progress.

## CLI

`nike list INPUT [--json]` (icons left out of JSON). Proposed: `nike icons INPUT OUTDIR`.

## Model / GUI

A list with icons and grade badges, and a group filter. It loads in the
background with a spinner.

## Tests

Synthetic TRP + SFM/ESFM fixtures (our own keys path, using public algorithm
vectors). Malformed and oversized resources, multiple sets, noncontiguous IDs.
Local checks: SSX PS3 (37 trophies), TMNT PS4 base/update (31 each), count and
ID match vs. pkg-tools.

## References

pkg-tools `README.md` (GNOME application, Trophies), `docs/dependencies-and-fixtures.md` (Trophy reader).
