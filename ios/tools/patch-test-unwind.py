#!/usr/bin/env python3
"""Changes a linked arm64 Mach-O test program's unwind info, for
ios/tools/build-cxx-test.sh:

  zero-compact BINARY SYMBOL   clears the function's __unwind_info entry
      (encoding 0: "no compact unwind info"), so an unwinder has to find its
      DWARF FDE in __eh_frame by itself, as for code linked with
      -no_compact_unwind;
  break-fde BINARY SYMBOL      points the CIE pointer of the function's FDE
      far outside __eh_frame: a malformed record an unwinder must skip.
"""
import struct
import sys


class MachO:
    def __init__(self, path):
        self.path = path
        self.data = bytearray(open(path, "rb").read())
        assert self.u32(0) == 0xFEEDFACF, "not a 64-bit Mach-O"
        self.text_address = None
        self.sections = {}  # name -> (address, file offset, size)
        symbols = None
        at = 32
        for _ in range(self.u32(16)):
            command, size = self.u32(at), self.u32(at + 4)
            if command == 0x19:  # LC_SEGMENT_64
                if self.data[at + 8:at + 24].rstrip(b"\0") == b"__TEXT":
                    self.text_address = self.u64(at + 24)
                for i in range(self.u32(at + 64)):
                    section = at + 72 + i * 80
                    name = self.data[section:section + 16].rstrip(b"\0").decode()
                    self.sections[name] = (self.u64(section + 32), self.u32(section + 48), self.u64(section + 40))
            elif command == 0x2:  # LC_SYMTAB
                symbols = (self.u32(at + 8), self.u32(at + 12), self.u32(at + 16))
            at += size
        assert self.text_address is not None and symbols
        self.symbols = symbols

    def u32(self, at):
        return struct.unpack_from("<I", self.data, at)[0]

    def u64(self, at):
        return struct.unpack_from("<Q", self.data, at)[0]

    def address_of(self, symbol):
        offset, count, strings = self.symbols
        for i in range(count):
            entry = offset + i * 16
            name_at = strings + self.u32(entry)
            if self.data[name_at:self.data.index(b"\0", name_at)].decode() == symbol:
                return self.u64(entry + 8)
        sys.exit(f"no symbol {symbol}")

    def save(self):
        open(self.path, "wb").write(self.data)


def zero_compact(image, symbol):
    data, u32 = image.data, image.u32
    target = image.address_of(symbol) - image.text_address
    u = image.sections["__unwind_info"][1]  # the section's own offsets count from here
    common, common_count = u32(u + 4), u32(u + 8)
    index, index_count = u + u32(u + 20), u32(u + 24)
    for i in range(index_count - 1):
        first, page = u32(index + i * 12), u + u32(index + i * 12 + 4)
        if not first <= target < u32(index + (i + 1) * 12):
            continue
        entries = page + struct.unpack_from("<H", data, page + 4)[0]
        entry_count = struct.unpack_from("<H", data, page + 6)[0]
        if u32(page) == 2:  # regular page: (function offset, encoding)
            for e in range(entry_count):
                if u32(entries + e * 8) == target:
                    struct.pack_into("<I", data, entries + e * 8 + 4, 0)
                    return
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
                    return
        sys.exit(f"{symbol} has no entry")
    sys.exit(f"{symbol} is not covered by __unwind_info")


def break_fde(image, symbol):
    """Only the FDE layout ld64.lld writes: a "zR"/"zPLR" CIE with pcrel
    absptr or sdata4 addresses."""
    target = image.address_of(symbol)
    address, start, size = image.sections["__eh_frame"]
    at = start
    while at < start + size:
        length, cie_id = image.u32(at), image.u32(at + 4)
        if length == 0:
            break
        if cie_id != 0:
            encoding = cie_address_encoding(image, at + 4 - cie_id)
            field = at + 8
            value = struct.unpack_from("<q" if encoding & 0x0F == 0 else "<i", image.data, field)[0]
            assert encoding & 0x70 == 0x10, "not a pcrel address"
            if address + (field - start) + value == target:
                struct.pack_into("<I", image.data, at + 4, 0x7FFFFFF0)
                return
        at += 4 + length
    sys.exit(f"{symbol} has no FDE")


def cie_address_encoding(image, cie):
    augmentation = image.data[cie + 9:image.data.index(b"\0", cie + 9)].decode()
    at = cie + 10 + len(augmentation)
    for _ in range(3):  # code alignment, data alignment, return address column
        while image.data[at] & 0x80:
            at += 1
        at += 1
    assert augmentation.startswith("z")
    while image.data[at] & 0x80:  # augmentation data length
        at += 1
    at += 1
    for letter in augmentation[1:]:
        if letter == "R":
            return image.data[at]
        if letter == "P":
            sizes = {0x0: 8, 0xB: 4, 0x3: 4}
            at += 1 + sizes[image.data[at] & 0x0F]
        elif letter == "L":
            at += 1
    sys.exit("CIE without an address encoding")


command, binary, name = sys.argv[1:4]
image = MachO(binary)
{"zero-compact": zero_compact, "break-fde": break_fde}[command](image, name)
image.save()
