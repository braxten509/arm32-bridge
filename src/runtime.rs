//! The 32-bit CPU (Unicorn) plus the bridge between guest code and host
//! functions.
//!
//! Every phone function the game imports gets an 8-byte ARM stand-in in the
//! stub area: `svc #index; bx lr`. When guest code calls it, the `svc` traps
//! into `on_interrupt`, which reads the guest's arguments (AAPCS softfp: r0-r3
//! then the stack), runs the Rust handler, and writes the result to r0/r1.
//! The `bx lr` then returns to the caller as if a real function had run.

use std::collections::HashMap;

use unicorn_engine::{ArmCpuModel, Arch, HookType, MemType, Mode, Prot, RegisterARM, Unicorn};

use crate::mem::{self, Bump, GuestHeap, GuestMem};

pub type Uc = Unicorn<'static, Runtime>;

/// What a handler hands back to the guest.
pub enum Ret {
    Void,
    U32(u32),
    U64(u64),
    F32(f32),
    F64(f64),
    /// The handler already set the registers itself (for example longjmp).
    Jumped,
}

pub type Handler = fn(&mut Uc, &mut Args) -> Ret;

pub struct Stub {
    pub name: String,
    pub handler: Option<Handler>,
    pub calls: u64,
}

/// Return traps: one per nesting depth, so a host→guest call made from
/// inside a handler stops at its own return address.
const MAX_DEPTH: u32 = 64;
const FIRST_STUB: u32 = MAX_DEPTH;
const SVC: u32 = 0xEF00_0000;
const BX_LR: u32 = 0xE12F_FF1E;
const UDF: u32 = 0xE7F0_00F0;

pub struct Runtime {
    pub mem: GuestMem,
    pub heap: GuestHeap,
    pub misc: Bump,
    pub stubs: Vec<Stub>,
    pub stub_by_name: HashMap<String, u32>,
    /// Exported symbols of loaded guest libraries (name → guest address).
    pub guest_exports: HashMap<String, u32>,
    pub depth: u32,
    pub fault: Option<String>,
    pub exit_code: Option<i32>,
    pub trace: bool,
    /// Order in which phone functions were first used.
    pub first_use: Vec<u32>,
    /// Names called that have no handler yet (they return 0).
    pub missing: Vec<String>,
    pub recent: std::collections::VecDeque<String>,
    /// Name of the phone function being handled right now.
    pub current_call: String,
    pub errno_slot: u32,
    pub tls_block: u32,
    pub state: crate::imports::State,
}

impl Runtime {
    pub fn misc_alloc(&mut self, size: u32, align: u32) -> u32 {
        self.misc.alloc(size, align)
    }
    pub fn misc_cstr(&mut self, text: &str) -> u32 {
        let guest = self.misc.alloc(text.len() as u32 + 1, 1);
        self.mem.write_bytes(guest, text.as_bytes());
        self.mem.write_u8(guest + text.len() as u32, 0);
        guest
    }
}

pub fn new_cpu(trace: bool) -> Uc {
    let mem = GuestMem::reserve();
    let runtime = Runtime {
        mem,
        heap: GuestHeap::new(mem),
        misc: Bump::new(mem::MISC_BASE, mem::MISC_END),
        stubs: Vec::new(),
        stub_by_name: HashMap::new(),
        guest_exports: HashMap::new(),
        depth: 0,
        fault: None,
        exit_code: None,
        trace,
        first_use: Vec::new(),
        missing: Vec::new(),
        recent: Default::default(),
        current_call: String::new(),
        errno_slot: 0,
        tls_block: 0,
        state: Default::default(),
    };
    let mut uc = Unicorn::new_with_data(Arch::ARM, Mode::ARM, runtime).expect("start Unicorn");
    uc.ctl_set_cpu_model(ArmCpuModel::CORTEX_A15 as i32).expect("pick Cortex-A15 (ARMv7 + NEON)");

    for (start, end) in mem::MAPPED_REGIONS {
        unsafe {
            uc.mem_map_ptr(start as u64, (end - start) as u64, Prot::ALL, mem.host(start).cast())
                .expect("map guest region");
        }
    }

    // Turn on the floating point / NEON unit: CPACR full access to cp10+cp11, FPEXC.EN.
    uc.reg_write(RegisterARM::C1_C0_2, 0x00F0_0000).unwrap();
    uc.reg_write(RegisterARM::FPEXC, 0x4000_0000).unwrap();

    // Return traps, then stubs are appended by `stub_for`.
    for depth in 0..MAX_DEPTH {
        mem.write_u32(trap_address(depth), UDF);
        mem.write_u32(trap_address(depth) + 4, UDF);
    }

    // Per-thread data: errno and a TLS block (bionic reads TPIDRURO).
    let errno_slot = uc.get_data_mut().misc_alloc(4, 4);
    let tls_block = uc.get_data_mut().misc_alloc(0x100, 8);
    uc.get_data_mut().errno_slot = errno_slot;
    uc.get_data_mut().tls_block = tls_block;
    uc.reg_write(RegisterARM::C13_C0_3, tls_block as u64).unwrap();

    uc.add_intr_hook(on_interrupt).expect("add stub hook");
    uc.add_mem_hook(HookType::MEM_INVALID, 0, u64::MAX, on_bad_memory).expect("add fault hook");
    crate::imports::install_data(&mut uc);
    uc
}

fn trap_address(depth: u32) -> u32 {
    mem::STUB_BASE + depth * 8
}

/// The guest address of the stand-in for `name`, creating it on first use.
pub fn stub_for(uc: &mut Uc, name: &str) -> u32 {
    if let Some(&index) = uc.get_data().stub_by_name.get(name) {
        return mem::STUB_BASE + index * 8;
    }
    let runtime = uc.get_data_mut();
    let index = FIRST_STUB + runtime.stubs.len() as u32;
    assert!(index * 8 < mem::STUB_SIZE, "too many stubs");
    runtime.stubs.push(Stub { name: name.to_string(), handler: crate::imports::handler_for(name), calls: 0 });
    runtime.stub_by_name.insert(name.to_string(), index);
    let address = mem::STUB_BASE + index * 8;
    runtime.mem.write_u32(address, SVC | index);
    runtime.mem.write_u32(address + 4, BX_LR);
    address
}

/// Hooks get the engine with a borrowed lifetime; ours is always `'static`.
fn as_static<'a, 'b>(uc: &'a mut Unicorn<'b, Runtime>) -> &'a mut Uc {
    unsafe { std::mem::transmute::<&'a mut Unicorn<'b, Runtime>, &'a mut Uc>(uc) }
}

fn on_interrupt(uc: &mut Unicorn<'_, Runtime>, interrupt: u32) {
    let uc = as_static(uc);
    let pc = uc.pc_read().unwrap() as u32;
    // EXCP_SWI is 2; the PC already points past the svc.
    let svc_address = pc.wrapping_sub(4);
    let in_stub_area = svc_address >= mem::STUB_BASE && svc_address < mem::STUB_BASE + mem::STUB_SIZE;
    if interrupt != 2 || !in_stub_area {
        let lr = uc.reg_read(RegisterARM::LR).unwrap();
        stop_with_fault(uc, format!("CPU exception {interrupt} at pc {pc:#010x} (lr {lr:#010x})"));
        return;
    }
    let index = uc.get_data().mem.read_u32(svc_address) & 0x00FF_FFFF;
    let stub_index = (index - FIRST_STUB) as usize;
    let (handler, name) = {
        let runtime = uc.get_data_mut();
        let stub = &mut runtime.stubs[stub_index];
        stub.calls += 1;
        let first = stub.calls == 1;
        let name = stub.name.clone();
        let handler = stub.handler;
        if first {
            runtime.first_use.push(index);
        }
        if runtime.recent.len() == 32 {
            runtime.recent.pop_front();
        }
        runtime.recent.push_back(name.clone());
        runtime.current_call = name.clone();
        (handler, name)
    };

    let mut args = Args::from_registers(uc);
    if uc.get_data().trace {
        let r = args.registers;
        eprintln!("[call] {name}({:#x}, {:#x}, {:#x}, {:#x})", r[0], r[1], r[2], r[3]);
    }
    let result = match handler {
        Some(handler) => handler(uc, &mut args),
        None => {
            let runtime = uc.get_data_mut();
            if !runtime.missing.contains(&name) {
                runtime.missing.push(name.clone());
                eprintln!("[bridge] no handler yet for {name}; returning 0");
            }
            Ret::U32(0)
        }
    };
    match result {
        Ret::Void | Ret::Jumped => {}
        Ret::U32(value) => uc.reg_write(RegisterARM::R0, value as u64).unwrap(),
        Ret::U64(value) => {
            uc.reg_write(RegisterARM::R0, value & 0xFFFF_FFFF).unwrap();
            uc.reg_write(RegisterARM::R1, value >> 32).unwrap();
        }
        Ret::F32(value) => uc.reg_write(RegisterARM::R0, value.to_bits() as u64).unwrap(),
        Ret::F64(value) => {
            let bits = value.to_bits();
            uc.reg_write(RegisterARM::R0, bits & 0xFFFF_FFFF).unwrap();
            uc.reg_write(RegisterARM::R1, bits >> 32).unwrap();
        }
    }
    if uc.get_data().fault.is_some() || uc.get_data().exit_code.is_some() {
        uc.emu_stop().unwrap();
    }
}

fn on_bad_memory(uc: &mut Unicorn<'_, Runtime>, kind: MemType, address: u64, size: usize, _value: i64) -> bool {
    let uc = as_static(uc);
    let pc = uc.pc_read().unwrap_or(0);
    let lr = uc.reg_read(RegisterARM::LR).unwrap_or(0);
    let message = format!("bad memory access ({kind:?}, {size} bytes at {address:#010x}) at pc {pc:#010x} (lr {lr:#010x})");
    if uc.get_data().fault.is_none() {
        uc.get_data_mut().fault = Some(message);
    }
    false
}

pub fn stop_with_fault(uc: &mut Uc, message: String) {
    if uc.get_data().fault.is_none() {
        uc.get_data_mut().fault = Some(message);
    }
    uc.emu_stop().unwrap();
}

/// Reads arguments the way an ARM32 softfp caller passes them.
pub struct Args {
    pub registers: [u32; 4],
    next_register: usize,
    stack: u32,
    mem: GuestMem,
}

impl Args {
    pub fn from_registers(uc: &Uc) -> Args {
        let r = |reg| uc.reg_read(reg).unwrap() as u32;
        Args {
            registers: [r(RegisterARM::R0), r(RegisterARM::R1), r(RegisterARM::R2), r(RegisterARM::R3)],
            next_register: 0,
            stack: r(RegisterARM::SP),
            mem: uc.get_data().mem,
        }
    }
}

/// A source of 32-bit-ABI arguments: live registers, or a guest `va_list`.
pub trait ArgSource {
    fn u32(&mut self) -> u32;
    fn u64(&mut self) -> u64;
    fn i32(&mut self) -> i32 {
        self.u32() as i32
    }
    fn i64(&mut self) -> i64 {
        self.u64() as i64
    }
    fn f64(&mut self) -> f64 {
        f64::from_bits(self.u64())
    }
    fn f32(&mut self) -> f32 {
        f32::from_bits(self.u32())
    }
}

impl ArgSource for Args {
    fn u32(&mut self) -> u32 {
        if self.next_register < 4 {
            self.next_register += 1;
            return self.registers[self.next_register - 1];
        }
        let value = self.mem.read_u32(self.stack);
        self.stack += 4;
        value
    }
    fn u64(&mut self) -> u64 {
        // 64-bit values use an even register pair, or an 8-aligned stack slot.
        if self.next_register < 4 {
            self.next_register = (self.next_register + 1) & !1;
        }
        if self.next_register < 4 {
            let low = self.registers[self.next_register] as u64;
            let high = self.registers[self.next_register + 1] as u64;
            self.next_register += 2;
            return low | (high << 32);
        }
        self.next_register = 4;
        self.stack = (self.stack + 7) & !7;
        let value = self.mem.read_u64(self.stack);
        self.stack += 8;
        value
    }
}

/// A guest `va_list` (on ARM32 it is just a pointer into the argument area).
pub struct VaList {
    pub pointer: u32,
    pub mem: GuestMem,
}

impl ArgSource for VaList {
    fn u32(&mut self) -> u32 {
        let value = self.mem.read_u32(self.pointer);
        self.pointer += 4;
        value
    }
    fn u64(&mut self) -> u64 {
        self.pointer = (self.pointer + 7) & !7;
        let value = self.mem.read_u64(self.pointer);
        self.pointer += 8;
        value
    }
}

/// Calls guest code at `address` (bit 0 set = Thumb) with 32-bit arguments.
/// Safe to use from inside a handler: the CPU state is saved and restored.
pub fn call_guest(uc: &mut Uc, address: u32, args: &[u32]) -> Result<u64, String> {
    if let Some(fault) = &uc.get_data().fault {
        return Err(fault.clone());
    }
    let depth = uc.get_data().depth;
    if depth >= MAX_DEPTH {
        return Err("guest calls nested too deeply".into());
    }
    let saved = if depth > 0 {
        let mut context = uc.context_alloc().map_err(|e| format!("{e:?}"))?;
        uc.context_save(&mut context).map_err(|e| format!("{e:?}"))?;
        Some(context)
    } else {
        None
    };

    let mut sp = if depth > 0 {
        (uc.reg_read(RegisterARM::SP).unwrap() as u32 - 256) & !7
    } else {
        mem::STACK_TOP - 64
    };
    let stack_args = args.len().saturating_sub(4);
    sp = (sp - stack_args as u32 * 4) & !7;
    let memory = uc.get_data().mem;
    for (i, value) in args.iter().skip(4).enumerate() {
        memory.write_u32(sp + i as u32 * 4, *value);
    }
    let registers = [RegisterARM::R0, RegisterARM::R1, RegisterARM::R2, RegisterARM::R3];
    for (i, value) in args.iter().take(4).enumerate() {
        uc.reg_write(registers[i], *value as u64).unwrap();
    }
    let trap = trap_address(depth);
    uc.reg_write(RegisterARM::SP, sp as u64).unwrap();
    uc.reg_write(RegisterARM::LR, trap as u64).unwrap();

    uc.get_data_mut().depth = depth + 1;
    let result = uc.emu_start(address as u64, trap as u64, 0, 0);
    uc.get_data_mut().depth = depth;

    let pc = uc.pc_read().unwrap() as u32;
    let r0 = uc.reg_read(RegisterARM::R0).unwrap();
    let r1 = uc.reg_read(RegisterARM::R1).unwrap();
    if let Some(context) = saved {
        uc.context_restore(&context).map_err(|e| format!("{e:?}"))?;
    }
    if let Some(fault) = &uc.get_data().fault {
        return Err(fault.clone());
    }
    if uc.get_data().exit_code.is_some() {
        return Err(format!("guest called exit({})", uc.get_data().exit_code.unwrap()));
    }
    if let Err(error) = result {
        let message = format!("CPU stopped with {error:?} at pc {pc:#010x}");
        uc.get_data_mut().fault = Some(message.clone());
        return Err(message);
    }
    if pc != trap {
        return Err(format!("guest stopped at {pc:#010x} instead of returning"));
    }
    Ok(r0 | (r1 << 32))
}
