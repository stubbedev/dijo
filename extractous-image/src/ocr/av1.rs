//! AV1 pictures decoded by rav1d, the port of dav1d to Rust, through the dav1d API it keeps:
//! one temporal unit in, its last picture out, with the planes copied before the decoder is
//! closed. The unsafe code of this module is the calls of that C-style API.

use std::mem::MaybeUninit;
use std::ptr::NonNull;

use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::headers::{
    DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422,
    DAV1D_PIXEL_LAYOUT_I444,
};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{
    dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings, dav1d_get_picture,
    dav1d_open, dav1d_picture_unref, dav1d_send_data,
};
use rav1d::Dav1dResult;

use super::decode::MAX_DECODED_PIXELS;

/// How the chroma planes are subsampled, as horizontal and vertical shifts, or none for a
/// picture of luma alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Chroma {
    Monochrome,
    Subsampled { x: u8, y: u8 },
}

/// A plane's samples, row by row without padding.
#[derive(Debug, Clone)]
pub(crate) struct Plane {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) samples: Vec<u16>,
}

impl Plane {
    pub(crate) fn at(&self, x: usize, y: usize) -> u16 {
        self.samples[y * self.width + x]
    }
}

/// A decoded picture: its planes, luma then the two chroma planes unless it is monochrome, of
/// `depth` bits a sample, and the colour description of its sequence header.
#[derive(Debug, Clone)]
pub(crate) struct Picture {
    pub(crate) depth: u8,
    pub(crate) chroma: Chroma,
    pub(crate) planes: Vec<Plane>,
    /// The colour primaries and the matrix coefficients, as ISO/IEC 23091-2 numbers them.
    pub(crate) primaries: u16,
    pub(crate) matrix: u16,
    pub(crate) full_range: bool,
}

/// A decoder context, closed when dropped.
struct Context(Option<Dav1dContext>);

impl Drop for Context {
    fn drop(&mut self) {
        unsafe { dav1d_close(Some(NonNull::from(&mut self.0))) };
    }
}

/// A picture the decoder returned, released when dropped.
struct Returned(Dav1dPicture);

impl Drop for Returned {
    fn drop(&mut self) {
        unsafe { dav1d_picture_unref(Some(NonNull::from(&mut self.0))) };
    }
}

fn checked(result: Dav1dResult, attempt: &str) -> Result<(), String> {
    match result.0 {
        0 => Ok(()),
        error => Err(format!("{attempt} failed with error {}", -error)),
    }
}

/// The picture of an AV1 temporal unit, such as an AV1 image item: the last one it decodes to,
/// which for an item of several layers is the whole image, with film grain applied. A frame of
/// more than [`MAX_DECODED_PIXELS`] pixels is not decoded.
pub(crate) fn decode(temporal_unit: &[u8]) -> Result<Picture, String> {
    let mut settings = MaybeUninit::<Dav1dSettings>::uninit();
    unsafe { dav1d_default_settings(NonNull::from(&mut settings).cast()) };
    let mut settings = unsafe { settings.assume_init() };
    settings.n_threads = 1;
    settings.max_frame_delay = 1;
    settings.apply_grain = 1;
    settings.frame_size_limit = MAX_DECODED_PIXELS as u32;
    let mut context = Context(None);
    checked(
        unsafe {
            dav1d_open(
                Some(NonNull::from(&mut context.0)),
                Some(NonNull::from(&mut settings)),
            )
        },
        "opening the AV1 decoder",
    )?;
    let handle = context.0.ok_or("the AV1 decoder opened no context")?;
    let mut data = Dav1dData::default();
    let buffer = unsafe { dav1d_data_create(Some(NonNull::from(&mut data)), temporal_unit.len()) };
    let buffer = NonNull::new(buffer).ok_or("cannot allocate the AV1 data")?;
    unsafe {
        std::ptr::copy_nonoverlapping(temporal_unit.as_ptr(), buffer.as_ptr(), temporal_unit.len())
    };
    let mut last: Option<Returned> = None;
    let sent = loop {
        let result = unsafe { dav1d_send_data(Some(handle), Some(NonNull::from(&mut data))) };
        if result.0 == 0 {
            break Ok(());
        }
        match next_picture(handle) {
            Some(picture) => last = Some(picture),
            None => break checked(result, "sending the AV1 data"),
        }
    };
    unsafe { dav1d_data_unref(Some(NonNull::from(&mut data))) };
    sent?;
    while let Some(picture) = next_picture(handle) {
        last = Some(picture);
    }
    let last = last.ok_or("the AV1 data decodes to no picture")?;
    let picture = copied(&last.0);
    drop(last);
    drop(context);
    picture
}

/// The next picture the decoder has, if any.
fn next_picture(handle: Dav1dContext) -> Option<Returned> {
    let mut picture = Returned(Dav1dPicture::default());
    let result = unsafe { dav1d_get_picture(Some(handle), Some(NonNull::from(&mut picture.0))) };
    (result.0 == 0).then_some(picture)
}

/// A returned picture's planes and colour description, copied out of the decoder's buffers.
fn copied(picture: &Dav1dPicture) -> Result<Picture, String> {
    let depth = picture.p.bpc;
    let width = usize::try_from(picture.p.w).map_err(|_| "a picture of negative width")?;
    let height = usize::try_from(picture.p.h).map_err(|_| "a picture of negative height")?;
    let chroma = match picture.p.layout {
        DAV1D_PIXEL_LAYOUT_I400 => Chroma::Monochrome,
        DAV1D_PIXEL_LAYOUT_I420 => Chroma::Subsampled { x: 1, y: 1 },
        DAV1D_PIXEL_LAYOUT_I422 => Chroma::Subsampled { x: 1, y: 0 },
        DAV1D_PIXEL_LAYOUT_I444 => Chroma::Subsampled { x: 0, y: 0 },
        layout => return Err(format!("the AV1 pixel layout {layout}")),
    };
    let sample_bytes: usize = match depth {
        8 => 1,
        10 | 12 => 2,
        _ => return Err(format!("AV1 samples of {depth} bits")),
    };
    let planes = match chroma {
        Chroma::Monochrome => 1,
        Chroma::Subsampled { .. } => 3,
    };
    let mut copies = Vec::with_capacity(planes);
    for index in 0..planes {
        let (plane_width, plane_height) = match (index, chroma) {
            (0, _) | (_, Chroma::Monochrome) => (width, height),
            (_, Chroma::Subsampled { x, y }) => (
                (width + usize::from(x)) >> x,
                (height + usize::from(y)) >> y,
            ),
        };
        let data = picture.data[index].ok_or_else(|| format!("plane {index} has no samples"))?;
        let stride = picture.stride[usize::from(index != 0)];
        let stride = usize::try_from(stride)
            .ok()
            .filter(|stride| *stride >= plane_width * sample_bytes)
            .ok_or_else(|| format!("plane {index} has a stride of {stride} bytes"))?;
        let bytes = unsafe {
            std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), stride * plane_height)
        };
        let mut samples = Vec::with_capacity(plane_width * plane_height);
        for row in bytes.chunks_exact(stride) {
            let row = &row[..plane_width * sample_bytes];
            match sample_bytes {
                1 => samples.extend(row.iter().map(|&sample| u16::from(sample))),
                _ => samples.extend(
                    row.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|&sample| u16::from_ne_bytes(sample)),
                ),
            }
        }
        copies.push(Plane {
            width: plane_width,
            height: plane_height,
            samples,
        });
    }
    let header = picture
        .seq_hdr
        .ok_or("the AV1 picture has no sequence header")?;
    let header = unsafe { header.as_ref() };
    Ok(Picture {
        depth: depth as u8,
        chroma,
        planes: copies,
        primaries: u16::try_from(header.pri).map_err(|_| "colour primaries past 65535")?,
        matrix: u16::try_from(header.mtrx).map_err(|_| "matrix coefficients past 65535")?,
        full_range: header.color_range != 0,
    })
}
