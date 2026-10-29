//! Fixes known Java-side problems of old games directly in their dex files.
//!
//! Fix 1 - AudioAttributes FLAG_HW_AV_SYNC (0x10): cocos2d-x 3.x builds its
//! SoundPool with setFlags(16), which asks for a hardware A/V sync output.
//! Phones without one refuse every sound effect. The bit is cleared from a
//! constant passed straight to AudioAttributes.Builder.setFlags.
//!
//! Fix 2 - cocos2d-x Cocos2dxSound.LOAD_TIME_OUT (5000 ms): the game thread
//! waits until Android has decoded a new effect, which froze the game for
//! 76-500 ms per effect. The wait becomes 1 ms; the effect still plays as soon
//! as its load-complete listener fires.
//!
//! Fix 3 - frame limiters: a Thread.sleep inside a GLSurfaceView renderer's
//! onDrawFrame only ever holds the game below the screen's rate. The call
//! becomes nops, so the game draws at the screen's rate.
//!
//! Fix 4 - expansion-file download screen: games built with Google Play's APK
//! Expansion kit first open a download screen that fetches the game data from
//! Play. Copies with the data already inside the APK still open it and get
//! stuck there. Only for APKs over Play's 100 MB limit for an APK that comes
//! with expansion files, which therefore must carry their data themselves:
//! every `const-class` of the download screen outside the screen itself
//! becomes the screen the download screen opens when it's done (the game).
//!
//! Fix 5 - text-to-number helpers that call themselves: a method taking one
//! String and returning a number whose first call is to itself with the same
//! arguments can only end in StackOverflowError (Assassin's Creed Pirates'
//! AlarmService.SafeParseInt, run at every start once the game has saved
//! reminders). The call becomes Java's own parser for that number type
//! (Integer.parseInt and so on), when the dex already refers to it.
//!
//! Fix 6 - old Play services' blocking ad-ID wait: the class behind
//! AdvertisingIdClient and GoogleAuthUtil (com.google.android.gms.common.a)
//! waits in BlockingQueue.take() for a ServiceConnection callback that Android
//! delivers on the main thread. When the main thread is itself stuck waiting
//! on a thread that waits here (Facebook's AppEventsLogger holds its lock while
//! the game resumes), the two deadlock and the game freezes on a black screen.
//! Offline old games don't need the ad ID, so the wait becomes the library's
//! own failure: the branch that skips `throw new IllegalStateException()` is
//! noped, and the method always throws. When the dex has `new
//! RemoteException()` that is thrown instead, which old Play services report
//! as IOException, the "not available" every caller handles.

use std::collections::HashMap;

const FLAG_HW_AV_SYNC: i32 = 0x10;
const DOWNLOADER_CLIENT: &str = "Lcom/google/android/vending/expansion/downloader/IDownloaderClient;";
const INTENT_FOR_CLASS: &str = "Landroid/content/Intent;.<init>:(Landroid/content/Context;Ljava/lang/Class;)V";
/// Play services' package, where the blocking service connection lives (fix 6).
const PLAY_SERVICES: &str = "Lcom/google/android/gms/";
/// The interface the blocking service connection implements (fix 6).
const SERVICE_CONNECTION: &str = "Landroid/content/ServiceConnection;";
/// The exception the blocked ad-ID wait throws once the branch is noped, and the one it throws
/// instead when the dex has its constructor (fix 6).
const ILLEGAL_STATE: &str = "Ljava/lang/IllegalStateException;";
const ILLEGAL_STATE_INIT: &str = "Ljava/lang/IllegalStateException;.<init>:()V";
const REMOTE_EXCEPTION: &str = "Landroid/os/RemoteException;";
const REMOTE_EXCEPTION_INIT: &str = "Landroid/os/RemoteException;.<init>:()V";
/// Google Play's size limit for an APK that comes with expansion files.
pub const PLAY_EXPANSION_APK_LIMIT: u64 = 100 * 1024 * 1024;
const SHORT_LOAD_WAIT: i32 = 1;

/// One change made to a dex file.
pub struct Fix {
    pub offset: usize,
    pub reason: &'static str,
    pub old: String,
    pub new: String,
}

struct Dex<'a> {
    data: &'a [u8],
}

impl<'a> Dex<'a> {
    fn u16(&self, at: usize) -> u16 {
        u16::from_le_bytes([self.data[at], self.data[at + 1]])
    }
    fn u32(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.data[at..at + 4].try_into().unwrap())
    }
    fn uleb(&self, at: &mut usize) -> u32 {
        let mut value = 0u32;
        for shift in (0..35).step_by(7) {
            let byte = self.data[*at];
            *at += 1;
            value |= u32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                break;
            }
        }
        value
    }
    fn string(&self, index: u32) -> String {
        let mut at = self.u32(self.u32(0x3C) as usize + index as usize * 4) as usize;
        self.uleb(&mut at);
        let end = self.data[at..].iter().position(|&b| b == 0).map_or(self.data.len(), |n| at + n);
        String::from_utf8_lossy(&self.data[at..end]).into_owned()
    }
    fn type_name(&self, index: u32) -> String {
        self.string(self.u32(self.u32(0x44) as usize + index as usize * 4))
    }
    fn type_index(&self, name: &str) -> Option<u32> {
        (0..self.u32(0x40)).find(|&index| self.type_name(index) == name)
    }
    /// The type indices of the interfaces class definition `class` implements.
    fn interfaces(&self, class_def: usize) -> Vec<u32> {
        let list = self.u32(class_def + 12) as usize;
        if list == 0 {
            return Vec::new();
        }
        (0..self.u32(list) as usize).map(|i| self.u16(list + 4 + i * 2).into()).collect()
    }
    /// `Lclass;.name:type` like dexdump prints a field reference.
    fn field(&self, index: u32) -> String {
        let at = self.u32(0x54) as usize + index as usize * 8;
        let (class, kind, name) = (self.u16(at), self.u16(at + 2), self.u32(at + 4));
        format!("{}.{}:{}", self.type_name(class.into()), self.string(name), self.type_name(kind.into()))
    }
    /// The index of the method reference printed as `Lclass;.name:(params)return`.
    fn method_index(&self, full_name: &str) -> Option<u32> {
        (0..self.u32(0x58)).find(|&index| self.method(index).0 == full_name)
    }
    /// (`Lclass;.name:(params)return`, name, signature) of a method reference.
    fn method(&self, index: u32) -> (String, String, String) {
        let at = self.u32(0x5C) as usize + index as usize * 8;
        let (class, proto, name) = (self.u16(at), self.u16(at + 2), self.u32(at + 4));
        let proto_at = self.u32(0x4C) as usize + proto as usize * 12;
        let (returns, parameters) = (self.u32(proto_at + 4), self.u32(proto_at + 8) as usize);
        let mut signature = String::from("(");
        if parameters != 0 {
            for i in 0..self.u32(parameters) as usize {
                signature.push_str(&self.type_name(self.u16(parameters + 4 + i * 2).into()));
            }
        }
        signature.push(')');
        signature.push_str(&self.type_name(returns));
        let name = self.string(name);
        (format!("{}.{}:{}", self.type_name(class.into()), name, signature), name, signature)
    }
}

/// Length in 16-bit units of the instruction at `units[0]` (payloads included).
fn instruction_length(units: &[u16]) -> usize {
    let opcode = units[0] & 0xFF;
    match units[0] {
        0x0100 => return 4 + units[1] as usize * 2,
        0x0200 => return 2 + units[1] as usize * 4,
        0x0300 => {
            let width = units[1] as usize;
            let count = units[2] as usize | (units[3] as usize) << 16;
            return 4 + (count * width + 1) / 2;
        }
        _ => {}
    }
    match opcode {
        0x02 | 0x05 | 0x08 | 0x13 | 0x15 | 0x16 | 0x19 | 0x1A | 0x1C | 0x1F | 0x20 | 0x22 | 0x23 | 0x29 => 2,
        0x2D..=0x3D | 0x44..=0x6D | 0x90..=0xAF | 0xD0..=0xE2 | 0xFE | 0xFF => 2,
        0x03 | 0x06 | 0x09 | 0x14 | 0x17 | 0x1B | 0x24..=0x26 | 0x2A..=0x2C | 0x6E..=0x72 | 0x74..=0x78 | 0xFC | 0xFD => 3,
        0xFA | 0xFB => 4,
        0x18 => 5,
        _ => 1,
    }
}

/// The (method index, code offset) of every method with code in class definition `class_def`.
fn methods(dex: &Dex, class_def: usize) -> Vec<(u32, usize)> {
    let class_data = dex.u32(class_def + 24) as usize;
    if class_data == 0 {
        return Vec::new();
    }
    let mut at = class_data;
    let counts: Vec<u32> = (0..4).map(|_| dex.uleb(&mut at)).collect();
    for _ in 0..counts[0] + counts[1] {
        dex.uleb(&mut at);
        dex.uleb(&mut at);
    }
    let mut found = Vec::new();
    for list in [counts[2], counts[3]] {
        let mut method_index = 0;
        for _ in 0..list {
            method_index += dex.uleb(&mut at);
            dex.uleb(&mut at);
            let code = dex.uleb(&mut at) as usize;
            if code != 0 {
                found.push((method_index, code));
            }
        }
    }
    found
}

/// The instructions of the method whose code item starts at `code`: (offset in the file, units).
fn instructions(dex: &Dex, code: usize) -> Vec<(usize, Vec<u16>)> {
    let size = dex.u32(code + 12) as usize;
    let start = code + 16;
    let units: Vec<u16> = (0..size).map(|i| dex.u16(start + i * 2)).collect();
    let mut list = Vec::new();
    let mut pc = 0;
    while pc < size {
        let length = instruction_length(&units[pc..]).max(1).min(size - pc);
        list.push((start + pc * 2, units[pc..pc + length].to_vec()));
        pc += length;
    }
    list
}

/// Fix 4: expansion-file download screens -> the screen each opens when its download is done.
fn download_screens(dex: &Dex) -> HashMap<u32, u32> {
    let (class_count, class_defs) = (dex.u32(0x60) as usize, dex.u32(0x64) as usize);
    let mut targets: HashMap<u32, Vec<u32>> = HashMap::new();
    for class in 0..class_count {
        let class_def = class_defs + class * 32;
        let own_type = dex.u32(class_def);
        if !dex.interfaces(class_def).iter().any(|&index| dex.type_name(index) == DOWNLOADER_CLIENT) {
            continue;
        }
        // `const-class vA, X` followed by `new Intent(context, X)`: the screen it opens.
        for (_, code) in methods(dex, class_def) {
            let list = instructions(dex, code);
            for pair in list.windows(2) {
                let (first, next) = (&pair[0].1, &pair[1].1);
                if first[0] & 0xFF == 0x1C
                    && u32::from(first[1]) != own_type
                    && next[0] & 0xFF == 0x70
                    && dex.method(next[1].into()).0 == INTENT_FOR_CLASS
                {
                    targets.entry(own_type).or_default().push(first[1].into());
                }
            }
        }
    }
    let opened: Vec<u32> = targets.values().flatten().copied().collect();
    targets
        .into_iter()
        .filter_map(|(screen, mut to)| {
            to.sort_unstable();
            to.dedup();
            // Exactly one place to go, and never a screen that another download screen opens
            // (the game's own screen can take part in the download kit too).
            (to.len() == 1 && !opened.contains(&screen)).then(|| (screen, to[0]))
        })
        .collect()
}

/// True when the game's own code starts Google Play's expansion-file download: a call to the
/// kit's `startDownloadServiceIfRequired` from outside the kit and outside a broadcast receiver.
/// The kit's sample alarm receiver only restarts a download that was already running, and games
/// that ship the kit unused (Puzzle Craft 2) have nothing else calling it; games that need their
/// data file (Assassin's Creed Pirates) call it from their loading screen.
pub fn starts_expansion_download(data: &[u8]) -> bool {
    let dex = Dex { data };
    let (class_count, class_defs) = (dex.u32(0x60) as usize, dex.u32(0x64) as usize);
    (0..class_count).any(|class| {
        let class_def = class_defs + class * 32;
        if dex.type_name(dex.u32(class_def)).starts_with("Lcom/google/android/vending/") {
            return false;
        }
        methods(&dex, class_def).into_iter().any(|(method_index, code)| {
            dex.method(method_index).1 != "onReceive"
                && instructions(&dex, code).iter().any(|(_, units)| {
                    matches!(units[0] & 0xFF, 0x6E..=0x72 | 0x74..=0x78) && units.len() > 1 && {
                        let called = dex.method(units[1].into()).0;
                        called.starts_with("Lcom/google/android/vending/expansion/downloader/")
                            && called.contains(".startDownloadServiceIfRequired:")
                    }
                })
        })
    })
}

/// True when the game's code holds the web address of a game data file (`http…/….obb`): it
/// downloads its data file itself (PvZ 2 mods fetch theirs from GitHub on first start), so it
/// doesn't need the one Google Play used to deliver, even when it also calls the download kit.
pub fn fetches_own_data(data: &[u8]) -> bool {
    let dex = Dex { data };
    (0..dex.u32(0x38)).any(|index| {
        let text = dex.string(index);
        (text.starts_with("http://") || text.starts_with("https://")) && text.ends_with(".obb")
    })
}

/// Finds every fix to make in a dex file. `carries_game_data`: the APK is too big to rely on
/// expansion files, so their download screen can be skipped (fix 4).
pub fn find_fixes(data: &[u8], carries_game_data: bool) -> Vec<Fix> {
    let dex = Dex { data };
    let mut fixes = Vec::new();
    let mut seen_code = std::collections::HashSet::new();
    let skipped_screens = if carries_game_data { download_screens(&dex) } else { HashMap::new() };
    let (class_count, class_defs) = (dex.u32(0x60) as usize, dex.u32(0x64) as usize);
    for class in 0..class_count {
        let class_def = class_defs + class * 32;
        let own_type = dex.u32(class_def);
        // Fix 6 only in Play services' own classes: a game's own connection may wait on purpose.
        let service_connection = dex.type_name(own_type).starts_with(PLAY_SERVICES)
            && dex.interfaces(class_def).iter().any(|&index| dex.type_name(index) == SERVICE_CONNECTION);
        for (method_index, code) in methods(&dex, class_def) {
            if seen_code.insert(code) {
                let (_, name, signature) = dex.method(method_index);
                scan_code(&dex, code, &name, &signature, &mut fixes);
                if let Some(fix) = self_calling_parser(&dex, code, method_index, &signature) {
                    fixes.push(fix);
                }
                if service_connection && signature == "()Landroid/os/IBinder;" {
                    fixes.extend(blocking_ad_id_wait(&dex, code));
                }
                for (offset, units) in instructions(&dex, code) {
                    let Some(&target) = skipped_screens.get(&u32::from(units.get(1).copied().unwrap_or(0))) else { continue };
                    if units[0] & 0xFF == 0x1C && u32::from(units[1]) != own_type {
                        fixes.push(Fix {
                            offset,
                            reason: "expansion-file download screen skipped (the APK already has the game data)",
                            old: dex.type_name(units[1].into()),
                            new: dex.type_name(target),
                        });
                    }
                }
            }
        }
    }
    fixes
}

/// Java's parser for each number type a String can be turned into.
fn number_parser(signature: &str) -> Option<&'static str> {
    Some(match signature {
        "(Ljava/lang/String;)I" => "Ljava/lang/Integer;.parseInt:(Ljava/lang/String;)I",
        "(Ljava/lang/String;)J" => "Ljava/lang/Long;.parseLong:(Ljava/lang/String;)J",
        "(Ljava/lang/String;)F" => "Ljava/lang/Float;.parseFloat:(Ljava/lang/String;)F",
        "(Ljava/lang/String;)D" => "Ljava/lang/Double;.parseDouble:(Ljava/lang/String;)D",
        "(Ljava/lang/String;)Z" => "Ljava/lang/Boolean;.parseBoolean:(Ljava/lang/String;)Z",
        _ => return None,
    })
}

/// The 35c argument registers of an invoke instruction.
fn invoke_arguments(units: &[u16]) -> Vec<u32> {
    let count = (units[0] >> 12) as usize;
    let packed = [units[2] & 0xF, (units[2] >> 4) & 0xF, (units[2] >> 8) & 0xF, units[2] >> 12, (units[0] >> 8) & 0xF];
    packed[..count.min(5)].iter().map(|&r| u32::from(r)).collect()
}

/// Fix 5: the method's first call is to itself with its own arguments, after nothing but
/// constants into other registers, so it recurses until the stack runs out.
fn self_calling_parser(dex: &Dex, code: usize, method_index: u32, signature: &str) -> Option<Fix> {
    let parser = number_parser(signature)?;
    let (registers, ins) = (u32::from(dex.u16(code)), u32::from(dex.u16(code + 2)));
    let incoming: Vec<u32> = (registers - ins..registers).collect();
    for (offset, units) in instructions(dex, code) {
        let opcode = units[0] & 0xFF;
        if (0x12..=0x19).contains(&opcode) && !incoming.contains(&u32::from((units[0] >> 8) & if opcode == 0x12 { 0xF } else { 0xFF })) {
            continue; // a constant into a scratch register
        }
        let self_call = matches!(opcode, 0x6E | 0x70 | 0x71)
            && units.len() == 3
            && u32::from(units[1]) == method_index
            && invoke_arguments(&units) == incoming;
        if !self_call || dex.method_index(parser).is_none() {
            return None;
        }
        return Some(Fix {
            offset,
            reason: "text-to-number helper calling itself forever",
            old: dex.method(method_index).0,
            new: parser.into(),
        });
    }
    None
}

/// True when `method` is `BlockingQueue.take()` or `LinkedBlockingQueue.take()`, the wait that
/// a blocking service connection uses (fix 6).
fn is_blocking_take(method: &str) -> bool {
    method == "Ljava/util/concurrent/BlockingQueue;.take:()Ljava/lang/Object;"
        || method == "Ljava/util/concurrent/LinkedBlockingQueue;.take:()Ljava/lang/Object;"
}

/// Fix 6: a no-argument method returning IBinder that waits in `BlockingQueue.take()` for a
/// ServiceConnection callback. The `if-eqz`/`if-nez` that skips `throw new IllegalStateException()`
/// (when the connection was already used) is noped, so the method always throws instead of
/// waiting. When the dex also refers to `new RemoteException()`, that is thrown instead: old Play
/// services turn it into IOException ("not available"), which every caller handles; an
/// IllegalStateException only callers that expect one (it is what a main-thread call gets).
fn blocking_ad_id_wait(dex: &Dex, code: usize) -> Vec<Fix> {
    let list = instructions(dex, code);
    let takes = list.iter().any(|(_, units)| {
        matches!(units[0] & 0xFF, 0x6E..=0x72 | 0x74..=0x78)
            && units.len() > 1
            && is_blocking_take(&dex.method(units[1].into()).0)
    });
    if !takes {
        return Vec::new();
    }
    let Some(at) = list.windows(2).position(|pair| {
        matches!(pair[0].1[0] & 0xFF, 0x38 | 0x39) && pair[1].1[0] & 0xFF == 0x22 && dex.type_name(pair[1].1[1].into()) == ILLEGAL_STATE
    }) else {
        return Vec::new();
    };
    let reason = "old Play services' blocking ad-ID wait always fails (offline game, no deadlock)";
    let branch = &list[at];
    let mut fixes = vec![Fix {
        offset: branch.0,
        reason,
        old: if branch.1[0] & 0xFF == 0x38 { "if-eqz".into() } else { "if-nez".into() },
        new: "nop".into(),
    }];
    let constructor = list.get(at + 2).filter(|(_, units)| units[0] & 0xFF == 0x70 && dex.method(units[1].into()).0 == ILLEGAL_STATE_INIT);
    if let Some(constructor) = constructor.filter(|_| dex.type_index(REMOTE_EXCEPTION).is_some() && dex.method_index(REMOTE_EXCEPTION_INIT).is_some()) {
        fixes.push(Fix { offset: list[at + 1].0, reason, old: ILLEGAL_STATE.into(), new: REMOTE_EXCEPTION.into() });
        fixes.push(Fix { offset: constructor.0, reason, old: ILLEGAL_STATE_INIT.into(), new: REMOTE_EXCEPTION_INIT.into() });
    }
    fixes
}

fn scan_code(dex: &Dex, code: usize, method_name: &str, method_signature: &str, fixes: &mut Vec<Fix>) {
    let in_on_draw_frame =
        method_name == "onDrawFrame" && method_signature == "(Ljavax/microedition/khronos/opengles/GL10;)V";
    let size = dex.u32(code + 12) as usize;
    let start = code + 16;
    let units: Vec<u16> = (0..size).map(|i| dex.u16(start + i * 2)).collect();
    // The previous instruction, if it was `const/16` or `const`: (offset, register, value).
    let mut previous: Option<(usize, u32, i32)> = None;
    let mut pc = 0;
    while pc < size {
        let unit = units[pc];
        let length = instruction_length(&units[pc..]).max(1);
        let offset = start + pc * 2;
        let opcode = unit & 0xFF;
        let a = u32::from(unit >> 8);
        match opcode {
            // invoke-static(/range) Thread.sleep(J) inside onDrawFrame
            0x71 | 0x77 if in_on_draw_frame && dex.method(units[pc + 1].into()).0 == "Ljava/lang/Thread;.sleep:(J)V" => {
                fixes.push(Fix { offset, reason: "frame limiter in onDrawFrame removed", old: "Thread.sleep".into(), new: "nop".into() });
            }
            // invoke-virtual {vC, vD}, AudioAttributes$Builder.setFlags(I)
            0x6E if a >> 4 == 2 => {
                let second = u32::from(units[pc + 2] >> 4) & 0xF;
                if let Some((const_offset, register, value)) = previous {
                    if register == second
                        && value & FLAG_HW_AV_SYNC != 0
                        && dex.method(units[pc + 1].into()).0.starts_with("Landroid/media/AudioAttributes$Builder;.setFlags:(I)")
                    {
                        fixes.push(Fix {
                            offset: const_offset,
                            reason: "AudioAttributes flags (no hardware A/V sync)",
                            old: value.to_string(),
                            new: (value & !FLAG_HW_AV_SYNC).to_string(),
                        });
                    }
                }
            }
            // sput vAA, Cocos2dxSound.LOAD_TIME_OUT
            0x67 => {
                if let Some((const_offset, register, value)) = previous {
                    if register == a
                        && value > SHORT_LOAD_WAIT
                        && dex.field(units[pc + 1].into()) == "Lorg/cocos2dx/lib/Cocos2dxSound;.LOAD_TIME_OUT:I"
                    {
                        fixes.push(Fix {
                            offset: const_offset,
                            reason: "cocos2d-x sound load wait in ms",
                            old: value.to_string(),
                            new: SHORT_LOAD_WAIT.to_string(),
                        });
                    }
                }
            }
            _ => {}
        }
        previous = match opcode {
            0x13 => Some((offset, a, units[pc + 1] as i16 as i32)),
            0x14 => Some((offset, a, (units[pc + 1] as u32 | (units[pc + 2] as u32) << 16) as i32)),
            _ => None,
        };
        pc += length;
    }
}

/// Applies the fixes and updates the header checksums. Returns what changed.
pub fn patch(data: &mut [u8], carries_game_data: bool) -> Vec<Fix> {
    let fixes = find_fixes(data, carries_game_data);
    let new_types: Vec<Option<u32>> = fixes.iter().map(|fix| Dex { data }.type_index(&fix.new)).collect();
    let new_methods: Vec<Option<u32>> = fixes.iter().map(|fix| number_parser_name(&fix.new).and_then(|name| Dex { data }.method_index(name))).collect();
    for ((fix, new_type), new_method) in fixes.iter().zip(new_types).zip(new_methods) {
        let at = fix.offset;
        if let Some(method) = new_method {
            // invoke-static {vString}, parser: the String is the call's last argument.
            let units = [u16::from_le_bytes([data[at], data[at + 1]]), 0, u16::from_le_bytes([data[at + 4], data[at + 5]])];
            let string = *invoke_arguments(&units).last().unwrap() as u16;
            for (i, unit) in [0x1071, method as u16, string].iter().enumerate() {
                data[at + i * 2..at + i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
            }
            continue;
        }
        match data[at] {
            0x71 | 0x77 => data[at..at + 6].fill(0),
            0x13 => {
                let value: i32 = fix.new.parse().unwrap();
                data[at + 2..at + 4].copy_from_slice(&(value as u16).to_le_bytes());
            }
            0x14 => {
                let value: i32 = fix.new.parse().unwrap();
                data[at + 2..at + 6].copy_from_slice(&(value as u32).to_le_bytes());
            }
            0x1C => {
                let index = new_type.expect("the new class is a type in this dex") as u16;
                data[at + 2..at + 4].copy_from_slice(&index.to_le_bytes());
            }
            0x38 | 0x39 => data[at..at + 4].fill(0), // if-eqz/if-nez -> two nops (fix 6)
            0x22 => {
                let index = new_type.expect("the new exception is a type in this dex") as u16;
                data[at + 2..at + 4].copy_from_slice(&index.to_le_bytes());
            }
            0x70 => {
                let index = Dex { data }.method_index(&fix.new).expect("the new constructor is a method in this dex") as u16;
                data[at + 2..at + 4].copy_from_slice(&index.to_le_bytes());
            }
            other => panic!("unexpected opcode {other:#x} at {at:#x}"),
        }
    }
    if !fixes.is_empty() {
        reseal(data);
    }
    fixes
}

/// Apache HttpClient's client class, and Game Bridge's offline subclass of it (fix 7): the
/// same length, so the name can change in place.
const DEFAULT_HTTP_CLIENT: &str = "Lorg/apache/http/impl/client/DefaultHttpClient;";
pub const OFFLINE_HTTP_CLIENT: &str = "Lorg/apache/http/impl/client/OfflineHttpClient;";

/// Fix 7 - offline answers: every use of Apache's DefaultHttpClient in the dex (new, its
/// constructors, subclasses) becomes Game Bridge's OfflineHttpClient, which answers the
/// game's requests from the phone (src/offline in the bridge). The class name's string is
/// rewritten in place. Refused (None, nothing changed) when the new name would no longer sort
/// between its neighbours (ART checks the string table is sorted) or when a method or field of
/// a class outside this dex takes, returns or holds the type (its signature would change).
pub fn offline_http_client(data: &mut [u8]) -> Option<Fix> {
    let index = http_client_string(data)?.ok()?;
    let dex = Dex { data };
    let mut at = dex.u32(dex.u32(0x3C) as usize + index as usize * 4) as usize;
    dex.uleb(&mut at);
    data[at..at + OFFLINE_HTTP_CLIENT.len()].copy_from_slice(OFFLINE_HTTP_CLIENT.as_bytes());
    reseal(data);
    Some(Fix { offset: at, reason: "offline answers from the phone (Apache HTTP client)", old: DEFAULT_HTTP_CLIENT.into(), new: OFFLINE_HTTP_CLIENT.into() })
}

/// Whether the dex has Apache's client class but fix 7 can't rename it (see above); a game
/// with such a dex keeps its network in every dex.
pub fn offline_http_client_refused(data: &[u8]) -> bool {
    matches!(http_client_string(data), Some(Err(())))
}

/// The client class name's string index: None without it, Err when the rename is refused.
fn http_client_string(data: &[u8]) -> Option<Result<u32, ()>> {
    let dex = Dex { data };
    let count = dex.u32(0x38);
    let index = (0..count).find(|&index| dex.string(index) == DEFAULT_HTTP_CLIENT)?;
    let sorted = (index == 0 || dex.string(index - 1).as_str() < OFFLINE_HTTP_CLIENT)
        && (index + 1 == count || dex.string(index + 1).as_str() > OFFLINE_HTTP_CLIENT);
    let outside = dex.type_index(DEFAULT_HTTP_CLIENT).is_some_and(|kind| used_outside(&dex, kind));
    Some(if sorted && !outside { Ok(index) } else { Err(()) })
}

/// Whether a method or field of a class not defined in this dex has `kind` in its signature.
fn used_outside(dex: &Dex, kind: u32) -> bool {
    let defined: std::collections::HashSet<u32> = (0..dex.u32(0x60) as usize).map(|i| dex.u32(dex.u32(0x64) as usize + i * 32)).collect();
    let outside = |class: u16| u32::from(class) != kind && !defined.contains(&u32::from(class));
    let fields = (0..dex.u32(0x50) as usize).map(|i| dex.u32(0x54) as usize + i * 8);
    if fields.into_iter().any(|at| outside(dex.u16(at)) && u32::from(dex.u16(at + 2)) == kind) {
        return true;
    }
    (0..dex.u32(0x58) as usize).map(|i| dex.u32(0x5C) as usize + i * 8).any(|at| {
        let proto = dex.u32(0x4C) as usize + dex.u16(at + 2) as usize * 12;
        let params = dex.u32(proto + 8) as usize;
        let mut types = vec![dex.u32(proto + 4)];
        if params != 0 {
            types.extend((0..dex.u32(params) as usize).map(|i| u32::from(dex.u16(params + 4 + i * 2))));
        }
        outside(dex.u16(at)) && types.contains(&kind)
    })
}

/// Redoes the header's SHA-1 signature (of [32..]) and adler32 checksum (of [12..]).
fn reseal(data: &mut [u8]) {
    use sha1::Digest;
    let digest = sha1::Sha1::digest(&data[32..]);
    data[12..32].copy_from_slice(&digest);
    let checksum = adler32(&data[12..]);
    data[8..12].copy_from_slice(&checksum.to_le_bytes());
}

/// `name` if it is one of Java's number parsers (a fix 5 target).
fn number_parser_name(name: &str) -> Option<&str> {
    ["I", "J", "F", "D", "Z"].iter().any(|kind| number_parser(&format!("(Ljava/lang/String;){kind}")) == Some(name)).then_some(name)
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    b << 16 | a
}

#[cfg(test)]
mod tests {
    /// A dex file holding only a string table (short ASCII strings).
    fn strings_only(strings: &[&str]) -> Vec<u8> {
        let mut data = vec![0u8; 0x70];
        data[0x38..0x3C].copy_from_slice(&(strings.len() as u32).to_le_bytes());
        data[0x3C..0x40].copy_from_slice(&0x70u32.to_le_bytes());
        let mut bodies = Vec::new();
        let first_body = 0x70 + strings.len() * 4;
        for text in strings {
            data.extend_from_slice(&((first_body + bodies.len()) as u32).to_le_bytes());
            bodies.push(text.len() as u8);
            bodies.extend_from_slice(text.as_bytes());
            bodies.push(0);
        }
        data.extend(bodies);
        data
    }

    #[test]
    fn own_data_download() {
        let mod_dex = strings_only(&["main.702.com.ea.game.pvz2_rqm.obb", "https://github.com/x/y/releases/download/v1/main.702.com.ea.game.pvz2_rqm.obb"]);
        assert!(super::fetches_own_data(&mod_dex));
        assert!(!super::fetches_own_data(&strings_only(&["main.obb", "https://example.com/news", "getObbDir"])));
    }

    const DEFAULT_CLIENT: &str = "Lorg/apache/http/impl/client/DefaultHttpClient;";
    const OFFLINE_CLIENT: &str = "Lorg/apache/http/impl/client/OfflineHttpClient;";

    fn strings_of(data: &[u8]) -> Vec<String> {
        let dex = super::Dex { data };
        (0..dex.u32(0x38)).map(|index| dex.string(index)).collect()
    }

    /// Fix 7: the game's Apache client class becomes Game Bridge's offline one, in place,
    /// with the string table still sorted and the header checksum and signature redone.
    #[test]
    fn http_client_becomes_the_offline_one() {
        let mut dex = strings_only(&["Lorg/apache/http/entity/StringEntity;", DEFAULT_CLIENT, "Lorg/apache/http/impl/conn/X;"]);
        let fix = super::offline_http_client(&mut dex).expect("renamed");
        assert_eq!((fix.old.as_str(), fix.new.as_str()), (DEFAULT_CLIENT, OFFLINE_CLIENT));
        assert_eq!(strings_of(&dex), ["Lorg/apache/http/entity/StringEntity;", OFFLINE_CLIENT, "Lorg/apache/http/impl/conn/X;"]);
        assert_eq!(u32::from_le_bytes(dex[8..12].try_into().unwrap()), super::adler32(&dex[12..]), "checksum redone");
    }

    /// The rename is refused when the new name would sort past a neighbour (ART rejects an
    /// unsorted string table), and does nothing for a dex without the class.
    #[test]
    fn http_client_rename_keeps_the_table_sorted() {
        let mut dex = strings_only(&[DEFAULT_CLIENT, "Lorg/apache/http/impl/client/Not;"]);
        let before = dex.clone();
        assert!(super::offline_http_client(&mut dex).is_none());
        assert_eq!(dex, before, "unchanged when refused");
        let mut other = strings_only(&["Ljava/lang/Object;"]);
        assert!(super::offline_http_client(&mut other).is_none());
    }

    /// A refusal is told apart from a dex without the class, so a game with several dex
    /// files gets the rename in all of them or in none (a class renamed in one dex only would
    /// no longer match the other dex files' references to it).
    #[test]
    fn http_client_refusal_is_reported() {
        assert!(super::offline_http_client_refused(&strings_only(&[DEFAULT_CLIENT, "Lorg/apache/http/impl/client/Not;"])));
        assert!(!super::offline_http_client_refused(&strings_only(&["Ljava/lang/Object;"])));
        assert!(!super::offline_http_client_refused(&strings_only(&["Lorg/apache/http/entity/StringEntity;", DEFAULT_CLIENT])));
    }

    #[test]
    fn adler32_known_value() {
        assert_eq!(super::adler32(b"Wikipedia"), 0x11E6_0398);
    }

    /// Old Play services' blocking service connection.
    const PLAY_CLASS: &str = "Lcom/google/android/gms/common/a;";

    fn uleb(mut value: u32, out: &mut Vec<u8>) {
        loop {
            let byte = (value & 0x7F) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
    }

    /// A one-class dex holding `Ltest/Blocker;` (implementing `interfaces`) with the old Play
    /// services blocking service-connection method `er()Landroid/os/IBinder;` that waits in
    /// `BlockingQueue.take()` after the `if-eqz` guarded `throw new IllegalStateException()`.
    /// With `remote`, the dex also refers to `new RemoteException()` (as games with old Play
    /// services' ad code do), so the wait can fail the way the library reports "not available".
    fn build_dex(class: &str, interfaces: &[&str], remote: bool) -> Vec<u8> {
        let strings = [
            "V", "L", "Ljava/lang/Object;", "Landroid/content/ServiceConnection;",
            "Ljava/lang/IllegalStateException;", "Landroid/os/IBinder;",
            "Ljava/util/concurrent/BlockingQueue;", "Z", class,
            "CN", "CO", "er", "<init>", "take", "Landroid/os/RemoteException;",
        ];
        let types = [
            "V", "Ljava/lang/Object;", "Landroid/content/ServiceConnection;",
            "Ljava/lang/IllegalStateException;", "Landroid/os/IBinder;",
            "Ljava/util/concurrent/BlockingQueue;", "Z", class, "Landroid/os/RemoteException;",
        ];
        let s = |name: &str| strings.iter().position(|x| *x == name).unwrap() as u32;
        let t = |name: &str| types.iter().position(|x| *x == name).unwrap() as u32;
        let mut d = vec![0u8; 0x70];
        d[..8].copy_from_slice(b"dex\n035\0");
        d[0x24..0x28].copy_from_slice(&0x70u32.to_le_bytes());
        d[0x28..0x2C].copy_from_slice(&0x12345678u32.to_le_bytes());

        // string_ids
        let string_ids_off = d.len();
        d.resize(d.len() + strings.len() * 4, 0);
        // type_ids
        let type_ids_off = d.len();
        for name in &types {
            d.extend_from_slice(&s(name).to_le_bytes());
        }
        // proto_ids: ()V, ()Landroid/os/IBinder;, ()Ljava/lang/Object;
        let proto_ids_off = d.len();
        for (shorty, ret) in [("V", "V"), ("L", "Landroid/os/IBinder;"), ("L", "Ljava/lang/Object;")] {
            d.extend_from_slice(&s(shorty).to_le_bytes());
            d.extend_from_slice(&t(ret).to_le_bytes());
            d.extend_from_slice(&0u32.to_le_bytes());
        }
        // field_ids: CN:Z, CO:BlockingQueue
        let field_ids_off = d.len();
        for (class, ftype, name) in [(class, "Z", "CN"), (class, "Ljava/util/concurrent/BlockingQueue;", "CO")] {
            d.extend_from_slice(&(t(class) as u16).to_le_bytes());
            d.extend_from_slice(&(t(ftype) as u16).to_le_bytes());
            d.extend_from_slice(&s(name).to_le_bytes());
        }
        // method_ids: er, IllegalStateException.<init>, BlockingQueue.take (, RemoteException.<init>)
        let method_ids_off = d.len();
        let mut method_ids = vec![
            (class, 1u16, "er"),
            ("Ljava/lang/IllegalStateException;", 0u16, "<init>"),
            ("Ljava/util/concurrent/BlockingQueue;", 2u16, "take"),
        ];
        if remote {
            method_ids.push(("Landroid/os/RemoteException;", 0u16, "<init>"));
        }
        for &(class, proto, name) in &method_ids {
            d.extend_from_slice(&(t(class) as u16).to_le_bytes());
            d.extend_from_slice(&proto.to_le_bytes());
            d.extend_from_slice(&s(name).to_le_bytes());
        }
        // class_defs: one class
        let class_defs_off = d.len();
        d.extend_from_slice(&t(class).to_le_bytes());
        d.extend_from_slice(&1u32.to_le_bytes());
        d.extend_from_slice(&t("Ljava/lang/Object;").to_le_bytes());
        let interfaces_at = d.len();
        d.extend_from_slice(&0u32.to_le_bytes());
        d.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        d.extend_from_slice(&0u32.to_le_bytes());
        let class_data_at = d.len();
        d.extend_from_slice(&0u32.to_le_bytes());
        d.extend_from_slice(&0u32.to_le_bytes());

        // type_list for the interfaces
        let interfaces_off = if interfaces.is_empty() {
            0
        } else {
            let off = d.len();
            d.extend_from_slice(&(interfaces.len() as u32).to_le_bytes());
            for name in interfaces {
                d.extend_from_slice(&(t(name) as u16).to_le_bytes());
            }
            if interfaces.len() % 2 == 1 {
                d.extend_from_slice(&0u16.to_le_bytes());
            }
            off
        };

        // class_data: no fields, one direct method (er) with code.
        while d.len() % 4 != 0 {
            d.push(0);
        }
        let class_data_off = d.len();
        for count in [0, 0, 1, 0] {
            uleb(count, &mut d);
        }
        uleb(0, &mut d); // method index difference
        uleb(1, &mut d); // access flags
        let code_at = d.len();
        d.push(0);
        d.push(0); // code offset, two bytes (this dex stays under 16 KiB)

        // code_item for er().
        while d.len() % 4 != 0 {
            d.push(0);
        }
        let code_off = d.len() as u32;
        let insns: [u16; 22] = [
            0x1052, 0x0000, // iget-boolean v0, p0 (v1), CN
            0x0038, 0x0008, // if-eqz v0, +8
            0x0022, 0x0003, // new-instance v0, IllegalStateException
            0x1070, 0x0001, 0x0000, // invoke-direct {v0}, <init>()V
            0x0027, // throw v0
            0x1012, // const/4 v0, 1
            0x1059, 0x0000, // iput-boolean v0, p0, CN
            0x1054, 0x0001, // iget-object v0, p0, CO
            0x1072, 0x0002, 0x0000, // invoke-interface {v0}, take()Object
            0x000C, // move-result-object v0
            0x001F, 0x0004, // check-cast v0, IBinder
            0x0011, // return-object v0
        ];
        d.extend_from_slice(&2u16.to_le_bytes()); // registers (v0, p0 = v1)
        d.extend_from_slice(&1u16.to_le_bytes()); // ins
        d.extend_from_slice(&1u16.to_le_bytes()); // outs
        d.extend_from_slice(&0u16.to_le_bytes()); // tries
        d.extend_from_slice(&0u32.to_le_bytes()); // debug info
        d.extend_from_slice(&(insns.len() as u32).to_le_bytes()); // insns size
        for unit in insns {
            d.extend_from_slice(&unit.to_le_bytes());
        }

        // string data
        let mut string_offsets = vec![0u32; strings.len()];
        for (index, text) in strings.iter().enumerate() {
            string_offsets[index] = d.len() as u32;
            let mut body = Vec::new();
            uleb(text.len() as u32, &mut body);
            d.extend_from_slice(&body);
            d.extend_from_slice(text.as_bytes());
            d.push(0);
        }

        // patch forward references
        for (index, off) in string_offsets.iter().enumerate() {
            let at = string_ids_off + index * 4;
            d[at..at + 4].copy_from_slice(&off.to_le_bytes());
        }
        d[interfaces_at..interfaces_at + 4].copy_from_slice(&(interfaces_off as u32).to_le_bytes());
        d[class_data_at..class_data_at + 4].copy_from_slice(&(class_data_off as u32).to_le_bytes());
        assert!(code_off < 0x4000, "the code offset needs two uleb bytes");
        d[code_at] = (code_off & 0x7F) as u8 | 0x80;
        d[code_at + 1] = (code_off >> 7) as u8;

        // header sizes and offsets
        let file_size = d.len() as u32;
        d[0x20..0x24].copy_from_slice(&file_size.to_le_bytes());
        d[0x38..0x3C].copy_from_slice(&(strings.len() as u32).to_le_bytes());
        d[0x3C..0x40].copy_from_slice(&(string_ids_off as u32).to_le_bytes());
        d[0x40..0x44].copy_from_slice(&(types.len() as u32).to_le_bytes());
        d[0x44..0x48].copy_from_slice(&(type_ids_off as u32).to_le_bytes());
        d[0x48..0x4C].copy_from_slice(&3u32.to_le_bytes());
        d[0x4C..0x50].copy_from_slice(&(proto_ids_off as u32).to_le_bytes());
        d[0x50..0x54].copy_from_slice(&2u32.to_le_bytes());
        d[0x54..0x58].copy_from_slice(&(field_ids_off as u32).to_le_bytes());
        d[0x58..0x5C].copy_from_slice(&(method_ids.len() as u32).to_le_bytes());
        d[0x5C..0x60].copy_from_slice(&(method_ids_off as u32).to_le_bytes());
        d[0x60..0x64].copy_from_slice(&1u32.to_le_bytes());
        d[0x64..0x68].copy_from_slice(&(class_defs_off as u32).to_le_bytes());
        d
    }

    #[test]
    fn fix_6_blocks_the_ad_id_wait() {
        let mut dex = build_dex(PLAY_CLASS, &["Landroid/content/ServiceConnection;"], false);
        let fixes = super::patch(&mut dex, false);
        assert_eq!(fixes.len(), 1, "exactly one fix: the blocking ad-ID wait");
        let fix = &fixes[0];
        assert_eq!(fix.old, "if-eqz");
        assert_eq!(fix.new, "nop");
        assert_eq!(&dex[fix.offset..fix.offset + 4], &[0, 0, 0, 0], "the branch became nops");
    }

    #[test]
    fn fix_6_leaves_non_service_connections_alone() {
        let mut dex = build_dex(PLAY_CLASS, &[], true);
        let fixes = super::patch(&mut dex, false);
        assert!(fixes.is_empty(), "a class that is no ServiceConnection keeps its method");
        let mut dex = build_dex("Lcom/example/game/Connection;", &["Landroid/content/ServiceConnection;"], true);
        assert!(super::patch(&mut dex, false).is_empty(), "the game's own connection keeps its wait");
    }

    /// With `new RemoteException()` in the dex, the wait throws that instead: old Play services
    /// turn it into IOException ("not available"), which every caller handles.
    #[test]
    fn fix_6_fails_the_wait_the_way_play_services_reports_it() {
        let mut dex = build_dex(PLAY_CLASS, &["Landroid/content/ServiceConnection;"], true);
        let fixes = super::patch(&mut dex, false);
        assert_eq!(fixes.len(), 3, "branch, new-instance and constructor");
        let new_instance = fixes[1].offset;
        assert_eq!(&dex[new_instance..new_instance + 4], &[0x22, 0x00, 8, 0], "new-instance v0, RemoteException");
        let constructor = fixes[2].offset;
        assert_eq!(&dex[constructor..constructor + 4], &[0x70, 0x10, 3, 0], "invoke-direct {{v0}}, RemoteException.<init>()V");
    }
}
