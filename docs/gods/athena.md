# Athena: inspect

> Goddess of wisdom. Athena tells you what a package *is*, without changing anything.

## Scope (parity)

- Detect input by magic: PS3 PKG (`7f 50 4b 47`), PS4 PKG, bare PARAM.SFO.
- PS4: header fields, container records (ID, offset, size, flags, key index),
  plaintext SFO, `icon0.png`/`pic1.png` (≤ 4 MiB, PNG ≤ 2048 × 2048 when shown).
- PS3: header, raw metadata records, SFO and `ICON0.PNG` from the package.
- SFO: every entry with format, used length, capacity and raw bytes. Invalid
  UTF-8 decodes to `null` rather than lossy text.
- Status flags kept from pkg-tools: SFO missing vs. encrypted (both exit 0),
  `fpkg_status: "unverified"` (header inspection never proves authenticity).
- Overview summary: title, title ID, application version, game/patch type,
  package size, content ID, source path.

## Library

```rust
pub fn inspect(src: impl Read + Seek, limits: &Limits) -> Result<Inspection, Error>;
pub struct Inspection { pub kind: Kind, pub summary: Summary, pub sfo: SfoStatus, pub details: Details }
pub fn artwork(src: impl Read + Seek, which: Artwork) -> Result<Option<Vec<u8>>, Error>; // bounded
```

`Details` is a typed tree (PS3 or PS4 variant), never preformatted text. CLI and
GUI each render it themselves. Binary previews are truncated by the **frontend**
(the GUI shows 64 bytes), and `--json` carries complete hex.

## CLI

`athena inspect INPUT [--json]`. Also `athena artwork INPUT --icon0|--pic1 OUT.png` (D29).
JSON schema `pantheon.athena.inspection/1`. It keeps pkg-tools' rules: numeric enums
stay numeric, and raw data/padding/keys get hex fields.

## Model / GUI

Overview page (artwork, summary rows, selectable text) and All Info page (header,
SFO, container entries). The setting *Use PS4 pic1.png* lives in the shell section
of the settings and falls back to `icon0.png`.

## Tests

- Port pkg-tools' `psf` parse/serialize cases and PS3/PS4 inspect cases as
  **behavioural** tests against new synthetic fixtures.
- Corruption sweep (`pantheon-core::testing::Corruptor`) over every fixture.
  Expect a typed error and no panic, and allocation stays under limit.
- Differential: `athena inspect --json` vs. `pkg-cli inspect --json` on the
  local private fixtures. The field mapping is documented, and the results are
  recorded without fingerprints.

## References

pkg-tools `README.md` (Libraries, Validation and limits), `docs/compatibility.md` §3–4, §6.
