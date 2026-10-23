//! CommonCrypto: hashing (CC_MD5, CC_SHA1, CC_SHA256, CCHmac) and
//! encryption (CCCrypt).

use crate::symbols::Registry;
use cipher::generic_array::GenericArray;
use cipher::{BlockDecrypt, BlockEncrypt, KeyInit};

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

const SHA1_START: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state = SHA1_START;
    for block in padded(data, true).chunks(64) {
        sha1_block(&mut state, block);
    }
    sha1_digest(&state)
}

fn sha1_digest(state: &[u32; 5]) -> [u8; 20] {
    let mut out = [0u8; 20];
    for (i, s) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&s.to_be_bytes());
    }
    out
}

fn sha1_block(state: &mut [u32; 5], block: &[u8]) {
    let mut w = [0u32; 80];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3]]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = *state;
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

// ----- CC_SHA1_Init / Update / Final -----
// Apple's CC_SHA1_CTX (96 bytes): h0..h4, Nl, Nh (bit count, low and high
// words), 64 bytes of waiting data, and how many of them are used.

#[repr(C)]
struct Sha1Context {
    state: [u32; 5],
    bits_low: u32,
    bits_high: u32,
    waiting: [u8; 64],
    used: u32,
}

extern "C" fn cc_sha1_init(context: *mut Sha1Context) -> i32 {
    unsafe { *context = Sha1Context { state: SHA1_START, bits_low: 0, bits_high: 0, waiting: [0; 64], used: 0 } };
    1
}

fn sha1_update(context: &mut Sha1Context, mut data: &[u8]) {
    let bits = ((u64::from(context.bits_high) << 32) | u64::from(context.bits_low)).wrapping_add((data.len() as u64).wrapping_mul(8));
    (context.bits_low, context.bits_high) = (bits as u32, (bits >> 32) as u32);
    let used = (context.used as usize).min(63);
    if used > 0 {
        let take = data.len().min(64 - used);
        context.waiting[used..used + take].copy_from_slice(&data[..take]);
        data = &data[take..];
        context.used = (used + take) as u32;
        if context.used < 64 {
            return;
        }
        let block = context.waiting;
        sha1_block(&mut context.state, &block);
        context.used = 0;
    }
    let mut blocks = data.chunks_exact(64);
    for block in &mut blocks {
        sha1_block(&mut context.state, block);
    }
    let rest = blocks.remainder();
    context.waiting[..rest.len()].copy_from_slice(rest);
    context.used = rest.len() as u32;
}

extern "C" fn cc_sha1_update(context: *mut Sha1Context, data: *const u8, length: u32) -> i32 {
    let bytes = if data.is_null() || length == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(data, length as usize) } };
    sha1_update(unsafe { &mut *context }, bytes);
    1
}

fn sha1_final(context: &mut Sha1Context) -> [u8; 20] {
    let bits = (u64::from(context.bits_high) << 32) | u64::from(context.bits_low);
    let used = context.used as usize % 64;
    let mut tail = vec![0x80u8];
    tail.resize(if used < 56 { 56 - used } else { 120 - used }, 0);
    sha1_update(context, &tail);
    sha1_update(context, &bits.to_be_bytes());
    sha1_digest(&context.state)
}

extern "C" fn cc_sha1_final(digest: *mut u8, context: *mut Sha1Context) -> i32 {
    let out = sha1_final(unsafe { &mut *context });
    unsafe {
        std::ptr::copy_nonoverlapping(out.as_ptr(), digest, 20);
        std::ptr::write_bytes(context as *mut u8, 0, std::mem::size_of::<Sha1Context>());
    }
    1
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

// ----- Encryption: CCCrypt -----
//
// CCCrypt(operation, algorithm, options, key, key length, iv, data in,
// data in length, data out, data out available, data out moved) encrypts
// or decrypts a whole message at once. Games use it for their save files
// and settings, so a wrong answer shows up as a crash much later.

/// CCCryptorStatus values.
const SUCCESS: i32 = 0;
const PARAM_ERROR: i32 = -4300;
const BUFFER_TOO_SMALL: i32 = -4301;
const ALIGNMENT_ERROR: i32 = -4303;
const DECODE_ERROR: i32 = -4304;
const UNIMPLEMENTED: i32 = -4305;

/// CCOptions bits.
const PKCS7_PADDING: u32 = 1;
const ECB_MODE: u32 = 2;

/// CCAlgorithm values, for the log.
const ALGORITHM_NAMES: [&str; 7] = ["AES", "DES", "3DES", "CAST", "RC4", "RC2", "Blowfish"];

/// A block cipher with its key set.
enum BlockCipher {
    Aes128(aes::Aes128),
    Aes192(aes::Aes192),
    Aes256(aes::Aes256),
    Des(des::Des),
    TripleDes(des::TdesEde3),
    Cast(cast5::Cast5),
    Blowfish(blowfish::Blowfish),
}

macro_rules! each_cipher {
    ($cipher:expr, $c:ident => $body:expr) => {
        match $cipher {
            BlockCipher::Aes128($c) => $body,
            BlockCipher::Aes192($c) => $body,
            BlockCipher::Aes256($c) => $body,
            BlockCipher::Des($c) => $body,
            BlockCipher::TripleDes($c) => $body,
            BlockCipher::Cast($c) => $body,
            BlockCipher::Blowfish($c) => $body,
        }
    };
}

impl BlockCipher {
    /// The cipher for a CCAlgorithm (RC4 is a stream cipher, not here), or the status to answer.
    fn new(algorithm: u32, key: &[u8]) -> Result<BlockCipher, i32> {
        fn keyed<C: KeyInit>(key: &[u8]) -> Result<C, i32> {
            C::new_from_slice(key).map_err(|_| PARAM_ERROR)
        }
        Ok(match (algorithm, key.len()) {
            (0, 16) => BlockCipher::Aes128(keyed(key)?),
            (0, 24) => BlockCipher::Aes192(keyed(key)?),
            (0, 32) => BlockCipher::Aes256(keyed(key)?),
            (1, 8) => BlockCipher::Des(keyed(key)?),
            (2, 24) => BlockCipher::TripleDes(keyed(key)?),
            (3, 5..=16) => BlockCipher::Cast(keyed(key)?),
            (6, 8..=56) => BlockCipher::Blowfish(keyed(key)?),
            (0..=3 | 6, _) => return Err(PARAM_ERROR),
            _ => return Err(UNIMPLEMENTED),
        })
    }

    fn block_size(&self) -> usize {
        match self {
            BlockCipher::Aes128(_) | BlockCipher::Aes192(_) | BlockCipher::Aes256(_) => 16,
            _ => 8,
        }
    }

    fn encrypt(&self, block: &mut [u8]) {
        each_cipher!(self, c => c.encrypt_block(GenericArray::from_mut_slice(block)))
    }

    fn decrypt(&self, block: &mut [u8]) {
        each_cipher!(self, c => c.decrypt_block(GenericArray::from_mut_slice(block)))
    }
}

/// RC4: a key stream XORed over the data (the same both ways), which
/// carries on from piece to piece.
struct Rc4 {
    state: Vec<u8>,
    i: u8,
    j: u8,
}

impl Rc4 {
    fn new(key: &[u8]) -> Result<Rc4, i32> {
        if key.is_empty() || key.len() > 512 {
            return Err(PARAM_ERROR);
        }
        let mut state: Vec<u8> = (0..=255).collect();
        let mut j = 0u8;
        for i in 0..256 {
            j = j.wrapping_add(state[i]).wrapping_add(key[i % key.len()]);
            state.swap(i, j as usize);
        }
        Ok(Rc4 { state, i: 0, j: 0 })
    }

    fn apply(&mut self, data: &[u8]) -> Vec<u8> {
        data.iter()
            .map(|byte| {
                self.i = self.i.wrapping_add(1);
                self.j = self.j.wrapping_add(self.state[self.i as usize]);
                self.state.swap(self.i as usize, self.j as usize);
                byte ^ self.state[self.state[self.i as usize].wrapping_add(self.state[self.j as usize]) as usize]
            })
            .collect()
    }
}

fn rc4(key: &[u8], data: &[u8]) -> Result<Vec<u8>, i32> {
    Ok(Rc4::new(key)?.apply(data))
}

/// The room CCCrypt asks for before it starts (CCCryptorGetOutputLength
/// for the final block): a padded message's length when encrypting with
/// padding, else the input's length.
fn room_needed(decrypt: bool, options: u32, block_size: usize, length: usize) -> usize {
    if !decrypt && options & PKCS7_PADDING != 0 {
        length + block_size - length % block_size
    } else {
        length
    }
}

/// One whole CCCrypt: the output, or the status to answer.
pub fn crypt(decrypt: bool, algorithm: u32, options: u32, key: &[u8], iv: Option<&[u8]>, input: &[u8]) -> Result<Vec<u8>, i32> {
    if algorithm == 4 {
        return rc4(key, input);
    }
    let cipher = BlockCipher::new(algorithm, key)?;
    let size = cipher.block_size();
    let padded = options & PKCS7_PADDING != 0;
    let mut data = input.to_vec();
    if !decrypt && padded {
        let pad = size - data.len() % size;
        data.resize(data.len() + pad, pad as u8);
    }
    if data.len() % size != 0 {
        return Err(ALIGNMENT_ERROR);
    }
    // CBC starts from the IV (zeros when there is none); ECB has no chaining.
    let chained = options & ECB_MODE == 0;
    let mut previous = vec![0u8; size];
    if let Some(iv) = iv {
        previous.copy_from_slice(&iv[..size]);
    }
    for block in data.chunks_mut(size) {
        if decrypt {
            let encrypted = block.to_vec();
            cipher.decrypt(block);
            if chained {
                block.iter_mut().zip(&previous).for_each(|(b, p)| *b ^= p);
                previous = encrypted;
            }
        } else {
            if chained {
                block.iter_mut().zip(&previous).for_each(|(b, p)| *b ^= p);
            }
            cipher.encrypt(block);
            if chained {
                previous.copy_from_slice(block);
            }
        }
    }
    if decrypt && padded {
        let pad = *data.last().ok_or(DECODE_ERROR)? as usize;
        if pad == 0 || pad > size || !data[data.len() - pad..].iter().all(|&b| b as usize == pad) {
            return Err(DECODE_ERROR);
        }
        data.truncate(data.len() - pad);
    }
    Ok(data)
}

/// A streaming CCCryptor (CCCryptorCreate / Update / Final): the same
/// ciphers, modes and padding as `crypt`, fed in pieces.
pub struct Cryptor {
    decrypt: bool,
    padded: bool,
    /// CBC (else ECB): `previous` is the last cipher block (the IV at first).
    chained: bool,
    cipher: Result<BlockCipher, Rc4>,
    previous: Vec<u8>,
    /// Input not yet turned into output (less than a block, or the block
    /// held back while decrypting with padding).
    pending: Vec<u8>,
}

impl Cryptor {
    /// `iv`: the chain's start for CBC (zeros when None).
    pub fn new(decrypt: bool, algorithm: u32, options: u32, key: &[u8], iv: Option<&[u8]>) -> Result<Cryptor, i32> {
        let cipher = if algorithm == 4 { Err(Rc4::new(key)?) } else { Ok(BlockCipher::new(algorithm, key)?) };
        let size = cipher.as_ref().map_or(1, BlockCipher::block_size);
        let mut previous = vec![0u8; size];
        if let (Some(iv), true) = (iv, cipher.is_ok()) {
            previous.copy_from_slice(&iv[..size]);
        }
        Ok(Cryptor {
            decrypt,
            padded: options & PKCS7_PADDING != 0 && cipher.is_ok(),
            chained: options & ECB_MODE == 0,
            cipher,
            previous,
            pending: Vec::new(),
        })
    }

    fn block_size(&self) -> usize {
        self.cipher.as_ref().map_or(1, BlockCipher::block_size)
    }

    /// How many pending bytes `update` turns into output now.
    fn ready(&self, pending: usize) -> usize {
        let size = self.block_size();
        let whole = pending / size * size;
        // Decrypting with padding: the last whole block may be the padding.
        if self.decrypt && self.padded && whole == pending && whole > 0 {
            whole - size
        } else {
            whole
        }
    }

    /// The bytes `update` (or, when `last`, `update` then `finish`) writes for `length` more bytes in.
    pub fn output_length(&self, length: usize, last: bool) -> usize {
        let total = self.pending.len() + length;
        match (last, self.decrypt, self.padded) {
            (false, _, _) => self.ready(total),
            (true, false, true) => total / self.block_size() * self.block_size() + self.block_size(),
            (true, _, _) => total,
        }
    }

    /// Whole blocks out (decrypting with padding holds the last one back for `finish`).
    pub fn update(&mut self, input: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(input);
        let ready = self.ready(self.pending.len());
        let mut data: Vec<u8> = self.pending.drain(..ready).collect();
        self.transform(&mut data);
        data
    }

    /// Runs the cipher over whole blocks (any length for RC4).
    fn transform(&mut self, data: &mut [u8]) {
        let cipher = match &mut self.cipher {
            Err(rc4) => {
                let out = rc4.apply(data);
                data.copy_from_slice(&out);
                return;
            }
            Ok(cipher) => cipher,
        };
        for block in data.chunks_mut(cipher.block_size()) {
            if self.decrypt {
                let encrypted = block.to_vec();
                cipher.decrypt(block);
                if self.chained {
                    block.iter_mut().zip(&self.previous).for_each(|(b, p)| *b ^= p);
                    self.previous = encrypted;
                }
            } else {
                if self.chained {
                    block.iter_mut().zip(&self.previous).for_each(|(b, p)| *b ^= p);
                }
                cipher.encrypt(block);
                if self.chained {
                    self.previous.copy_from_slice(block);
                }
            }
        }
    }

    /// The rest: padding added (encrypt) or checked and removed (decrypt).
    pub fn finish(&mut self) -> Result<Vec<u8>, i32> {
        let size = self.block_size();
        let mut data = std::mem::take(&mut self.pending);
        if !self.decrypt && self.padded {
            let pad = size - data.len() % size;
            data.resize(data.len() + pad, pad as u8);
        }
        if data.len() % size != 0 {
            return Err(ALIGNMENT_ERROR);
        }
        if self.decrypt && self.padded && data.len() != size {
            return Err(DECODE_ERROR);
        }
        self.transform(&mut data);
        if self.decrypt && self.padded {
            let pad = *data.last().ok_or(DECODE_ERROR)? as usize;
            if pad == 0 || pad > size || !data[data.len() - pad..].iter().all(|&b| b as usize == pad) {
                return Err(DECODE_ERROR);
            }
            data.truncate(data.len() - pad);
        }
        Ok(data)
    }
}

/// The kinds of CCCrypt use seen so far, so each is logged once.
static CRYPT_KINDS: std::sync::Mutex<Vec<(u32, u32, u32, usize)>> = std::sync::Mutex::new(Vec::new());

/// Calls answered so far; the first few are logged with their answer.
static CRYPT_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[allow(clippy::too_many_arguments)]
extern "C" fn cc_crypt(
    operation: u32,
    algorithm: u32,
    options: u32,
    key: *const u8,
    key_length: usize,
    iv: *const u8,
    data_in: *const u8,
    data_in_length: usize,
    data_out: *mut u8,
    data_out_available: usize,
    data_out_moved: *mut usize,
) -> i32 {
    let name = ALGORITHM_NAMES.get(algorithm as usize).copied().unwrap_or("unknown");
    let kind = (operation, algorithm, options, key_length);
    {
        let mut seen = CRYPT_KINDS.lock().unwrap();
        if !seen.contains(&kind) {
            seen.push(kind);
            let action = if operation == 1 { "decrypt" } else { "encrypt" };
            let mode = if options & ECB_MODE != 0 { "ECB" } else { "CBC" };
            let padding = if options & PKCS7_PADDING != 0 { ", PKCS7 padding" } else { "" };
            crate::log!("CommonCrypto: CCCrypt {action} {name} ({}-byte key), {mode}{padding}", key_length);
        }
    }
    let moved = |value: usize| {
        if !data_out_moved.is_null() {
            unsafe { *data_out_moved = value };
        }
    };
    moved(0);
    if operation > 1 || (key.is_null() && key_length != 0) || (data_in.is_null() && data_in_length != 0) {
        return PARAM_ERROR;
    }
    let block_size = match algorithm {
        0 => 16,
        4 => 1,
        _ => 8,
    };
    let needed = room_needed(operation == 1, options, block_size, data_in_length);
    if needed > data_out_available || (data_out.is_null() && needed != 0) {
        moved(needed);
        return BUFFER_TOO_SMALL;
    }
    let (key, input) = unsafe { (bytes(key, key_length), bytes(data_in, data_in_length)) };
    let iv = (!iv.is_null() && algorithm != 4).then(|| unsafe { std::slice::from_raw_parts(iv, block_size) });
    let result = crypt(operation == 1, algorithm, options, key, iv, input);
    if CRYPT_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 8 {
        match &result {
            Ok(output) => crate::log!("CommonCrypto: CCCrypt {data_in_length} bytes in, {} out (room {data_out_available})", output.len()),
            Err(status) => crate::log!("CommonCrypto: CCCrypt {data_in_length} bytes in: status {status}"),
        }
    }
    match result {
        Ok(output) => {
            unsafe { write(data_out, &output) };
            moved(output.len());
            SUCCESS
        }
        Err(status) => {
            if status == UNIMPLEMENTED {
                crate::log!("CommonCrypto: CCCrypt with {name} isn't here (answered 'unimplemented')");
            }
            status
        }
    }
}

pub fn register(r: &mut Registry) {
    r.function("CC_MD5", cc_md5 as usize);
    r.function("CC_SHA1", cc_sha1 as usize);
    r.function("CC_SHA1_Init", cc_sha1_init as usize);
    r.function("CC_SHA1_Update", cc_sha1_update as usize);
    r.function("CC_SHA1_Final", cc_sha1_final as usize);
    r.function("CC_SHA256", cc_sha256 as usize);
    r.function("CCHmac", cc_hmac as usize);
    r.function("CCCrypt", cc_crypt as usize);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Fed in uneven pieces (crossing block edges), the streaming SHA-1
    /// gives the one-shot answer, also when the end needs an extra block.
    #[test]
    fn sha1_in_pieces_matches_one_shot() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 7 + 3) as u8).collect();
        for total in [0usize, 55, 56, 63, 64, 1000] {
            let mut context: Sha1Context = unsafe { std::mem::zeroed() };
            cc_sha1_init(&mut context);
            let mut at = 0;
            for piece in [1usize, 62, 3, 130].iter().cycle() {
                if at >= total {
                    break;
                }
                let end = (at + piece).min(total);
                cc_sha1_update(&mut context, data[at..end].as_ptr(), (end - at) as u32);
                at = end;
            }
            let mut digest = [0u8; 20];
            cc_sha1_final(digest.as_mut_ptr(), &mut context);
            assert_eq!(digest, sha1(&data[..total]), "{total} bytes");
        }
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

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn known_ciphertexts() {
        let block = unhex("00112233445566778899aabbccddeeff");
        // FIPS-197 appendix C (one block, so ECB).
        let aes128 = crypt(false, 0, ECB_MODE, &unhex("000102030405060708090a0b0c0d0e0f"), None, &block).unwrap();
        assert_eq!(hex(&aes128), "69c4e0d86a7b0430d8cdb78070b4c55a");
        let key256 = unhex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        assert_eq!(hex(&crypt(false, 0, ECB_MODE, &key256, None, &block).unwrap()), "8ea2b7ca516745bfeafc49904b496089");
        // SP 800-38A F.2.1, AES-128 CBC, two blocks.
        let key = unhex("2b7e151628aed2a6abf7158809cf4f3c");
        let iv = unhex("000102030405060708090a0b0c0d0e0f");
        let plain = unhex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51");
        let encrypted = crypt(false, 0, 0, &key, Some(&iv), &plain).unwrap();
        assert_eq!(hex(&encrypted), "7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2");
        assert_eq!(crypt(true, 0, 0, &key, Some(&iv), &encrypted).unwrap(), plain);
        // DES, and 3DES with the same key three times (the same as DES).
        let des_key = unhex("133457799bbcdff1");
        let des_plain = unhex("0123456789abcdef");
        assert_eq!(hex(&crypt(false, 1, ECB_MODE, &des_key, None, &des_plain).unwrap()), "85e813540f0ab405");
        let triple: Vec<u8> = des_key.iter().chain(&des_key).chain(&des_key).copied().collect();
        assert_eq!(hex(&crypt(false, 2, ECB_MODE, &triple, None, &des_plain).unwrap()), "85e813540f0ab405");
        assert_eq!(hex(&crypt(false, 6, ECB_MODE, &[0; 8], None, &[0; 8]).unwrap()), "4ef997456198dd78");
        assert_eq!(hex(&crypt(false, 4, 0, b"Key", None, b"Plaintext").unwrap()), "bbf316e8d940af0ad3");
    }

    #[test]
    fn padding_and_errors() {
        let key = [7u8; 32];
        for length in [0, 1, 15, 16, 17, 100] {
            let plain: Vec<u8> = (0..length as u8).collect();
            let encrypted = crypt(false, 0, PKCS7_PADDING, &key, None, &plain).unwrap();
            assert_eq!(encrypted.len(), room_needed(false, PKCS7_PADDING, 16, length));
            assert_eq!(crypt(true, 0, PKCS7_PADDING, &key, None, &encrypted).unwrap(), plain);
        }
        assert_eq!(crypt(false, 0, 0, &key, None, &[1; 15]), Err(ALIGNMENT_ERROR));
        assert_eq!(crypt(false, 0, 0, &[1; 20], None, &[1; 16]), Err(PARAM_ERROR));
        assert_eq!(crypt(false, 5, 0, &[1; 16], None, &[1; 16]), Err(UNIMPLEMENTED));
        // A wrong key leaves padding that doesn't check out.
        let encrypted = crypt(false, 0, PKCS7_PADDING, &key, None, b"save data").unwrap();
        assert_eq!(crypt(true, 0, PKCS7_PADDING, &[8; 32], None, &encrypted), Err(DECODE_ERROR));

        // Too little room: the room needed comes back, nothing is written.
        let plain = [3u8; 20];
        let mut out = [0u8; 32];
        let mut moved = 0usize;
        let status = cc_crypt(0, 0, PKCS7_PADDING, key.as_ptr(), 32, std::ptr::null(), plain.as_ptr(), 20, out.as_mut_ptr(), 31, &mut moved);
        assert_eq!((status, moved, out), (BUFFER_TOO_SMALL, 32, [0; 32]));
        let status = cc_crypt(0, 0, PKCS7_PADDING, key.as_ptr(), 32, std::ptr::null(), plain.as_ptr(), 20, out.as_mut_ptr(), 32, &mut moved);
        assert_eq!((status, moved), (SUCCESS, 32));
        let mut back = [0u8; 32];
        let status = cc_crypt(1, 0, PKCS7_PADDING, key.as_ptr(), 32, std::ptr::null(), out.as_ptr(), 32, back.as_mut_ptr(), 32, &mut moved);
        assert_eq!((status, moved, &back[..20]), (SUCCESS, 20, &plain[..]));
    }

    #[test]
    fn streaming_matches_one_shot() {
        let iv = [9u8; 16];
        let kinds: [(u32, u32, &[u8]); 5] = [
            (0, PKCS7_PADDING, &[7; 16]),
            (0, ECB_MODE, &[7; 32]),
            (0, 0, &[7; 24]),
            (1, PKCS7_PADDING, &[5; 8]),
            (4, 0, b"Key"),
        ];
        for (algorithm, options, key) in kinds {
            let size = if algorithm == 0 { 16 } else { 8 };
            for length in [0usize, 1, 15, 16, 17, 100] {
                if options & PKCS7_PADDING == 0 && algorithm != 4 && length % size != 0 {
                    continue;
                }
                let plain: Vec<u8> = (0..length).map(|i| i as u8).collect();
                let iv = (algorithm != 4).then_some(&iv[..size]);
                let whole = crypt(false, algorithm, options, key, iv, &plain).unwrap();
                for piece in [1, 7, 16, 33] {
                    let run = |decrypt: bool, input: &[u8]| {
                        let mut cryptor = Cryptor::new(decrypt, algorithm, options, key, iv).unwrap();
                        let mut out = Vec::new();
                        for chunk in input.chunks(piece) {
                            let room = cryptor.output_length(chunk.len(), false);
                            let written = cryptor.update(chunk);
                            assert!(written.len() <= room, "update wrote {} with room {room}", written.len());
                            out.extend(written);
                        }
                        let room = cryptor.output_length(0, true);
                        let last = cryptor.finish().unwrap();
                        assert!(last.len() <= room);
                        out.extend(last);
                        out
                    };
                    assert_eq!(run(false, &plain), whole, "algorithm {algorithm} options {options} length {length} pieces of {piece}");
                    assert_eq!(run(true, &whole), plain);
                }
            }
        }
        // Without padding a partial block left at the end is an error.
        let mut cryptor = Cryptor::new(false, 0, 0, &[7; 16], None).unwrap();
        cryptor.update(&[1; 20]);
        assert_eq!(cryptor.finish(), Err(ALIGNMENT_ERROR));
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
