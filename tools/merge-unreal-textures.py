#!/usr/bin/env python3
"""Makes a new copy of an iPhone Unreal Engine 3 game with the textures of an
Android build of the same game written into it.

iPhone builds keep their textures as PVRTC (often 2 bits per pixel, blurry);
some Android builds of the same game keep the same textures as ASTC, which
looks much cleaner at the same size. For every texture both builds have (same
package file, same object path, same size, every picture level fitting), the
Android levels are written over the iPhone levels in place, padded with zeros,
except for compositing sources (the engine reads those itself),
so no file changes size and nothing else in the game moves. The game still
says PVRTC; GameBridgeTextures.txt in the new copy lists the merged levels so
Game Bridge's iPhone layer sends them to the graphics chip as ASTC
(ios/src/fw/gl.rs). Everything that doesn't match stays as it was.

usage: merge-unreal-textures.py IPHONE.app ANDROID.apk OUT.app [--name NAME] [--id BUNDLE_ID]
Run with python3 -I. The two inputs are only read.
"""
import argparse, os, plistlib, shutil, struct, sys, zipfile

TEXTURE_CLASSES = {'Texture2D', 'LightMapTexture2D', 'ShadowMapTexture2D', 'TerrainWeightMapTexture'}
# Only plain pictures are merged: a lightmap's brightness comes with scale
# numbers stored in the level, which belong to the iPhone build's own lightmaps.
MERGED_CLASSES = {'Texture2D'}
LIST_NAME = 'GameBridgeTextures.txt'


class Bad(Exception):
    """This package (or texture) isn't the layout this reader knows; it's left alone."""


class Reader:
    def __init__(self, data, at=0):
        self.data, self.at = data, at

    def take(self, fmt):
        size = struct.calcsize(fmt)
        if self.at < 0 or self.at + size > len(self.data):
            raise Bad('read past the end')
        value = struct.unpack_from(fmt, self.data, self.at)
        self.at += size
        return value if len(value) > 1 else value[0]

    def count(self, limit=1 << 24):
        n = self.take('<i')
        if not 0 <= n <= limit:
            raise Bad('bad count %d' % n)
        return n

    def text(self):
        n = self.take('<i')
        if n == 0:
            return ''
        if n > 0:
            raw = self.data[self.at:self.at + n]
            if len(raw) != n:
                raise Bad('text past the end')
            self.at += n
            return raw[:-1].decode('latin1')
        raw = self.data[self.at:self.at - 2 * n]
        if len(raw) != -2 * n:
            raise Bad('text past the end')
        self.at += -2 * n
        return raw[:-2].decode('utf-16le')


class Package:
    """An uncompressed UE3 package: names, imports, exports."""

    def __init__(self, data):
        r = Reader(data)
        if r.take('<I') != 0x9E2A83C1:
            raise Bad('not a package')
        self.version, self.licensee = r.take('<HH')
        r.take('<i')
        r.text()
        r.take('<I')
        names_count, names_at, exports_count, exports_at, imports_count, imports_at, _ = r.take('<7i')
        r.take('<4i')
        r.at += 16
        generations = r.count(1 << 16)
        r.at += 12 * generations
        r.take('<2i')
        if r.take('<I') != 0 or r.take('<i') != 0:
            raise Bad('compressed package')
        self.data = data
        self.names = []
        r.at = names_at
        for _ in range(names_count):
            self.names.append(r.text())
            r.take('<Q')
        self.imports = []
        r.at = imports_at
        for _ in range(imports_count):
            r.take('<4i')
            outer, name, number = r.take('<3i')
            self.imports.append((outer, self.name(name), number))
        self.exports = []
        r.at = exports_at
        for _ in range(exports_count):
            klass, _, outer, name, number, _ = r.take('<6i')
            r.take('<Q')
            size, offset = r.take('<2i')
            r.take('<I')
            net_objects = r.count(1 << 20)
            r.at += 4 * net_objects
            r.at += 16
            r.take('<I')
            if size < 0 or offset < 0 or offset + size > len(data):
                raise Bad('export outside the file')
            self.exports.append(dict(klass=klass, outer=outer, name=self.name(name), number=number, size=size, offset=offset))

    def name(self, index):
        if not 0 <= index < len(self.names):
            raise Bad('bad name index')
        return self.names[index]

    def object_name(self, index):
        if index > 0:
            e = self.exports[index - 1]
            return e['name'], e['number'], e['outer']
        outer, name, number = self.imports[-index - 1]
        return name, number, outer

    def path(self, index):
        """Object path with Unreal's name numbers (Name_<n-1> when n > 0)."""
        parts, seen = [], 0
        while index:
            name, number, index = self.object_name(index)
            parts.append(name + ('_%d' % (number - 1) if number > 0 else ''))
            seen += 1
            if seen > 64:
                raise Bad('outer loop')
        return '.'.join(reversed(parts))

    def class_name(self, export):
        klass = export['klass']
        return self.object_name(klass)[0] if klass else 'Class'

    def textures(self, classes=TEXTURE_CLASSES):
        """(path, properties, levels) per texture; a level is (width, height, data at, data size)."""
        for i, e in enumerate(self.exports):
            if self.class_name(e) not in classes:
                continue
            try:
                yield self.path(i + 1), *self.texture(e)
            except Bad:
                continue

    def cube_faces(self):
        """Object paths of the textures that cube maps use as faces."""
        faces = set()
        for e in self.exports:
            if self.class_name(e) != 'TextureCube':
                continue
            try:
                properties, _ = self.texture(e, levels=False)
            except Bad:
                continue
            for index in properties.get('objects', []):
                if index:
                    faces.add(self.path(index))
        return faces

    def texture(self, e, levels=True):
        r = Reader(self.data, e['offset'])
        end = e['offset'] + e['size']
        r.take('<i')
        properties = {}
        while True:
            name = self.name(r.take('<i'))
            r.take('<i')
            if name == 'None':
                break
            kind = self.name(r.take('<i'))
            r.take('<i')
            size = r.take('<i')
            r.take('<i')
            if kind in ('StructProperty', 'ByteProperty'):
                enum = self.name(r.take('<i'))
                r.take('<i')
            if kind == 'BoolProperty':
                properties[name] = r.take('<B')
                continue
            if size < 0 or r.at + size > end:
                raise Bad('property past the object')
            if kind == 'IntProperty' and size == 4:
                properties[name] = struct.unpack_from('<i', self.data, r.at)[0]
            elif kind == 'ObjectProperty' and size == 4:
                properties.setdefault('objects', []).append(struct.unpack_from('<i', self.data, r.at)[0])
            elif kind == 'ByteProperty' and size == 8:
                properties[name] = self.name(struct.unpack_from('<i', self.data, r.at)[0])
            r.at += size
        if not levels:
            return properties, []
        self.bulk(r, end)  # source art
        levels = []
        for _ in range(r.count(32)):
            flags, at, size = self.bulk(r, end)
            width, height = r.take('<2i')
            if flags != 0 or size <= 0 or width <= 0 or height <= 0:
                raise Bad('level stored elsewhere or compressed')
            levels.append((width, height, at, size))
        if r.at > end:
            raise Bad('texture past the object')
        return properties, levels

    def bulk(self, r, end):
        flags, _, size, _ = r.take('<Ii2i')
        at = r.at
        if not flags & 1 and size > 0:
            r.at += size
        if r.at > end:
            raise Bad('picture data past the object')
        return flags, at, size


def astc_block(format_name):
    """(w, h) from PF_ASTC_<w>x<h>; None for other formats."""
    if not format_name or not format_name.startswith('PF_ASTC_'):
        return None
    try:
        w, h = (int(x) for x in format_name[len('PF_ASTC_'):].split('x'))
    except ValueError:
        return None
    return (w, h) if (w, h) in ASTC_ENUMS else None


ASTC_ENUMS = {(4, 4), (5, 4), (5, 5), (6, 5), (6, 6), (8, 5), (8, 6), (8, 8), (10, 5), (10, 6), (10, 8), (10, 10), (12, 10), (12, 12)}


def astc_size(width, height, block):
    return -(-width // block[0]) * -(-height // block[1]) * 16


def key(data):
    """FNV-1a 64 of the first 4096 bytes, as gl.rs computes it."""
    h = 0xcbf29ce484222325
    for b in data[:4096]:
        h = ((h ^ b) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return h


def pvrtc_size(width, height, two_bits):
    return max(width, 16 if two_bits else 8) * max(height, 8) * (2 if two_bits else 4) // 8


def plan_package(ios, android):
    """What merging `android` into `ios` would write: (textures, kept level keys, skipped).
    A texture is a list of (at, size, slot bytes, list entry) for all its levels."""
    i_pkg, a_pkg = Package(ios), Package(android)
    if (i_pkg.version, i_pkg.licensee) != (a_pkg.version, a_pkg.licensee):
        raise Bad('different package versions')
    theirs = {}
    for path, props, levels in a_pkg.textures():
        theirs.setdefault(path, []).append((props, levels))
    faces = i_pkg.cube_faces() | a_pkg.cube_faces()
    textures, kept, skipped = [], set(), 0
    for path, props, levels in i_pkg.textures(MERGED_CLASSES):
        writes = plan_texture(path, props, levels, theirs.get(path, []), faces, android)
        if writes:
            textures.append(writes)
            continue
        skipped += 1
        for width, height, at, size in levels:
            kept.add((width, height, size, key(ios[at:at + size])))
    return textures, kept, skipped


def plan_texture(path, props, levels, partners, faces, android):
    # The engine reads compositing sources itself (to build one picture out of
    # several, e.g. a character's gear) and only knows their PVRTC; cube maps
    # need all six faces alike.
    if len(partners) != 1 or props.get('bIsCompositingSource') or path in faces or not levels:
        return None
    # iPhone chips only take PVRTC: its levels must be PVRTC-sized (2 or 4 bits a pixel).
    width, height, _, size = levels[0]
    if props.get('Format') not in ('PF_DXT1', 'PF_DXT3', 'PF_DXT5') or size not in (pvrtc_size(width, height, True), pvrtc_size(width, height, False)):
        return None
    a_props, a_levels = partners[0]
    block = astc_block(a_props.get('Format'))
    same_size = (props.get('SizeX'), props.get('SizeY')) == (a_props.get('SizeX'), a_props.get('SizeY'))
    if not block or not same_size or len(a_levels) < len(levels):
        return None
    writes = []
    for n, (width, height, at, size) in enumerate(levels):
        if (width, height) != (max(1, levels[0][0] >> n), max(1, levels[0][1] >> n)):
            return None
        a_width, a_height, a_at, a_size = a_levels[n]
        # Some builds write 4x4 for levels smaller than that.
        if (a_width, a_height) not in ((width, height), (max(width, 4), max(height, 4))):
            return None
        need = astc_size(width, height, block)
        if a_size != need or need > size:
            return None
        slot = android[a_at:a_at + a_size] + bytes(size - a_size)
        writes.append((at, size, slot, (width, height, size, key(slot), block)))
    return writes


def android_packages(apk):
    """Package file name -> zip entry name, for stored packages under assets/<game>/Cooked*/."""
    found = {}
    with zipfile.ZipFile(apk) as z:
        for info in z.infolist():
            parts = info.filename.split('/')
            if len(parts) == 4 and parts[0] == 'assets' and parts[2].startswith('Cooked') and parts[3].endswith('.xxx'):
                found[parts[3]] = info.filename
    return found


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    ap.add_argument('ios_app')
    ap.add_argument('android_apk')
    ap.add_argument('out_app')
    ap.add_argument('--name', help='name shown in Game Bridge (CFBundleDisplayName)')
    ap.add_argument('--id', help='bundle id of the new copy (keeps its app and name apart from the original)')
    args = ap.parse_args()
    cooked = os.path.join(args.ios_app, 'CookedIPhone')
    if not os.path.isdir(cooked):
        sys.exit('no CookedIPhone folder: not an iPhone Unreal Engine 3 game')
    if os.path.exists(args.out_app):
        sys.exit('%s already exists' % args.out_app)
    theirs = android_packages(args.android_apk)
    ours = sorted(f for f in os.listdir(cooked) if f.endswith('.xxx'))
    shared = [f for f in ours if f in theirs]
    print('%d iPhone packages, %d Android packages, %d in both' % (len(ours), len(theirs), len(shared)))
    if len(shared) < 0.8 * len(ours):
        sys.exit('fewer than 80% of the packages are in both: not the same game')
    shutil.copytree(args.ios_app, args.out_app + '.partial')
    out = args.out_app + '.partial'
    plans, kept, skipped = {}, set(), 0
    with zipfile.ZipFile(args.android_apk) as z:
        for f in shared:
            data = open(os.path.join(out, 'CookedIPhone', f), 'rb').read()
            try:
                textures, kept_here, skipped_here = plan_package(data, z.read(theirs[f]))
            except Bad as e:
                print('  %s left as it is: %s' % (f, e))
                continue
            plans[f] = textures
            kept |= kept_here
            skipped += skipped_here
    # A merged level must not be mistaken for a kept one (or for another merged
    # level with a different block): such textures stay as they are.
    blocks = {}
    for textures in plans.values():
        for writes in textures:
            for *_, entry in writes:
                blocks.setdefault(entry[:4], set()).add(entry[4])
    clash = {k for k, b in blocks.items() if len(b) > 1 or k in kept}
    merged, entries = 0, []
    for f, textures in plans.items():
        good = [w for w in textures if not any(entry[:4] in clash for *_, entry in w)]
        skipped += len(textures) - len(good)
        if not good:
            continue
        path = os.path.join(out, 'CookedIPhone', f)
        with open(path, 'r+b') as file:
            for writes in good:
                for at, size, slot, entry in writes:
                    file.seek(at)
                    file.write(slot)
                    entries.append(entry)
        merged += len(good)
    entries = sorted(set(entries))
    with open(os.path.join(out, LIST_NAME), 'w') as file:
        file.write('# Picture levels merged from an Android build: width height bytes key(FNV-1a 64 of the first 4096 bytes) astc-block-w astc-block-h\n')
        for width, height, size, k, block in entries:
            file.write('%d %d %d %016x %d %d\n' % (width, height, size, k, block[0], block[1]))
    info_path = os.path.join(out, 'Info.plist')
    with open(info_path, 'rb') as file:
        info = plistlib.load(file)
    binary = open(info_path, 'rb').read(8) == b'bplist00'
    if args.name:
        info['CFBundleDisplayName'] = args.name
    if args.id:
        info['CFBundleIdentifier'] = args.id
    with open(info_path, 'wb') as file:
        plistlib.dump(info, file, fmt=plistlib.FMT_BINARY if binary else plistlib.FMT_XML)
    os.rename(out, args.out_app)
    print('%d textures merged (%d picture levels listed), %d kept as they were' % (merged, len(entries), skipped))


if __name__ == '__main__':
    main()
