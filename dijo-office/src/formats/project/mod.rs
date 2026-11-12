//! Microsoft Project files (MPP), which Tika reads as compound documents of their summary
//! information alone, read for their plan as MPXJ reads them: the summary information and the
//! project's properties as metadata, then the tasks in the order the plan lists them, nested
//! by their outline level, each its name and notes, then the resources, each its name and
//! notes. Notes are RTF, read with [`crate::formats::rtf`].
//!
//! The file's format comes from its `CompObj` stream, and the project's data from the storage
//! named for that format: `   19` for MPP9 (Project 2000 to 2003), `   112` for MPP12 (Project
//! 2007) and `   114` for MPP14 (Project 2010 and later). Each class of items is a storage of
//! its own, `TBkndTask` and `TBkndRsc`, of fixed records and variable data ([`blocks`]), whose
//! fields the project's `Props` place in field maps. A file of an older or unknown format is
//! not read; a file protected by a password to open it fails as encrypted, as Tika fails on a
//! protected compound document.

mod blocks;
mod plan;

#[cfg(test)]
mod tests;

use std::borrow::Cow;

use blocks::{Field, Location, Props};
use plan::{Item, Plan};

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::compound::CompoundDocumentParser;
use crate::formats::crypt;
use crate::formats::metadata::TikaMetadata;
use crate::formats::ole2::{add_multi, office, Summaries};
use crate::formats::poi::Unread;
use crate::formats::rtf;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Parser, Prepared};
use crate::ole2::{EntryId, EntryKind, Ole2};
use crate::text::charset::Charset;

const MEDIA_TYPE: &str = "application/vnd.ms-project";
const COMP_OBJ: &str = "\u{1}CompObj";
/// The prefix of the file formats `CompObj` names for Project's files.
const PROJECT_FORMAT: &str = "MSProject.";
/// What Project 4.0's `CompObj` names in place of a file format.
const PROJECT_4: &str = "Microsoft Project 4.0";
/// What `CompObj` starts its strings after.
const COMP_OBJ_HEADER: usize = 28;

/// The keys of the values of a Props stream that this parser reads (`PropsKey`).
mod key {
    pub(super) const PASSWORD_FLAG: u32 = 893_386_752;
    pub(super) const PROTECTION_PASSWORD_HASH: u32 = 893_386_756;
    pub(super) const ENCRYPTION_CODE: u32 = 893_386_759;
    pub(super) const TASK_FIELD_MAP: u32 = 131_092;
    pub(super) const TASK_FIELD_MAP2: u32 = 50_331_668;
    pub(super) const RESOURCE_FIELD_MAP: u32 = 131_093;
    pub(super) const RESOURCE_FIELD_MAP2: u32 = 50_331_669;
    pub(super) const COMPANY: u32 = 37_748_736;
    pub(super) const MANAGER: u32 = 37_748_737;
    pub(super) const COMMENTS: u32 = 37_748_741;
    pub(super) const TITLE: u32 = 37_748_744;
    pub(super) const SUBJECT: u32 = 37_748_745;
    pub(super) const AUTHOR: u32 = 37_748_748;
    pub(super) const KEYWORDS: u32 = 37_748_749;
    pub(super) const CATEGORY: u32 = 37_748_809;
}

/// The bit of the password flag that a password to open the file sets.
const READ_PASSWORD: u8 = 0x01;
/// The bytes of a decoded MPP9 password hash that hold the password's characters, in order
/// (`MPPUtility.PASSWORD_MASK`).
const PASSWORD_MASK: [usize; 16] = [60, 30, 48, 2, 6, 14, 8, 22, 44, 12, 38, 10, 62, 16, 34, 24];
/// The shortest MPP9 password hash that holds a password.
const MINIMUM_PASSWORD_DATA_LENGTH: usize = 64;

/// The field types of tasks and of resources start with these words (`TASK_FIELD_BASE`,
/// `RESOURCE_FIELD_BASE`).
const TASK_CLASS: u16 = 0x0B40;
const RESOURCE_CLASS: u16 = 0x0C40;

const TASK_UNIQUE_ID: Field = Field {
    name: "unique ID",
    ids: &[86],
};
const TASK_ID: Field = Field {
    name: "ID",
    ids: &[23],
};
const OUTLINE_LEVEL: Field = Field {
    name: "outline level",
    ids: &[85, 249],
};
const TASK_NAME: Field = Field {
    name: "name",
    ids: &[14],
};
const TASK_NOTES: Field = Field {
    name: "notes",
    ids: &[15, 94],
};
const RESOURCE_UNIQUE_ID: Field = Field {
    name: "unique ID",
    ids: &[27],
};
const RESOURCE_NAME: Field = Field {
    name: "name",
    ids: &[1],
};
const RESOURCE_NOTES: Field = Field {
    name: "notes",
    ids: &[20, 28],
};

/// Where a field of the default field maps lies, where the project's Props hold no map.
const fn fixed(offset: usize) -> Location {
    Location::Fixed { block: 0, offset }
}

/// The parts of `FieldMap9`'s default maps that this parser reads.
const TASK_DEFAULTS_9: [(&[u16], Location); 5] = [
    (TASK_UNIQUE_ID.ids, fixed(0)),
    (TASK_ID.ids, fixed(4)),
    (OUTLINE_LEVEL.ids, fixed(40)),
    (TASK_NAME.ids, Location::Var(11)),
    (TASK_NOTES.ids, Location::Var(144)),
];
const RESOURCE_DEFAULTS_9: [(&[u16], Location); 3] = [
    (RESOURCE_UNIQUE_ID.ids, fixed(0)),
    (RESOURCE_NAME.ids, Location::Var(1)),
    (RESOURCE_NOTES.ids, Location::Var(124)),
];
/// The parts of `FieldMap12`'s and `FieldMap14`'s default maps that this parser reads.
const TASK_DEFAULTS_12: [(&[u16], Location); 5] = [
    (TASK_UNIQUE_ID.ids, fixed(0)),
    (TASK_ID.ids, fixed(4)),
    (OUTLINE_LEVEL.ids, fixed(40)),
    (TASK_NAME.ids, Location::Var(14)),
    (TASK_NOTES.ids, Location::Var(15)),
];
const RESOURCE_DEFAULTS_12: [(&[u16], Location); 3] = [
    (RESOURCE_UNIQUE_ID.ids, fixed(0)),
    (RESOURCE_NAME.ids, Location::Var(1)),
    (RESOURCE_NOTES.ids, Location::Var(20)),
];

/// What is wrong in a Project file, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Malformed(String);

/// The formats of Project files this parser reads, as MPXJ's readers tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Version {
    /// Project 2000 to 2003.
    Mpp9,
    /// Project 2007.
    Mpp12,
    /// Project 2010 and later.
    Mpp14,
}

impl Version {
    /// The version of the file format `CompObj` names: of a project, a template or the global
    /// file, as `MPPReader.FILE_CLASS_MAP` maps them.
    fn of_format(format: &str) -> Option<Version> {
        match format {
            "MSProject.MPP9" | "MSProject.MPT9" | "MSProject.GLOBAL9" => Some(Version::Mpp9),
            "MSProject.MPP12" | "MSProject.MPT12" | "MSProject.GLOBAL12" => Some(Version::Mpp12),
            "MSProject.MPP14" | "MSProject.MPT14" | "MSProject.GLOBAL14" => Some(Version::Mpp14),
            _ => None,
        }
    }

    fn number(self) -> u8 {
        match self {
            Version::Mpp9 => 9,
            Version::Mpp12 => 12,
            Version::Mpp14 => 14,
        }
    }

    /// The storage of the project's data, `   1` and the version.
    fn project_storage(self) -> String {
        format!("   1{}", self.number())
    }

    /// The root's Props stream, which says whether the file is protected.
    fn root_props(self) -> String {
        format!("Props{}", self.number())
    }

    fn task_defaults(self) -> &'static [(&'static [u16], Location)] {
        match self {
            Version::Mpp9 => &TASK_DEFAULTS_9,
            Version::Mpp12 | Version::Mpp14 => &TASK_DEFAULTS_12,
        }
    }

    fn resource_defaults(self) -> &'static [(&'static [u16], Location)] {
        match self {
            Version::Mpp9 => &RESOURCE_DEFAULTS_9,
            Version::Mpp12 | Version::Mpp14 => &RESOURCE_DEFAULTS_12,
        }
    }
}

/// What a Project file's `CompObj` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Format {
    /// A format this parser reads.
    Read(Version),
    /// A format of Project's this parser does not read, by its name.
    Unread(String),
}

/// The format of Project's that the compound document's `CompObj` names, if any.
fn project_format(file: &Ole2<'_>) -> Result<Option<Format>, Malformed> {
    let Some(entry) = file.child(file.root(), COMP_OBJ) else {
        return Ok(None);
    };
    let data = document(file, entry, "the CompObj stream")?;
    let string = |at: usize| -> Result<(String, usize), Malformed> {
        let length = blocks::le::<4>(&data, at)
            .map(u32::from_le_bytes)
            .and_then(|length| usize::try_from(length).ok())
            .ok_or_else(|| {
                Malformed("the CompObj stream ends before a string's length".to_owned())
            })?;
        let start = at + 4;
        let bytes = data
            .get(start..start.saturating_add(length))
            .ok_or_else(|| Malformed("the CompObj stream ends inside a string".to_owned()))?;
        let text = String::from_utf8_lossy(blocks::ansi(bytes)).into_owned();
        Ok((text, start + length))
    };
    let (application, next) = string(COMP_OBJ_HEADER)?;
    if application == PROJECT_4 {
        return Ok(Some(Format::Unread("MSProject.MPP4".to_owned())));
    }
    if blocks::le::<4>(&data, next).is_none() {
        return Ok(None);
    }
    let (format, _) = string(next)?;
    if !format.starts_with(PROJECT_FORMAT) {
        return Ok(None);
    }
    Ok(Some(match Version::of_format(&format) {
        Some(version) => Format::Read(version),
        None => Format::Unread(format),
    }))
}

/// Whether the compound document `file` is a Project file by its `CompObj`, which
/// [`ProjectParser`] reads whatever type its detection names.
pub(crate) fn is_project(file: &Ole2<'_>) -> bool {
    matches!(project_format(file), Ok(Some(_)))
}

/// Microsoft Project files.
pub struct ProjectParser;

impl Parser for ProjectParser {
    fn name(&self) -> &'static str {
        "dijo::ProjectParser"
    }
    fn tika_name(&self) -> &'static str {
        "dijo::ProjectParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[MEDIA_TYPE]
    }

    /// Reads the plan; a compound document whose `CompObj` names no format of Project's is
    /// read as `OfficeParser` reads it, for its summary information.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        metadata: &TikaMetadata,
        configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let file = office::open_file(input).map_err(Unread::declined)?;
        let malformed = |Malformed(reason)| {
            Decline::Fails(Error::ParseError(format!("the Project file: {reason}")))
        };
        let version = match project_format(&file).map_err(malformed)? {
            None => return CompoundDocumentParser.prepare(input, metadata, configs),
            Some(Format::Unread(format)) => {
                return Err(Decline::Unread(format!(
                    "a Project file of the format {format}: only MPP9 (Project 2000 to 2003), \
                     MPP12 (Project 2007) and MPP14 (Project 2010 and later) are read"
                )))
            }
            Some(Format::Read(version)) => version,
        };
        let summaries = Summaries::read(&file, file.root())?;
        let project = read(&file, version).map_err(|read| match read {
            Read::Protected => Decline::Fails(crypt::encrypted(None)),
            Read::Malformed(reason) => malformed(reason),
        })?;
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            summaries.write(metadata);
            project.write_properties(metadata);
            metadata.set("Content-Type", MEDIA_TYPE);
            project.plan.write(xhtml)
        }))
    }
}

/// Why a Project file is not read.
enum Read {
    /// A password protects it from being opened.
    Protected,
    Malformed(Malformed),
}

impl From<Malformed> for Read {
    fn from(malformed: Malformed) -> Self {
        Read::Malformed(malformed)
    }
}

/// What is read of a Project file.
struct Project {
    /// The project's Props, which hold its properties.
    props: Props,
    plan: Plan,
}

impl Project {
    /// The project's properties that its summary information does not hold, as
    /// `ProjectPropertiesReader` takes them from its Props, written as
    /// [`Summaries`] writes them.
    fn write_properties(&self, metadata: &mut TikaMetadata) {
        let missing = |metadata: &TikaMetadata, name: &str| metadata.values(name).is_empty();
        let text = |key: u32| self.props.text(key);
        let summary_subjects = !missing(metadata, "dc:subject");
        if missing(metadata, "dc:title") {
            if let Some(title) = text(key::TITLE) {
                metadata.set("dc:title", title);
            }
        }
        if missing(metadata, "dc:creator") {
            if let Some(author) = text(key::AUTHOR) {
                add_multi(metadata, "dc:creator", &author);
            }
        }
        if missing(metadata, "meta:keyword") {
            if let Some(keywords) = text(key::KEYWORDS) {
                metadata.add("meta:keyword", keywords.clone());
                metadata.add("dc:subject", keywords);
            }
        }
        if !summary_subjects {
            if let Some(subject) = text(key::SUBJECT) {
                metadata.add("dc:subject", subject);
            }
        }
        if missing(metadata, "w:Comments") {
            if let Some(comments) = text(key::COMMENTS) {
                metadata.set("w:Comments", comments);
            }
        }
        if missing(metadata, "extended-properties:Company") {
            if let Some(company) = text(key::COMPANY) {
                metadata.set("extended-properties:Company", company);
            }
        }
        if missing(metadata, "extended-properties:Manager") {
            if let Some(manager) = text(key::MANAGER) {
                add_multi(metadata, "extended-properties:Manager", &manager);
            }
        }
        if missing(metadata, "cp:category") {
            if let Some(category) = text(key::CATEGORY) {
                metadata.set("cp:category", category);
            }
        }
    }
}

/// How the streams a password sets the flag of are obfuscated: each byte XORed with a mask the
/// root's Props derive from their encryption code (`DocumentInputStreamFactory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Obfuscation(Option<u8>);

impl Obfuscation {
    fn of(root: &Props) -> Obfuscation {
        if root.byte(key::PASSWORD_FLAG) == 0 {
            return Obfuscation(None);
        }
        Obfuscation(Some(match root.byte(key::ENCRYPTION_CODE) {
            0 => 0,
            code => 0xFF - code,
        }))
    }

    fn reveal<'a>(self, data: Cow<'a, [u8]>) -> Cow<'a, [u8]> {
        match self.0 {
            None | Some(0) => data,
            Some(mask) => Cow::Owned(data.iter().map(|byte| byte ^ mask).collect()),
        }
    }
}

/// Whether a password protects the file from being opened: the flag's bit, and for MPP9 a
/// password in the hash (a file may set the flag without one, which Project opens), for MPP14
/// the hash, as MPXJ's readers decide.
fn read_protected(version: Version, root: &Props, obfuscation: Obfuscation) -> bool {
    if root.byte(key::PASSWORD_FLAG) & READ_PASSWORD == 0 {
        return false;
    }
    let hash = root.bytes(key::PROTECTION_PASSWORD_HASH);
    match version {
        Version::Mpp9 => hash.is_some_and(|hash| {
            hash.len() >= MINIMUM_PASSWORD_DATA_LENGTH && {
                let mask = obfuscation.0.unwrap_or(0);
                hash[PASSWORD_MASK[0]] ^ mask != 0
            }
        }),
        Version::Mpp12 => true,
        Version::Mpp14 => hash.is_some(),
    }
}

/// The content of the document `entry`, named `name`.
fn document<'a>(file: &Ole2<'a>, entry: EntryId, name: &str) -> Result<Cow<'a, [u8]>, Malformed> {
    if file.kind(entry) != EntryKind::Document {
        return Err(Malformed(format!("{name} is not a stream")));
    }
    file.content(entry)
        .map(|content| content.bytes())
        .map_err(|unreadable| Malformed(format!("cannot read {name}: {}", unreadable.0)))
}

/// The storage `name` in `directory`, whose path is `path`.
fn storage(
    file: &Ole2<'_>,
    directory: EntryId,
    name: &str,
    path: &str,
) -> Result<EntryId, Malformed> {
    file.child(directory, name)
        .filter(|&entry| file.kind(entry) != EntryKind::Document)
        .ok_or_else(|| Malformed(format!("the file has no storage {path}/{name}")))
}

/// The stream `name` in `directory`, whose path is `path`.
fn stream<'a>(
    file: &Ole2<'a>,
    directory: EntryId,
    path: &str,
    name: &str,
) -> Result<Cow<'a, [u8]>, Malformed> {
    let path = format!("{path}/{name}");
    let entry = file
        .child(directory, name)
        .ok_or_else(|| Malformed(format!("the file has no stream {path}")))?;
    document(file, entry, &path)
}

/// The project's properties and plan, unless a password protects it.
fn read(file: &Ole2<'_>, version: Version) -> Result<Project, Read> {
    let root_name = version.root_props();
    let root_props = Props::read(&stream(file, file.root(), "", &root_name)?, &root_name)?;
    let obfuscation = Obfuscation::of(&root_props);
    if read_protected(version, &root_props, obfuscation) {
        return Err(Read::Protected);
    }
    let name = version.project_storage();
    let project = storage(file, file.root(), &name, "")?;
    let path = format!("/{name}");
    let props_path = format!("{path}/Props");
    let props = Props::read(
        &obfuscation.reveal(stream(file, project, &path, "Props")?),
        &props_path,
    )?;
    let plan = plan::read(file, project, &path, version, &props, obfuscation)?;
    Ok(Project { props, plan })
}

impl Plan {
    /// The tasks as nested lists, a list deeper for each outline level deeper, then the
    /// resources as a list.
    fn write(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        let mut open: Vec<u16> = Vec::new();
        for task in self.tasks.iter().filter(|task| task.item.is_written()) {
            let level = task.level;
            match open.last() {
                None => start_list(xhtml, "tasks")?,
                Some(&deepest) if level > deepest => start_list(xhtml, "tasks")?,
                Some(_) => {
                    xhtml.end_element("li")?;
                    while open.len() > 1 && level <= open[open.len() - 2] {
                        open.pop();
                        xhtml.end_element("ul")?;
                        xhtml.end_element("li")?;
                    }
                    open.pop();
                }
            }
            open.push(level);
            xhtml.start_element("li")?;
            task.item.write(xhtml)?;
        }
        for _ in &open {
            xhtml.end_element("li")?;
            xhtml.end_element("ul")?;
        }
        let mut resources = self
            .resources
            .iter()
            .filter(|item| item.is_written())
            .peekable();
        if resources.peek().is_some() {
            start_list(xhtml, "resources")?;
            for resource in resources {
                xhtml.start_element("li")?;
                resource.write(xhtml)?;
                xhtml.end_element("li")?;
            }
            xhtml.end_element("ul")?;
        }
        Ok(())
    }
}

fn start_list(xhtml: &mut Xhtml, class: &str) -> Result<(), Failure> {
    Ok(xhtml.start_element_with("ul", &[("class", class)])?)
}

impl Item {
    /// Whether the item has any text: a name or notes.
    fn is_written(&self) -> bool {
        self.name.is_some() || self.notes.is_some()
    }

    /// The name, then the notes: RTF read as an RTF document's body, else text in Windows'
    /// code page 1252, as Project writes notes of other origins.
    fn write(&self, xhtml: &mut Xhtml) -> Result<(), Failure> {
        if let Some(name) = &self.name {
            xhtml.element("p", name)?;
        }
        let Some(notes) = &self.notes else {
            return Ok(());
        };
        if notes.starts_with(br"{\rtf") {
            return rtf::write_inline(notes, xhtml);
        }
        let text = Charset::named("WINDOWS-1252")
            .decode(notes)
            .ok_or_else(|| Failure::Malformed("notes not in code page 1252".to_owned()))?;
        Ok(xhtml.element("p", &text)?)
    }
}
