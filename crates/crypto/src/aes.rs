//! AES: single blocks, CBC, CTR, CMAC and XTS.
//!
//! CTR and CMAC are small helpers over the `aes` block cipher (docs/02-dependencies.md §2):
//! a mode of operation is not a new primitive.

use crate::{Error, Result};
use ::aes::cipher::array::Array;
use ::aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, BlockModeDecrypt, BlockModeEncrypt};
use ::aes::cipher::{KeyInit, KeyIvInit};
use ::aes::{Aes128, Aes256};

/// AES block size in bytes.
pub const BLOCK: usize = 16;
/// `log2(BLOCK)`, to turn a byte offset into a block index.
const BLOCK_SHIFT: u32 = 4;
/// `BLOCK - 1`, to take a byte offset within its block.
const BLOCK_MASK: u64 = 0xf;
/// Smallest XTS sector: one whole block.
const MIN_SECTOR: usize = BLOCK;
/// CMAC subkey reduction constant for a 128-bit block (RFC 4493 §2.3, `const_Rb`).
const CMAC_RB: u128 = 0x87;
/// CMAC padding marker after the last message byte (RFC 4493 §2.4).
const CMAC_PAD: u8 = 0x80;

/// An AES key accepted by the CBC helpers: `[u8; 16]` (AES-128) or `[u8; 32]` (AES-256).
pub trait AesKey: sealed::Sealed {}
impl AesKey for [u8; 16] {}
impl AesKey for [u8; 32] {}

mod sealed {
    pub trait Sealed {
        fn cbc_decrypt(&self, iv: &[u8; 16], buf: &mut [u8]);
        fn cbc_encrypt(&self, iv: &[u8; 16], buf: &mut [u8]);
    }
}

/// Splits a block-aligned buffer into AES blocks (the caller checked the alignment).
fn blocks(buf: &mut [u8]) -> &mut [Array<u8, ::aes::cipher::consts::U16>] {
    Array::slice_as_chunks_mut(buf).0
}

macro_rules! cbc_key {
    ($len:literal, $cipher:ty) => {
        impl sealed::Sealed for [u8; $len] {
            fn cbc_decrypt(&self, iv: &[u8; 16], buf: &mut [u8]) {
                cbc::Decryptor::<$cipher>::new(self.into(), iv.into()).decrypt_blocks(blocks(buf));
            }
            fn cbc_encrypt(&self, iv: &[u8; 16], buf: &mut [u8]) {
                cbc::Encryptor::<$cipher>::new(self.into(), iv.into()).encrypt_blocks(blocks(buf));
            }
        }
    };
}
cbc_key!(16, Aes128);
cbc_key!(32, Aes256);

fn check_aligned(buf: &[u8]) -> Result<()> {
    if buf.len().is_multiple_of(BLOCK) {
        Ok(())
    } else {
        Err(Error::Unaligned {
            len: buf.len(),
            unit: BLOCK,
        })
    }
}

/// AES-CBC decryption in place, no padding. `buf` must hold whole blocks.
pub fn cbc_decrypt_in_place<K: AesKey>(key: &K, iv: &[u8; 16], buf: &mut [u8]) -> Result<()> {
    check_aligned(buf)?;
    key.cbc_decrypt(iv, buf);
    Ok(())
}

/// AES-CBC encryption in place, no padding. `buf` must hold whole blocks.
pub fn cbc_encrypt_in_place<K: AesKey>(key: &K, iv: &[u8; 16], buf: &mut [u8]) -> Result<()> {
    check_aligned(buf)?;
    key.cbc_encrypt(iv, buf);
    Ok(())
}

/// Encrypts one block with AES-128 (ECB on a single block).
pub fn encrypt_block(key: &[u8; 16], block: &mut [u8; 16]) {
    Aes128::new(key.into()).encrypt_block(block.into());
}

/// Decrypts one block with AES-128 (ECB on a single block).
pub fn decrypt_block(key: &[u8; 16], block: &mut [u8; 16]) {
    Aes128::new(key.into()).decrypt_block(block.into());
}

/// AES-128-CTR keystream XOR, in place (encryption and decryption are the same).
///
/// `iv` is the big-endian 128-bit counter of stream byte 0; `offset` is the stream byte
/// position of `buf[0]` and need not be block-aligned. The counter wraps modulo 2^128.
pub fn ctr_apply(key: &[u8; 16], iv: [u8; 16], offset: u64, buf: &mut [u8]) {
    let cipher = Aes128::new(key.into());
    let mut counter = u128::from_be_bytes(iv).wrapping_add(u128::from(offset >> BLOCK_SHIFT));
    // Masked to 0..BLOCK, so the conversion cannot fail.
    let mut skip = usize::try_from(offset & BLOCK_MASK).unwrap_or(0);
    let mut rest = buf;
    while !rest.is_empty() {
        let mut stream = counter.to_be_bytes();
        cipher.encrypt_block((&mut stream).into());
        let keystream = stream.get(skip..).unwrap_or_default();
        let take = rest.len().min(keystream.len());
        let (head, tail) = rest.split_at_mut(take);
        for (byte, key_byte) in head.iter_mut().zip(keystream) {
            *byte ^= key_byte;
        }
        rest = tail;
        skip = 0;
        counter = counter.wrapping_add(1);
    }
}

/// AES-128-CMAC (RFC 4493, NIST SP 800-38B) of `data`.
pub fn cmac(key: &[u8; 16], data: &[u8]) -> [u8; 16] {
    let cipher = Aes128::new(key.into());
    let encrypt = |mut block: [u8; 16]| {
        cipher.encrypt_block((&mut block).into());
        block
    };
    let double = |value: u128| (value << 1) ^ if value >> 127 == 1 { CMAC_RB } else { 0 };
    let k1 = double(u128::from_be_bytes(encrypt([0; BLOCK])));
    let k2 = double(k1);

    // The last block (possibly empty or partial) gets the subkey; all others are plain CBC-MAC.
    let (mut body, mut last) = data.as_chunks::<BLOCK>();
    if last.is_empty()
        && let Some((full, init)) = body.split_last()
    {
        (body, last) = (init, full.as_slice());
    }
    let mut state = [0u8; BLOCK];
    for block in body {
        state = encrypt(xor(state, block));
    }
    let mut padded = [0u8; BLOCK];
    padded[..last.len()].copy_from_slice(last);
    let subkey = if last.len() == BLOCK {
        k1
    } else {
        padded[last.len()] = CMAC_PAD;
        k2
    };
    let tail = u128::from_be_bytes(padded) ^ subkey;
    encrypt(xor(state, &tail.to_be_bytes()))
}

fn xor(mut state: [u8; 16], block: &[u8]) -> [u8; 16] {
    for (s, b) in state.iter_mut().zip(block) {
        *s ^= b;
    }
    state
}

/// AES-128-XTS (IEEE 1619) with the default little-endian sector-number tweak, as PFS uses.
pub struct Xts128(xts_mode::Xts128<Aes128>);

impl Xts128 {
    /// `data_key` encrypts the blocks, `tweak_key` encrypts the sector tweak.
    pub fn new(data_key: &[u8; 16], tweak_key: &[u8; 16]) -> Xts128 {
        Xts128(xts_mode::Xts128::new(
            Aes128::new(data_key.into()),
            Aes128::new(tweak_key.into()),
        ))
    }

    /// Encrypts whole sectors in place; `buf[0]` starts sector `first_sector`.
    pub fn encrypt_sectors(
        &self,
        buf: &mut [u8],
        sector_size: usize,
        first_sector: u128,
    ) -> Result<()> {
        check_sectors(buf, sector_size)?;
        self.0
            .encrypt_area(buf, sector_size, first_sector, xts_mode::get_tweak_default);
        Ok(())
    }

    /// Decrypts whole sectors in place; `buf[0]` starts sector `first_sector`.
    pub fn decrypt_sectors(
        &self,
        buf: &mut [u8],
        sector_size: usize,
        first_sector: u128,
    ) -> Result<()> {
        check_sectors(buf, sector_size)?;
        self.0
            .decrypt_area(buf, sector_size, first_sector, xts_mode::get_tweak_default);
        Ok(())
    }
}

/// `xts-mode` panics on a sector shorter than one block; require whole sectors of at least one.
fn check_sectors(buf: &[u8], sector_size: usize) -> Result<()> {
    if sector_size < MIN_SECTOR {
        return Err(Error::BadKey("XTS sector size below one block"));
    }
    if buf.len().is_multiple_of(sector_size) {
        Ok(())
    } else {
        Err(Error::Unaligned {
            len: buf.len(),
            unit: sector_size,
        })
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::test_util::{arr, hex};

    // NIST SP 800-38A Appendix F: the shared plaintext and the AES-128 key.
    const SP800_38A_PLAIN: &str = "6bc1bee22e409f96e93d7e117393172a ae2d8a571e03ac9c9eb76fac45af8e51
                                   30c81c46a35ce411e5fbc1191a0a52ef f69f2445df4f9b17ad2b417be66c3710";
    const KEY128: &str = "2b7e151628aed2a6abf7158809cf4f3c";

    #[test]
    fn cbc_aes128_sp800_38a_f2_1_and_f2_2() {
        let iv = arr("000102030405060708090a0b0c0d0e0f");
        let cipher = hex(
            "7649abac8119b246cee98e9b12e9197d 5086cb9b507219ee95db113a917678b2
                          73bed6b8e3c1743b7116e69e22229516 3ff1caa1681fac09120eca307586e1a7",
        );
        let mut buf = hex(SP800_38A_PLAIN);
        cbc_encrypt_in_place(&arr::<16>(KEY128), &iv, &mut buf).unwrap();
        assert_eq!(buf, cipher);
        cbc_decrypt_in_place(&arr::<16>(KEY128), &iv, &mut buf).unwrap();
        assert_eq!(buf, hex(SP800_38A_PLAIN));
    }

    #[test]
    fn cbc_aes256_sp800_38a_f2_5_and_f2_6() {
        let key: [u8; 32] = arr("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4");
        let iv = arr("000102030405060708090a0b0c0d0e0f");
        let cipher = hex(
            "f58c4c04d6e5f1ba779eabfb5f7bfbd6 9cfc4e967edb808d679f777bc6702c7d
                          39f23369a9d9bacfa530e26304231461 b2eb05e2c39be9fcda6c19078c6a9d1b",
        );
        let mut buf = hex(SP800_38A_PLAIN);
        cbc_encrypt_in_place(&key, &iv, &mut buf).unwrap();
        assert_eq!(buf, cipher);
        cbc_decrypt_in_place(&key, &iv, &mut buf).unwrap();
        assert_eq!(buf, hex(SP800_38A_PLAIN));
    }

    #[test]
    fn cbc_rejects_partial_blocks() {
        let mut buf = [0u8; 17];
        let err = cbc_decrypt_in_place(&[0u8; 16], &[0; 16], &mut buf).unwrap_err();
        assert_eq!(err, Error::Unaligned { len: 17, unit: 16 });
        assert!(cbc_encrypt_in_place(&[0u8; 32], &[0; 16], &mut buf).is_err());
    }

    #[test]
    fn block_fips197_appendix_c1() {
        // FIPS 197 Appendix C.1 (AES-128).
        let key = arr("000102030405060708090a0b0c0d0e0f");
        let mut block = arr("00112233445566778899aabbccddeeff");
        encrypt_block(&key, &mut block);
        assert_eq!(block, arr::<16>("69c4e0d86a7b0430d8cdb78070b4c55a"));
        decrypt_block(&key, &mut block);
        assert_eq!(block, arr::<16>("00112233445566778899aabbccddeeff"));
    }

    // NIST SP 800-38A F.5.1 (CTR-AES128.Encrypt).
    const CTR_IV: &str = "f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff";
    const CTR_CIPHER: &str = "874d6191b620e3261bef6864990db6ce 9806f66b7970fdff8617187bb9fffdff
                              5ae4df3edbd5d35e5b4f09020db03eab 1e031dda2fbe03d1792170a0f3009cee";

    #[test]
    fn ctr_sp800_38a_f5_1() {
        let mut buf = hex(SP800_38A_PLAIN);
        ctr_apply(&arr(KEY128), arr(CTR_IV), 0, &mut buf);
        assert_eq!(buf, hex(CTR_CIPHER));
        ctr_apply(&arr(KEY128), arr(CTR_IV), 0, &mut buf);
        assert_eq!(buf, hex(SP800_38A_PLAIN));
    }

    #[test]
    fn ctr_unaligned_offsets_match_the_whole_stream() {
        let plain = hex(SP800_38A_PLAIN);
        let cipher = hex(CTR_CIPHER);
        for start in 0..plain.len() {
            for end in start..=plain.len() {
                let mut part = plain[start..end].to_vec();
                ctr_apply(&arr(KEY128), arr(CTR_IV), start as u64, &mut part);
                assert_eq!(part, cipher[start..end], "{start}..{end}");
            }
        }
    }

    #[test]
    fn ctr_counter_wraps() {
        // Counter 2^128 - 1 then 0: the second block must equal a stream starting at IV 0.
        let mut wrapped = [0u8; 32];
        ctr_apply(&[7; 16], [0xff; 16], 0, &mut wrapped);
        let mut zero = [0u8; 16];
        ctr_apply(&[7; 16], [0; 16], 0, &mut zero);
        assert_eq!(wrapped[16..], zero);
    }

    #[test]
    fn cmac_rfc4493_examples() {
        // RFC 4493 §4, Examples 1-4 (key = SP 800-38A AES-128 key).
        let msg = hex(SP800_38A_PLAIN);
        let key = arr(KEY128);
        for (len, tag) in [
            (0, "bb1d6929e95937287fa37d129b756746"),
            (16, "070a16b46b4d4144f79bdd9dd04a287c"),
            (40, "dfa66747de9ae63030ca32611497c827"),
            (64, "51f0bebf7e3b9d92fc49741779363cfe"),
        ] {
            assert_eq!(cmac(&key, &msg[..len]), arr::<16>(tag), "length {len}");
        }
    }

    #[test]
    fn xts_ieee1619_vector_4() {
        // IEEE 1619-2007 Annex B, XTS-AES-128 Vector 4: data unit 0, 512-byte sector,
        // plaintext bytes 00..ff twice, key1 (data) = 2718..., key2 (tweak) = 3141...
        let xts = Xts128::new(
            &arr("27182818284590452353602874713526"),
            &arr("31415926535897932384626433832795"),
        );
        let plain: Vec<u8> = (0..512).map(|i| i as u8).collect();
        let mut buf = plain.clone();
        xts.encrypt_sectors(&mut buf, 512, 0).unwrap();
        // First 96 and last 32 ciphertext bytes (cross-checked against Python `cryptography`).
        let head = "27a7479befa1d476489f308cd4cfa6e2a96e4bbe3208ff25287dd3819616e89c
                    c78cf7f5e543445f8333d8fa7f56000005279fa5d8b5e4ad40e736ddb4d35412
                    328063fd2aab53e5ea1e0a9f332500a5df9487d07a5c92cc512c8866c7e860ce";
        let tail = "eb4a427d1923ce3ff262735779a418f20a282df920147beabe421ee5319d0568";
        assert_eq!(buf[..96], hex(head));
        assert_eq!(buf[480..], hex(tail));
        xts.decrypt_sectors(&mut buf, 512, 0).unwrap();
        assert_eq!(buf, plain);
    }

    #[test]
    fn xts_sector_index_advances() {
        // Two sectors from index 5 equal sector 5 and sector 6 encrypted separately.
        let xts = Xts128::new(&[1; 16], &[2; 16]);
        let mut both = [0x5a; 64];
        xts.encrypt_sectors(&mut both, 32, 5).unwrap();
        let mut second = [0x5a; 32];
        xts.encrypt_sectors(&mut second, 32, 6).unwrap();
        assert_eq!(both[32..], second);
    }

    #[test]
    fn xts_rejects_bad_geometry() {
        let xts = Xts128::new(&[1; 16], &[2; 16]);
        assert!(xts.encrypt_sectors(&mut [0; 32], 8, 0).is_err());
        assert!(xts.decrypt_sectors(&mut [0; 40], 32, 0).is_err());
        assert!(xts.decrypt_sectors(&mut [0; 32], 0, 0).is_err());
    }
}
