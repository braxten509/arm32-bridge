//! Security's keys: RSA public keys as the keychain hands them out
//! (kSecReturnRef on a kSecClassKey item holding the key's bytes) and
//! SecKeyRawVerify on them (PKCS#1 v1.5 signatures).

use crate::fw::object::define_class;
use crate::objc::runtime::{self, Id};
use crate::symbols::Registry;
use std::sync::OnceLock;

pub const SUCCESS: i32 = 0;
/// errSecParam
pub const PARAM_ERROR: i32 = -50;
/// errSSLCrypto: Apple's answer for a signature that doesn't check out.
pub const VERIFY_FAILED: i32 = -9809;

// SecPadding
const PADDING_NONE: u32 = 0;
const PADDING_PKCS1: u32 = 1;
const PADDING_PKCS1_MD2: u32 = 0x8000;
const PADDING_PKCS1_MD5: u32 = 0x8001;
const PADDING_PKCS1_SHA1: u32 = 0x8002;
const PADDING_PKCS1_SHA224: u32 = 0x8003;
const PADDING_PKCS1_SHA256: u32 = 0x8004;
const PADDING_PKCS1_SHA384: u32 = 0x8005;
const PADDING_PKCS1_SHA512: u32 = 0x8006;

/// The DER DigestInfo before each hash kind's digest.
const MD2_PREFIX: &[u8] = &[0x30, 0x20, 0x30, 0x0c, 0x06, 0x08, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x02, 0x02, 0x05, 0x00, 0x04, 0x10];
const MD5_PREFIX: &[u8] = &[0x30, 0x20, 0x30, 0x0c, 0x06, 0x08, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x02, 0x05, 0x05, 0x00, 0x04, 0x10];
const SHA1_PREFIX: &[u8] = &[0x30, 0x21, 0x30, 0x09, 0x06, 0x05, 0x2b, 0x0e, 0x03, 0x02, 0x1a, 0x05, 0x00, 0x04, 0x14];
const SHA2_PREFIX: [u8; 19] = [0x30, 0, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0, 0x05, 0x00, 0x04, 0];

/// A SHA-2 DigestInfo prefix: its total length, hash number and digest length.
fn sha2_prefix(total: u8, number: u8, digest: u8) -> Vec<u8> {
    let mut prefix = SHA2_PREFIX.to_vec();
    (prefix[1], prefix[14], prefix[18]) = (total, number, digest);
    prefix
}

/// An RSA public key: modulus and exponent as little-endian 32-bit limbs.
pub struct PublicKey {
    modulus: Vec<u32>,
    exponent: Vec<u8>,
    /// The modulus in bytes (the signature length).
    bytes: usize,
}

/// One DER element: (tag, contents, what follows).
fn der(bytes: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = bytes.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        (rest[..count].iter().fold(0usize, |n, &b| n << 8 | b as usize), &rest[count..])
    };
    (rest.len() >= length).then(|| (tag, &rest[..length], &rest[length..]))
}

fn strip_zeros(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    &bytes[start..]
}

/// Big-endian bytes as `count` little-endian limbs.
fn limbs(big_endian: &[u8], count: usize) -> Vec<u32> {
    let mut out = vec![0u32; count];
    for (i, &byte) in big_endian.iter().rev().enumerate() {
        out[i / 4] |= (byte as u32) << (8 * (i % 4));
    }
    out
}

impl PublicKey {
    /// A PKCS#1 RSAPublicKey, or an X.509 SubjectPublicKeyInfo holding one.
    pub fn from_der(bytes: &[u8]) -> Option<PublicKey> {
        let (tag, body, _) = der(bytes)?;
        if tag != 0x30 {
            return None;
        }
        let (first_tag, first, rest) = der(body)?;
        match first_tag {
            // { algorithm, BIT STRING { RSAPublicKey } }
            0x30 => {
                let (bits_tag, bits, _) = der(rest)?;
                (bits_tag == 0x03 && bits.first() == Some(&0)).then(|| PublicKey::from_der(&bits[1..]))?
            }
            // { modulus, exponent }
            0x02 => {
                let (exponent_tag, exponent, _) = der(rest)?;
                let (modulus, exponent) = (strip_zeros(first), strip_zeros(exponent));
                if exponent_tag != 0x02 || modulus.is_empty() || modulus[modulus.len() - 1] & 1 == 0 || exponent.is_empty() {
                    return None;
                }
                Some(PublicKey { modulus: limbs(modulus, modulus.len().div_ceil(4)), exponent: exponent.to_vec(), bytes: modulus.len() })
            }
            _ => None,
        }
    }

    pub fn block_size(&self) -> usize {
        self.bytes
    }

    /// signature ^ exponent mod modulus, as big-endian bytes of the key's
    /// length; None when the signature isn't below the modulus.
    fn raw(&self, signature: &[u8]) -> Option<Vec<u8>> {
        let n = &self.modulus;
        let value = limbs(signature, n.len());
        if !below(&value, n) {
            return None;
        }
        let montgomery = Montgomery::new(n);
        let base = montgomery.multiply(&value, &montgomery.r_squared);
        let mut one = vec![0u32; n.len()];
        one[0] = 1;
        let mut result = montgomery.multiply(&one, &montgomery.r_squared);
        for byte in &self.exponent {
            for bit in (0..8).rev() {
                result = montgomery.multiply(&result, &result);
                if byte >> bit & 1 == 1 {
                    result = montgomery.multiply(&result, &base);
                }
            }
        }
        let result = montgomery.multiply(&result, &one);
        let mut out: Vec<u8> = result.iter().flat_map(|limb| limb.to_le_bytes()).collect();
        out.truncate(self.bytes);
        out.reverse();
        Some(out)
    }
}

/// a < b (a may have one more limb than b).
fn below(a: &[u32], b: &[u32]) -> bool {
    for i in (0..a.len().max(b.len())).rev() {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x < y;
        }
    }
    false
}

/// a -= b (a >= b).
fn subtract(a: &mut [u32], b: &[u32]) {
    let mut borrow = 0i64;
    for (i, limb) in a.iter_mut().enumerate() {
        let difference = *limb as i64 - b.get(i).copied().unwrap_or(0) as i64 - borrow;
        *limb = difference as u32;
        borrow = (difference < 0) as i64;
    }
}

/// Montgomery multiplication modulo an odd modulus (R = 2^(32 * limbs)).
struct Montgomery<'a> {
    n: &'a [u32],
    /// -n^-1 mod 2^32
    n0: u32,
    r_squared: Vec<u32>,
}

impl<'a> Montgomery<'a> {
    fn new(n: &'a [u32]) -> Montgomery<'a> {
        let mut inverse = 1u32;
        for _ in 0..5 {
            inverse = inverse.wrapping_mul(2u32.wrapping_sub(n[0].wrapping_mul(inverse)));
        }
        // R^2 mod n by doubling 1 2 * 32 * limbs times.
        let mut r = vec![0u32; n.len() + 1];
        r[0] = 1;
        for _ in 0..64 * n.len() {
            let mut carry = 0;
            for limb in r.iter_mut() {
                let next = *limb >> 31;
                *limb = *limb << 1 | carry;
                carry = next;
            }
            if !below(&r, n) {
                subtract(&mut r, n);
            }
        }
        r.truncate(n.len());
        Montgomery { n, n0: inverse.wrapping_neg(), r_squared: r }
    }

    /// a * b / R mod n (CIOS).
    fn multiply(&self, a: &[u32], b: &[u32]) -> Vec<u32> {
        let (n, k) = (self.n, self.n.len());
        let mut t = vec![0u32; k + 2];
        for &b_limb in b.iter().take(k) {
            let mut carry = 0u64;
            for j in 0..k {
                let sum = t[j] as u64 + a[j] as u64 * b_limb as u64 + carry;
                t[j] = sum as u32;
                carry = sum >> 32;
            }
            let sum = t[k] as u64 + carry;
            t[k] = sum as u32;
            t[k + 1] = (sum >> 32) as u32;
            let m = t[0].wrapping_mul(self.n0);
            let mut carry = (t[0] as u64 + m as u64 * n[0] as u64) >> 32;
            for j in 1..k {
                let sum = t[j] as u64 + m as u64 * n[j] as u64 + carry;
                t[j - 1] = sum as u32;
                carry = sum >> 32;
            }
            let sum = t[k] as u64 + carry;
            t[k - 1] = sum as u32;
            t[k] = t[k + 1] + (sum >> 32) as u32;
        }
        let mut result = t[..=k].to_vec();
        if !below(&result, n) {
            subtract(&mut result, n);
        }
        result.truncate(k);
        result
    }
}

/// SecKeyRawVerify's check: the signature, opened with the key, must be
/// the PKCS#1 v1.5 block of `data` (a digest of the padding's hash kind,
/// a whole DigestInfo for kSecPaddingPKCS1, the raw block for none).
pub fn verify(key: &PublicKey, padding: u32, data: &[u8], signature: &[u8]) -> i32 {
    if signature.len() != key.bytes {
        return VERIFY_FAILED;
    }
    let Some(opened) = key.raw(signature) else { return VERIFY_FAILED };
    let content = match padding {
        PADDING_NONE => {
            let mut expected = vec![0u8; key.bytes.saturating_sub(data.len())];
            expected.extend_from_slice(data);
            return if expected == opened { SUCCESS } else { VERIFY_FAILED };
        }
        PADDING_PKCS1 => data.to_vec(),
        PADDING_PKCS1_MD2 => [MD2_PREFIX, data].concat(),
        PADDING_PKCS1_MD5 => [MD5_PREFIX, data].concat(),
        PADDING_PKCS1_SHA1 => [SHA1_PREFIX, data].concat(),
        PADDING_PKCS1_SHA224 => [&sha2_prefix(0x2d, 4, 28)[..], data].concat(),
        PADDING_PKCS1_SHA256 => [&sha2_prefix(0x31, 1, 32)[..], data].concat(),
        PADDING_PKCS1_SHA384 => [&sha2_prefix(0x41, 2, 48)[..], data].concat(),
        PADDING_PKCS1_SHA512 => [&sha2_prefix(0x51, 3, 64)[..], data].concat(),
        _ => return PARAM_ERROR,
    };
    // 00 01 FF..FF 00 content, with at least 8 bytes of FF.
    if content.len() + 11 > key.bytes {
        return PARAM_ERROR;
    }
    let mut expected = vec![0x00, 0x01];
    expected.resize(key.bytes - content.len() - 1, 0xFF);
    expected.push(0);
    expected.extend(content);
    if expected == opened {
        SUCCESS
    } else {
        VERIFY_FAILED
    }
}

static CLASS: OnceLock<usize> = OnceLock::new();

/// A SecKeyRef for a public key's bytes (None when they aren't one).
pub fn key_object(der: &[u8]) -> Option<Id> {
    let key = PublicKey::from_der(der)?;
    let object = runtime::alloc_instance(*CLASS.get()?);
    unsafe { *((object + 8) as *mut *const PublicKey) = Box::into_raw(Box::new(key)) };
    Some(object)
}

fn key_of(object: Id) -> Option<&'static PublicKey> {
    let class = *CLASS.get()?;
    if object == 0 || runtime::lock().class_of(object) != class {
        return None;
    }
    unsafe { (*((object + 8) as *const *const PublicKey)).as_ref() }
}

/// SecKeyRawVerify(key, padding, signed data, length, signature, length).
/// Only keys the keychain made from stored public keys exist here.
pub extern "C" fn sec_key_raw_verify(key: Id, padding: u32, data: *const u8, length: usize, signature: *const u8, signature_length: usize) -> i32 {
    let Some(key) = key_of(key) else { return PARAM_ERROR };
    if (data.is_null() && length != 0) || signature.is_null() {
        return PARAM_ERROR;
    }
    let data = if length == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(data, length) } };
    let signature = unsafe { std::slice::from_raw_parts(signature, signature_length) };
    verify(key, padding, data, signature)
}

extern "C" fn sec_key_get_block_size(key: Id) -> usize {
    key_of(key).map_or(0, PublicKey::block_size)
}

pub fn register(r: &mut Registry) {
    let _ = CLASS.set(define_class("__SecKey", "NSObject", 16));
    r.function("SecKeyRawVerify", sec_key_raw_verify as usize);
    r.function("SecKeyGetBlockSize", sec_key_get_block_size as usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
    }

    /// A made-up 1024-bit test key (openssl genrsa), as PKCS#1 and as X.509.
    const PUBLIC_PKCS1: &str = "30818902818100db2f7433bd294d7e90f47030f70c1b8a59d18b4685f881d0900f8d33bad371feffcc8c2850f38ee5dbf5498e1cf969e76a5802125027edce16a41b3db0db75489d0cbbb55d75e34e534eb04838a53fe75b0f4a9c40924dbb6807f35f77f889c0f56eded39965a8a2565f1279224b645b60cc5aa2474d9a04ea0fe4eba17c85d30203010001";
    const PUBLIC_X509: &str = "30819f300d06092a864886f70d010101050003818d0030818902818100db2f7433bd294d7e90f47030f70c1b8a59d18b4685f881d0900f8d33bad371feffcc8c2850f38ee5dbf5498e1cf969e76a5802125027edce16a41b3db0db75489d0cbbb55d75e34e534eb04838a53fe75b0f4a9c40924dbb6807f35f77f889c0f56eded39965a8a2565f1279224b645b60cc5aa2474d9a04ea0fe4eba17c85d30203010001";
    /// That key's PKCS#1 v1.5 SHA-1 signature of "abc" (openssl dgst -sha1 -sign).
    const SIGNATURE: &str = "526c531910ecdc9b3a198d95b743bb0c29af8536d592460859dabbdf8871bb09e3969202a43b07af3207461a66d0ed8e4b1b84648e49f02fea3aa0ac5aa60bd75b7fa39e70da1495f51a989440b19aad310bad1473c7e17a3b1daa05cd00da98a7a42a4c0f463c7e986d9cef9fb2d7ac7b7dffdeca53705a450974ee229816b6";

    #[test]
    fn raw_verify_checks_pkcs1_signatures() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(crate::run::setup);
        let digest = crate::fw::crypto::sha1(b"abc");
        let signature = unhex(SIGNATURE);
        for der in [PUBLIC_PKCS1, PUBLIC_X509] {
            let key = PublicKey::from_der(&unhex(der)).expect("an RSA public key");
            assert_eq!(key.block_size(), 128);
            assert_eq!(verify(&key, PADDING_PKCS1_SHA1, &digest, &signature), SUCCESS);
            // kSecPaddingPKCS1: the caller's data is the DigestInfo itself.
            let info = [SHA1_PREFIX, &digest[..]].concat();
            assert_eq!(verify(&key, PADDING_PKCS1, &info, &signature), SUCCESS);
            let mut wrong = signature.clone();
            wrong[5] ^= 1;
            assert_eq!(verify(&key, PADDING_PKCS1_SHA1, &digest, &wrong), VERIFY_FAILED);
            assert_eq!(verify(&key, PADDING_PKCS1_SHA1, &crate::fw::crypto::sha1(b"abd"), &signature), VERIFY_FAILED);
            assert_eq!(verify(&key, PADDING_PKCS1_SHA256, &digest, &signature), VERIFY_FAILED, "another hash kind");
        }
        assert!(PublicKey::from_der(b"not a key").is_none());
        // Through the C function, with a key object as the keychain hands out.
        let key = key_object(&unhex(PUBLIC_PKCS1)).expect("a SecKey");
        let raw = |key: Id, signature: &[u8]| sec_key_raw_verify(key, 0x8002, digest.as_ptr(), 20, signature.as_ptr(), signature.len());
        assert_eq!(raw(key, &signature), SUCCESS);
        assert_eq!(raw(key, &signature[1..]), VERIFY_FAILED, "a signature of the wrong length");
        assert_eq!(raw(0, &signature), PARAM_ERROR, "no key");
    }
}
