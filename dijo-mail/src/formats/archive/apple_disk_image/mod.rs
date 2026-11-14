//! Apple disk images (`.dmg`), which Tika detects and reads with no parser, read as the archive
//! parsers read theirs: each file of each HFS Plus or HFSX volume of the image's disk, depth
//! first in the catalog's order of names, handed to the embedded document extractor with the
//! metadata `handleEntryMetadata` gives an archive's entry, its path, size and dates; each
//! volume's name as the image's title, and the first volume's dates as the image's.
//!
//! The image's block map ([`udif`]) places the disk's sectors in chunks, which the disk
//! ([`disk`]) reads in place where they are stored and decodes on demand where they are coded:
//! zlib, bzip2, ADC ([`adc`]) or LZFSE ([`lzfse`], with [`lzvn`]); never the whole image at
//! once. A raw image, which has no block map, is read as its disk, its partitions those of its
//! partition map ([`maps`]). The volumes ([`hfs`]) are found at the starts of the partitions.
//!
//! Only data forks are read: a file whose data HFS Plus compresses into its resource fork is
//! recorded as unreadable, as is one whose extents or chunks cannot be read, and a folder whose
//! catalog records cannot be. Symbolic links are recorded, not followed; a hard link to a file
//! is read as the file, one to a folder recorded as unreadable. The file system's own files
//! and folders, its journal and the private folders of hard links, are not handed on. An
//! image of APFS or HFS volumes alone is not read; where it holds them beside an HFS Plus
//! volume, its metadata says so. As with a decoded archive, the files handed on stop at
//! [`decode_limit`] bytes in all, and what the disk is decoded and read for at as much again,
//! so that chunks and catalogs pointing many times at the same bytes are no bomb.

mod adc;
mod disk;
mod hfs;
mod lzfse;
mod lzvn;
mod maps;
mod udif;

use std::collections::{BTreeMap, BTreeSet};

use disk::{Disk, Fault};
use hfs::{Found, Item, Kind, Volume, ROOT_FOLDER};
use udif::{Partition, Unread, SECTOR};

use super::{past_the_limit, record_unreadable, write_file, Cut};
use crate::compress::{decode_limit, ArchiveFile, Decoding, Undecoded};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::EXCEPTION_WARNING;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::input::{Input, Pages};

/// The image's files, as a failure names them.
const WHAT: &str = "the files of the Apple disk image";
/// The symbolic links of the image, each as its path, ` -> ` and its target.
const SYMBOLIC_LINK: &str = "dmg:symbolic-link";
/// The longest target of a symbolic link read.
const MAX_LINK: u64 = 4096;
/// The fewest bytes a partition holding a volume header has.
const SMALLEST_VOLUME: u64 = 1024 + 512;

/// The parser of Apple disk images.
pub struct AppleDiskImageParser;

impl Parser for AppleDiskImageParser {
    fn name(&self) -> &'static str {
        "dijo::AppleDiskImageParser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::AppleDiskImageParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-apple-diskimage"]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        self.prepare_paged(input, Pages::IN_MEMORY, metadata, configs)
    }

    fn prepare_paged<'a>(
        &self,
        input: &'a [u8],
        pages: Pages<'a>,
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let image = udif::read(input).map_err(|unread| match unread {
            Unread::Encrypted => Decline::Fails(Error::Encrypted(
                "the Apple disk image is encrypted, which dijo does not decrypt".into(),
            )),
            Unread::Unsupported(reason) => {
                Decline::Unread(format!("the Apple disk image: {reason}"))
            }
            Unread::Malformed(reason) => {
                Decline::Fails(Error::ParseError(format!("the Apple disk image: {reason}")))
            }
        })?;
        let mut disk = Disk::new(input, pages, image.chunks);
        let partitions = if image.partitions.is_empty() {
            let sectors = disk.sectors();
            maps::partitions(&mut disk, sectors).map_err(|fault| declined(&fault))?
        } else {
            image.partitions
        };
        let found = find_volumes(&mut disk, &partitions).map_err(|fault| declined(&fault))?;
        if found.volumes.is_empty() {
            return Err(if found.unsupported.is_empty() {
                Decline::Fails(Error::ParseError(format!(
                    "the Apple disk image holds no HFS Plus volume{}",
                    found
                        .unread
                        .iter()
                        .map(|reason| format!("; {reason}"))
                        .collect::<String>()
                )))
            } else {
                Decline::Unread(format!(
                    "the Apple disk image holds no HFS Plus volume, only {}",
                    found.unsupported.join(", ")
                ))
            });
        }
        Ok(Box::new(move |xhtml| {
            for note in found.unsupported.iter().chain(&found.unread) {
                xhtml.metadata_mut().add(
                    EXCEPTION_WARNING,
                    format!("the Apple disk image holds {note}"),
                );
            }
            write(&mut disk, &found.volumes, xhtml)
        }))
    }
}

/// Why the image cannot be read, for the `fault` reading its disk.
fn declined(fault: &Fault) -> Decline<'static> {
    match fault {
        Fault::Corrupt(reason) => {
            Decline::Fails(Error::ParseError(format!("the Apple disk image: {reason}")))
        }
        Fault::Spent => Decline::Fails(past_the_limit(WHAT)),
    }
}

/// What the partitions of a disk hold.
struct Volumes {
    /// The HFS Plus volumes, each with the name of its partition.
    volumes: Vec<(String, Volume)>,
    /// The volumes of file systems dijo does not read, as they are named.
    unsupported: Vec<String>,
    /// The partitions whose start cannot be read, each with why.
    unread: Vec<String>,
}

/// The volumes at the starts of `partitions` of `disk`.
fn find_volumes(disk: &mut Disk, partitions: &[Partition]) -> Result<Volumes, Fault> {
    let mut found = Volumes {
        volumes: Vec::new(),
        unsupported: Vec::new(),
        unread: Vec::new(),
    };
    for partition in partitions {
        if partition.sectors.saturating_mul(SECTOR) < SMALLEST_VOLUME {
            continue;
        }
        let name = &partition.name;
        match hfs::probe(disk, partition.sector * SECTOR) {
            Ok(Found::Volume(volume)) => found.volumes.push((name.clone(), volume)),
            Ok(Found::Apfs) => found.unsupported.push(format!(
                "an APFS container in {name}, which dijo does not read"
            )),
            Ok(Found::Hfs) => found
                .unsupported
                .push(format!("an HFS volume in {name}, which dijo does not read")),
            Ok(Found::Nothing) => {}
            Err(Fault::Corrupt(reason)) => found.unread.push(format!("{name}, unread: {reason}")),
            Err(Fault::Spent) => return Err(Fault::Spent),
        }
    }
    Ok(found)
}

/// Why the writing of the image's files stops short of a folder's end.
enum Halt {
    Fault(Fault),
    Failed(Failure),
}

impl From<Fault> for Halt {
    fn from(fault: Fault) -> Self {
        Halt::Fault(fault)
    }
}

impl From<Failure> for Halt {
    fn from(failure: Failure) -> Self {
        Halt::Failed(failure)
    }
}

impl From<Stop> for Halt {
    fn from(stop: Stop) -> Self {
        Halt::Failed(Failure::Stopped(stop))
    }
}

/// Writes the files of `volumes`, of `disk`, each volume's name and the first's dates as the
/// image's metadata, and records what cannot be read.
fn write(disk: &mut Disk, volumes: &[(String, Volume)], xhtml: &mut Xhtml) -> Result<(), Failure> {
    let mut walk = Walk {
        handed: 0,
        limit: decode_limit(disk.image_length()),
        targets: None,
    };
    for (index, (partition, volume)) in volumes.iter().enumerate() {
        let written = walk.volume(disk, volume, index, volumes.len() > 1, xhtml);
        match written {
            Ok(()) => {}
            Err(Halt::Fault(Fault::Corrupt(reason))) => {
                record_unreadable(xhtml.metadata_mut(), partition, &reason)
            }
            Err(Halt::Fault(Fault::Spent)) => return Cut::end(Some(Cut(WHAT.to_owned()))),
            Err(Halt::Failed(failure)) => return Err(failure),
        }
    }
    Ok(())
}

/// What the writing of an image's files has handed on, and the files the hard links of the
/// volume it writes stand for, once read.
struct Walk {
    handed: u64,
    limit: u64,
    targets: Option<BTreeMap<u32, Item>>,
}

/// When a file was created and last modified.
type Dates = (Option<i64>, Option<i64>);

impl Walk {
    /// Writes the files of `volume`, the `index`th of the image, its paths `prefixed` with its
    /// name where the image holds several.
    fn volume(
        &mut self,
        disk: &mut Disk,
        volume: &Volume,
        index: usize,
        prefixed: bool,
        xhtml: &mut Xhtml,
    ) -> Result<(), Halt> {
        let root = volume.root(disk)?;
        let name = root
            .as_ref()
            .map(|root| root.name.clone())
            .unwrap_or_default();
        let metadata = xhtml.metadata_mut();
        if !name.is_empty() {
            metadata.add("dc:title", name.clone());
        }
        if index == 0 {
            let created = root.as_ref().and_then(|root| root.created);
            for (key, millis) in [
                ("dcterms:created", created),
                ("dcterms:modified", volume.modified),
            ] {
                if let Some(millis) = millis {
                    metadata.set(key, crate::java::time::format_instant(millis));
                }
            }
        }
        self.targets = None;
        let mut read = BTreeSet::from([ROOT_FOLDER]);
        let base = if prefixed { name } else { String::new() };
        let mut pending = vec![(ROOT_FOLDER, base)];
        while let Some((folder, path)) = pending.pop() {
            let mut subfolders = Vec::new();
            let listed = volume.list(disk, folder, |disk, item: Item| -> Result<(), Halt> {
                if volume.is_own(folder, &item.name) {
                    return Ok(());
                }
                let path = joined(&path, &item.name);
                match item.kind {
                    Kind::Folder(id) => subfolders.push((id, path)),
                    Kind::File(file) => self.file(
                        disk,
                        volume,
                        path,
                        file,
                        (item.created, item.modified),
                        xhtml,
                    )?,
                }
                Ok(())
            });
            match listed {
                Ok(()) => {}
                Err(Halt::Fault(Fault::Corrupt(reason))) => {
                    let shown = if path.is_empty() { "/" } else { &path };
                    record_unreadable(xhtml.metadata_mut(), shown, &reason);
                }
                Err(halt) => return Err(halt),
            }
            for (id, path) in subfolders.into_iter().rev() {
                if read.insert(id) {
                    pending.push((id, path));
                } else {
                    record_unreadable(
                        xhtml.metadata_mut(),
                        &path,
                        &format!("it is the folder {id}, which is read already"),
                    );
                }
            }
        }
        Ok(())
    }

    /// Writes the file `file` of `volume` at `path`, of `dates`, or records what it is or why
    /// it is not read; a hard link is read for the file it stands for.
    fn file(
        &mut self,
        disk: &mut Disk,
        volume: &Volume,
        path: String,
        mut file: hfs::File,
        dates: Dates,
        xhtml: &mut Xhtml,
    ) -> Result<(), Halt> {
        let (mut created, mut modified) = dates;
        if let hfs::FileKind::HardLink(number) = file.kind {
            if self.targets.is_none() {
                self.targets = Some(volume.link_targets(disk)?);
            }
            let target = self
                .targets
                .as_ref()
                .and_then(|targets| targets.get(&number));
            let Some(Item {
                kind: Kind::File(target),
                created: target_created,
                modified: target_modified,
                ..
            }) = target
            else {
                record_unreadable(
                    xhtml.metadata_mut(),
                    &path,
                    &format!("it is a hard link to iNode{number}, which the volume does not hold"),
                );
                return Ok(());
            };
            (file, created, modified) = (*target, *target_created, *target_modified);
        }
        let unread = match file.kind {
            hfs::FileKind::Regular => None,
            hfs::FileKind::SymbolicLink => match link_target(disk, volume, &file) {
                Ok(target) => {
                    xhtml
                        .metadata_mut()
                        .add(SYMBOLIC_LINK, format!("{path} -> {target}"));
                    return Ok(());
                }
                Err(Fault::Corrupt(reason)) => Some(reason),
                Err(Fault::Spent) => return Err(Halt::Fault(Fault::Spent)),
            },
            hfs::FileKind::HardLink(_) => Some("it is a hard link to another hard link".into()),
            hfs::FileKind::FolderLink => {
                Some("it is a hard link to a folder, which dijo does not follow".into())
            }
            hfs::FileKind::Compressed => Some(
                "its data is compressed by HFS Plus into its resource fork, which dijo does not \
                 read"
                    .into(),
            ),
        };
        if let Some(reason) = unread {
            record_unreadable(xhtml.metadata_mut(), &path, &reason);
            return Ok(());
        }
        self.handed = self.handed.saturating_add(file.fork.size);
        if self.handed > self.limit {
            return Err(Halt::Fault(Fault::Spent));
        }
        let content = match content(disk, volume, &file, &path)? {
            Ok(content) => content,
            Err(reason) => {
                record_unreadable(xhtml.metadata_mut(), &path, &reason);
                return Ok(());
            }
        };
        let file = ArchiveFile {
            size: i64::try_from(file.fork.size).expect("at most the decode limit"),
            name: path,
            modified_millis: modified,
            created_millis: created,
            directory: false,
            content,
        };
        write_file(&file, xhtml)?;
        Ok(())
    }
}

/// `name` in the folder at `path`.
fn joined(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_owned()
    } else {
        format!("{path}/{name}")
    }
}

/// The data fork of `file` of `volume`: in place where one stored chunk of the image holds it
/// whole, else spooled; or why it cannot be read.
fn content<'a>(
    disk: &mut Disk<'a>,
    volume: &Volume,
    file: &hfs::File,
    path: &str,
) -> Result<Result<Input<'a>, String>, Halt> {
    let spans = match volume.spans(disk, file.id, &file.fork) {
        Ok(spans) => spans,
        Err(Fault::Corrupt(reason)) => return Ok(Err(reason)),
        Err(Fault::Spent) => return Err(Halt::Fault(Fault::Spent)),
    };
    match spans.as_slice() {
        [] => return Ok(Ok(Input::Borrowed(&[]))),
        [span] => {
            if let Some(bytes) = disk.in_place(span.offset, span.length) {
                return Ok(Ok(Input::Within(bytes, disk.pages())));
            }
        }
        _ => {}
    }
    let unheld = |undecoded: Undecoded| {
        Halt::Failed(Failure::Extraction(Error::IoError(format!(
            "{WHAT}: {path}: {undecoded}"
        ))))
    };
    let mut out = Decoding::spooled(disk.image_length(), Some(file.fork.size));
    for span in &spans {
        let visited = disk.visit(span.offset, span.length, |bytes| {
            out.write(bytes).map_err(unheld)
        });
        match visited {
            Ok(()) => {}
            Err(Halt::Fault(Fault::Corrupt(reason))) => return Ok(Err(reason)),
            Err(halt) => return Err(halt),
        }
    }
    let content = out
        .finish()
        .and_then(|limited| limited.whole())
        .map_err(unheld)?;
    Ok(Ok(content))
}

/// The path the symbolic link `file` of `volume` holds.
fn link_target(disk: &mut Disk, volume: &Volume, file: &hfs::File) -> Result<String, Fault> {
    if file.fork.size > MAX_LINK {
        return Err(Fault::Corrupt(format!(
            "it is a symbolic link of {} bytes, longer than any path",
            file.fork.size
        )));
    }
    let mut target = Vec::with_capacity(file.fork.size as usize);
    for span in volume.spans(disk, file.id, &file.fork)? {
        disk.visit::<Fault>(span.offset, span.length, |bytes| {
            target.extend_from_slice(bytes);
            Ok(())
        })?;
    }
    Ok(String::from_utf8_lossy(&target).into_owned())
}

#[cfg(test)]
mod tests;
