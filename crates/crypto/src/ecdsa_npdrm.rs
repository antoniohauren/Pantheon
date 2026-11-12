//! ECDSA over the 160-bit short-Weierstrass curves NPDRM uses, on `num-bigint-dig`.
//!
//! Curve parameters and keys are passed in by callers. Field and scalar inverses use
//! Fermat's little theorem, so `p` and `n` must be prime (callers pass published curves).
//! Signing keys used with this module are public, so constant time is not a goal.

// BigUint arithmetic cannot overflow; every subtraction below is written `x + m - y`
// with `x, y < m`, so it cannot underflow either.
#![allow(clippy::arithmetic_side_effects)]

use crate::hash::{SHA1_LEN, Sha1};
use crate::{Error, Result};
use num_bigint_dig::BigUint;

/// Field element length (160-bit curves).
pub const FIELD_BYTES: usize = 20;
/// Scalar length: the order `n`, private keys and the `r`/`s` halves are 21 bytes in NPDRM.
pub const SCALAR_BYTES: usize = 21;
/// Nonce retries before giving up (an `r` or `s` of zero has probability ~2^-160).
const MAX_NONCE_ATTEMPTS: u32 = 64;

/// Published curve parameters, big-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurveParams {
    pub p: [u8; FIELD_BYTES],
    pub a: [u8; FIELD_BYTES],
    pub b: [u8; FIELD_BYTES],
    pub n: [u8; SCALAR_BYTES],
    pub gx: [u8; FIELD_BYTES],
    pub gy: [u8; FIELD_BYTES],
}

/// A public key point, big-endian coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PublicKey {
    pub x: [u8; FIELD_BYTES],
    pub y: [u8; FIELD_BYTES],
}

/// An NPDRM ECDSA signature, big-endian `r` and `s`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature {
    pub r: [u8; SCALAR_BYTES],
    pub s: [u8; SCALAR_BYTES],
}

/// `None` is the point at infinity.
type Point = Option<(BigUint, BigUint)>;

/// A validated curve, ready for signing and verification.
pub struct Curve {
    p: BigUint,
    a: BigUint,
    b: BigUint,
    n: BigUint,
    g: (BigUint, BigUint),
}

fn uint(bytes: &[u8]) -> BigUint {
    BigUint::from_bytes_be(bytes)
}

fn small(value: u8) -> BigUint {
    BigUint::from(value)
}

impl Curve {
    /// Checks the parameters (odd `p, n > 3`, `a, b < p`, `G` on the curve).
    pub fn new(params: &CurveParams) -> Result<Curve> {
        let curve = Curve {
            p: uint(&params.p),
            a: uint(&params.a),
            b: uint(&params.b),
            n: uint(&params.n),
            g: (uint(&params.gx), uint(&params.gy)),
        };
        let odd_above_3 = |v: &BigUint| v > &small(3) && v % small(2) == small(1);
        if !odd_above_3(&curve.p) || !odd_above_3(&curve.n) {
            return Err(Error::BadKey("curve p and n must be odd primes"));
        }
        if curve.a >= curve.p || curve.b >= curve.p || !curve.on_curve(&curve.g) {
            return Err(Error::BadKey("curve generator or coefficients"));
        }
        Ok(curve)
    }

    fn on_curve(&self, (x, y): &(BigUint, BigUint)) -> bool {
        let p = &self.p;
        x < p && y < p && (y * y) % p == (x * x * x + &self.a * x + &self.b) % p
    }

    fn inverse(value: &BigUint, modulus: &BigUint) -> BigUint {
        value.modpow(&(modulus - small(2)), modulus)
    }

    fn add(&self, left: &Point, right: &Point) -> Point {
        let (Some((x1, y1)), Some((x2, y2))) = (left, right) else {
            return left.clone().or_else(|| right.clone());
        };
        let p = &self.p;
        let slope = if x1 == x2 {
            if (y1 + y2) % p == small(0) {
                return None;
            }
            (small(3) * x1 * x1 + &self.a) * Curve::inverse(&(small(2) * y1 % p), p) % p
        } else {
            (y2 + p - y1) * Curve::inverse(&((x2 + p - x1) % p), p) % p
        };
        let x3 = (&slope * &slope + p + p - x1 - x2) % p;
        let y3 = (&slope * ((x1 + p - &x3) % p) + p - y1) % p;
        Some((x3, y3))
    }

    fn multiply(&self, scalar: &BigUint, point: &Point) -> Point {
        // Double-and-add from the most significant bit.
        let mut out = None;
        for byte in scalar.to_bytes_be() {
            for shift in (0..u8::BITS).rev() {
                out = self.add(&out, &out);
                if (byte >> shift) & 1 == 1 {
                    out = self.add(&out, point);
                }
            }
        }
        out
    }
}

/// Fixed-width big-endian encoding of a value known to be below `n`.
fn scalar_bytes(value: &BigUint) -> Result<[u8; SCALAR_BYTES]> {
    let bytes = value.to_bytes_be();
    let mut out = [0u8; SCALAR_BYTES];
    let start = SCALAR_BYTES
        .checked_sub(bytes.len())
        .ok_or(Error::BadKey("scalar wider than 21 bytes"))?;
    out[start..].copy_from_slice(&bytes);
    Ok(out)
}

/// Signs a SHA-1 `digest` with private key `d` and a deterministic nonce.
///
/// The nonce for attempt `i` is `SHA-1(nonce_seed || digest || i as u32 BE) mod (n-1) + 1`;
/// pass the format's seed to reproduce its reference output byte for byte.
pub fn sign(
    curve: &Curve,
    d: &[u8; SCALAR_BYTES],
    digest: &[u8; SHA1_LEN],
    nonce_seed: &[u8],
) -> Result<Signature> {
    let d = uint(d);
    if d == small(0) || d >= curve.n {
        return Err(Error::BadKey("private key outside 1..n"));
    }
    for attempt in 0..MAX_NONCE_ATTEMPTS {
        let mut h = Sha1::new();
        h.update(nonce_seed);
        h.update(digest);
        h.update(&attempt.to_be_bytes());
        let k = uint(&h.finalize()) % (&curve.n - small(1)) + small(1);
        if let Some(signature) = sign_with_nonce(curve, &d, digest, &k)? {
            return Ok(signature);
        }
    }
    Err(Error::BadKey("no usable nonce"))
}

/// One ECDSA signing attempt with nonce `k` in `1..n`; `None` when `r` or `s` is zero.
fn sign_with_nonce(
    curve: &Curve,
    d: &BigUint,
    digest: &[u8; SHA1_LEN],
    k: &BigUint,
) -> Result<Option<Signature>> {
    let n = &curve.n;
    let Some((x, _)) = curve.multiply(k, &Some(curve.g.clone())) else {
        return Ok(None);
    };
    let r = x % n;
    let s = Curve::inverse(k, n) * ((uint(digest) + &r * d) % n) % n;
    if r == small(0) || s == small(0) {
        return Ok(None);
    }
    Ok(Some(Signature {
        r: scalar_bytes(&r)?,
        s: scalar_bytes(&s)?,
    }))
}

/// Verifies `signature` over SHA-1 `digest` against `public`. Hostile input yields `false`.
pub fn verify(
    curve: &Curve,
    public: &PublicKey,
    digest: &[u8; SHA1_LEN],
    signature: &Signature,
) -> bool {
    let n = &curve.n;
    let (r, s) = (uint(&signature.r), uint(&signature.s));
    let in_range = |v: &BigUint| v != &small(0) && v < n;
    let q = (uint(&public.x), uint(&public.y));
    if !in_range(&r) || !in_range(&s) || !curve.on_curve(&q) {
        return false;
    }
    let w = Curve::inverse(&s, n);
    let u1 = uint(digest) * &w % n;
    let u2 = &r * &w % n;
    let sum = curve.add(
        &curve.multiply(&u1, &Some(curve.g.clone())),
        &curve.multiply(&u2, &Some(q)),
    );
    sum.is_some_and(|(x, _)| x % n == r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::arr;
    use pantheon_core::testing::Corruptor;

    /// secp160r1 (SEC 2 v1.0 §2.4.2).
    fn secp160r1() -> Curve {
        Curve::new(&CurveParams {
            p: arr("ffffffffffffffffffffffffffffffff7fffffff"),
            a: arr("ffffffffffffffffffffffffffffffff7ffffffc"),
            b: arr("1c97befc54bd7a8b65acf89f81d4d4adc565fa45"),
            n: arr("0100000000000000000001f4c8f927aed3ca752257"),
            gx: arr("4a96b5688ef573284664698968c38bb913cbfc82"),
            gy: arr("23a628553168947d59dcc912042351377ac5fb32"),
        })
        .unwrap()
    }

    // SEC "GEC 2: Test Vectors for SEC 1" v0.3 §2.1.2-2.1.3: ECDSA over secp160r1,
    // message "abc", SHA-1. Decimal values from GEC 2 converted to hex; cross-checked
    // with an independent Python implementation.
    const D: &str = "00aa374ffc3ce144e6b073307972cb6d57b2a4e982";
    const QX: &str = "51b4496fecc406ed0e75a24a3c03206251419dc0";
    const QY: &str = "c28dcb4b73a514b468d793894f381ccc1756aa6c";
    const K: &str = "7b012db7681a3f28b9185c8b2ac5d528decd52da";
    const R: &str = "00ce2873e5be449563391feb47ddcba2dc16379191";
    const S: &str = "003480ec1371a091a464b31ce47df0cb8aa2d98b54";

    fn public() -> PublicKey {
        PublicKey {
            x: arr(QX),
            y: arr(QY),
        }
    }

    #[test]
    fn gec2_public_key_and_signature() {
        let curve = secp160r1();
        let d = uint(&arr::<21>(D));
        let q = curve.multiply(&d, &Some(curve.g.clone())).unwrap();
        assert_eq!((q.0, q.1), (uint(&public().x), uint(&public().y)));
        let digest = Sha1::digest(b"abc");
        let sig = sign_with_nonce(&curve, &d, &digest, &uint(&arr::<20>(K)))
            .unwrap()
            .unwrap();
        assert_eq!(
            sig,
            Signature {
                r: arr(R),
                s: arr(S)
            }
        );
        assert!(verify(&curve, &public(), &digest, &sig));
    }

    #[test]
    fn sign_verify_roundtrip_and_tamper() {
        let curve = secp160r1();
        let digest = Sha1::digest(b"SELF header");
        let sig = sign(&curve, &arr(D), &digest, b"test nonce seed").unwrap();
        assert_eq!(
            sig,
            sign(&curve, &arr(D), &digest, b"test nonce seed").unwrap()
        );
        assert!(verify(&curve, &public(), &digest, &sig));
        assert!(!verify(&curve, &public(), &Sha1::digest(b"other"), &sig));
        let mut bad = sig;
        bad.s[20] ^= 1;
        assert!(!verify(&curve, &public(), &digest, &bad));
    }

    #[test]
    fn rejects_bad_parameters() {
        let curve = secp160r1();
        assert!(sign(&curve, &[0; 21], &[0; 20], b"").is_err());
        assert!(sign(&curve, &[0xff; 21], &[0; 20], b"").is_err());
        let mut params = CurveParams {
            p: [0; 20],
            a: [0; 20],
            b: [0; 20],
            n: [0; 21],
            gx: [0; 20],
            gy: [0; 20],
        };
        assert!(Curve::new(&params).is_err());
        params.p = arr("ffffffffffffffffffffffffffffffff7fffffff");
        params.n = arr("0100000000000000000001f4c8f927aed3ca752257");
        params.gx[19] = 1; // (1, 0) is not on y^2 = x^3
        assert!(Curve::new(&params).is_err());
    }

    #[test]
    fn corrupted_signatures_and_keys_never_panic() {
        let curve = secp160r1();
        let digest = Sha1::digest(b"abc");
        let mut valid = [0u8; 82];
        valid[..21].copy_from_slice(&arr::<21>(R));
        valid[21..42].copy_from_slice(&arr::<21>(S));
        valid[42..62].copy_from_slice(&arr::<20>(QX));
        valid[62..].copy_from_slice(&arr::<20>(QY));
        Corruptor::new(0x5eed).run(&valid, 64, |input| {
            if input.len() != valid.len() {
                return;
            }
            let sig = Signature {
                r: arr_from(&input[..21]),
                s: arr_from(&input[21..42]),
            };
            let key = PublicKey {
                x: arr_from(&input[42..62]),
                y: arr_from(&input[62..]),
            };
            let ok = verify(&curve, &key, &digest, &sig);
            assert_eq!(ok, input == valid);
        });
    }

    fn arr_from<const N: usize>(bytes: &[u8]) -> [u8; N] {
        bytes.try_into().unwrap()
    }
}
