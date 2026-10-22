#!/usr/bin/env python3
"""Clears one function's entry in a linked arm64 Mach-O's __unwind_info
(encoding 0: "no compact unwind info"), so an unwinder has to find the
function's DWARF FDE in __eh_frame by itself, as for code linked with
-no_compact_unwind. For ios/tools/build-cxx-test.sh.

usage: zero-compact-unwind.py BINARY SYMBOL
"""
import struct
import sys


def main(path, symbol):
    data = bytearray(open(path, "rb").read())
    u32 = lambda at: struct.unpack_from("<I", data, at)[0]
    assert u32(0) == 0xFEEDFACF, "not a 64-bit Mach-O"
    commands, at = u32(16), 32
    text_address = unwind = None
    symbols = None
    for _ in range(commands):
        command, size = u32(at), u32(at + 4)
        if command == 0x19:  # LC_SEGMENT_64
            segment = data[at + 8:at + 24].rstrip(b"\0")
            if segment == b"__TEXT":
                text_address = struct.unpack_from("<Q", data, at + 24)[0]
            for i in range(u32(at + 64)):
                section = at + 72 + i * 80
                if data[section:section + 16].rstrip(b"\0") == b"__unwind_info":
                    unwind = u32(section + 48)
        elif command == 0x2:  # LC_SYMTAB
            symbols = (u32(at + 8), u32(at + 12), u32(at + 16))
        at += size
    assert text_address is not None and unwind is not None and symbols
    symbol_offset, count, strings = symbols
    address = None
    for i in range(count):
        entry = symbol_offset + i * 16
        name_at = strings + u32(entry)
        name = data[name_at:data.index(b"\0", name_at)].decode()
        if name == symbol:
            address = struct.unpack_from("<Q", data, entry + 8)[0]
    assert address is not None, f"no symbol {symbol}"
    target = address - text_address

    u = unwind  # file offset; the section's own offsets count from here
    common, common_count = u32(u + 4), u32(u + 8)
    index, index_count = u + u32(u + 20), u32(u + 24)
    for i in range(index_count - 1):
        first, page = u32(index + i * 12), u + u32(index + i * 12 + 4)
        if not first <= target < u32(index + (i + 1) * 12):
            continue
        entries, entry_count = page + struct.unpack_from("<H", data, page + 4)[0], struct.unpack_from("<H", data, page + 6)[0]
        if u32(page) == 2:  # regular page: (function offset, encoding)
            for e in range(entry_count):
                if u32(entries + e * 8) == target:
                    struct.pack_into("<I", data, entries + e * 8 + 4, 0)
                    break
            else:
                sys.exit(f"{symbol} has no entry")
        else:  # compressed page: an encoding index per function
            encodings = page + struct.unpack_from("<H", data, page + 8)[0]
            for e in range(entry_count):
                entry = u32(entries + e * 4)
                if first + (entry & 0xFFFFFF) == target:
                    which = entry >> 24
                    at = u + common + which * 4 if which < common_count else encodings + (which - common_count) * 4
                    users = sum(1 for k in range(entry_count) if u32(entries + k * 4) >> 24 == which)
                    assert users == 1, f"{symbol}'s encoding is shared"
                    struct.pack_into("<I", data, at, 0)
                    break
            else:
                sys.exit(f"{symbol} has no entry")
        open(path, "wb").write(data)
        return
    sys.exit(f"{symbol} is not covered by __unwind_info")


main(sys.argv[1], sys.argv[2])
