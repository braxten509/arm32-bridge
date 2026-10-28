//! Rule the Kingdom's sharper pictures (texture_pack.rs): every picture file
//! the game reads goes through one function, Render::GetImageData, which
//! returns its pixels (premultiplied RGBA, or RGB) for the game to upload.
//! The bridge steps into it to learn which picture those pixels are.

use crate::runtime::{mem, ArgSource, Args, Ret, Uc};

/// `Render::GetImageData(char const* path, unsigned& w, unsigned& h, unsigned& bpp, void*)` (libkingdom.so 5.11).
pub const GET_IMAGE_DATA: &str = "_ZN6Render12GetImageDataEPKcRjS2_S2_Pv";

/// The stand-in (wraps.rs): runs the original, then tells the pack layer what it loaded.
pub fn get_image_data(uc: &mut Uc, args: &mut Args) -> Ret {
    // The fifth argument is on the stack; call_guest puts it back there.
    let arguments = [args.u32(), args.u32(), args.u32(), args.u32(), args.u32()];
    let pixels = match crate::runtime::call_guest(uc, crate::wraps::original(GET_IMAGE_DATA), &arguments) {
        Ok(result) => result as u32,
        Err(why) => {
            crate::log::line(&format!("[textures] {GET_IMAGE_DATA}: {why}"));
            return Ret::U32(0);
        }
    };
    let [path, width, height, bits, _] = arguments;
    if pixels != 0 && path != 0 && crate::texture_pack::active() {
        let memory = mem();
        let name = memory.string(path);
        crate::texture_pack::loaded(&name, pixels, memory.read_u32(width), memory.read_u32(height), memory.read_u32(bits));
    }
    Ret::U32(pixels)
}
