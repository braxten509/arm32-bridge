//! PVRTC1 texture decoding (2 and 4 bits per pixel), for graphics chips
//! without PowerVR texture support.
//!
//! Ported from Imagination Technologies' PVRTDecompress.cpp (PowerVR SDK):
//! Copyright (c) Imagination Technologies Ltd. Released under the MIT
//! License: permission is hereby granted, free of charge, to any person
//! obtaining a copy of this software and associated documentation files
//! (the "Software"), to deal in the Software without restriction, including
//! without limitation the rights to use, copy, modify, merge, publish,
//! distribute, sublicense, and/or sell copies of the Software, and to permit
//! persons to whom the Software is furnished to do so, subject to the
//! following conditions: The above copyright notice and this permission
//! notice shall be included in all copies or substantial portions of the
//! Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
//! EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN
//! NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
//! DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
//! OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
//! USE OR OTHER DEALINGS IN THE SOFTWARE.

#[derive(Clone, Copy, Default)]
struct Pixel {
    red: i32,
    green: i32,
    blue: i32,
    alpha: i32,
}

#[derive(Clone, Copy)]
struct Word {
    modulation: u32,
    color: u32,
}

fn color_a(data: u32) -> Pixel {
    if data & 0x8000 != 0 {
        Pixel {
            red: ((data & 0x7c00) >> 10) as i32,
            green: ((data & 0x3e0) >> 5) as i32,
            blue: ((data & 0x1e) | ((data & 0x1e) >> 4)) as i32,
            alpha: 0xf,
        }
    } else {
        Pixel {
            red: (((data & 0xf00) >> 7) | ((data & 0xf00) >> 11)) as i32,
            green: (((data & 0xf0) >> 3) | ((data & 0xf0) >> 7)) as i32,
            blue: (((data & 0xe) << 1) | ((data & 0xe) >> 2)) as i32,
            alpha: ((data & 0x7000) >> 11) as i32,
        }
    }
}

fn color_b(data: u32) -> Pixel {
    if data & 0x8000_0000 != 0 {
        Pixel {
            red: ((data & 0x7c00_0000) >> 26) as i32,
            green: ((data & 0x3e0_0000) >> 21) as i32,
            blue: ((data & 0x1f_0000) >> 16) as i32,
            alpha: 0xf,
        }
    } else {
        Pixel {
            red: (((data & 0xf00_0000) >> 23) | ((data & 0xf00_0000) >> 27)) as i32,
            green: (((data & 0xf0_0000) >> 19) | ((data & 0xf0_0000) >> 23)) as i32,
            blue: (((data & 0xf_0000) >> 15) | ((data & 0xf_0000) >> 19)) as i32,
            alpha: ((data & 0x7000_0000) >> 27) as i32,
        }
    }
}

fn add(a: &mut Pixel, b: Pixel) {
    a.red += b.red;
    a.green += b.green;
    a.blue += b.blue;
    a.alpha += b.alpha;
}

fn minus(a: Pixel, b: Pixel) -> Pixel {
    Pixel { red: a.red - b.red, green: a.green - b.green, blue: a.blue - b.blue, alpha: a.alpha - b.alpha }
}

fn times(a: Pixel, n: i32) -> Pixel {
    Pixel { red: a.red * n, green: a.green * n, blue: a.blue * n, alpha: a.alpha * n }
}

fn interpolate(p: Pixel, q: Pixel, r: Pixel, s: Pixel, out: &mut [Pixel; 32], bpp: u32) {
    let word_width: usize = if bpp == 2 { 8 } else { 4 };
    let word_height: usize = 4;
    let q_minus_p = minus(q, p);
    let s_minus_r = minus(s, r);
    let mut hp = times(p, word_width as i32);
    let mut hr = times(r, word_width as i32);
    if bpp == 2 {
        for x in 0..word_width {
            let mut result = times(hp, 4);
            let dy = minus(hr, hp);
            for y in 0..word_height {
                out[y * word_width + x] = Pixel {
                    red: (result.red >> 7) + (result.red >> 2),
                    green: (result.green >> 7) + (result.green >> 2),
                    blue: (result.blue >> 7) + (result.blue >> 2),
                    alpha: (result.alpha >> 5) + (result.alpha >> 1),
                };
                add(&mut result, dy);
            }
            add(&mut hp, q_minus_p);
            add(&mut hr, s_minus_r);
        }
    } else {
        for y in 0..word_height {
            let mut result = times(hp, 4);
            let dy = minus(hr, hp);
            for x in 0..word_width {
                out[y * word_width + x] = Pixel {
                    red: (result.red >> 6) + (result.red >> 1),
                    green: (result.green >> 6) + (result.green >> 1),
                    blue: (result.blue >> 6) + (result.blue >> 1),
                    alpha: (result.alpha >> 4) + result.alpha,
                };
                add(&mut result, dy);
            }
            add(&mut hp, q_minus_p);
            add(&mut hr, s_minus_r);
        }
    }
}

type Grid = [[i32; 8]; 16];

fn unpack_modulations(word: Word, offset_x: usize, offset_y: usize, values: &mut Grid, modes: &mut Grid, bpp: u32) {
    let mut mode = word.color & 1;
    let mut bits = word.modulation;
    if bpp == 2 {
        if mode != 0 {
            if bits & 1 != 0 {
                mode = if bits & (1 << 20) != 0 { 3 } else { 2 };
                if bits & (1 << 21) != 0 {
                    bits |= 1 << 20;
                } else {
                    bits &= !(1 << 20);
                }
            }
            if bits & 2 != 0 {
                bits |= 1;
            } else {
                bits &= !1;
            }
            for y in 0..4 {
                for x in 0..8 {
                    modes[x + offset_x][y + offset_y] = mode as i32;
                    if (x ^ y) & 1 == 0 {
                        values[x + offset_x][y + offset_y] = (bits & 3) as i32;
                        bits >>= 2;
                    }
                }
            }
        } else {
            for y in 0..4 {
                for x in 0..8 {
                    modes[x + offset_x][y + offset_y] = mode as i32;
                    values[x + offset_x][y + offset_y] = if bits & 1 != 0 { 3 } else { 0 };
                    bits >>= 1;
                }
            }
        }
    } else if mode != 0 {
        for y in 0..4 {
            for x in 0..4 {
                values[y + offset_y][x + offset_x] = match bits & 3 {
                    1 => 4,
                    2 => 14, // +10: punch-through alpha
                    3 => 8,
                    v => v as i32,
                };
                bits >>= 2;
            }
        }
    } else {
        for y in 0..4 {
            for x in 0..4 {
                let mut value = (bits & 3) as i32 * 3;
                if value > 3 {
                    value -= 1;
                }
                values[y + offset_y][x + offset_x] = value;
                bits >>= 2;
            }
        }
    }
}

fn modulation_value(values: &Grid, modes: &Grid, x: usize, y: usize, bpp: u32) -> i32 {
    const REPEAT: [i32; 4] = [0, 3, 5, 8];
    if bpp != 2 {
        return values[x][y];
    }
    let at = |x: usize, y: usize| REPEAT[values[x][y] as usize];
    if modes[x][y] == 0 || (x ^ y) & 1 == 0 {
        at(x, y)
    } else if modes[x][y] == 1 {
        (at(x, y - 1) + at(x, y + 1) + at(x - 1, y) + at(x + 1, y) + 2) / 4
    } else if modes[x][y] == 2 {
        (at(x - 1, y) + at(x + 1, y) + 1) / 2
    } else {
        (at(x, y - 1) + at(x, y + 1) + 1) / 2
    }
}

fn decode_words(p: Word, q: Word, r: Word, s: Word, out: &mut [[u8; 4]; 32], bpp: u32) {
    let mut values: Grid = [[0; 8]; 16];
    let mut modes: Grid = [[0; 8]; 16];
    let mut upscaled_a = [Pixel::default(); 32];
    let mut upscaled_b = [Pixel::default(); 32];
    let word_width: usize = if bpp == 2 { 8 } else { 4 };
    let word_height: usize = 4;
    unpack_modulations(p, 0, 0, &mut values, &mut modes, bpp);
    unpack_modulations(q, word_width, 0, &mut values, &mut modes, bpp);
    unpack_modulations(r, 0, word_height, &mut values, &mut modes, bpp);
    unpack_modulations(s, word_width, word_height, &mut values, &mut modes, bpp);
    interpolate(color_a(p.color), color_a(q.color), color_a(r.color), color_a(s.color), &mut upscaled_a, bpp);
    interpolate(color_b(p.color), color_b(q.color), color_b(r.color), color_b(s.color), &mut upscaled_b, bpp);
    for y in 0..word_height {
        for x in 0..word_width {
            let mut modulation = modulation_value(&values, &modes, x + word_width / 2, y + word_height / 2, bpp);
            let punch_through = modulation > 10;
            if punch_through {
                modulation -= 10;
            }
            let a = upscaled_a[y * word_width + x];
            let b = upscaled_b[y * word_width + x];
            let mix = |a: i32, b: i32| (a * (8 - modulation) + b * modulation) / 8;
            let pixel = [
                mix(a.red, b.red) as u8,
                mix(a.green, b.green) as u8,
                mix(a.blue, b.blue) as u8,
                if punch_through { 0 } else { mix(a.alpha, b.alpha) as u8 },
            ];
            let index = if bpp == 2 { y * word_width + x } else { y + x * word_height };
            out[index] = pixel;
        }
    }
}

/// Position of a block in PVRTC's interleaved ("twiddled") block order.
fn twiddle(x_size: u32, y_size: u32, x: u32, y: u32) -> u32 {
    let (min_dimension, mut max_value) = if y_size < x_size { (y_size, x) } else { (x_size, y) };
    let mut twiddled = 0;
    let mut source_bit = 1;
    let mut target_bit = 1;
    let mut shift = 0;
    while source_bit < min_dimension {
        if y & source_bit != 0 {
            twiddled |= target_bit;
        }
        if x & source_bit != 0 {
            twiddled |= target_bit << 1;
        }
        source_bit <<= 1;
        target_bit <<= 2;
        shift += 1;
    }
    max_value >>= shift;
    twiddled | (max_value << (2 * shift))
}

/// Bytes a PVRTC image of this size takes.
pub fn size(width: u32, height: u32, two_bits: bool) -> usize {
    let (width, height) = if two_bits { (width.max(16), height.max(8)) } else { (width.max(8), height.max(8)) };
    (width as usize * height as usize) / if two_bits { 4 } else { 2 }
}

/// Decodes to RGBA, rows top to bottom. Sizes must be powers of two.
/// Big textures are decoded on several cores at once.
pub fn decode(data: &[u8], width: u32, height: u32, two_bits: bool) -> Option<Vec<u8>> {
    // Below this many pixels, splitting costs more than it saves.
    const SPLIT_FROM: u32 = 128 * 128;
    let cores = if width * height < SPLIT_FROM {
        1
    } else {
        crate::fw::workers::cores()
    };
    decode_on(data, width, height, two_bits, cores)
}

/// `decode` on this many cores (threads).
fn decode_on(data: &[u8], width: u32, height: u32, two_bits: bool, cores: usize) -> Option<Vec<u8>> {
    if !width.is_power_of_two() || !height.is_power_of_two() || data.len() < size(width, height, two_bits) {
        return None;
    }
    let bpp = if two_bits { 2 } else { 4 };
    let true_width = width.max(if two_bits { 16 } else { 8 });
    let true_height = height.max(8);
    let word_width: u32 = if two_bits { 8 } else { 4 };
    let word_height: u32 = 4;
    let words_x = true_width / word_width;
    let words_y = true_height / word_height;
    let word_at = |x: u32, y: u32| {
        let index = twiddle(words_x, words_y, x, y) as usize * 8;
        Word {
            modulation: u32::from_le_bytes(data[index..index + 4].try_into().unwrap()),
            color: u32::from_le_bytes(data[index + 4..index + 8].try_into().unwrap()),
        }
    };
    let wrap = |count: u32, n: i64| ((n + count as i64) % count as i64) as u32;
    let (ww, wh, tw) = (word_width as usize, word_height as usize, true_width as usize);
    // Each row of words `word_y` (from -1) decodes the lower half of word
    // row `word_y` (`top`) and the upper half of the next one (`bottom`),
    // wrapping around; no two rows write the same pixels.
    let band = |word_y: i64, top: &mut [[u8; 4]], bottom: &mut [[u8; 4]]| {
        let mut pixels = [[0u8; 4]; 32];
        for word_x in -1..(words_x as i64 - 1) {
            let p = (wrap(words_x, word_x), wrap(words_y, word_y));
            let q = (wrap(words_x, word_x + 1), wrap(words_y, word_y));
            let r = (wrap(words_x, word_x), wrap(words_y, word_y + 1));
            let s = (wrap(words_x, word_x + 1), wrap(words_y, word_y + 1));
            decode_words(word_at(p.0, p.1), word_at(q.0, q.1), word_at(r.0, r.1), word_at(s.0, s.1), &mut pixels, bpp);
            let (left, right) = (p.0 as usize * ww, q.0 as usize * ww);
            for y in 0..wh / 2 {
                for x in 0..ww / 2 {
                    top[y * tw + left + x + ww / 2] = pixels[y * ww + x];
                    top[y * tw + right + x] = pixels[y * ww + x + ww / 2];
                    bottom[y * tw + left + x + ww / 2] = pixels[(y + wh / 2) * ww + x];
                    bottom[y * tw + right + x] = pixels[(y + wh / 2) * ww + x + ww / 2];
                }
            }
        }
    };
    let mut out = vec![[0u8; 4]; (true_width * true_height) as usize];
    let half = wh / 2 * tw;
    let (first, rest) = out.split_at_mut(half);
    let (middle, last) = rest.split_at_mut(rest.len() - half);
    let mut bands = vec![(-1i64, last, first)];
    for (word_y, rows) in middle.chunks_mut(2 * half).enumerate() {
        let (top, bottom) = rows.split_at_mut(half);
        bands.push((word_y as i64, top, bottom));
    }
    if cores <= 1 {
        for (word_y, top, bottom) in bands {
            band(word_y, top, bottom);
        }
    } else {
        crate::fw::workers::split(&mut bands, cores, &|group| {
            for (word_y, top, bottom) in group {
                band(*word_y, top, bottom);
            }
        });
    }
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height as usize {
        for x in 0..width as usize {
            rgba.extend_from_slice(&out[y * tw + x]);
        }
    }
    Some(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One opaque color in every block (both colors the same) gives that color everywhere.
    #[test]
    fn flat_color_4bpp() {
        // Color B opaque RGB555 (31, 0, 0); color A opaque RGB554 (31, 0, 0); mode 0.
        let color: u32 = 0x8000_0000 | (31 << 26) | 0x8000 | (31 << 10);
        let mut data = Vec::new();
        for _ in 0..4 {
            data.extend_from_slice(&0u32.to_le_bytes());
            data.extend_from_slice(&color.to_le_bytes());
        }
        let rgba = decode(&data, 8, 8, false).unwrap();
        assert_eq!(rgba.len(), 8 * 8 * 4);
        for pixel in rgba.chunks(4) {
            assert_eq!(pixel, [255, 0, 0, 255], "{pixel:?}");
        }
    }

    /// Made-up texture bytes, the same every run.
    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        (0..len).map(|_| {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 56) as u8
        }).collect()
    }

    fn fnv(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
    }

    /// Unpacking on several cores gives the same pixels as the
    /// one-core unpacker did (its results are the numbers below).
    #[test]
    fn every_core_count_unpacks_the_same_pixels() {
        let cases: [(u32, u32, bool, u64); 6] = [
            (8, 8, false, 2797267487922463627),
            (16, 8, true, 18441435060227567776),
            (4, 4, false, 8489950762733553910),
            (256, 128, false, 4873371005225445656),
            (64, 256, true, 3162502010997953893),
            (512, 512, false, 2507185222192835500),
        ];
        let found: Vec<u64> = cases.iter().enumerate().map(|(index, &(width, height, two_bits, _))| {
            let data = noise(size(width, height, two_bits), index as u64 + 1);
            let one = decode_on(&data, width, height, two_bits, 1).unwrap();
            for cores in [2, 3, 8] {
                assert!(decode_on(&data, width, height, two_bits, cores).unwrap() == one, "{width}x{height} on {cores} cores");
            }
            assert!(decode(&data, width, height, two_bits).unwrap() == one);
            fnv(&one)
        }).collect();
        assert_eq!(found, cases.iter().map(|c| c.3).collect::<Vec<_>>());
    }

    #[test]
    fn sizes() {
        assert_eq!(size(256, 256, false), 32768);
        assert_eq!(size(4, 4, false), 32);
        assert_eq!(size(256, 256, true), 16384);
        assert_eq!(size(8, 8, true), 32);
    }
}
