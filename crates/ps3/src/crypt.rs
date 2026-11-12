//! The PS3 PKG data-region keystreams: AES-128-CTR (retail) and SHA-1 (debug).

use pantheon_crypto::{aes, hash::Sha1};

use crate::Header;

/// Normal PS3 package release key. Public: RPCS3 `Crypto/key_vault.h`
/// (pinned revision in pkg-tools `docs/ps3.md`).
pub const RETAIL_KEY: [u8; 16] = [
    0x2e, 0x7b, 0x71, 0xd7, 0xc9, 0xc9, 0xa1, 0x4e, 0xa3, 0x22, 0x1f, 0x18, 0x88, 0x28, 0xb8, 0xf8,
];

const BLOCK: u64 = 16;
const DEBUG_INPUT_LEN: usize = 64;
const DEBUG_COUNTER_AT: usize = 56;

/// XORs the keystream for data-region offset `offset` into `buf` (encrypts and
/// decrypts alike).
pub(crate) fn apply(header: &Header, offset: u64, buf: &mut [u8]) {
    if header.is_retail() {
        aes::ctr_apply(&RETAIL_KEY, header.nonce, offset, buf);
    } else {
        debug_apply(&header.qa_digest, offset, buf);
    }
}

/// Debug packages: block `n` of the keystream is the first 16 bytes of
/// SHA-1(qa[0..8] ‖ qa[0..8] ‖ qa[8..16] ‖ qa[8..16] ‖ zeros ‖ n as u64 BE).
fn debug_apply(qa: &[u8; 16], offset: u64, buf: &mut [u8]) {
    let mut input = [0u8; DEBUG_INPUT_LEN];
    let (low, high) = qa.split_at(8);
    for (i, part) in [low, low, high, high].into_iter().enumerate() {
        let at = i.saturating_mul(8);
        if let Some(dst) = input.get_mut(at..at.saturating_add(8)) {
            dst.copy_from_slice(part);
        }
    }
    let mut block = offset / BLOCK;
    // offset % 16 < 16, so it fits usize.
    let mut skip = usize::try_from(offset % BLOCK).unwrap_or(0);
    let mut rest = buf;
    while !rest.is_empty() {
        if let Some(dst) = input.get_mut(DEBUG_COUNTER_AT..) {
            dst.copy_from_slice(&block.to_be_bytes());
        }
        let digest = Sha1::digest(&input);
        let stream = digest.get(skip..16).unwrap_or_default();
        let take = rest.len().min(stream.len());
        let (head, tail) = rest.split_at_mut(take);
        for (byte, key) in head.iter_mut().zip(stream) {
            *byte ^= key;
        }
        rest = tail;
        skip = 0;
        block = block.wrapping_add(1);
    }
}
