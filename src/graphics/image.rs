use std::path;
use std::io::Read;

use gfx;
use gfx_device_gl;
use image;

use graphics::*;
use graphics::shader::*;
use context::Context;
use GameResult;
use GameError;

/// Generic in-GPU-memory image data available to be drawn on the screen.
#[derive(Clone)]
pub struct ImageGeneric<R>
where
    R: gfx::Resources,
{
    // TODO: Rename to shader_view or such.
    pub(crate) texture: gfx::handle::ShaderResourceView<R, [f32; 4]>,
    pub(crate) texture_handle: gfx::handle::Texture<R, gfx::format::R8_G8_B8_A8>,
    pub(crate) sampler_info: gfx::texture::SamplerInfo,
    pub(crate) blend_mode: Option<BlendMode>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// In-GPU-memory image data available to be drawn on the screen,
/// using the OpenGL backend.
///
/// Under the hood this is just an `Arc`'ed texture handle and
/// some metadata, so cloning it is fairly cheap; it doesn't
/// make another copy of the underlying image data.
pub type Image = ImageGeneric<gfx_device_gl::Resources>;

/// The supported formats for saving an image.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ImageFormat {
    /// .png image format (defaults to RGBA with 8-bit channels.)
    Png,
}

impl Image {
    /// Load a new image from the file at the given path.
    pub fn new<P: AsRef<path::Path>>(context: &mut Context, path: P) -> GameResult<Image> {
        let img = {
            let mut buf = Vec::new();
            let mut reader = context.filesystem.open(path)?;
            reader.read_to_end(&mut buf)?;
            image::load_from_memory(&buf)?.to_rgba()
        };
        let (width, height) = img.dimensions();
        Image::from_rgba8(context, width as u16, height as u16, &img)
    }

    /// Creates a new `Image` from the given buffer of `u8` RGBA values.
    pub fn from_rgba8(
        context: &mut Context,
        width: u16,
        height: u16,
        rgba: &[u8],
    ) -> GameResult<Image> {
        Image::make_raw(
            &mut context.gfx_context.factory,
            &context.gfx_context.default_sampler_info,
            width,
            height,
            rgba,
        )
    }

    /// Dumps the `Image`'s data to a `Vec` of `u8` RGBA values.
    pub fn to_rgba8(&self, ctx: &mut Context) -> GameResult<Vec<u8>> {
        use gfx::memory::Typed;
        use gfx::format::Formatted;
        use gfx::format::SurfaceTyped;
        use gfx::traits::FactoryExt;

        let gfx = &mut ctx.gfx_context;
        let (w, h, _, _) = gfx.data.out.get_dimensions();
        type SurfaceData = <<ColorFormat as Formatted>::Surface as SurfaceTyped>::DataType;

        // Note: In the GFX example, the download buffer is created ahead of time
        // and updated on screen resize events. This may be preferable, but then
        // the buffer also needs to be updated when we switch to/from a canvas.
        // Unsure of the performance impact of creating this as it is needed.
        let dl_buffer = gfx.factory
            .create_download_buffer::<SurfaceData>(w as usize * h as usize)?;

        let mut local_encoder: gfx::Encoder<
            gfx_device_gl::Resources,
            gfx_device_gl::CommandBuffer,
        > = gfx.factory.create_command_buffer().into();

        local_encoder.copy_texture_to_buffer_raw(
            self.texture_handle.raw(),
            None,
            gfx::texture::RawImageInfo {
                xoffset: 0,
                yoffset: 0,
                zoffset: 0,
                width: self.width as u16,
                height: self.height as u16,
                depth: 0,
                format: ColorFormat::get_format(),
                mipmap: 0,
            },
            dl_buffer.raw(),
            0,
        )?;
        local_encoder.flush(&mut *gfx.device);

        let reader = gfx.factory.read_mapping(&dl_buffer)?;

        // intermediary buffer to avoid casting
        // and also to reverse the order in which we pass the rows
        // so the screenshot isn't upside-down
        let mut data = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        for row in reader.chunks(w as usize).rev() {
            for pixel in row.iter() {
                data.extend(pixel);
            }
        }
        Ok(data)
    }

    /// Encode the `Image` to the given file format and
    /// write it out to the given path.
    ///
    /// See the `filesystem` module docs for where exactly
    /// the file will end up.
    pub fn encode<P: AsRef<path::Path>>(
        &self,
        ctx: &mut Context,
        format: ImageFormat,
        path: P,
    ) -> GameResult<()> {
        use std::io;
        let data = self.to_rgba8(ctx)?;
        let f = ctx.filesystem.create(path)?;
        let writer = &mut io::BufWriter::new(f);
        let color_format = image::ColorType::RGBA(8);
        match format {
            ImageFormat::Png => image::png::PNGEncoder::new(writer)
                .encode(&data, self.width, self.height, color_format)
                .map_err(|e| e.into()),
        }
    }

    /// A helper function that just takes a factory directly so we can make an image
    /// without needing the full context object, so we can create an Image while still
    /// creating the GraphicsContext.
    pub(crate) fn make_raw(
        factory: &mut gfx_device_gl::Factory,
        sampler_info: &texture::SamplerInfo,
        width: u16,
        height: u16,
        rgba: &[u8],
    ) -> GameResult<Image> {
        if width == 0 || height == 0 {
            let msg = format!(
                "Tried to create a texture of size {}x{}, each dimension must
                be >0",
                width, height
            );
            return Err(GameError::ResourceLoadError(msg));
        }
        // TODO: Check for overflow on 32-bit systems here
        let expected_bytes = width as usize * height as usize * 4;
        if expected_bytes != rgba.len() {
            let msg = format!("Tried to create a texture of size {}x{}, but gave {} bytes of data (expected {})", width, height, rgba.len(), expected_bytes);
            return Err(GameError::ResourceLoadError(msg));
        }
        let kind = gfx::texture::Kind::D2(width, height, gfx::texture::AaMode::Single);
        let (tex, view) = factory.create_texture_immutable_u8::<gfx::format::Srgba8>(
            kind,
            gfx::texture::Mipmap::Provided,
            &[rgba],
        )?;
        Ok(Image {
            texture: view,
            texture_handle: tex,
            sampler_info: *sampler_info,
            blend_mode: None,
            width: u32::from(width),
            height: u32::from(height),
        })
    }

    /// A little helper function that creates a new Image that is just
    /// a solid square of the given size and color.  Mainly useful for
    /// debugging.
    pub fn solid(context: &mut Context, size: u16, color: Color) -> GameResult<Image> {
        // let pixel_array: [u8; 4] = color.into();
        let (r, g, b, a) = color.into();
        let pixel_array: [u8; 4] = [r, g, b, a];
        let size_squared = size as usize * size as usize;
        let mut buffer = Vec::with_capacity(size_squared);
        for _i in 0..size_squared {
            buffer.extend(&pixel_array[..]);
        }
        Image::from_rgba8(context, size, size, &buffer)
    }

    /// Return the width of the image.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Return the height of the image.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Get the filter mode for the image.
    pub fn get_filter(&self) -> FilterMode {
        self.sampler_info.filter.into()
    }

    /// Set the filter mode for the image.
    pub fn set_filter(&mut self, mode: FilterMode) {
        self.sampler_info.filter = mode.into();
    }

    /// Returns the dimensions of the image.
    pub fn get_dimensions(&self) -> Rect {
        Rect::new(0.0, 0.0, self.width() as f32, self.height() as f32)
    }

    /// Gets the `Image`'s `WrapMode` along the X and Y axes.
    pub fn get_wrap(&self) -> (WrapMode, WrapMode) {
        (self.sampler_info.wrap_mode.0, self.sampler_info.wrap_mode.1)
    }

    /// Sets the `Image`'s `WrapMode` along the X and Y axes.
    pub fn set_wrap(&mut self, wrap_x: WrapMode, wrap_y: WrapMode) {
        self.sampler_info.wrap_mode.0 = wrap_x;
        self.sampler_info.wrap_mode.1 = wrap_y;
    }
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "<Image: {}x{}, {:p}, texture address {:p}, sampler: {:?}>",
            self.width(),
            self.height(),
            self,
            &self.texture,
            &self.sampler_info
        )
    }
}

impl Drawable for Image {
    fn draw_ex(&self, ctx: &mut Context, param: DrawParam) -> GameResult<()> {
        let gfx = &mut ctx.gfx_context;
        let src_width = param.src.w;
        let src_height = param.src.h;
        // We have to mess with the scale to make everything
        // be its-unit-size-in-pixels.
        let real_scale = Point2::new(
            src_width * param.scale.x * self.width as f32,
            src_height * param.scale.y * self.height as f32,
        );
        let mut new_param = param;
        new_param.scale = real_scale;
        gfx.update_instance_properties(new_param)?;
        let sampler = gfx.samplers
            .get_or_insert(self.sampler_info, gfx.factory.as_mut());
        gfx.data.vbuf = gfx.quad_vertex_buffer.clone();
        gfx.data.tex = (self.texture.clone(), sampler);
        let previous_mode: Option<BlendMode> = if let Some(mode) = self.blend_mode {
            let current_mode = gfx.get_blend_mode();
            if current_mode != mode {
                gfx.set_blend_mode(mode)?;
                Some(current_mode)
            } else {
                None
            }
        } else {
            None
        };
        gfx.draw(None)?;
        if let Some(mode) = previous_mode {
            gfx.set_blend_mode(mode)?;
        }
        Ok(())
    }
    fn set_blend_mode(&mut self, mode: Option<BlendMode>) {
        self.blend_mode = mode;
    }
    fn get_blend_mode(&self) -> Option<BlendMode> {
        self.blend_mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // We need to set up separate unit tests for CI vs non-CI environments; see issue #234
    // #[test]
    #[allow(dead_code)]
    fn test_invalid_image_size() {
        let c = conf::Conf::new();
        let ctx = &mut Context::load_from_conf("unittest", "unittest", c).unwrap();
        let _i = assert!(Image::from_rgba8(ctx, 0, 0, &vec![]).is_err());
        let _i = assert!(Image::from_rgba8(ctx, 3432, 432, &vec![]).is_err());
        let _i = Image::from_rgba8(ctx, 2, 2, &vec![99;16]).unwrap();
    }
}