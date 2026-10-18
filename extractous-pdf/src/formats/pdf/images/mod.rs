//! The images of the pages, as Tika's `PDF2XHTML.extractImages` extracts them under
//! `setExtractInlineImages`: after the text of each page, every image the page draws
//! ([`engine`]), numbered across the document, an image XObject drawn again under its own
//! number, where `ImageGraphicsEngine` numbers it 0, or, under
//! `setExtractUniqueInlineImagesOnly`, skipped; each written as an `img` element and its file,
//! as `writeToBuffer` writes it, parsed as an embedded document. JPEG data in grey or RGB,
//! JPEG 2000 data in grey or RGB without masks, and JBIG2 data is copied as it is; other images,
//! and those with masks, are read as PDFBox reads them ([`sampled`], [`filters`]) and written
//! as PNG, with the Content-Type of PNG where Tika keeps that of the data it does not copy
//! (#131), a JPEG of another colour space as a JPEG and a CCITT image as a TIFF, with the
//! `image` crate's encoders where Tika uses Java's ([`written`], #125).

mod colorspace;
mod engine;
mod filters;
mod image;
mod png;
mod sampled;
#[cfg(test)]
mod tests;
mod written;

use std::collections::HashMap;

use super::attachment::EmbeddedFile;
use super::cos::{Identity, Pdf};
use super::document::Page;
use super::metadata::xmp_metadata;
use super::writer::{PageWriter, WriteFailure};
use super::Unported;
use crate::formats::embedded::{
    ResourceType, EMBEDDED_STREAM_EXCEPTION, EXCEPTION_WARNING, RESOURCE_NAME,
};
use crate::formats::metadata::TikaMetadata;
pub(super) use crate::formats::thrown::Exception;

use engine::Drawn;
use image::{PdImage, Suffix, JB2, JP2, JPEG};

/// `ImageGraphicsEngine.MAX_IMAGE_LENGTH_BYTES`: the most data of an image copied as it is.
const MAX_IMAGE_LENGTH_BYTES: usize = 100 * 1024 * 1024;
/// `TikaCoreProperties.RESOURCE_NAME_EXTENSION_INFERRED`.
const EXTENSION_INFERRED: &str = "tk:resource-name-extension-inferred";
/// `TikaPagedText.PAGE_NUMBER`.
const PAGE_NUMBER: &str = "tk:page:number";

/// Why an image's file is not written.
#[derive(Debug)]
pub(super) enum Unwritten {
    /// An `IOException` `writeToBuffer` throws, which `processImage` records as an exception
    /// of the embedded stream.
    Stream(Exception),
    /// A `MissingImageReaderException`, which `processImage` records as a warning.
    MissingReader(Exception),
    /// Data larger than `ImageGraphicsEngine` copies.
    TooLarge,
    Unported(Unported),
}

impl From<Unported> for Unwritten {
    fn from(unported: Unported) -> Self {
        Self::Unported(unported)
    }
}

/// The images of a document being extracted: those numbered so far, by the image XObject.
pub(super) struct Images {
    unique_only: bool,
    /// `processedInlineImages`.
    processed: HashMap<Identity, usize>,
    /// `inlineImageCounter`.
    counter: usize,
}

impl Images {
    pub(super) fn new(unique_only: bool) -> Self {
        Self {
            unique_only,
            processed: HashMap::new(),
            counter: 0,
        }
    }

    /// `extractImages` of page `page_number`, written by `writer`.
    pub(super) fn extract(
        &mut self,
        pdf: &Pdf,
        page: Page,
        page_number: usize,
        writer: &mut PageWriter,
    ) -> Result<(), WriteFailure> {
        engine::run(pdf, page, &mut |drawn| {
            let (image, number) = match drawn {
                Drawn::XObject {
                    identity,
                    stream,
                    resources,
                } => {
                    let number = match self.processed.get(&identity) {
                        Some(_) if self.unique_only => return Ok(()),
                        Some(&number) => number,
                        None => {
                            let number = self.counter;
                            self.counter += 1;
                            self.processed.insert(identity, number);
                            number
                        }
                    };
                    (PdImage::xobject(pdf, stream, resources), number)
                }
                Drawn::Inline { image, resources } => {
                    let Some(image) = PdImage::inline(pdf, image, resources)? else {
                        return Ok(());
                    };
                    let number = self.counter;
                    self.counter += 1;
                    (image, number)
                }
            };
            self.process(pdf, &image, number, page_number, writer)
        })
    }

    /// `processImage`.
    fn process(
        &self,
        pdf: &Pdf,
        image: &PdImage,
        number: usize,
        page_number: usize,
        writer: &mut PageWriter,
    ) -> Result<(), WriteFailure> {
        let mut metadata = TikaMetadata::new();
        let (extension, content_type) = match image.suffix() {
            _ if image.has_masks() => ("png", "image/png"),
            None | Some(Suffix::Png) => ("png", "image/png"),
            Some(Suffix::Jpg) => ("jpg", "image/jpeg"),
            Some(Suffix::Tiff) => ("tif", "image/tiff"),
            Some(Suffix::Jpx) => ("jp2", "image/jp2"),
            Some(Suffix::Jb2) => ("jb2", "image/x-jbig2"),
        };
        metadata.set("Content-Type", content_type);
        let name = format!("image-{number}.{extension}");
        writer.start(
            "img",
            &[("src", &format!("embedded:{name}")), ("alt", &name)],
        )?;
        writer.end("img")?;
        metadata.set(RESOURCE_NAME, name);
        metadata.set(EXTENSION_INFERRED, "true");
        ResourceType::Inline.set(&mut metadata);
        metadata.set(PAGE_NUMBER, page_number.to_string());
        if !image.is_inline() {
            xmp_metadata(pdf, image.metadata(), &mut metadata);
        }
        match write_to_buffer(image, extension) {
            Ok(data) => {
                writer.embeds = true;
                writer.out.embedded(EmbeddedFile { metadata, data })?;
            }
            Err(Unwritten::Stream(thrown)) => {
                writer
                    .out
                    .add(EMBEDDED_STREAM_EXCEPTION, format!("{thrown}\n"));
            }
            Err(Unwritten::MissingReader(thrown)) => {
                writer.out.add(EXCEPTION_WARNING, format!("{thrown}\n"));
            }
            Err(Unwritten::TooLarge) if !image.is_inline() => {}
            Err(Unwritten::TooLarge) => {
                return Err(Unported(
                    "an inline image larger than Tika copies, whose failure ends the page's \
                     images"
                        .to_owned(),
                )
                .into())
            }
            Err(Unwritten::Unported(unported)) => return Err(unported.into()),
        }
        Ok(())
    }
}

/// `ImageGraphicsEngine.writeToBuffer`: the image's file of type `extension`.
fn write_to_buffer(image: &PdImage, extension: &str) -> Result<Vec<u8>, Unwritten> {
    match extension {
        "jpg" => {
            let color_space = image.color_space()?;
            if !matches!(color_space.name(), "DeviceGray" | "DeviceRGB") {
                return Ok(written::jpeg(&sampled::image(image)?));
            }
            copied(image.data_until(&JPEG)?)
        }
        "jp2" => {
            let color_space = image.color_space()?;
            if !image.has_masks() && matches!(color_space.name(), "DeviceGray" | "DeviceRGB") {
                return copied(image.data_until(&JP2)?);
            }
            sampled::image(image)?;
            unreachable!("Java has no JPEG 2000 reader, so the image does not decode")
        }
        "tif" if image.color_space()?.is_device_gray() => {
            Ok(written::tiff(&written::bitonal(&sampled::image(image)?)))
        }
        "tif" => Ok(written::tiff(&sampled::image(image)?)),
        "jb2" => copied(image.data_until(&JB2)?),
        _ => Ok(png::write(&sampled::image(image)?)),
    }
}

/// `copyUpToMaxLength`.
fn copied(data: Vec<u8>) -> Result<Vec<u8>, Unwritten> {
    if data.len() > MAX_IMAGE_LENGTH_BYTES {
        return Err(Unwritten::TooLarge);
    }
    Ok(data)
}
