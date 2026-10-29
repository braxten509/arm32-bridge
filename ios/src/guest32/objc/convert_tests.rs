//! Tests for convert.rs on a made-up boundary (no translator, no host runtime).

use super::convert::*;
use super::encoding::Signature;
use super::selectors::{IdArrays, Varargs};
use std::collections::HashMap;

const BASE: u32 = 0x1_0000;
const HOST_ID: usize = 0x7_0000_0000;
const HOST_SEL: usize = 0x8_0000_0000;
/// Guest words for opaque host pointers start here.
const OPAQUE: u32 = 0x9000_0000;

/// Guest memory is a real buffer, so host_address() can be dereferenced.
/// Guest id g <-> host id HOST_ID + g; guest selector g <-> HOST_SEL + g.
struct Fake {
    memory: Vec<u8>,
    next: u32,
    texts: HashMap<u32, String>,
    /// Opaque host pointer <-> guest word (the bridge's shared table).
    opaque: Vec<usize>,
}

impl Fake {
    fn new() -> Self {
        Fake { memory: vec![0; 0x4000], next: BASE + 0x2000, texts: HashMap::new(), opaque: Vec::new() }
    }
    fn put(&mut self, addr: u32, data: &[u8]) {
        self.write(addr, data);
    }
    fn get(&self, addr: u32, len: u32) -> Vec<u8> {
        self.read(addr, len)
    }
}

impl Boundary for Fake {
    fn read(&self, addr: u32, len: u32) -> Vec<u8> {
        let i = (addr - BASE) as usize;
        self.memory[i..i + len as usize].to_vec()
    }
    fn write(&mut self, addr: u32, data: &[u8]) {
        let i = (addr - BASE) as usize;
        self.memory[i..i + data.len()].copy_from_slice(data);
    }
    fn alloc(&mut self, size: u32) -> u32 {
        let at = self.next;
        self.next += size.div_ceil(8) * 8;
        at
    }
    fn free(&mut self, _addr: u32) {}
    fn host_address(&self, addr: u32) -> usize {
        self.memory.as_ptr() as usize + (addr - BASE) as usize
    }
    fn guest_address(&self, host: usize) -> Option<u32> {
        let start = self.memory.as_ptr() as usize;
        (host >= start && host < start + self.memory.len()).then(|| BASE + (host - start) as u32)
    }
    fn id_to_host(&mut self, guest: u32) -> Result<usize, String> {
        Ok(if guest == 0 { 0 } else { HOST_ID + guest as usize })
    }
    fn id_to_guest(&mut self, host: usize) -> Result<u32, String> {
        Ok(if host == 0 { 0 } else { (host - HOST_ID) as u32 })
    }
    fn sel_to_host(&mut self, guest: u32) -> Result<usize, String> {
        Ok(HOST_SEL + guest as usize)
    }
    fn sel_to_guest(&mut self, host: usize) -> Result<u32, String> {
        Ok((host - HOST_SEL) as u32)
    }
    fn object_text(&mut self, guest: u32) -> Option<String> {
        self.texts.get(&guest).cloned()
    }
    fn opaque_to_guest(&mut self, host: usize) -> Result<u32, String> {
        let index = self.opaque.iter().position(|h| *h == host).unwrap_or_else(|| {
            self.opaque.push(host);
            self.opaque.len() - 1
        });
        Ok(OPAQUE + index as u32 * 4)
    }
    fn opaque_to_host(&mut self, guest: u32) -> Option<usize> {
        guest.checked_sub(OPAQUE).and_then(|i| self.opaque.get(i as usize / 4)).copied()
    }
}

fn words(list: &[u32]) -> impl Fn(u32) -> u32 + '_ {
    move |i| list.get(i as usize).copied().unwrap_or(0)
}

fn plain() -> HostCallOptions {
    HostCallOptions::default()
}

fn f(v: f32) -> u32 {
    v.to_bits()
}

#[test]
fn guest_arguments_land_in_aapcs64_registers() {
    let mut b = Fake::new();
    // -[UIView initWithFrame:]: self, _cmd, CGRect (floats) -> x0, x1, d0-d3 as doubles.
    let sig = Signature::parse("@@:{CGRect={CGPoint=ff}{CGSize=ff}}").unwrap();
    let w = [0x100, 0x200, f(1.5), f(2.0), f(320.0), f(480.0)];
    let call = prepare_host_call(&sig, plain(), &words(&w), &mut b).unwrap();
    assert_eq!(&call.args.x[..2], &[(HOST_ID + 0x100) as u64, (HOST_SEL + 0x200) as u64]);
    let d: Vec<f64> = call.args.d[..4].iter().map(|b| f64::from_bits(*b)).collect();
    assert_eq!(d, [1.5, 2.0, 320.0, 480.0]);
    // Integers and doubles use separate register files; a double spans two guest words.
    let sig = Signature::parse("v@:ciqd").unwrap();
    let w = [0x100, 0x200, 1, (-5i32) as u32, 7, 0, 0, 0x4000_0000];
    let call = prepare_host_call(&sig, plain(), &words(&w), &mut b).unwrap();
    assert_eq!(call.args.x[2], 1);
    assert_eq!(call.args.x[3] as i64, -5);
    assert_eq!(call.args.x[4], 7);
    assert_eq!(f64::from_bits(call.args.d[0]), 2.0);
    // A scalar 'f' is CGFloat (double) unless the row says it is a real float.
    let sig = Signature::parse("@@:f").unwrap();
    let w = [0x100, 0x200, f(0.25)];
    let cg = prepare_host_call(&sig, plain(), &words(&w), &mut b).unwrap();
    assert_eq!(f64::from_bits(cg.args.d[0]), 0.25);
    let real = prepare_host_call(&sig, HostCallOptions { real_float: true, ..plain() }, &words(&w), &mut b).unwrap();
    assert_eq!(real.args.d[0], u64::from(f(0.25)));
}

#[test]
fn struct_returns_and_float_returns_convert_back() {
    let mut b = Fake::new();
    // -[UIView frame]: guest stret pointer in r0, host HFA in d0-d3.
    let sig = Signature::parse("{CGRect={CGPoint=ff}{CGSize=ff}}@:").unwrap();
    let out = b.alloc(16);
    let w = [out, 0x100, 0x200];
    let call = prepare_host_call(&sig, plain(), &words(&w), &mut b).unwrap();
    assert_eq!(call.args.x[0], (HOST_ID + 0x100) as u64);
    let ret = HostRet { x: [0; 2], d: [1.0f64.to_bits(), 2.0f64.to_bits(), 3.0f64.to_bits(), 4.0f64.to_bits()] };
    let r = finish_host_call(call, &ret, &mut b).unwrap();
    assert_eq!(r.r0, out);
    let floats: Vec<f32> = b.get(out, 16).chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(floats, [1.0, 2.0, 3.0, 4.0]);
    // NSRange is two NSUIntegers: x0/x1 on the host, 8 guest bytes via stret.
    let sig = Signature::parse("{_NSRange=II}@:@").unwrap();
    let out = b.alloc(8);
    let call = prepare_host_call(&sig, plain(), &words(&[out, 1, 2, 3]), &mut b).unwrap();
    let r = finish_host_call(call, &HostRet { x: [5, 9], d: [0; 4] }, &mut b).unwrap();
    assert_eq!((r.r0, b.get(out, 8)), (out, [5, 0, 0, 0, 9, 0, 0, 0].to_vec()));
    // Float returns: a host float (s0, upper bits zero) and a host CGFloat (d0) both arrive.
    let sig = Signature::parse("f@:").unwrap();
    for d0 in [u64::from(f(0.5)), 0.5f64.to_bits()] {
        let call = prepare_host_call(&sig, plain(), &words(&[1, 2]), &mut b).unwrap();
        let r = finish_host_call(call, &HostRet { x: [0; 2], d: [d0, 0, 0, 0] }, &mut b).unwrap();
        assert_eq!(f32::from_bits(r.r0), 0.5);
    }
    // A double comes back in r0:r1, an id is mapped.
    let sig = Signature::parse("d@:").unwrap();
    let call = prepare_host_call(&sig, plain(), &words(&[1, 2]), &mut b).unwrap();
    let r = finish_host_call(call, &HostRet { x: [0; 2], d: [2.5f64.to_bits(), 0, 0, 0] }, &mut b).unwrap();
    assert_eq!(f64::from_bits(u64::from(r.r0) | u64::from(r.r1) << 32), 2.5);
    let sig = Signature::parse("@@:").unwrap();
    let call = prepare_host_call(&sig, plain(), &words(&[1, 2]), &mut b).unwrap();
    let r = finish_host_call(call, &HostRet { x: [(HOST_ID + 0x4444) as u64, 0], d: [0; 4] }, &mut b).unwrap();
    assert_eq!(r.r0, 0x4444);
}

/// NSNotFound is NSIntegerMax: 0x7fffffff for a 32-bit game, not the low
/// half of the host's 64-bit one. Only where it means "not found": results
/// of rows marked `not-found` (indexOfObject: and the like) and NSRange
/// locations; other numbers and range lengths convert as they are.
#[test]
fn not_found_reaches_32_bit_games_as_their_own() {
    let mut b = Fake::new();
    let not_found = isize::MAX as u64;
    let index = HostCallOptions { not_found: true, ..plain() };
    let sig = Signature::parse("I@:@").unwrap();
    let call = prepare_host_call(&sig, index, &words(&[1, 2, 3]), &mut b).unwrap();
    let r = finish_host_call(call, &HostRet { x: [not_found, 0], d: [0; 4] }, &mut b).unwrap();
    assert_eq!(r.r0, 0x7fff_ffff, "an index result");
    let call = prepare_host_call(&sig, plain(), &words(&[1, 2, 3]), &mut b).unwrap();
    let r = finish_host_call(call, &HostRet { x: [not_found, 0], d: [0; 4] }, &mut b).unwrap();
    assert_eq!(r.r0, u32::MAX, "an ordinary number is cut to its low half, as on Apple");
    let sig = Signature::parse("{_NSRange=II}@:@").unwrap();
    let out = b.alloc(8);
    let call = prepare_host_call(&sig, plain(), &words(&[out, 1, 2, 3]), &mut b).unwrap();
    finish_host_call(call, &HostRet { x: [not_found, not_found], d: [0; 4] }, &mut b).unwrap();
    assert_eq!(b.get(out, 8), [0xff, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff, 0xff].to_vec(), "the location only");
    // The game's NSNotFound as a range location is the host's; other numbers stay.
    let sig = Signature::parse("v@:{_NSRange=II}i").unwrap();
    let call = prepare_host_call(&sig, plain(), &words(&[1, 2, 0x7fff_ffff, 0x7fff_ffff, 0x7fff_ffff]), &mut b).unwrap();
    assert_eq!(&call.args.x[2..5], &[not_found, 0x7fff_ffff, 0x7fff_ffff]);
}

#[test]
fn c_strings_and_out_parameters_cross_into_guest_memory() {
    let mut b = Fake::new();
    // UTF8String: the host's bytes are copied into guest memory.
    let text = std::ffi::CString::new("héllo").unwrap();
    let sig = Signature::parse("r*@:").unwrap();
    let call = prepare_host_call(&sig, plain(), &words(&[1, 2]), &mut b).unwrap();
    let r = finish_host_call(call, &HostRet { x: [text.as_ptr() as u64, 0], d: [0; 4] }, &mut b).unwrap();
    assert_eq!(b.get(r.r0, 7), text.as_bytes_with_nul());
    // A guest C string argument is passed as its host address.
    b.put(BASE + 0x100, b"abc\0");
    let sig = Signature::parse("@@:r*").unwrap();
    let call = prepare_host_call(&sig, plain(), &words(&[1, 2, BASE + 0x100]), &mut b).unwrap();
    assert_eq!(call.args.x[2] as usize, b.host_address(BASE + 0x100));
    // NSError **: the host writes a 64-bit id into a temporary; the guest's
    // 4-byte slot gets the mapped id after the call.
    // The slot is often an uninitialized local (`NSError *error;`): it is
    // never read; the host's temporary starts at a mark no object has, and
    // left so, the guest's slot is not written.
    let slot = BASE + 0x200;
    b.put(slot, &0xdead_beefu32.to_le_bytes());
    let sig = Signature::parse("c@:@^@").unwrap();
    let call = prepare_host_call(&sig, plain(), &words(&[1, 2, 3, slot]), &mut b).unwrap();
    let temp = call.args.x[3] as *mut u64;
    assert_ne!(unsafe { *temp }, 0xdead_beef, "the slot is not read");
    finish_host_call(call, &HostRet { x: [0, 0], d: [0; 4] }, &mut b).unwrap();
    assert_eq!(b.get(slot, 4), 0xdead_beefu32.to_le_bytes().to_vec(), "not set by the host: left alone");
    let call = prepare_host_call(&sig, plain(), &words(&[1, 2, 3, slot]), &mut b).unwrap();
    let temp = call.args.x[3] as *mut u64;
    unsafe { *temp = (HOST_ID + 0x5150) as u64 };
    let r = finish_host_call(call, &HostRet { x: [1, 0], d: [0; 4] }, &mut b).unwrap();
    assert_eq!((r.r0, b.get(slot, 4)), (1, 0x5150u32.to_le_bytes().to_vec()));
    // NSInteger *: an 8-byte temporary, the low half written back.
    let sig = Signature::parse("c@:^i").unwrap();
    let call = prepare_host_call(&sig, plain(), &words(&[1, 2, slot]), &mut b).unwrap();
    unsafe { *(call.args.x[2] as *mut i64) = -42 };
    finish_host_call(call, &HostRet { x: [1, 0], d: [0; 4] }, &mut b).unwrap();
    assert_eq!(b.get(slot, 4), (-42i32).to_le_bytes().to_vec());
}

#[test]
fn opaque_host_pointers_cross_as_guest_words_both_ways() {
    let mut b = Fake::new();
    // -[UIImage CGImage] returns a host pointer outside guest memory; the guest
    // gets a word for it, and that word goes back as the same host pointer.
    let image = 0x7_1234_5670usize;
    let sig = Signature::parse("^{CGImage=}@:").unwrap();
    let call = prepare_host_call(&sig, plain(), &words(&[1, 2]), &mut b).unwrap();
    let r = finish_host_call(call, &HostRet { x: [image as u64, 0], d: [0; 4] }, &mut b).unwrap();
    assert_eq!(b.opaque_to_host(r.r0), Some(image));
    let sig = Signature::parse("@@:^{CGImage=}").unwrap();
    let call = prepare_host_call(&sig, plain(), &words(&[1, 2, r.r0]), &mut b).unwrap();
    assert_eq!(call.args.x[2] as usize, image);
    // A pointer into guest memory stays a guest address.
    let call = prepare_host_call(&sig, plain(), &words(&[1, 2, BASE + 0x40]), &mut b).unwrap();
    assert_eq!(call.args.x[2] as usize, b.host_address(BASE + 0x40));
}

#[test]
fn counted_id_arrays_and_fast_enumeration() {
    let mut b = Fake::new();
    // +arrayWithObjects:count: reads `count` ids from the pointer.
    let list = BASE + 0x300;
    b.put(list, &[0x11u32.to_le_bytes(), 0x22u32.to_le_bytes(), 0x33u32.to_le_bytes()].concat());
    let sig = Signature::parse("@@:^@I").unwrap();
    let options = HostCallOptions { arrays: IdArrays::In, ..plain() };
    let call = prepare_host_call(&sig, options, &words(&[1, 2, list, 3]), &mut b).unwrap();
    let ids = unsafe { std::slice::from_raw_parts(call.args.x[2] as *const u64, 3) };
    assert_eq!(ids, [(HOST_ID + 0x11) as u64, (HOST_ID + 0x22) as u64, (HOST_ID + 0x33) as u64]);
    assert_eq!(call.args.x[3], 3);

    // -countByEnumeratingWithState:objects:count: fills up to `count` ids
    // (the guest buffer is not read first) and points the state's itemsPtr
    // and mutationsPtr into the temporaries; the guest gets its own buffer
    // and its own state back.
    let (state, buffer) = (BASE + 0x400, BASE + 0x500);
    b.put(buffer, &[0xee; 16 * 4]);
    let sig = Signature::parse("I@:^{NSFastEnumerationState=I^@^I[5I]}^@I").unwrap();
    let options = HostCallOptions { arrays: IdArrays::Out, ..plain() };
    let call = prepare_host_call(&sig, options, &words(&[1, 2, state, buffer, 16]), &mut b).unwrap();
    let host_state = call.args.x[2] as *mut u64;
    let host_buffer = call.args.x[3] as *mut u64;
    unsafe {
        *host_buffer.add(15) = (HOST_ID + 0x99) as u64;
        *host_buffer = (HOST_ID + 0x44) as u64;
        *host_state = 16;
        *host_state.add(1) = host_buffer as u64;
        *host_state.add(2) = host_state.add(3) as u64;
    }
    let r = finish_host_call(call, &HostRet { x: [16, 0], d: [0; 4] }, &mut b).unwrap();
    assert_eq!(r.r0, 16);
    assert_eq!(b.get(buffer, 4), 0x44u32.to_le_bytes());
    assert_eq!(b.get(buffer + 60, 4), 0x99u32.to_le_bytes());
    let fields: Vec<u32> = b.get(state, 12).chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(fields, [16, buffer, state + 12]);
}

/// -getObjects:range: (`out-array` with no integer argument): the array
/// holds as many ids as the range's length, each written back as a guest id.
#[test]
fn apple_id_arrays_counted_by_a_range_length() {
    let mut b = Fake::new();
    let row = super::selectors::FrameworkSelectors::builtin().get("getObjects:range:").expect("a getObjects:range: row");
    let options = HostCallOptions { arrays: row.arrays, ..plain() };
    let buffer = BASE + 0x600;
    b.put(buffer, &[0xee; 3 * 4]);
    let call = prepare_host_call(&row.signature, options, &words(&[1, 2, buffer, 4, 3]), &mut b).unwrap();
    assert_eq!(call.out_counts(), [3], "room for the range's 3 ids");
    let host_buffer = call.args.x[2] as *mut u64;
    unsafe {
        for (i, id) in [0x51u32, 0x52, 0x53].iter().enumerate() {
            *host_buffer.add(i) = (HOST_ID + *id as usize) as u64;
        }
    }
    finish_host_call(call, &HostRet { x: [0, 0], d: [0; 4] }, &mut b).unwrap();
    let ids: Vec<u32> = b.get(buffer, 12).chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(ids, [0x51, 0x52, 0x53]);
}

/// CGFloat arrays: -setLineDash:count:phase: reads `count` 32-bit floats as
/// the host's doubles (`in-array`); -getLineDash:count:phase: gets as many
/// back as the host writes to the count (`counted-out`), never more; a C
/// `float *` (`float`, -[NSScanner scanFloat:]) stays 4 bytes.
#[test]
fn cgfloat_arrays_cross_as_doubles() {
    let mut b = Fake::new();
    let floats = |values: &[f32]| values.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>();
    let pattern = BASE + 0x300;
    b.put(pattern, &floats(&[4.0, 2.5]));
    let sig = Signature::parse("v@:^fif").unwrap();
    let options = HostCallOptions { arrays: IdArrays::In, ..plain() };
    let call = prepare_host_call(&sig, options, &words(&[1, 2, pattern, 2, 1.0f32.to_bits()]), &mut b).unwrap();
    assert_eq!(unsafe { std::slice::from_raw_parts(call.args.x[2] as *const f64, 2) }, [4.0, 2.5]);

    let (out, count, phase) = (BASE + 0x400, BASE + 0x440, BASE + 0x450);
    b.put(out, &[0xee; 16]);
    let sig = Signature::parse("v@:^f^i^f").unwrap();
    let options = HostCallOptions { arrays: IdArrays::CountedOut, ..plain() };
    let call = prepare_host_call(&sig, options, &words(&[1, 2, out, count, phase]), &mut b).unwrap();
    unsafe {
        let host = call.args.x[2] as *mut f64;
        (*host, *host.add(1), *host.add(2)) = (3.0, 1.5, 9.0);
        *(call.args.x[3] as *mut i64) = 2;
        *(call.args.x[4] as *mut f64) = 0.5;
    }
    finish_host_call(call, &HostRet { x: [0, 0], d: [0; 4] }, &mut b).unwrap();
    assert_eq!(b.get(out, 12), [floats(&[3.0, 1.5]), vec![0xee; 4]].concat(), "two written, the third left alone");
    assert_eq!((b.get(count, 4), b.get(phase, 4)), (2i32.to_le_bytes().to_vec(), floats(&[0.5])));

    let sig = Signature::parse("c@:^f").unwrap();
    let call = prepare_host_call(&sig, HostCallOptions { real_float: true, ..plain() }, &words(&[1, 2, phase]), &mut b).unwrap();
    unsafe { *(call.args.x[2] as *mut f32) = 7.25 };
    finish_host_call(call, &HostRet { x: [1, 0], d: [0; 4] }, &mut b).unwrap();
    assert_eq!(b.get(phase, 4), floats(&[7.25]));
}

#[test]
fn variadic_arguments_go_to_stack_slots() {
    let mut b = Fake::new();
    b.texts.insert(0x300, "%d %@ %f %lld %s".into());
    let sig = Signature::parse("@@:@").unwrap();
    let double = 1.25f64.to_bits();
    let w = [1, 2, 0x300, (-3i32) as u32, 0x400, double as u32, (double >> 32) as u32, 9, 0, BASE + 0x10];
    let call = prepare_host_call(&sig, HostCallOptions { varargs: Varargs::Format, ..plain() }, &words(&w), &mut b).unwrap();
    assert_eq!(call.args.x[2], (HOST_ID + 0x300) as u64);
    assert_eq!(call.args.stack[0] as i32, -3);
    assert_eq!(call.args.stack[1], (HOST_ID + 0x400) as u64);
    assert_eq!(f64::from_bits(call.args.stack[2]), 1.25);
    assert_eq!(call.args.stack[3], 9);
    assert_eq!(call.args.stack[4] as usize, b.host_address(BASE + 0x10));
    // A predicate format's %K is a key path object, like %@ (not an int).
    b.texts.insert(0x310, "%K == %@ AND %K > %d".into());
    let w = [1, 2, 0x310, 0x500, 0x510, 0x520, (-1i32) as u32];
    let call = prepare_host_call(&sig, HostCallOptions { varargs: Varargs::Format, ..plain() }, &words(&w), &mut b).unwrap();
    assert_eq!(call.args.stack[..3], [(HOST_ID + 0x500) as u64, (HOST_ID + 0x510) as u64, (HOST_ID + 0x520) as u64]);
    assert_eq!(call.args.stack[3] as i32, -1);
    // nil-ended object lists end with a 0 slot.
    let call = prepare_host_call(&sig, HostCallOptions { varargs: Varargs::NilList, ..plain() }, &words(&[1, 2, 0x10, 0x20, 0x30, 0]), &mut b).unwrap();
    assert_eq!(call.args.stack, [(HOST_ID + 0x20) as u64, (HOST_ID + 0x30) as u64, 0]);
    // A nil first object (the last named argument, as SpellCraft's
    // otherButtonTitles:nil) is the whole list: the words after it are the
    // caller's own stack, not part of the list.
    let call = prepare_host_call(&sig, HostCallOptions { varargs: Varargs::NilList, ..plain() }, &words(&[1, 2, 0, 0x30, 0x40, 0]), &mut b).unwrap();
    assert_eq!(call.args.stack, Vec::<u64>::new());
}

/// A positional specifier ("%2$@") names one argument by place: the highest
/// place sets how many arguments are read, and each place keeps its kind.
#[test]
fn positional_format_arguments_are_counted_once() {
    assert_eq!(format_kinds("%1$@"), [FormatArg::Object]);
    assert_eq!(format_kinds("%2$d"), [FormatArg::Int, FormatArg::Int]);
    assert_eq!(format_kinds("%2$@ %1$d"), [FormatArg::Int, FormatArg::Object]);
    assert_eq!(format_kinds("%1$lld"), [FormatArg::Long]);
}

#[test]
fn host_calls_into_guest_methods_convert_both_ways() {
    let mut b = Fake::new();
    // -[GameView touchAt:scale:object:] called by the host: CGPoint in d0/d1,
    // a CGFloat in d2, an object in x2.
    let sig = Signature::parse("{CGSize=ff}16@0:4{CGPoint=ff}8f16@20").unwrap();
    let mut frame = HostFrame::default();
    frame.x[0] = (HOST_ID + 0x100) as u64;
    frame.x[1] = (HOST_SEL + 0x200) as u64;
    frame.x[2] = (HOST_ID + 0x300) as u64;
    frame.d[0] = 10.0f64.to_bits();
    frame.d[1] = 20.0f64.to_bits();
    frame.d[2] = 0.5f64.to_bits();
    let call = prepare_guest_call(&sig, &frame, &mut b).unwrap();
    // CGSize is 8 bytes: returned through a guest stret pointer in r0.
    let stret = call.words[0];
    assert_eq!(&call.words[1..], &[0x100, 0x200, f(10.0), f(20.0), f(0.5), 0x300]);
    b.put(stret, &[f(3.0).to_le_bytes(), f(4.0).to_le_bytes()].concat());
    finish_guest_call(call, stret, 0, &mut frame, &mut b).unwrap();
    assert_eq!((f64::from_bits(frame.ret.d[0]), f64::from_bits(frame.ret.d[1])), (3.0, 4.0));
    // A float argument the host passed as a C float (s register) is read as one.
    let sig = Signature::parse("c@:f").unwrap();
    let mut frame = HostFrame::default();
    frame.d[0] = u64::from(f(0.75));
    let call = prepare_guest_call(&sig, &frame, &mut b).unwrap();
    assert_eq!(call.words[2], f(0.75));
    finish_guest_call(call, 1, 0, &mut frame, &mut b).unwrap();
    assert_eq!(frame.ret.x[0] & 0xff, 1);
}

/// A raw pointer the game gave the host (+beginAnimations:context:'s
/// void *) comes back unchanged where the game's method says id
/// (SpellCraft's -encounterCardSlidOff:finished:context:(id), v20@0:4@8@12@16),
/// as Apple passes the same pointer on: not converted as a host object.
#[test]
fn a_game_pointer_comes_back_unchanged_where_its_method_says_id() {
    let mut b = Fake::new();
    let sig = Signature::parse("v20@0:4@8@12@16").unwrap();
    let context = BASE + 0x40;
    let mut frame = HostFrame::default();
    frame.x[..5].copy_from_slice(&[(HOST_ID + 1) as u64, (HOST_SEL + 2) as u64, (HOST_ID + 3) as u64, (HOST_ID + 4) as u64, b.host_address(context) as u64]);
    let call = prepare_guest_call(&sig, &frame, &mut b).unwrap();
    assert_eq!(&call.words[2..5], &[3, 4, context]);
}

#[test]
fn host_pointer_arguments_reach_guest_methods_as_typed_temporaries() {
    let mut b = Fake::new();
    // -getResourceValue:(id *)value forKey:(id)key error:(NSError **)error,
    // called by the host with slots in host memory.
    let sig = Signature::parse("c@:^@@^@").unwrap();
    let (mut value, mut error) = (0u64, 0u64);
    let mut frame = HostFrame::default();
    frame.x[..5].copy_from_slice(&[(HOST_ID + 1) as u64, (HOST_SEL + 2) as u64, &mut value as *mut u64 as u64, (HOST_ID + 3) as u64, &mut error as *mut u64 as u64]);
    let call = prepare_guest_call(&sig, &frame, &mut b).unwrap();
    let (value_temp, error_temp) = (call.words[2], call.words[4]);
    assert_eq!((b.get(value_temp, 4), b.get(error_temp, 4)), (vec![0; 4], vec![0; 4]), "guest temporaries start with the host's values");
    // The guest method stores its outputs and returns YES.
    b.put(value_temp, &0x77u32.to_le_bytes());
    b.put(error_temp, &0x88u32.to_le_bytes());
    finish_guest_call(call, 1, 0, &mut frame, &mut b).unwrap();
    assert_eq!((value, error), ((HOST_ID + 0x77) as u64, (HOST_ID + 0x88) as u64));
    // BOOL *stop: one host byte; its neighbour stays.
    let sig = Signature::parse("v@:^c").unwrap();
    let mut flags = [0u8, 0xaa];
    let mut frame = HostFrame::default();
    frame.x[2] = flags.as_mut_ptr() as u64;
    let call = prepare_guest_call(&sig, &frame, &mut b).unwrap();
    b.put(call.words[2], &[1]);
    finish_guest_call(call, 0, 0, &mut frame, &mut b).unwrap();
    assert_eq!(flags, [1, 0xaa]);
    // -scrollViewWillEndDragging:withVelocity:targetContentOffset: (CGPoint *):
    // the game reads and moves the target as two floats; the host sees doubles.
    let sig = Signature::parse("v20@0:4@8{CGPoint=ff}12^{CGPoint=ff}20").unwrap();
    let mut target = [60.0f64, 0.0];
    let mut frame = HostFrame::default();
    frame.x[..4].copy_from_slice(&[(HOST_ID + 1) as u64, (HOST_SEL + 2) as u64, (HOST_ID + 3) as u64, target.as_mut_ptr() as u64]);
    frame.d[0] = 0.5f64.to_bits();
    let call = prepare_guest_call(&sig, &frame, &mut b).unwrap();
    assert_eq!(&call.words[3..5], &[f(0.5), f(0.0)]);
    let temp = call.words[5];
    assert_eq!(b.get(temp, 8), [f(60.0).to_le_bytes(), f(0.0).to_le_bytes()].concat());
    b.put(temp, &[f(200.0).to_le_bytes(), f(0.0).to_le_bytes()].concat());
    finish_guest_call(call, 0, 0, &mut frame, &mut b).unwrap();
    assert_eq!(target, [200.0, 0.0]);
    // Left alone by the game, the host's doubles stay exactly as they were
    // (not rounded through floats), so the host sees no change.
    let mut target = [309.749916583f64, 0.1];
    frame.x[3] = target.as_mut_ptr() as u64;
    let call = prepare_guest_call(&sig, &frame, &mut b).unwrap();
    finish_guest_call(call, 0, 0, &mut frame, &mut b).unwrap();
    assert_eq!(target.map(f64::to_bits), [309.749916583f64, 0.1].map(f64::to_bits));
}

/// A remembered message's plan (worked out once) converts each later call
/// as a fresh look at its signature does: structs of CGFloats, C floats,
/// selectors, id out-parameters, NSRange both ways.
#[test]
fn a_call_plan_made_once_converts_each_call_like_a_fresh_one() {
    let mut b = Fake::new();
    let (slot, range_out) = (b.alloc(8), b.alloc(8));
    b.put(slot, &0x55u32.to_le_bytes());
    // (types, real float, argument words of two calls, x register holding a temporary, result)
    let cases: [(&str, bool, [Vec<u32>; 2], Option<usize>, HostRet); 5] = [
        ("v@:{CGRect={CGPoint=ff}{CGSize=ff}}", false, [vec![1, 2, f(1.0), f(2.0), f(3.0), f(4.0)], vec![5, 6, f(-1.5), f(0.0), f(9.0), f(0.25)]], None, HostRet::default()),
        ("f@:f", true, [vec![1, 2, f(0.5)], vec![3, 4, f(7.0)]], None, HostRet { x: [0; 2], d: [u64::from(f(2.5)), 0, 0, 0] }),
        (":@::", false, [vec![1, 2, 0x30], vec![1, 2, 0]], None, HostRet { x: [HOST_SEL as u64 + 0x40, 0], d: [0; 4] }),
        ("c@:@^@", false, [vec![1, 2, 3, slot], vec![1, 2, 0, slot]], Some(3), HostRet { x: [1, 0], d: [0; 4] }),
        ("{_NSRange=II}@:{_NSRange=II}", false, [vec![range_out, 1, 2, 3, 4], vec![range_out, 5, 6, 7, 8]], None, HostRet { x: [9, 10], d: [0; 4] }),
    ];
    for (types, real_float, calls, temp_at, ret) in cases {
        let sig = Signature::parse(types).unwrap();
        let options = HostCallOptions { real_float, ..plain() };
        let plan = std::sync::Arc::new(CallPlan::new(&sig, options).unwrap());
        for w in calls {
            let mut shapes = Vec::new();
            for planned in [true, false] {
                let call = if planned {
                    prepare_planned(&sig, &plan, options, &words(&w), &mut b).unwrap()
                } else {
                    prepare_host_call(&sig, options, &words(&w), &mut b).unwrap()
                };
                let mut x = call.args.x;
                if let Some(at) = temp_at {
                    x[at] = 0;
                }
                let (d, stack, out) = (call.args.d, call.args.stack.clone(), call.first_out_value());
                let r = finish_host_call(call, &ret, &mut b).unwrap();
                shapes.push((x, d, stack, out, r, b.get(slot, 4), b.get(range_out, 8)));
            }
            assert_eq!(shapes[0], shapes[1], "{types} with {w:?}");
        }
    }
}

/// A message's arguments and result are worked on in place: the bytes of a
/// common argument or result (up to a CGAffineTransform) take no heap
/// memory, as each memory request cost time on every message.
#[test]
fn common_arguments_and_results_take_no_heap() {
    let list = [7u32; 16];
    for size in [1, 4, 8, 16, 24] {
        assert!(guest_bytes(&words(&list), 0, size).is_inline(), "{size} guest bytes");
    }
    let ret = HostRet { x: [1, 2], d: [3, 4, 5, 6] };
    let rect = HostTy::Struct { fields: (0..4).map(|i| (i * 8, HostTy::F64)).collect(), size: 32, align: 8 };
    for hty in [HostTy::Ptr, HostTy::Int { size: 4, signed: true }, HostTy::F64, rect] {
        assert!(host_result_bytes(&hty, &ret, false, None).is_inline(), "{hty:?} result");
    }
}
