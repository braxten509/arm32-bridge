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

use std::collections::HashMap;

const FLAG_HW_AV_SYNC: i32 = 0x10;
const DOWNLOADER_CLIENT: &str = "Lcom/google/android/vending/expansion/downloader/IDownloaderClient;";
const INTENT_FOR_CLASS: &str = "Landroid/content/Intent;.<init>:(Landroid/content/Context;Ljava/lang/Class;)V";
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
        for (method_index, code) in methods(&dex, class_def) {
            if seen_code.insert(code) {
                let (_, name, signature) = dex.method(method_index);
                scan_code(&dex, code, &name, &signature, &mut fixes);
                if let Some(fix) = self_calling_parser(&dex, code, method_index, &signature) {
                    fixes.push(fix);
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
            other => panic!("unexpected opcode {other:#x} at {at:#x}"),
        }
    }
    if !fixes.is_empty() {
        // Header: magic[8], adler32 of [12..], SHA-1 of [32..].
        use sha1::Digest;
        let digest = sha1::Sha1::digest(&data[32..]);
        data[12..32].copy_from_slice(&digest);
        let checksum = adler32(&data[12..]);
        data[8..12].copy_from_slice(&checksum.to_le_bytes());
    }
    fixes
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

    #[test]
    fn adler32_known_value() {
        assert_eq!(super::adler32(b"Wikipedia"), 0x11E6_0398);
    }
}
