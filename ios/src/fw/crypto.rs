//! CommonCrypto hashing: CC_MD5, CC_SHA1, CC_SHA256 and CCHmac.

use crate::symbols::Registry;

pub fn md5(data: &[u8]) -> [u8; 16] {
    const SHIFT: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let table: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();
    let mut state = [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476];
    for block in padded(data, false).chunks(64) {
        let words: Vec<u32> = block.chunks(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let rotated = a.wrapping_add(f).wrapping_add(table[i]).wrapping_add(words[g]).rotate_left(SHIFT[i]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(rotated);
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d]) {
            *s = s.wrapping_add(v);
        }
    }
    let mut out = [0u8; 16];
    for (i, s) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&s.to_le_bytes());
    }
    out
}

/// The message with its end marker and bit length (big- or little-endian).
fn padded(data: &[u8], big_endian: bool) -> Vec<u8> {
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    let bits = (data.len() as u64).wrapping_mul(8);
    message.extend_from_slice(&if big_endian { bits.to_be_bytes() } else { bits.to_le_bytes() });
    message
}

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state = [0x67452301u32, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    for block in padded(data, true).chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = state;
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i / 20 {
                0 => ((b & c) | (!b & d), 0x5A827999),
                1 => (b ^ c ^ d, 0x6ED9EBA1),
                2 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let next = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d, e]) {
            *s = s.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for (i, s) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&s.to_be_bytes());
    }
    out
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
        0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
        0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut state = [0x6a09e667u32, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    for block in padded(data, true).chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = state;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let choose = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(choose).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let majority = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(majority);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for (s, x) in state.iter_mut().zip(v) {
            *s = s.wrapping_add(x);
        }
    }
    let mut out = [0u8; 32];
    for (i, s) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&s.to_be_bytes());
    }
    out
}

fn hmac(hash: fn(&[u8]) -> Vec<u8>, key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut key = if key.len() > 64 { hash(key) } else { key.to_vec() };
    key.resize(64, 0);
    let inner: Vec<u8> = key.iter().map(|b| b ^ 0x36).chain(data.iter().copied()).collect();
    let mut outer: Vec<u8> = key.iter().map(|b| b ^ 0x5c).collect();
    outer.extend(hash(&inner));
    hash(&outer)
}

unsafe fn bytes<'a>(data: *const u8, length: usize) -> &'a [u8] {
    if data.is_null() || length == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(data, length)
    }
}

unsafe fn write(out: *mut u8, digest: &[u8]) -> *mut u8 {
    if !out.is_null() {
        std::ptr::copy_nonoverlapping(digest.as_ptr(), out, digest.len());
    }
    out
}

extern "C" fn cc_md5(data: *const u8, length: u32, out: *mut u8) -> *mut u8 {
    unsafe { write(out, &md5(bytes(data, length as usize))) }
}

extern "C" fn cc_sha1(data: *const u8, length: u32, out: *mut u8) -> *mut u8 {
    unsafe { write(out, &sha1(bytes(data, length as usize))) }
}

extern "C" fn cc_sha256(data: *const u8, length: u32, out: *mut u8) -> *mut u8 {
    unsafe { write(out, &sha256(bytes(data, length as usize))) }
}

/// CCHmac(algorithm, key, key length, data, data length, out).
extern "C" fn cc_hmac(algorithm: u32, key: *const u8, key_length: usize, data: *const u8, length: usize, out: *mut u8) {
    let hash: fn(&[u8]) -> Vec<u8> = match algorithm {
        0 => |d| sha1(d).to_vec(),
        1 => |d| md5(d).to_vec(),
        2 => |d| sha256(d).to_vec(),
        _ => {
            crate::log!("STOP: CCHmac with hash kind {algorithm} (only SHA1, MD5 and SHA256 are here)");
            crate::report::finish(1);
        }
    };
    unsafe {
        let digest = hmac(hash, bytes(key, key_length), bytes(data, length));
        write(out, &digest);
    }
}

pub fn register(r: &mut Registry) {
    r.function("CC_MD5", cc_md5 as usize);
    r.function("CC_SHA1", cc_sha1 as usize);
    r.function("CC_SHA256", cc_sha256 as usize);
    r.function("CCHmac", cc_hmac as usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn known_digests() {
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        let long = vec![b'a'; 1000];
        assert_eq!(hex(&sha1(&long)), "291e9a6c66994949b57ba5e650361e98fc36b1ba");
        assert_eq!(
            hex(&hmac(|d| sha256(d).to_vec(), b"key", b"The quick brown fox jumps over the lazy dog")),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn global_initializers_move_into_main() {
        let source = "uniform float A;\nfloat B = A * 2.0;\nlowp vec2 C = vec2(A, 1.0), D;\nvoid main()\n{\n gl_FragColor = vec4(B);\n}\n";
        let fixed = crate::fw::gl::move_global_initializers(source);
        assert!(fixed.contains("float B;"), "{fixed}");
        assert!(fixed.contains("{\nB = A * 2.0;\n"), "{fixed}");
        assert!(fixed.contains("lowp vec2 C = vec2(A, 1.0), D;"), "{fixed}");
    }
}
