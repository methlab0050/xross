//! Dev screenshots (`xross msg screenshot PATH`): read back the frame just rendered and
//! save it as a PNG. Lets scripts check what's on screen in the nested backend, where
//! the host compositor won't let `grim` in.
//!
//! Split in two because mapping the copy makes the GL context current without the
//! window surface: [`start`] runs before the frame is submitted, [`finish`] after.

use std::{error::Error, fs::File, io::BufWriter, path::Path};

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ExportMem,
            gles::{GlesMapping, GlesRenderer, GlesTarget},
        },
    },
    utils::{Physical, Rectangle, Size},
};

/// Queue a copy of the framebuffer (before submitting it).
pub fn start(
    renderer: &mut GlesRenderer,
    framebuffer: &GlesTarget,
    size: Size<i32, Physical>,
) -> Result<GlesMapping, Box<dyn Error>> {
    Ok(renderer.copy_framebuffer(framebuffer, Rectangle::from_size((size.w, size.h).into()), Fourcc::Abgr8888)?)
}

/// Read the copy back and write the PNG (after submitting).
pub fn finish(
    renderer: &mut GlesRenderer,
    mapping: &GlesMapping,
    size: Size<i32, Physical>,
    path: &Path,
) -> Result<(), Box<dyn Error>> {
    let data = renderer.map_texture(mapping)?;
    // GL reads rows bottom-up.
    let stride = size.w as usize * 4;
    let mut rgba = Vec::with_capacity(data.len());
    for row in data.chunks_exact(stride).rev() {
        rgba.extend_from_slice(row);
    }
    let mut encoder = png::Encoder::new(BufWriter::new(File::create(path)?), size.w as u32, size.h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    Ok(())
}
