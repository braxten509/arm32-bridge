//! CommonCrypto's streaming cryptor for 32-bit programs (CCCryptorCreate,
//! Update, Final, Reset, Release), on the 64-bit layer's ciphers
//! (fw::crypto). A CCCryptorRef is a small guest allocation whose address
//! names the host-side cryptor. Also the streaming digests (their contexts
//! are words, the same on both sides) and Security's signature check.

use std::collections::HashMap;
use std::sync::Mutex;

use arm32_bridge::handler;
use arm32_bridge::runtime::{mem, ArgSource, Ret};

use super::fw::{host_functions, Obj, Ptr, Word, I32, U32};
use super::function;
use crate::fw::crypto::Cryptor;

const PARAM_ERROR: i32 = -4300;
const BUFFER_TOO_SMALL: i32 = -4301;

static CRYPTORS: Mutex<Option<HashMap<u32, Cryptor>>> = Mutex::new(None);

fn with_cryptor<R>(reference: u32, f: impl FnOnce(&mut Cryptor) -> R) -> Option<R> {
    CRYPTORS.lock().unwrap_or_else(|e| e.into_inner()).as_mut()?.get_mut(&reference).map(f)
}

/// Writes `output` if it fits `available` (with *moved set either way to
/// what was or would be written).
fn hand_out(output: &[u8], out: u32, available: u32, moved: u32) -> i32 {
    if moved != 0 {
        mem().write_u32(moved, output.len() as u32);
    }
    if output.len() as u32 > available || (out == 0 && !output.is_empty()) {
        return BUFFER_TOO_SMALL;
    }
    mem().write_bytes(out, output);
    0
}

pub fn register() {
    let digests = host_functions! {
        "CC_MD5_Init" => fn(context: Ptr) -> I32;
        "CC_MD5_Update" => fn(context: Ptr, data: Ptr, length: U32) -> I32;
        "CC_MD5_Final" => fn(digest: Ptr, context: Ptr) -> I32;
        "CC_SHA1_Init" => fn(context: Ptr) -> I32;
        "CC_SHA1_Update" => fn(context: Ptr, data: Ptr, length: U32) -> I32;
        "CC_SHA1_Final" => fn(digest: Ptr, context: Ptr) -> I32;
        "CC_SHA256_Init" => fn(context: Ptr) -> I32;
        "CC_SHA256_Update" => fn(context: Ptr, data: Ptr, length: U32) -> I32;
        "CC_SHA256_Final" => fn(digest: Ptr, context: Ptr) -> I32;
        // SecKeyRawVerify(key, padding, data, length, signature, length): keys
        // come from public keys stored in the keychain (fw::seckey).
        "SecKeyRawVerify" => fn(key: Obj, padding: U32, data: Ptr, length: Word, signature: Ptr, signature_length: Word) -> I32;
        "SecKeyGetBlockSize" => fn(key: Obj) -> Word;
    };
    for (name, handler) in digests {
        function(name, handler);
    }
    // CCCrypt(operation, algorithm, options, key, key length, iv, in, in length,
    // out, room, &moved): the 64-bit one on guest memory; size_t is 4 bytes here.
    function(
        "CCCrypt",
        handler!(|uc, a| {
            let (operation, algorithm, options, key, key_length, iv) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            let (input, input_length, out, room, moved) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            let host = |address: u32| if address == 0 { std::ptr::null_mut() } else { mem().host(address) };
            let mut host_moved = 0usize;
            let status = crate::fw::crypto::cc_crypt(
                operation, algorithm, options, host(key), key_length as usize, host(iv), host(input), input_length as usize,
                host(out), room as usize, &mut host_moved,
            );
            if moved != 0 {
                mem().write_u32(moved, host_moved as u32);
            }
            Ret::U32(status as u32)
        })
        .unwrap(),
    );
    // CCCryptorCreate(operation, algorithm, options, key, key length, iv, &cryptor)
    function(
        "CCCryptorCreate",
        handler!(|uc, a| {
            let (operation, algorithm, options, key, key_length, iv, out) =
                (a.u32(), a.u32(), a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            if operation > 1 || out == 0 || (key == 0 && key_length != 0) {
                return Ret::U32(PARAM_ERROR as u32);
            }
            let key = mem().bytes(key, key_length as usize).to_vec();
            let iv = (iv != 0 && algorithm != 4).then(|| mem().bytes(iv, if algorithm == 0 { 16 } else { 8 }).to_vec());
            match Cryptor::new(operation == 1, algorithm, options, &key, iv.as_deref()) {
                Ok(cryptor) => {
                    let reference = super::alloc(4);
                    CRYPTORS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(reference, cryptor);
                    mem().write_u32(out, reference);
                    Ret::U32(0)
                }
                Err(status) => {
                    mem().write_u32(out, 0);
                    Ret::U32(status as u32)
                }
            }
        })
        .unwrap(),
    );
    // CCCryptorUpdate(cryptor, data in, length, data out, room, &moved)
    function(
        "CCCryptorUpdate",
        handler!(|uc, a| {
            let (reference, input, length, out, available, moved) = (a.u32(), a.u32(), a.u32(), a.u32(), a.u32(), a.u32());
            let input = mem().bytes(input, length as usize).to_vec();
            let status = with_cryptor(reference, |cryptor| {
                let room = cryptor.output_length(input.len(), false) as u32;
                if room > available {
                    if moved != 0 {
                        mem().write_u32(moved, room);
                    }
                    return BUFFER_TOO_SMALL;
                }
                hand_out(&cryptor.update(&input), out, available, moved)
            });
            Ret::U32(status.unwrap_or(PARAM_ERROR) as u32)
        })
        .unwrap(),
    );
    // CCCryptorFinal(cryptor, data out, room, &moved)
    function(
        "CCCryptorFinal",
        handler!(|uc, a| {
            let (reference, out, available, moved) = (a.u32(), a.u32(), a.u32(), a.u32());
            // As Apple's: the room is checked against the real output (after
            // padding is removed); when it is too small the cryptor is left as
            // it was, so the caller can try again with more.
            let status = with_cryptor(reference, |cryptor| {
                let mut finished = cryptor.clone();
                match finished.finish() {
                    Ok(output) => {
                        let status = hand_out(&output, out, available, moved);
                        if status == 0 {
                            *cryptor = finished;
                        }
                        status
                    }
                    Err(status) => status,
                }
            });
            Ret::U32(status.unwrap_or(PARAM_ERROR) as u32)
        })
        .unwrap(),
    );
    // CCCryptorReset(cryptor, iv): a NULL iv is zeros.
    function(
        "CCCryptorReset",
        handler!(|uc, a| {
            let (reference, iv) = (a.u32(), a.u32());
            let status = with_cryptor(reference, |cryptor| {
                let iv = (iv != 0).then(|| mem().bytes(iv, cryptor.block_size()).to_vec());
                cryptor.reset(iv.as_deref()).err().unwrap_or(0)
            });
            Ret::U32(status.unwrap_or(PARAM_ERROR) as u32)
        })
        .unwrap(),
    );
    function(
        "CCCryptorRelease",
        handler!(|uc, a| {
            let reference = a.u32();
            let removed = CRYPTORS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|all| all.remove(&reference));
            if removed.is_some() {
                super::free(reference);
            }
            Ret::U32(if removed.is_some() { 0 } else { PARAM_ERROR as u32 })
        })
        .unwrap(),
    );
}
