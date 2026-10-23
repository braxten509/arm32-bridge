//! CommonCrypto's streaming cryptor for 32-bit programs (CCCryptorCreate,
//! Update, Final, Release), on the 64-bit layer's ciphers (fw::crypto).
//! A CCCryptorRef is a small guest allocation whose address names the
//! host-side cryptor.

use std::collections::HashMap;
use std::sync::Mutex;

use arm32_bridge::handler;
use arm32_bridge::runtime::{mem, ArgSource, Ret};

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
            let status = with_cryptor(reference, |cryptor| {
                let room = cryptor.output_length(0, true) as u32;
                if room > available {
                    if moved != 0 {
                        mem().write_u32(moved, room);
                    }
                    return BUFFER_TOO_SMALL;
                }
                match cryptor.finish() {
                    Ok(output) => hand_out(&output, out, available, moved),
                    Err(status) => status,
                }
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
