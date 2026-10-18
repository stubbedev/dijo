//! HEIC and HEIF images decoded for OCR by libheif, as our Java `HeifDecoder` has them decoded,
//! since no decoder of HEVC in Rust is proven yet: by the libheif linked into the static
//! executable, and otherwise by macOS's own `sips`, by the heif-png the OCR bundle carries, or
//! by libheif's `heif-dec` on the `PATH`, each writing a PNG file this module reads back.

use std::time::Duration;

use super::bitmap::Bitmap;
use super::decode::Undecoded;

/// The primary image of a HEIC or HEIF file, decoded within `timeout` when it has one.
pub(crate) fn decode(input: &[u8], timeout: Option<Duration>) -> Result<Bitmap, Undecoded> {
    imp::decode(input, timeout)
}

/// The primary image of a HEIC or HEIF file decoded by libheif on every platform, the decoder
/// the native AVIF decoder is held to.
#[cfg(test)]
pub(crate) fn libheif(input: &[u8]) -> Result<Bitmap, Undecoded> {
    imp::libheif(input, None)
}

#[cfg(feature = "static")]
mod imp {
    use std::ffi::{c_char, c_int, c_void, CStr};
    use std::sync::Once;
    use std::time::Duration;

    use image::RgbImage;

    use crate::ocr::bitmap::Bitmap;
    use crate::ocr::decode::{decodable, Undecoded};

    #[repr(C)]
    struct Context {
        _opaque: [u8; 0],
    }

    #[repr(C)]
    struct Handle {
        _opaque: [u8; 0],
    }

    #[repr(C)]
    struct Image {
        _opaque: [u8; 0],
    }

    #[repr(C)]
    struct Error {
        code: c_int,
        subcode: c_int,
        message: *const c_char,
    }

    /// `heif_colorspace_RGB`.
    const RGB: c_int = 1;
    /// `heif_chroma_interleaved_RGB`.
    const INTERLEAVED_RGB: c_int = 10;
    /// `heif_channel_interleaved`.
    const INTERLEAVED: c_int = 10;

    extern "C" {
        fn heif_init(params: *const c_void) -> Error;
        fn heif_context_alloc() -> *mut Context;
        fn heif_context_free(context: *mut Context);
        fn heif_context_read_from_memory_without_copy(
            context: *mut Context,
            memory: *const c_void,
            size: usize,
            options: *const c_void,
        ) -> Error;
        fn heif_context_get_primary_image_handle(
            context: *mut Context,
            handle: *mut *mut Handle,
        ) -> Error;
        fn heif_image_handle_release(handle: *const Handle);
        fn heif_decode_image(
            handle: *const Handle,
            image: *mut *mut Image,
            colorspace: c_int,
            chroma: c_int,
            options: *const c_void,
        ) -> Error;
        fn heif_image_release(image: *const Image);
        fn heif_image_get_width(image: *const Image, channel: c_int) -> c_int;
        fn heif_image_get_height(image: *const Image, channel: c_int) -> c_int;
        fn heif_image_get_plane_readonly2(
            image: *const Image,
            channel: c_int,
            stride: *mut usize,
        ) -> *const u8;
    }

    /// Fails with libheif's message when a call failed.
    fn checked(error: Error, attempt: &str) -> Result<(), Undecoded> {
        if error.code == 0 {
            return Ok(());
        }
        let message = match error.message.is_null() {
            true => "unknown error".into(),
            false => unsafe { CStr::from_ptr(error.message) }.to_string_lossy(),
        };
        Err(Undecoded::Failed(format!("{attempt}: {message}")))
    }

    struct OwnedContext(*mut Context);

    impl Drop for OwnedContext {
        fn drop(&mut self) {
            unsafe { heif_context_free(self.0) };
        }
    }

    struct OwnedHandle(*mut Handle);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe { heif_image_handle_release(self.0) };
        }
    }

    struct OwnedImage(*mut Image);

    impl Drop for OwnedImage {
        fn drop(&mut self) {
            unsafe { heif_image_release(self.0) };
        }
    }

    /// The primary image decoded by the platform's decoder, the linked libheif.
    pub(super) fn decode(input: &[u8], timeout: Option<Duration>) -> Result<Bitmap, Undecoded> {
        libheif(input, timeout)
    }

    /// The primary image decoded to RGB with its transformations applied, as `heif.cc` decodes
    /// it for Java. The linked libheif runs no external program, so takes no timeout.
    pub(super) fn libheif(input: &[u8], _: Option<Duration>) -> Result<Bitmap, Undecoded> {
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            unsafe { heif_init(std::ptr::null()) };
        });
        let context = OwnedContext(unsafe { heif_context_alloc() });
        if context.0.is_null() {
            return Err(Undecoded::Failed(
                "Cannot allocate a HEIF context".to_owned(),
            ));
        }
        checked(
            unsafe {
                heif_context_read_from_memory_without_copy(
                    context.0,
                    input.as_ptr().cast(),
                    input.len(),
                    std::ptr::null(),
                )
            },
            "Cannot read the HEIF container",
        )?;
        let mut primary = std::ptr::null_mut();
        checked(
            unsafe { heif_context_get_primary_image_handle(context.0, &mut primary) },
            "The HEIF file has no primary image",
        )?;
        let handle = OwnedHandle(primary);
        let mut decoded = std::ptr::null_mut();
        checked(
            unsafe {
                heif_decode_image(
                    handle.0,
                    &mut decoded,
                    RGB,
                    INTERLEAVED_RGB,
                    std::ptr::null(),
                )
            },
            "Cannot decode the primary HEIF image",
        )?;
        let image = OwnedImage(decoded);
        let width = unsafe { heif_image_get_width(image.0, INTERLEAVED) };
        let height = unsafe { heif_image_get_height(image.0, INTERLEAVED) };
        let mut stride = 0;
        let plane = unsafe { heif_image_get_plane_readonly2(image.0, INTERLEAVED, &mut stride) };
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return Err(Undecoded::Failed(
                "The decoded HEIF image has no pixels".to_owned(),
            ));
        };
        if plane.is_null() || width == 0 || height == 0 || stride < width as usize * 3 {
            return Err(Undecoded::Failed(
                "The decoded HEIF image has no pixels".to_owned(),
            ));
        }
        decodable(width, height).map_err(Undecoded::Failed)?;
        let rows = unsafe { std::slice::from_raw_parts(plane, stride * height as usize) };
        let mut samples = Vec::with_capacity(width as usize * height as usize * 3);
        for row in rows.chunks_exact(stride) {
            samples.extend_from_slice(&row[..width as usize * 3]);
        }
        RgbImage::from_raw(width, height, samples)
            .map(Bitmap::Rgb)
            .ok_or_else(|| {
                Undecoded::Failed(
                    "The decoded HEIF image holds fewer pixels than its size".to_owned(),
                )
            })
    }
}

#[cfg(not(feature = "static"))]
mod imp {
    use std::io::Read;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use crate::ocr::bitmap::Bitmap;
    use crate::ocr::decode::{bitmap, decodable, Undecoded};

    /// libheif's decoder.
    const PROGRAM: &str = "heif-dec";
    /// Where libheif loads decoder plugins from, which the bundled heif-png, linked statically
    /// with its own decoders, must not load: a plugin built for another libheif and C library.
    const PLUGIN_PATH: &str = "LIBHEIF_PLUGIN_PATH";
    const MAC_OS_PROGRAM: &str = "/usr/bin/sips";
    /// How much of a failed program's standard error its failure reports.
    const MAX_REPORTED_ERROR_CHARS: usize = 2000;
    /// How often a running program is checked on.
    const POLL: Duration = Duration::from_millis(5);

    /// A directory of its own for one decoding, removed with what is in it when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Result<Self, Undecoded> {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = crate::isolate::temp_dir().join(format!(
                "extractous-heif-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).map_err(|e| {
                Undecoded::Failed(format!(
                    "Cannot create {} to decode in: {e}",
                    path.display()
                ))
            })?;
            Ok(Self(path))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// How a program the decoding runs ended.
    enum Outcome {
        NotStarted,
        Finished { success: bool, failure: String },
    }

    /// Runs a program to its end or its timeout, whichever comes first, with its standard
    /// error kept for its failure.
    fn run(command: &mut Command, timeout: Option<Duration>) -> Result<Outcome, Undecoded> {
        let program = command.get_program().to_string_lossy().into_owned();
        let Ok(mut child) = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        else {
            return Ok(Outcome::NotStarted);
        };
        let mut errors = child.stderr.take().expect("standard error is piped");
        let reader = std::thread::spawn(move || {
            let mut text = Vec::new();
            errors.read_to_end(&mut text).map(|_| text)
        });
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if timeout.is_some_and(|timeout| started.elapsed() >= timeout) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(Outcome::Finished {
                        success: false,
                        failure: format!(
                            "{program} did not finish within {} ms",
                            timeout.unwrap_or_default().as_millis()
                        ),
                    });
                }
                Ok(None) => std::thread::sleep(POLL),
                Err(e) => {
                    return Err(Undecoded::Failed(format!(
                        "Waiting for {program} failed: {e}"
                    )))
                }
            }
        };
        let errors = reader
            .join()
            .map_err(|_| Undecoded::Failed(format!("Reading {program}'s errors panicked")))?
            .map_err(|e| Undecoded::Failed(format!("Reading {program}'s errors failed: {e}")))?;
        let errors = String::from_utf8_lossy(&errors);
        let errors = errors.trim();
        let tail: String = {
            let skip = errors
                .chars()
                .count()
                .saturating_sub(MAX_REPORTED_ERROR_CHARS);
            errors.chars().skip(skip).collect()
        };
        Ok(Outcome::Finished {
            success: status.success(),
            failure: match tail.is_empty() {
                true => format!("{program} exited with {status}"),
                false => format!("{program} exited with {status}: {tail}"),
            },
        })
    }

    /// The PNG a program decoded the image to, in the colours ImageIO reads it in.
    fn decoded(outcome: Outcome, png: &Path) -> Result<Bitmap, Undecoded> {
        match outcome {
            Outcome::NotStarted => Err(Undecoded::NoDecoder),
            Outcome::Finished {
                success: false,
                failure,
            } => Err(Undecoded::Failed(format!(
                "Decoding the image for OCR failed: {failure}"
            ))),
            Outcome::Finished { success: true, .. } => {
                let unreadable = |e: &dyn std::fmt::Display| {
                    Undecoded::Failed(format!("Cannot read the decoded {}: {e}", png.display()))
                };
                let (width, height) = image::image_dimensions(png).map_err(|e| unreadable(&e))?;
                decodable(width, height).map_err(Undecoded::Failed)?;
                let mut reader = image::ImageReader::open(png).map_err(|e| unreadable(&e))?;
                reader.no_limits();
                Ok(bitmap(reader.decode().map_err(|e| unreadable(&e))?))
            }
        }
    }

    /// The primary image decoded by the platform's decoder: macOS's `sips`, and elsewhere
    /// libheif.
    pub(super) fn decode(input: &[u8], timeout: Option<Duration>) -> Result<Bitmap, Undecoded> {
        if cfg!(target_os = "macos") {
            return converted(input, timeout, |image, png| {
                let mut command = Command::new(MAC_OS_PROGRAM);
                command
                    .args(["-s", "format", "png"])
                    .arg(image)
                    .arg("--out")
                    .arg(png);
                Ok(command)
            });
        }
        libheif(input, timeout)
    }

    /// The primary image decoded by libheif: the bundled heif-png, or else `heif-dec` on the
    /// `PATH`.
    pub(super) fn libheif(input: &[u8], timeout: Option<Duration>) -> Result<Bitmap, Undecoded> {
        converted(input, timeout, |image, png| {
            let bundled = crate::bundle::bundled_heif_decoder().map_err(|e| {
                Undecoded::Failed(format!("Cannot unpack the bundled heif-png: {e}"))
            })?;
            let mut command = match bundled {
                Some(bundled) => {
                    let mut command = Command::new(bundled);
                    command.env_remove(PLUGIN_PATH);
                    command
                }
                None => {
                    let mut command = Command::new(PROGRAM);
                    command.arg("--quiet");
                    command
                }
            };
            command.arg(image).arg(png);
            Ok(command)
        })
    }

    /// The primary image converted to a PNG by the program `command` builds for the image's and
    /// the PNG's paths, and read back.
    fn converted(
        input: &[u8],
        timeout: Option<Duration>,
        command: impl FnOnce(&Path, &Path) -> Result<Command, Undecoded>,
    ) -> Result<Bitmap, Undecoded> {
        let scratch = Scratch::new()?;
        let image = scratch.0.join("image.heif");
        let png = scratch.0.join("image.png");
        std::fs::write(&image, input).map_err(|e| {
            Undecoded::Failed(format!(
                "Cannot write {} to decode it: {e}",
                image.display()
            ))
        })?;
        let outcome = run(&mut command(&image, &png)?, timeout)?;
        decoded(outcome, &png)
    }
}
