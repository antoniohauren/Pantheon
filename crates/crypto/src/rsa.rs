//! RSA-2048: PKCS#1 v1.5 unwrap with a public (fake) private key, and raw public wrap.
//!
//! Only public fixed keys are used with this module, which is why RUSTSEC-2023-0071
//! (Marvin timing attack) is accepted (P17).

use crate::{Error, Result};
use ::rsa::traits::PublicKeyParts;
use ::rsa::{BigUint, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};

/// RSA-2048 modulus and block length in bytes.
pub const RSA_BYTES: usize = 256;
/// RSA-2048 modulus length in bits.
const RSA_BITS: usize = 2048;
/// Length of every key the PS4 formats wrap.
pub const WRAPPED_KEY_BYTES: usize = 32;

/// An RSA-2048 private key built from public primes (the PS4 fake keys).
pub struct PrivateKey(RsaPrivateKey);

impl PrivateKey {
    /// Builds the key from big-endian primes `p`, `q` and public exponent `e`.
    pub fn from_primes(p: &[u8], q: &[u8], e: u32) -> Result<PrivateKey> {
        let primes = vec![BigUint::from_bytes_be(p), BigUint::from_bytes_be(q)];
        let mut key = RsaPrivateKey::from_primes(primes, BigUint::from(e))
            .map_err(|_| Error::BadKey("RSA primes"))?;
        if key.n().bits() != RSA_BITS {
            return Err(Error::BadKey("RSA modulus is not 2048 bits"));
        }
        key.validate().map_err(|_| Error::BadKey("RSA key"))?;
        key.precompute()
            .map_err(|_| Error::BadKey("RSA CRT values"))?;
        Ok(PrivateKey(key))
    }

    /// The big-endian modulus, for [`public_wrap`] (synthetic fixture builders).
    pub fn modulus(&self) -> [u8; RSA_BYTES] {
        let n = self.0.n().to_bytes_be();
        let mut out = [0u8; RSA_BYTES];
        // `from_primes` checked the modulus is exactly 2048 bits, so it fills `out`.
        if let Some(tail) = RSA_BYTES
            .checked_sub(n.len())
            .and_then(|s| out.get_mut(s..))
        {
            tail.copy_from_slice(&n);
        }
        out
    }
}

/// PKCS#1 v1.5 decryption of a wrapped 32-byte key with a fake (public) private key.
///
/// Fails with [`Error::WrongKey`] on bad padding or a payload that is not 32 bytes.
pub fn fake_unwrap(
    key: &PrivateKey,
    ciphertext: &[u8; RSA_BYTES],
) -> Result<[u8; WRAPPED_KEY_BYTES]> {
    let plain = key
        .0
        .decrypt(Pkcs1v15Encrypt, ciphertext)
        .map_err(|_| Error::WrongKey)?;
    plain.try_into().map_err(|_| Error::WrongKey)
}

/// Raw RSA-2048 public operation `block^e mod n` (textbook, no padding).
///
/// The caller builds the padded `block` (the formats use deterministic padding).
/// `modulus` is big-endian with its top bit set; `block` must be below it.
pub fn public_wrap(
    modulus: &[u8; RSA_BYTES],
    exponent: u32,
    block: &[u8; RSA_BYTES],
) -> Result<[u8; RSA_BYTES]> {
    let n = BigUint::from_bytes_be(modulus);
    if n.bits() != RSA_BITS {
        return Err(Error::BadKey("RSA modulus is not 2048 bits"));
    }
    let key = RsaPublicKey::new(n, BigUint::from(exponent))
        .map_err(|_| Error::BadKey("RSA public key"))?;
    let m = BigUint::from_bytes_be(block);
    if &m >= key.n() {
        return Err(Error::Malformed("RSA block is not below the modulus"));
    }
    let c = m.modpow(key.e(), key.n()).to_bytes_be();
    let mut out = [0u8; RSA_BYTES];
    // `c < n < 2^2048`, so it fits; the start is checked anyway rather than trusted.
    let start = RSA_BYTES
        .checked_sub(c.len())
        .ok_or(Error::Malformed("RSA output exceeds the modulus width"))?;
    out[start..].copy_from_slice(&c);
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects, clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::test_util::{arr, hex};
    use pantheon_core::testing::Corruptor;

    // Test-only RSA-2048 key generated for this crate with `openssl genrsa 2048`
    // (never used by any format). The known ciphertext was computed independently with
    // Python: pow(block, 65537, n) for the block built by `type2_block` below.
    const P: &str = "f08dc51e0f4f40806d22c87f1e7593b3f6ab87f991bbbeb0819b4be3f86bb369757bf2a4723de4224610b64db6a1a7c33c54edeca161aa162fc9e52233e2491e33406f96837729ddd116d8e5166d214f1a9f671c485c66491631d809eb04401395894012417dfbf99a0e78e6dd7e4c76a5551baa2cf52f124e66d0404f8cbfa7";
    const Q: &str = "c2335050ee9285459747bedd657a1e95191679a5c450856d258424c573442305b749e02aec2b224cd598e62a491140fa4d88b867dadba77048f3d6c60f5646061bdd2b313a79e5ab9c86d10df9955cda656cdabf7acf894eb3b56a66e16f98a1d81f5e79108d9c73c488a33749673380d9b3924700a1c0bb2a4961194ff0a4d5";
    const N: &str = "b67ba71766ab9530225c964363ea638e44deda059dbc51f3579c197db49dd0b83940ac8f75808a5e2f4b1bb80495b1d4a67a0157a3ee4c0380665769084a5511873b7146817681d3f43e45e6ddb18010de97d37cf8f3ac0d5474283b57256c01d820ba7641b139d3beb8dbb04849ff7ea3101125e3cfc6d2c3f3d832d3e0b32070e09da3d5b56012b0b3837f556689122b3916e53eeb7bc289cac103ac902a17df5d71536aeb2bb38a0c55a55e4ca876921eb38129a0d44dea6717296ecd6079b760eb9c02ac3e3801c7aa09d58f0ec0ad773ef7d1cfeabcfba6509de756c9bdf55547e616540928b807d09d155bffa880a7623b016dd91953c779df907271f3";
    const C: &str = "12b4cb12f997abf1f44ffdee1be35d5c20d5fcc941e7e4d067d498f4fba0f5769a7579872a93e990db867a9ddd4ef9e8874039c8d1e6baa1e55ac3e534a2fb99c5035192dfe5601db44cd93dd8a6269f8741d6b320ee003f185991b388182813cb076303d55f441a851bc5a437ecdbe9f101a7312be6bdd036e0ee0d5c671ce184354392b1957395f00408fb973ee5059126ce286d65df91d618d4fbc72a3a4e67ba379272cac141a6f40310b26e605509d62c38d87f1ade7a9ab430928d280e417cde252002bc57039e5814f49cbd3d5a1eac7928035e0212d1c35376839a3b1dd21f081d266c88da6790102b20786d172875f3cab60aa2f29748ea8f529ff1";
    const E: u32 = 65537;

    fn message() -> [u8; 32] {
        std::array::from_fn(|i| 0x20 + i as u8)
    }

    /// PKCS#1 v1.5 type-2 block with fixed non-zero padding.
    fn type2_block() -> [u8; RSA_BYTES] {
        let mut block = [0u8; RSA_BYTES];
        block[1] = 2;
        for (i, b) in block[2..RSA_BYTES - 33].iter_mut().enumerate() {
            *b = ((i * 37 + 11) % 255) as u8 + 1;
        }
        block[RSA_BYTES - 32..].copy_from_slice(&message());
        block
    }

    fn key() -> PrivateKey {
        PrivateKey::from_primes(&hex(P), &hex(Q), E).unwrap()
    }

    #[test]
    fn unwrap_known_ciphertext() {
        assert_eq!(fake_unwrap(&key(), &arr(C)).unwrap(), message());
    }

    #[test]
    fn public_wrap_known_ciphertext_and_roundtrip() {
        assert_eq!(key().modulus(), arr::<RSA_BYTES>(N));
        let c = public_wrap(&arr(N), E, &type2_block()).unwrap();
        assert_eq!(c, arr::<RSA_BYTES>(C));
        assert_eq!(fake_unwrap(&key(), &c).unwrap(), message());
    }

    #[test]
    fn rejects_bad_keys_and_blocks() {
        assert!(PrivateKey::from_primes(&hex(P), &hex(P), E).is_err());
        assert!(PrivateKey::from_primes(&[0x0b], &[0x0d], E).is_err());
        assert!(public_wrap(&[0; RSA_BYTES], E, &[0; RSA_BYTES]).is_err());
        assert!(public_wrap(&arr(N), E, &[0xff; RSA_BYTES]).is_err());
        assert!(public_wrap(&arr(N), 1, &type2_block()).is_err());
    }

    #[test]
    fn unwrap_wrong_length_payload_is_wrong_key() {
        let mut block = type2_block();
        block[RSA_BYTES - 33] = 1; // the separator moves: payload is 31 bytes
        block[RSA_BYTES - 32] = 0;
        let c = public_wrap(&arr(N), E, &block).unwrap();
        assert_eq!(fake_unwrap(&key(), &c), Err(Error::WrongKey));
    }

    #[test]
    fn corrupted_ciphertexts_never_panic() {
        let key = key();
        Corruptor::new(0xc0ffee).run(&hex(C), 64, |input| {
            if let Ok(c) = <[u8; RSA_BYTES]>::try_from(input) {
                let _ = fake_unwrap(&key, &c);
            }
        });
    }
}
