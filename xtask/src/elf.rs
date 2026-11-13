//! A minimal ELF64 (little-endian, x86-64) reader for `xtask payload`: turns
//! the payload's ELF into the flat image a loader runs, and counts the
//! relocations a loader would have to apply. Our own code, no ELF crate.

/// The flat image and what the checks need to know about the ELF.
#[derive(Debug, PartialEq, Eq)]
pub struct Flat {
    /// Every `PT_LOAD` segment at `vaddr - base`, `.bss` included as zeros.
    pub image: Vec<u8>,
    /// `e_entry - base`: where the loader jumps, as an offset into `image`.
    pub entry_offset: u64,
    /// Relocation entries left in `SHT_REL`/`SHT_RELA`/`SHT_RELR` sections
    /// or in the dynamic table's size entries.
    pub relocations: u64,
    /// A `PT_INTERP` segment asks for a dynamic linker.
    pub has_interpreter: bool,
}

const MAGIC: [u8; 4] = *b"\x7fELF";
const CLASS_64: u8 = 2;
const DATA_LSB: u8 = 1;
const MACHINE_X86_64: u16 = 62;
const IDENT_CLASS: usize = 4;
const IDENT_DATA: usize = 5;
const OFF_MACHINE: usize = 18;
const OFF_ENTRY: usize = 24;
const OFF_PHOFF: usize = 32;
const OFF_SHOFF: usize = 40;
const OFF_PHENTSIZE: usize = 54;
const OFF_PHNUM: usize = 56;
const OFF_SHENTSIZE: usize = 58;
const OFF_SHNUM: usize = 60;
const PHDR_LEN: usize = 56;
const SHDR_LEN: usize = 64;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const SHT_RELA: u32 = 4;
const SHT_REL: u32 = 9;
const SHT_RELR: u32 = 19;
/// Dynamic-table tags whose value is a relocation table size in bytes.
const DT_PLTRELSZ: u64 = 2;
const DT_RELASZ: u64 = 8;
const DT_RELSZ: u64 = 18;
const DT_RELRSZ: u64 = 35;
const DT_NULL: u64 = 0;
const DYN_LEN: usize = 16;
/// Sanity bound on the flat image (the real budget is checked later).
const MAX_IMAGE: u64 = 16 * 1024 * 1024;

fn u16_at(b: &[u8], at: usize) -> Result<u16, String> {
    b.get(at..at + 2)
        .and_then(|s| s.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or_else(|| format!("ELF truncated at {at:#x}"))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| format!("ELF truncated at {at:#x}"))
}

fn u64_at(b: &[u8], at: usize) -> Result<u64, String> {
    b.get(at..at + 8)
        .and_then(|s| s.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| format!("ELF truncated at {at:#x}"))
}

fn usize_of(v: u64) -> Result<usize, String> {
    usize::try_from(v).map_err(|_| format!("ELF value {v:#x} does not fit"))
}

struct Segment {
    kind: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

pub fn flatten(elf: &[u8]) -> Result<Flat, String> {
    if elf.get(..4) != Some(&MAGIC[..]) {
        return Err("not an ELF file".into());
    }
    if elf.get(IDENT_CLASS) != Some(&CLASS_64) || elf.get(IDENT_DATA) != Some(&DATA_LSB) {
        return Err("not a little-endian ELF64 file".into());
    }
    if u16_at(elf, OFF_MACHINE)? != MACHINE_X86_64 {
        return Err("not an x86-64 ELF file".into());
    }
    let entry = u64_at(elf, OFF_ENTRY)?;
    let phoff = usize_of(u64_at(elf, OFF_PHOFF)?)?;
    let phnum = usize::from(u16_at(elf, OFF_PHNUM)?);
    if phnum > 0 && usize::from(u16_at(elf, OFF_PHENTSIZE)?) != PHDR_LEN {
        return Err("unexpected program header size".into());
    }
    let mut segments = Vec::with_capacity(phnum);
    for i in 0..phnum {
        let at = phoff + i * PHDR_LEN;
        segments.push(Segment {
            kind: u32_at(elf, at)?,
            offset: u64_at(elf, at + 8)?,
            vaddr: u64_at(elf, at + 16)?,
            filesz: u64_at(elf, at + 32)?,
            memsz: u64_at(elf, at + 40)?,
        });
    }
    let loads: Vec<&Segment> = segments.iter().filter(|s| s.kind == PT_LOAD).collect();
    let base = loads
        .iter()
        .map(|s| s.vaddr)
        .min()
        .ok_or("ELF has no PT_LOAD segment")?;
    let end = loads
        .iter()
        .map(|s| s.vaddr.checked_add(s.memsz).ok_or("segment end overflows"))
        .collect::<Result<Vec<u64>, &str>>()?
        .into_iter()
        .max()
        .unwrap_or(base);
    let size = end - base;
    if size > MAX_IMAGE {
        return Err(format!("flat image of {size} bytes is implausibly large"));
    }
    let mut image = vec![0u8; usize_of(size)?];
    for s in &loads {
        if s.filesz > s.memsz {
            return Err("segment file size exceeds its memory size".into());
        }
        let from = usize_of(s.offset)?;
        let len = usize_of(s.filesz)?;
        let data = elf
            .get(from..from + len)
            .ok_or("segment lies outside the file")?;
        let to = usize_of(s.vaddr - base)?;
        image[to..to + len].copy_from_slice(data);
    }

    let mut relocations = 0u64;
    let shoff = usize_of(u64_at(elf, OFF_SHOFF)?)?;
    let shnum = usize::from(u16_at(elf, OFF_SHNUM)?);
    if shnum > 0 && usize::from(u16_at(elf, OFF_SHENTSIZE)?) != SHDR_LEN {
        return Err("unexpected section header size".into());
    }
    for i in 0..shnum {
        let at = shoff + i * SHDR_LEN;
        let kind = u32_at(elf, at + 4)?;
        if matches!(kind, SHT_REL | SHT_RELA | SHT_RELR) {
            let size = u64_at(elf, at + 32)?;
            let entsize = u64_at(elf, at + 56)?.max(1);
            relocations += size / entsize;
        }
    }
    for s in segments.iter().filter(|s| s.kind == PT_DYNAMIC) {
        let from = usize_of(s.offset)?;
        let table = elf
            .get(from..from + usize_of(s.filesz)?)
            .ok_or("dynamic table lies outside the file")?;
        for entry in table.as_chunks::<DYN_LEN>().0 {
            let tag = u64_at(entry, 0)?;
            let value = u64_at(entry, 8)?;
            match tag {
                DT_NULL => break,
                DT_PLTRELSZ | DT_RELASZ | DT_RELSZ | DT_RELRSZ if value > 0 => relocations += 1,
                _ => {}
            }
        }
    }
    Ok(Flat {
        image,
        entry_offset: entry.wrapping_sub(base),
        relocations,
        has_interpreter: segments.iter().any(|s| s.kind == PT_INTERP),
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A minimal ELF: `code` in one PT_LOAD at `vaddr`, `bss` extra zero
    /// bytes, entry at `entry`, and optionally a non-empty `.rela.dyn`.
    pub fn synthetic(code: &[u8], vaddr: u64, entry: u64, bss: u64, rela: bool) -> Vec<u8> {
        let phoff = 64usize;
        let data_off = 0x100usize;
        let shoff = (data_off + code.len()).next_multiple_of(8);
        let shnum = if rela { 3 } else { 2 };
        let mut elf = vec![0u8; shoff + shnum * SHDR_LEN];
        elf[..4].copy_from_slice(&MAGIC);
        elf[IDENT_CLASS] = CLASS_64;
        elf[IDENT_DATA] = DATA_LSB;
        elf[6] = 1;
        elf[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
        elf[OFF_MACHINE..OFF_MACHINE + 2].copy_from_slice(&MACHINE_X86_64.to_le_bytes());
        elf[OFF_ENTRY..OFF_ENTRY + 8].copy_from_slice(&entry.to_le_bytes());
        elf[OFF_PHOFF..OFF_PHOFF + 8].copy_from_slice(&(phoff as u64).to_le_bytes());
        elf[OFF_SHOFF..OFF_SHOFF + 8].copy_from_slice(&(shoff as u64).to_le_bytes());
        elf[OFF_PHENTSIZE..OFF_PHENTSIZE + 2].copy_from_slice(&(PHDR_LEN as u16).to_le_bytes());
        elf[OFF_PHNUM..OFF_PHNUM + 2].copy_from_slice(&1u16.to_le_bytes());
        elf[OFF_SHENTSIZE..OFF_SHENTSIZE + 2].copy_from_slice(&(SHDR_LEN as u16).to_le_bytes());
        elf[OFF_SHNUM..OFF_SHNUM + 2].copy_from_slice(&(shnum as u16).to_le_bytes());
        let ph = phoff;
        elf[ph..ph + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        elf[ph + 8..ph + 16].copy_from_slice(&(data_off as u64).to_le_bytes());
        elf[ph + 16..ph + 24].copy_from_slice(&vaddr.to_le_bytes());
        elf[ph + 32..ph + 40].copy_from_slice(&(code.len() as u64).to_le_bytes());
        elf[ph + 40..ph + 48].copy_from_slice(&(code.len() as u64 + bss).to_le_bytes());
        elf[data_off..data_off + code.len()].copy_from_slice(code);
        // Section 1: .text (PROGBITS); section 2: .rela.dyn with one entry.
        let sh = shoff + SHDR_LEN;
        elf[sh + 4..sh + 8].copy_from_slice(&1u32.to_le_bytes());
        if rela {
            let sh = shoff + 2 * SHDR_LEN;
            elf[sh + 4..sh + 8].copy_from_slice(&SHT_RELA.to_le_bytes());
            elf[sh + 32..sh + 40].copy_from_slice(&24u64.to_le_bytes());
            elf[sh + 56..sh + 64].copy_from_slice(&24u64.to_le_bytes());
        }
        elf
    }

    #[test]
    fn flattens_loads_and_finds_entry() {
        let elf = synthetic(b"\xEB\xFEcode", 0, 0, 10, false);
        let flat = flatten(&elf).unwrap();
        assert_eq!(&flat.image[..6], b"\xEB\xFEcode");
        assert_eq!(flat.image.len(), 16, "bss is zero-filled");
        assert_eq!((flat.entry_offset, flat.relocations), (0, 0));
        assert!(!flat.has_interpreter);

        let flat = flatten(&synthetic(b"abcd", 0x1000, 0x1002, 0, true)).unwrap();
        assert_eq!((flat.entry_offset, flat.relocations), (2, 1));
    }

    #[test]
    fn rejects_non_elf_and_truncation() {
        assert!(flatten(b"MZ").is_err());
        let elf = synthetic(b"abcd", 0, 0, 0, false);
        for cut in [4, 20, 63, 120, 0x102] {
            assert!(flatten(&elf[..cut]).is_err(), "cut at {cut}");
        }
    }
}
