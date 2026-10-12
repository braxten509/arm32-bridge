#!/usr/bin/env python3
"""Fixes known Java-side bugs in an old game's dex files, in place.

Fix 1 - AudioAttributes FLAG_HW_AV_SYNC (0x10): cocos2d-x 3.x builds its
SoundPool with setFlags(16), which asks for a hardware A/V sync output. Phones
without one (the Pixel 10) refuse every sound effect with error -38. The bit is
cleared from any constant that is passed straight to AudioAttributes.Builder
.setFlags.

usage: patch_dex.py <dexdump binary> <classes.dex>...
Prints one line per change; exit code 0 even when nothing needed fixing.
"""
import hashlib
import re
import subprocess
import sys
import zlib

FLAG_HW_AV_SYNC = 0x10
SET_FLAGS = "Landroid/media/AudioAttributes$Builder;.setFlags:(I)"
# "238790: 1502 803f   |0004: const/16 v2, #int 16 // #10"
INSTRUCTION = re.compile(r"^([0-9a-f]{6}): [0-9a-f ]+\|[0-9a-f]{4}: (.*)$")
CONST = re.compile(r"^const(/16)? v(\d+), #int (-?\d+)")
INVOKE = re.compile(r"^invoke-virtual \{v\d+, v(\d+)\}, (.*)$")


def fixes(dexdump: str, path: str):
    """Yields (file offset of a const instruction, its opcode form, value)."""
    listing = subprocess.run([dexdump, "-d", path], capture_output=True, text=True, errors="replace").stdout
    previous = None
    for line in listing.splitlines():
        match = INSTRUCTION.match(line)
        if not match:
            continue
        offset, text = int(match.group(1), 16), match.group(2)
        invoke = INVOKE.match(text)
        if invoke and SET_FLAGS in invoke.group(2) and previous:
            const_offset, form, register, value = previous
            if register == int(invoke.group(1)) and value & FLAG_HW_AV_SYNC:
                yield const_offset, form, value
        const = CONST.match(text)
        previous = (offset, const.group(1) or "", int(const.group(2)), int(const.group(3))) if const else None


def patch(dexdump: str, path: str) -> int:
    data = bytearray(open(path, "rb").read())
    changed = 0
    for offset, form, value in fixes(dexdump, path):
        new_value = value & ~FLAG_HW_AV_SYNC
        if form == "/16":  # 13 AA BBBB
            assert data[offset] == 0x13
            data[offset + 2 : offset + 4] = (new_value & 0xFFFF).to_bytes(2, "little")
        else:  # const: 14 AA BBBBBBBB
            assert data[offset] == 0x14
            data[offset + 2 : offset + 6] = (new_value & 0xFFFFFFFF).to_bytes(4, "little")
        print(f"{path}: AudioAttributes flags {value:#x} -> {new_value:#x} at {offset:#x} (no hardware A/V sync)")
        changed += 1
    if changed:
        # Header: magic[8], adler32 checksum of [12:], SHA-1 of [32:].
        data[12:32] = hashlib.sha1(data[32:]).digest()
        data[8:12] = zlib.adler32(bytes(data[12:])).to_bytes(4, "little")
        open(path, "wb").write(data)
    return changed


def main() -> None:
    dexdump, paths = sys.argv[1], sys.argv[2:]
    for path in paths:
        patch(dexdump, path)


if __name__ == "__main__":
    main()
