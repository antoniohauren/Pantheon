//! SHA-1, SHA-256 (streaming and one-shot) and HMAC over them.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Digest;

/// SHA-1 digest length.
pub const SHA1_LEN: usize = 20;
/// SHA-256 digest length.
pub const SHA256_LEN: usize = 32;

macro_rules! streaming_hash {
    ($name:ident, $inner:ty, $len:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Default)]
        pub struct $name($inner);

        impl $name {
            /// A fresh hasher.
            pub fn new() -> $name {
                $name::default()
            }

            /// Feeds more input.
            pub fn update(&mut self, data: &[u8]) {
                self.0.update(data);
            }

            /// The digest of everything fed so far.
            pub fn finalize(self) -> [u8; $len] {
                self.0.finalize().into()
            }

            /// One-shot digest of `data`.
            pub fn digest(data: &[u8]) -> [u8; $len] {
                <$inner>::digest(data).into()
            }
        }
    };
}

streaming_hash!(Sha1, sha1::Sha1, SHA1_LEN, "Streaming SHA-1 (FIPS 180-4).");
streaming_hash!(
    Sha256,
    sha2::Sha256,
    SHA256_LEN,
    "Streaming SHA-256 (FIPS 180-4)."
);

/// HMAC-SHA1 (RFC 2104) of `data` under `key` (any length).
pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; SHA1_LEN] {
    // HMAC accepts keys of any length (long keys are hashed), so this cannot fail.
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// HMAC-SHA256 (RFC 2104) of `data` under `key` (any length).
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; SHA256_LEN] {
    // HMAC accepts keys of any length (long keys are hashed), so this cannot fail.
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{arr, hex};

    const MILLION_A: usize = 1_000_000;

    #[test]
    fn sha1_fips180_examples() {
        // FIPS 180-2 Appendix A ("abc", two-block message, one million 'a').
        assert_eq!(
            Sha1::digest(b"abc"),
            arr::<20>("a9993e364706816aba3e25717850c26c9cd0d89d")
        );
        let two = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(
            Sha1::digest(two),
            arr::<20>("84983e441c3bd26ebaae4aa1f95129e5e54670f1")
        );
        let mut h = Sha1::new();
        for _ in 0..MILLION_A / 1000 {
            h.update(&[b'a'; 1000]);
        }
        assert_eq!(
            h.finalize(),
            arr::<20>("34aa973cd4c4daa4f61eeb2bdbad27316534016f")
        );
    }

    #[test]
    fn sha256_fips180_examples() {
        // FIPS 180-2 Appendix B; the empty-string digest is the well-known value.
        assert_eq!(
            Sha256::digest(b"abc"),
            arr::<32>("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(
            Sha256::digest(b""),
            arr::<32>("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        let mut h = Sha256::new();
        h.update(b"abcdbcdecdefdefgefghfghighijhijk");
        h.update(b"ijkljklmklmnlmnomnopnopq");
        assert_eq!(
            h.finalize(),
            arr::<32>("248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1")
        );
    }

    #[test]
    fn hmac_sha1_rfc2202() {
        // RFC 2202 §3, test cases 1, 2 and 6 (key longer than the block).
        assert_eq!(
            hmac_sha1(&[0x0b; 20], b"Hi There"),
            arr::<20>("b617318655057264e28bc0b6fb378c8ef146be00")
        );
        assert_eq!(
            hmac_sha1(b"Jefe", b"what do ya want for nothing?"),
            arr::<20>("effcdf6ae5eb2fa2d27416d5f184df9c259a7c79")
        );
        assert_eq!(
            hmac_sha1(
                &[0xaa; 80],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            arr::<20>("aa4ae5e15272d00e95705637ce8a3b55ed402112")
        );
    }

    #[test]
    fn hmac_sha256_rfc4231() {
        // RFC 4231 §4.2, §4.3 and §4.7 (test cases 1, 2 and 6).
        assert_eq!(
            hmac_sha256(&[0x0b; 20], b"Hi There"),
            arr::<32>("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7")
        );
        assert_eq!(
            hmac_sha256(b"Jefe", b"what do ya want for nothing?"),
            arr::<32>("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
        );
        assert_eq!(
            hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            hex("60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54")[..]
        );
    }
}
