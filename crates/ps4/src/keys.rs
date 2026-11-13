//! The fake-package key chain and container record decryption.
//!
//! The RSA keysets are public interoperability constants (primes only):
//! `RSAKeyset.PkgDerivedKey3Keyset` and `RSAKeyset.FakeKeyset` from LibOrbisPkg
//! `Util/Keys.cs` @ `643477263b2644e0803e0f58b8726ea4e3f3b7d4`, as recorded in
//! pkg-tools `docs/extraction.md`. No upstream code is used.

use std::io::{Read, Seek, SeekFrom};
use std::sync::OnceLock;

use pantheon_core::CHUNK;
use pantheon_crypto::aes::{self, BLOCK as AES_BLOCK};
use pantheon_crypto::hash::Sha256;
use pantheon_crypto::rsa::{self, PrivateKey, RSA_BYTES};

use crate::{Entry, Error};

const FAKE_P: &str = "fef6bf1d69ab16250847556b86e43588722ab13df8b644cab3ab19d10424280a7455b8154509cc131cf2ba37a903908f0210ff257986cc18509a105f5b4c1c4eb0a7e359b12da0c6b0202c213312b3af723483cd522faf0f205a1bc0e2a376340fd7fcc141c9f979401742213e9dfdc7c150de445ac931896a7805be65b4e82d";
const FAKE_Q: &str = "c79e4758007d6282b0d22281d4a8971b790c3ab0d7c930e3c3538e57eff09b9fb39052c6942236aae64a5f721d70e87658c8b291ce9cc3e9097f2e4797cc9039153531de1f0c8c0dc1c292be97bf2f91a18c7d50a8212fd7a29a7eb5a72a9002d9f33dd1ebb8e05a799e7d8dca186dbd9ea180286b2afe51249b6f4d84778023";
const DK3_P: &str = "f967ad9912310c56a22e161c46b34d5b43be42a2f686968042c3c73fc342f58749339f075d6e2c04fde3e1b2ae0a0cf0c7a61ca16350c8099c5124526c5e5ebd1e2706bbbc9e94e135d46db3cb3c68dd68b3fe6ccb8d8220762363b7e96810014edcba275d01c12d805e2baf826bd884b6105286a7898eae9ae289c6f7d587fb";
const DK3_Q: &str = "d7a10f9a8bf2c91195329a8cf0d94047f568a00dbdc1fc432f65f9c3610f257754add758ac8440608d3ff3658975b5c62c511a2f1f22e4431154bec9b4c7b51b050bbc569acd4ad973685e5cfb92b78b0dfff507cab4c89b963c079e3e6b2a11f28ab18ad72e1ba5532406ed50b89067b1e241c69201ee10f061bbfbb27d4a73";
const EXPONENT: u32 = 65_537;

/// Record `0x10`: 32-byte digests at 32, then 256-byte wrapped keys at 256.
pub(crate) const KEYS_ID: u32 = 0x10;
pub(crate) const KEYS_LEN: usize = 2048;
/// Record `0x20`: the RSA-wrapped PFS image key, CBC-encrypted with key index 3.
pub(crate) const IMAGE_KEY_ID: u32 = 0x20;
pub(crate) const IMAGE_KEY_LEN: usize = RSA_BYTES;
/// The only container key index the public keysets unlock.
pub(crate) const FAKE_KEY_INDEX: u8 = 3;
/// `flags1` bit of the newer container encryption, which is unsupported.
pub(crate) const NEW_CRYPT_FLAG: u32 = 0x1000_0000;
const DIGESTS_AT: usize = 32;
const DIGEST_LEN: usize = 32;
const WRAPPED_AT: usize = 256;
const DK3_SLOT: usize = 3;
const RECORD_FIELDS: usize = 6;

/// The NP binding record stores (and hashes) 0x220 CBC bytes for 0x214 declared.
const NPBIND_ID: u32 = 0x403;
const NPBIND_LEN: u32 = 0x214;
const NPBIND_STORED: u64 = 0x220;
const NPBIND_FLAGS1: u32 = 0x8000_0000;
const NPBIND_FLAGS2: u32 = 0x3000;

/// The two public keysets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Keyset {
    /// Wraps DK3 in record `0x10`.
    Dk3,
    /// Wraps the PFS image key in record `0x20`.
    Fake,
}

fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .filter_map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
        .collect()
}

/// Built once per process: key validation and CRT precomputation are costly.
pub(crate) fn private_key(which: Keyset) -> Result<&'static PrivateKey, Error> {
    static DK3: OnceLock<Option<PrivateKey>> = OnceLock::new();
    static FAKE: OnceLock<Option<PrivateKey>> = OnceLock::new();
    let (cell, p, q) = match which {
        Keyset::Dk3 => (&DK3, DK3_P, DK3_Q),
        Keyset::Fake => (&FAKE, FAKE_P, FAKE_Q),
    };
    cell.get_or_init(|| PrivateKey::from_primes(&unhex(p), &unhex(q), EXPONENT).ok())
        .as_ref()
        .ok_or(Error::KeyRecovery("built-in public keyset"))
}

fn unwrap(which: Keyset, block: &[u8]) -> Result<[u8; 32], Error> {
    let block: &[u8; RSA_BYTES] = block
        .try_into()
        .map_err(|_| Error::Malformed("key block"))?;
    rsa::fake_unwrap(private_key(which)?, block)
        .map_err(|_| Error::KeyRecovery("unsupported keyset or damaged key material"))
}

/// The digest stored for a 32-byte key: `SHA-256(key) XOR key`.
pub(crate) fn key_digest(key: &[u8; 32]) -> [u8; 32] {
    let mut d = Sha256::digest(key);
    d.iter_mut().zip(key).for_each(|(a, b)| *a ^= b);
    d
}

/// DK3 from record `0x10`, checked against its stored digest.
pub(crate) fn recover_dk3(keys: &[u8; KEYS_LEN]) -> Result<[u8; 32], Error> {
    let at = WRAPPED_AT.saturating_add(DK3_SLOT.saturating_mul(RSA_BYTES));
    let wrapped = keys.get(at..at.saturating_add(RSA_BYTES));
    let dk3 = unwrap(Keyset::Dk3, wrapped.unwrap_or_default())?;
    let at = DIGESTS_AT.saturating_add(DK3_SLOT.saturating_mul(DIGEST_LEN));
    if keys.get(at..at.saturating_add(DIGEST_LEN)) != Some(&key_digest(&dk3)[..]) {
        return Err(Error::Malformed("recovered key digest mismatch"));
    }
    Ok(dk3)
}

/// `SHA-256(record || dk3)`: IV (first half) and AES-128 key (second half).
pub(crate) fn entry_material(e: &Entry, dk3: &[u8; 32]) -> [u8; 32] {
    let mut record = [0u8; 32];
    let fields: [u32; RECORD_FIELDS] = [e.id, e.name_offset, e.flags1, e.flags2, e.offset, e.size];
    for (dst, v) in record.as_chunks_mut::<4>().0.iter_mut().zip(fields) {
        dst.copy_from_slice(&v.to_be_bytes());
    }
    let mut h = Sha256::new();
    h.update(&record);
    h.update(dk3);
    h.finalize()
}

pub(crate) fn split_material(m: &[u8; 32]) -> ([u8; 16], [u8; 16]) {
    let mut iv = [0u8; 16];
    let mut key = [0u8; 16];
    let (a, b) = m.split_at(16);
    iv.copy_from_slice(a);
    key.copy_from_slice(b);
    (iv, key)
}

/// The PFS image key (EKPFS) from record `0x20`.
pub(crate) fn recover_ekpfs(
    image: &mut [u8; IMAGE_KEY_LEN],
    entry: &Entry,
    dk3: &[u8; 32],
) -> Result<[u8; 32], Error> {
    let (iv, key) = split_material(&entry_material(entry, dk3));
    aes::cbc_decrypt_in_place(&key, &iv, image).map_err(|_| Error::Malformed("image key"))?;
    // The wrapped key is used as-is: the new-crypt seed transform is already applied.
    unwrap(Keyset::Fake, image)
}

/// The end of a record's CBC part; the 1..15 bytes after it are a CFB tail.
fn cbc_end(e: &Entry) -> u64 {
    if e.id == NPBIND_ID
        && e.size == NPBIND_LEN
        && e.flags1 == NPBIND_FLAGS1
        && e.flags2 == NPBIND_FLAGS2
    {
        NPBIND_STORED
    } else {
        u64::from(e.size) & !BLOCK_MASK
    }
}

const BLOCK_MASK: u64 = (AES_BLOCK as u64) - 1;

fn read_at<R: Read + Seek>(src: &mut R, at: u64, buf: &mut [u8]) -> Result<(), Error> {
    src.seek(SeekFrom::Start(at))?;
    src.read_exact(buf)?;
    Ok(())
}

/// Reads plaintext bytes `offset..offset + buf.len()` of record `e`.
///
/// Full blocks are AES-128-CBC; a final partial block is CFB over the previous
/// ciphertext block (or the IV), so stored length equals declared length.
pub(crate) fn read_record<R: Read + Seek>(
    src: &mut R,
    e: &Entry,
    dk3: &[u8; 32],
    mut offset: u64,
    mut buf: &mut [u8],
) -> Result<(), Error> {
    let len = u64::try_from(buf.len()).map_err(|_| Error::Malformed("read too large"))?;
    pantheon_core::Span::new(offset, len, u64::from(e.size))
        .map_err(|_| Error::Malformed("read outside container record"))?;
    let base = u64::from(e.offset);
    let abs = |o: u64| base.checked_add(o).ok_or(Error::Malformed("record offset"));
    if !e.encrypted() {
        return read_at(src, abs(offset)?, buf);
    }
    let (iv0, key) = split_material(&entry_material(e, dk3));
    let end = cbc_end(e);
    let mut chunk = vec![0u8; CHUNK];
    while !buf.is_empty() {
        let start = offset & !BLOCK_MASK;
        let skip = usize::try_from(offset.saturating_sub(start)).unwrap_or(0);
        let mut iv = iv0;
        if start != 0 {
            read_at(src, abs(start.saturating_sub(16))?, &mut iv)?;
        }
        let n = if start >= end {
            let mut stream = iv;
            aes::encrypt_block(&key, &mut stream);
            read_at(src, abs(offset)?, buf)?;
            let pad = stream.get(skip..).unwrap_or_default();
            if pad.len() < buf.len() {
                return Err(Error::Malformed("container record tail"));
            }
            buf.iter_mut().zip(pad).for_each(|(b, s)| *b ^= s);
            buf.len()
        } else {
            let want = buf
                .len()
                .saturating_add(skip)
                .next_multiple_of(AES_BLOCK)
                .min(CHUNK);
            let stored = want.min(usize::try_from(end.saturating_sub(start)).unwrap_or(CHUNK));
            let data = chunk.get_mut(..stored).unwrap_or_default();
            read_at(src, abs(start)?, data)?;
            aes::cbc_decrypt_in_place(&key, &iv, data)
                .map_err(|_| Error::Malformed("container record CBC"))?;
            let n = buf.len().min(stored.saturating_sub(skip));
            let plain = data
                .get(skip..skip.saturating_add(n))
                .ok_or(Error::Malformed("container record"))?;
            buf.get_mut(..n)
                .ok_or(Error::Malformed("container record"))?
                .copy_from_slice(plain);
            n
        };
        if n == 0 {
            return Err(Error::Malformed("container record"));
        }
        offset = offset.saturating_add(u64::try_from(n).unwrap_or(u64::MAX));
        buf = std::mem::take(&mut buf).get_mut(n..).unwrap_or_default();
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)] // test-only code
mod tests {
    use super::*;

    #[test]
    fn keysets_build() {
        assert!(private_key(Keyset::Dk3).is_ok());
        assert!(private_key(Keyset::Fake).is_ok());
    }
}
