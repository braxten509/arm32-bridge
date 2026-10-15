//! S3TC (DXT1/DXT3/DXT5) texture decoding, for drivers that lack the formats.
//!
//! Games made for NVIDIA Tegra devices (Ouya, Shield) upload DXT textures
//! without checking GL_EXT_texture_compression_s3tc. Most phone GPUs reject
//! them, and the texture stays black. The bridge decodes them to RGBA8 instead
//! (4x the GPU memory of DXT5, exact colors).

pub const DXT1_RGB: u32 = 0x83F0;
pub const DXT1_RGBA: u32 = 0x83F1;
pub const DXT3: u32 = 0x83F2;
pub const DXT5: u32 = 0x83F3;
pub const SRGB_DXT1: u32 = 0x8C4C;
pub const SRGB_ALPHA_DXT1: u32 = 0x8C4D;
pub const SRGB_ALPHA_DXT3: u32 = 0x8C4E;
pub const SRGB_ALPHA_DXT5: u32 = 0x8C4F;

pub fn is_s3tc(format: u32) -> bool {
    matches!(format, DXT1_RGB | DXT1_RGBA | DXT3 | DXT5 | SRGB_DXT1 | SRGB_ALPHA_DXT1 | SRGB_ALPHA_DXT3 | SRGB_ALPHA_DXT5)
}

pub fn is_srgb(format: u32) -> bool {
    matches!(format, SRGB_DXT1 | SRGB_ALPHA_DXT1 | SRGB_ALPHA_DXT3 | SRGB_ALPHA_DXT5)
}

/// Bytes per 4x4 block.
fn block_size(format: u32) -> usize {
    match format {
        DXT1_RGB | DXT1_RGBA | SRGB_DXT1 | SRGB_ALPHA_DXT1 => 8,
        _ => 16,
    }
}

/// Bytes a `width` x `height` image takes in `format`.
pub fn data_size(format: u32, width: u32, height: u32) -> usize {
    width.div_ceil(4) as usize * height.div_ceil(4) as usize * block_size(format)
}

/// The 565 color as 8-bit RGB.
fn expand_565(color: u16) -> [u8; 3] {
    let (r, g, b) = ((color >> 11) & 31, (color >> 5) & 63, color & 31);
    [((r << 3) | (r >> 2)) as u8, ((g << 2) | (g >> 4)) as u8, ((b << 3) | (b >> 2)) as u8]
}

/// The four colors of a color block. DXT1 blocks whose first endpoint is not
/// larger use three colors plus transparent black (index 3); DXT3/5 color
/// blocks always use four.
fn palette(block: &[u8], dxt1: bool) -> [[u8; 4]; 4] {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let (a, b) = (expand_565(c0), expand_565(c1));
    let mix = |wa: u32, wb: u32, total: u32| -> [u8; 4] {
        let channel = |i: usize| ((a[i] as u32 * wa + b[i] as u32 * wb) / total) as u8;
        [channel(0), channel(1), channel(2), 255]
    };
    if c0 > c1 || !dxt1 {
        [[a[0], a[1], a[2], 255], [b[0], b[1], b[2], 255], mix(2, 1, 3), mix(1, 2, 3)]
    } else {
        [[a[0], a[1], a[2], 255], [b[0], b[1], b[2], 255], mix(1, 1, 2), [0, 0, 0, 0]]
    }
}

/// The 16 alpha values of a DXT5 alpha block.
fn dxt5_alpha(block: &[u8]) -> [u8; 16] {
    let (a0, a1) = (block[0] as u32, block[1] as u32);
    let mut levels = [0u8; 8];
    levels[0] = a0 as u8;
    levels[1] = a1 as u8;
    if a0 > a1 {
        for i in 1..7 {
            levels[i + 1] = (((7 - i as u32) * a0 + i as u32 * a1) / 7) as u8;
        }
    } else {
        for i in 1..5 {
            levels[i + 1] = (((5 - i as u32) * a0 + i as u32 * a1) / 5) as u8;
        }
        levels[6] = 0;
        levels[7] = 255;
    }
    let mut bits = 0u64;
    for (i, &byte) in block[2..8].iter().enumerate() {
        bits |= (byte as u64) << (8 * i);
    }
    std::array::from_fn(|pixel| levels[((bits >> (3 * pixel)) & 7) as usize])
}

/// Decodes `data` (a `width` x `height` S3TC image) to tightly packed RGBA8.
/// Returns None when `data` is too short.
pub fn decode(format: u32, data: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let (width, height) = (width as usize, height as usize);
    if data.len() < data_size(format, width as u32, height as u32) {
        return None;
    }
    let size = block_size(format);
    let dxt1 = size == 8;
    // DXT1 without alpha draws index 3 as opaque black.
    let opaque = matches!(format, DXT1_RGB | SRGB_DXT1);
    let blocks_across = width.div_ceil(4);
    let mut out = vec![0u8; width * height * 4];
    for block_y in 0..height.div_ceil(4) {
        for block_x in 0..blocks_across {
            let block = &data[(block_y * blocks_across + block_x) * size..][..size];
            let (alpha, color) = if dxt1 { (None, block) } else { (Some(&block[..8]), &block[8..]) };
            let colors = palette(color, dxt1);
            let indices = u32::from_le_bytes([color[4], color[5], color[6], color[7]]);
            let alphas: Option<[u8; 16]> = alpha.map(|alpha| {
                if matches!(format, DXT3 | SRGB_ALPHA_DXT3) {
                    std::array::from_fn(|pixel| {
                        let nibble = (alpha[pixel / 2] >> (4 * (pixel % 2))) & 15;
                        nibble * 17
                    })
                } else {
                    dxt5_alpha(alpha)
                }
            });
            for py in 0..4 {
                let y = block_y * 4 + py;
                if y >= height {
                    break;
                }
                for px in 0..4 {
                    let x = block_x * 4 + px;
                    if x >= width {
                        break;
                    }
                    let pixel = py * 4 + px;
                    let mut rgba = colors[((indices >> (2 * pixel)) & 3) as usize];
                    if opaque {
                        rgba[3] = 255;
                    }
                    if let Some(alphas) = &alphas {
                        rgba[3] = alphas[pixel];
                    }
                    out[(y * width + x) * 4..][..4].copy_from_slice(&rgba);
                }
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dxt1_four_colors() {
        // c0 = pure red (0xF800) > c1 = pure blue (0x001F); indices 0,1,2,3 on the first row.
        let block = [0x00, 0xF8, 0x1F, 0x00, 0b11_10_01_00, 0, 0, 0];
        let out = decode(DXT1_RGB, &block, 4, 4).unwrap();
        assert_eq!(&out[0..4], &[255, 0, 0, 255]);
        assert_eq!(&out[4..8], &[0, 0, 255, 255]);
        assert_eq!(&out[8..12], &[170, 0, 85, 255]);
        assert_eq!(&out[12..16], &[85, 0, 170, 255]);
        // Second row: index 0.
        assert_eq!(&out[16..20], &[255, 0, 0, 255]);
    }

    #[test]
    fn dxt1_three_colors_and_transparent() {
        // c0 = blue <= c1 = red: index 2 is the midpoint, index 3 transparent (opaque black without alpha).
        let block = [0x1F, 0x00, 0x00, 0xF8, 0b11_10_01_00, 0, 0, 0];
        let rgba = decode(DXT1_RGBA, &block, 4, 4).unwrap();
        assert_eq!(&rgba[8..12], &[127, 0, 127, 255]);
        assert_eq!(&rgba[12..16], &[0, 0, 0, 0]);
        let rgb = decode(DXT1_RGB, &block, 4, 4).unwrap();
        assert_eq!(&rgb[12..16], &[0, 0, 0, 255]);
    }

    #[test]
    fn dxt3_explicit_alpha() {
        let mut block = [0u8; 16];
        block[0] = 0xF0; // pixel 0 alpha 0, pixel 1 alpha 15
        block[8..10].copy_from_slice(&0xFFFFu16.to_le_bytes()); // white
        let out = decode(DXT3, &block, 4, 4).unwrap();
        assert_eq!(&out[0..4], &[255, 255, 255, 0]);
        assert_eq!(&out[4..8], &[255, 255, 255, 255]);
    }

    #[test]
    fn dxt5_interpolated_alpha() {
        let mut block = [0u8; 16];
        block[0] = 255;
        block[1] = 0;
        // pixel 0 -> index 0 (255), pixel 1 -> index 1 (0), pixel 2 -> index 2 (6/7 * 255 = 218)
        let bits: u64 = 1 << 3 | 2 << 6;
        block[2..8].copy_from_slice(&bits.to_le_bytes()[..6]);
        block[8..10].copy_from_slice(&0xFFFFu16.to_le_bytes());
        let out = decode(DXT5, &block, 4, 4).unwrap();
        assert_eq!(out[3], 255);
        assert_eq!(out[7], 0);
        assert_eq!(out[11], 218);
        // Six-level mode: index 6 is 0, index 7 is 255.
        block[0] = 0;
        block[1] = 255;
        let bits: u64 = 6 | 7 << 3;
        block[2..8].copy_from_slice(&bits.to_le_bytes()[..6]);
        let out = decode(DXT5, &block, 4, 4).unwrap();
        assert_eq!((out[3], out[7]), (0, 255));
    }

    #[test]
    fn small_mip_levels_crop() {
        // A 2x1 image still takes one whole block.
        let block = [0x00, 0xF8, 0x1F, 0x00, 0b01_00, 0, 0, 0];
        let out = decode(DXT1_RGB, &block, 2, 1).unwrap();
        assert_eq!(out, vec![255, 0, 0, 255, 0, 0, 255, 255]);
        assert!(decode(DXT5, &block, 4, 4).is_none());
    }
}
