//! jsoup's `Tag` and `TagSet` as JSoupParser configures them: jsoup's default HTML tag set
//! (`TagSet.initHtmlDefault`), with the tags Tika lets close themselves (`self-closeable-tags.txt`
//! from the HTML module, which `just html-reference` writes to `models/`), and the parser options
//! of `HtmlTagOptions`. Tags the parse meets that the set lacks are added to the parse's copy.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::formats::java;

const TIKA_SELF_CLOSEABLE: &str = include_str!("../../../models/tika-self-closeable-tags.txt");

pub(super) const KNOWN: u32 = 1;
pub(super) const VOID: u32 = 1 << 1;
const BLOCK: u32 = 1 << 2;
const INLINE_CONTAINER: u32 = 1 << 3;
pub(super) const SELF_CLOSE: u32 = 1 << 4;
const PRESERVE_WHITESPACE: u32 = 1 << 6;
pub(super) const RCDATA: u32 = 1 << 7;
pub(super) const DATA: u32 = 1 << 8;
const FORM_SUBMITTABLE: u32 = 1 << 9;
const TEXT_BOUNDARY: u32 = 1 << 10;

pub(super) const SCOPE: u32 = 1;
pub(super) const LIST_SCOPE: u32 = 1 << 1;
pub(super) const BUTTON_SCOPE: u32 = 1 << 2;
pub(super) const TABLE_SCOPE: u32 = 1 << 3;
pub(super) const SELECT_SCOPE_MEMBER: u32 = 1 << 4;
pub(super) const IMPLIED_END: u32 = 1 << 5;
pub(super) const THOROUGH_IMPLIED_END: u32 = 1 << 6;
pub(super) const SPECIAL: u32 = 1 << 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Namespace {
    Html,
    Svg,
    MathMl,
}

#[derive(Debug, Clone)]
pub(super) struct Tag {
    /// The name as the element shows it (`getName`): lower case in HTML, as written in foreign
    /// content.
    pub(super) name: String,
    pub(super) normal: String,
    pub(super) namespace: Namespace,
    options: u32,
    parser_options: u32,
}

impl Tag {
    fn new(name: &str, normal: &str, namespace: Namespace) -> Self {
        Tag {
            name: name.to_string(),
            normal: normal.to_string(),
            namespace,
            options: 0,
            parser_options: parser_options(normal, namespace),
        }
    }

    pub(super) fn is(&self, option: u32) -> bool {
        self.options & option != 0
    }

    fn set(&mut self, option: u32) {
        self.options |= option | KNOWN;
    }

    pub(super) fn has_parser_option(&self, option: u32) -> bool {
        self.parser_options & option != 0
    }

    pub(super) fn is_known(&self) -> bool {
        self.is(KNOWN)
    }

    /// A void tag (`isEmpty`).
    pub(super) fn is_void(&self) -> bool {
        self.is(VOID)
    }

    pub(super) fn is_self_closing(&self) -> bool {
        self.is(SELF_CLOSE) || self.is(VOID)
    }
}

const SCOPE_TAGS: [&str; 10] = [
    "applet", "caption", "html", "marquee", "object", "select", "table", "td", "template", "th",
];
const IMPLIED_END_TAGS: [&str; 10] = [
    "dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc",
];
const THOROUGH_IMPLIED_END_TAGS: [&str; 18] = [
    "caption", "colgroup", "dd", "dt", "li", "optgroup", "option", "p", "rb", "rp", "rt", "rtc",
    "tbody", "td", "tfoot", "th", "thead", "tr",
];
const SPECIAL_TAGS: &[&str] = &[
    "address",
    "applet",
    "area",
    "article",
    "aside",
    "base",
    "basefont",
    "bgsound",
    "blockquote",
    "body",
    "br",
    "button",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dir",
    "div",
    "dl",
    "dt",
    "embed",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hgroup",
    "hr",
    "html",
    "iframe",
    "img",
    "input",
    "keygen",
    "li",
    "link",
    "listing",
    "main",
    "marquee",
    "menu",
    "meta",
    "nav",
    "noembed",
    "noframes",
    "noscript",
    "object",
    "ol",
    "p",
    "param",
    "plaintext",
    "pre",
    "script",
    "search",
    "section",
    "select",
    "source",
    "style",
    "summary",
    "table",
    "tbody",
    "td",
    "template",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
    "wbr",
    "xmp",
];

/// The parser options of a tag (`HtmlTagOptions.optionsFor`).
fn parser_options(normal: &str, namespace: Namespace) -> u32 {
    let mut options = 0;
    let has = |list: &[&str]| list.contains(&normal);
    match namespace {
        Namespace::Html => {
            if has(&IMPLIED_END_TAGS) {
                options |= IMPLIED_END;
            }
            if has(&THOROUGH_IMPLIED_END_TAGS) {
                options |= THOROUGH_IMPLIED_END;
            }
            if has(&["optgroup", "option"]) {
                options |= SELECT_SCOPE_MEMBER;
            }
            if has(&SCOPE_TAGS) {
                options |= SCOPE;
            }
            if has(&["ol", "ul"]) {
                options |= LIST_SCOPE;
            }
            if has(&["button"]) {
                options |= BUTTON_SCOPE;
            }
            if has(&["html", "table", "template"]) {
                options |= TABLE_SCOPE;
            }
            if has(SPECIAL_TAGS) {
                options |= SPECIAL;
            }
        }
        Namespace::MathMl => {
            if has(&["annotation-xml", "mi", "mn", "mo", "ms", "mtext"]) {
                options |= SCOPE | SPECIAL;
            }
        }
        Namespace::Svg => {
            if has(&["desc", "foreignobject", "title"]) {
                options |= SCOPE | SPECIAL;
            }
        }
    }
    options
}

pub(super) type TagId = usize;

/// The tags of one parse (`TagSet`), each kept once by namespace and name.
#[derive(Clone)]
pub(super) struct TagSet {
    tags: Vec<Tag>,
    index: HashMap<(Namespace, String), TagId>,
}

/// The tag set JSoupParser copies for each parse.
static TIKA_TAGS: LazyLock<TagSet> = LazyLock::new(|| {
    let mut tags = TagSet {
        tags: Vec::new(),
        index: HashMap::new(),
    };
    let html_lists: [(&[&str], u32); 9] = [
        (
            &[
                "html",
                "head",
                "body",
                "frameset",
                "script",
                "noscript",
                "style",
                "meta",
                "link",
                "title",
                "frame",
                "noframes",
                "section",
                "nav",
                "aside",
                "hgroup",
                "header",
                "footer",
                "p",
                "h1",
                "h2",
                "h3",
                "h4",
                "h5",
                "h6",
                "dialog",
                "search",
                "ul",
                "ol",
                "pre",
                "div",
                "blockquote",
                "hr",
                "address",
                "figure",
                "figcaption",
                "form",
                "fieldset",
                "dl",
                "dt",
                "dd",
                "li",
                "table",
                "caption",
                "thead",
                "tfoot",
                "tbody",
                "colgroup",
                "col",
                "tr",
                "th",
                "td",
                "details",
                "menu",
                "plaintext",
                "template",
                "article",
                "main",
                "center",
                "dir",
                "applet",
                "marquee",
                "listing",
                "#root",
            ],
            BLOCK,
        ),
        (
            &[
                "object", "base", "font", "tt", "i", "b", "u", "big", "small", "em", "strong",
                "dfn", "code", "samp", "kbd", "var", "cite", "abbr", "time", "acronym", "mark",
                "ruby", "rt", "rp", "rtc", "a", "img", "wbr", "map", "q", "sub", "sup", "bdo",
                "iframe", "embed", "span", "input", "select", "textarea", "label", "audio",
                "video", "canvas", "optgroup", "option", "legend", "datalist", "keygen", "output",
                "progress", "meter", "area", "param", "source", "track", "summary", "command",
                "device", "basefont", "bgsound", "menuitem", "data", "bdi", "s", "strike", "nobr",
                "ins", "del", "button", "picture", "slot", "rb",
            ],
            0,
        ),
        (
            &[
                "title", "p", "h1", "h2", "h3", "h4", "h5", "h6", "pre", "address", "li", "th",
                "td", "script", "style",
            ],
            INLINE_CONTAINER,
        ),
        (
            &[
                "meta", "link", "base", "frame", "img", "br", "wbr", "embed", "hr", "input",
                "keygen", "col", "command", "device", "area", "basefont", "bgsound", "menuitem",
                "param", "source", "track",
            ],
            VOID,
        ),
        (
            &["pre", "plaintext", "title", "textarea", "script"],
            PRESERVE_WHITESPACE,
        ),
        (&["title", "textarea"], RCDATA),
        (
            &["iframe", "noembed", "noframes", "script", "style", "xmp"],
            DATA,
        ),
        (
            &["input", "keygen", "object", "select", "textarea"],
            FORM_SUBMITTABLE,
        ),
        (
            &[
                "button", "input", "select", "textarea", "option", "output", "progress", "meter",
                "img", "picture", "audio", "video", "canvas", "object", "embed", "iframe",
            ],
            TEXT_BOUNDARY,
        ),
    ];
    for (names, option) in html_lists {
        tags.setup(Namespace::Html, names, option);
    }
    tags.setup(Namespace::MathMl, &["math"], BLOCK);
    tags.setup(Namespace::MathMl, &["mi", "mo", "msup", "mn", "mtext"], 0);
    tags.setup(Namespace::Svg, &["svg", "femerge", "femergenode"], BLOCK);
    tags.setup(Namespace::Svg, &["text"], 0);
    tags.setup(Namespace::Svg, &["script"], DATA);
    for line in TIKA_SELF_CLOSEABLE.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let name = line.trim();
        let id = tags.value_of(name, None, Namespace::Html, true);
        tags.tags[id].set(SELF_CLOSE);
    }
    tags
});

impl TagSet {
    /// A copy of JSoupParser's tag set, for one parse.
    pub(super) fn tika() -> Self {
        TIKA_TAGS.clone()
    }

    fn setup(&mut self, namespace: Namespace, names: &[&str], option: u32) {
        for &name in names {
            let id = match self.index.get(&(namespace, name.to_string())) {
                Some(&id) => id,
                None => {
                    let mut tag = Tag::new(name, name, namespace);
                    tag.options = KNOWN;
                    self.add(tag)
                }
            };
            self.tags[id].set(option);
        }
    }

    fn add(&mut self, tag: Tag) -> TagId {
        let id = self.tags.len();
        self.index.insert((tag.namespace, tag.name.clone()), id);
        self.tags.push(tag);
        id
    }

    fn get(&self, name: &str, namespace: Namespace) -> Option<TagId> {
        self.index.get(&(namespace, name.to_string())).copied()
    }

    /// The tag of a name, added when the set lacks it (`TagSet.valueOf`). `normal` is the lower
    /// case name when the tokeniser has it; with `preserve_case`, a known tag met in another case
    /// gets a copy under that name.
    pub(super) fn value_of(
        &mut self,
        name: &str,
        normal: Option<&str>,
        namespace: Namespace,
        preserve_case: bool,
    ) -> TagId {
        let name = if normal.is_none() {
            java::trim(name)
        } else {
            name
        };
        if let Some(id) = self.get(name, namespace) {
            return id;
        }
        let normal = match normal {
            Some(normal) => normal.to_string(),
            None => java::trim(&name.to_lowercase()).to_string(),
        };
        let name = if preserve_case { name } else { normal.as_str() };
        if let Some(id) = self.get(&normal, namespace) {
            if preserve_case && name != normal {
                let mut copy = self.tags[id].clone();
                copy.name = name.to_string();
                return self.add(copy);
            }
            return id;
        }
        self.add(Tag::new(name, &normal, namespace))
    }

    pub(super) fn tag(&self, id: TagId) -> &Tag {
        &self.tags[id]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configures_tags_as_jsoup_parser_does() {
        let mut tags = TagSet::tika();
        let div = tags.value_of("div", Some("div"), Namespace::Html, false);
        assert!(tags.tag(div).is_self_closing() && !tags.tag(div).is_void());
        let br = tags.value_of("br", Some("br"), Namespace::Html, false);
        assert!(tags.tag(br).is_void() && tags.tag(br).has_parser_option(SPECIAL));
        let path = tags.value_of("path", Some("path"), Namespace::Html, false);
        assert!(tags.tag(path).is_known() && tags.tag(path).is_self_closing());
        let custom = tags.value_of("Custom", Some("custom"), Namespace::Html, false);
        assert!(!tags.tag(custom).is_known());
        assert_eq!(tags.tag(custom).name, "custom");
        let object = tags.value_of("foreignObject", Some("foreignobject"), Namespace::Svg, true);
        assert_eq!(tags.tag(object).name, "foreignObject");
        assert!(tags.tag(object).has_parser_option(SCOPE));
        let iframe = tags.value_of("iframe", Some("iframe"), Namespace::Html, false);
        assert!(tags.tag(iframe).is(DATA) && tags.tag(iframe).is_self_closing());
    }
}
