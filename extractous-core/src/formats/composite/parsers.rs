//! Recorded from Tika's composite parser and kept as a frozen reference: each type Tika's
//! default types declare whose parser in Tika's composite parser is not the one of its
//! supertype, with the class of that parser, in the order of the types' names.

pub(super) const TIKA_PARSERS: &[(&str, &str)] = &[
    (
        "application/applefile",
        "org.apache.tika.parser.apple.AppleSingleFileParser",
    ),
    (
        "application/atom+xml",
        "org.apache.tika.parser.feed.FeedParser",
    ),
    (
        "application/dif+xml",
        "org.apache.tika.parser.dif.DIFParser",
    ),
    (
        "application/epub+zip",
        "org.apache.tika.parser.epub.EpubParser",
    ),
    (
        "application/gzip",
        "org.apache.tika.parser.pkg.CompressorParser",
    ),
    ("application/hwp+zip", "ai.yobix.HwpxParser"),
    (
        "application/java-vm",
        "org.apache.tika.parser.asm.ClassParser",
    ),
    ("application/mbox", "org.apache.tika.parser.mbox.MboxParser"),
    ("application/mp4", "org.apache.tika.parser.mp4.MP4Parser"),
    ("application/ogg", "org.apache.tika.parser.ogg.OggParser"),
    (
        "application/onenote; format=one",
        "org.apache.tika.parser.microsoft.onenote.OneNoteParser",
    ),
    ("application/pdf", "ai.yobix.TextLayerCheckingPdfParser"),
    ("application/pgp-encrypted", "ai.yobix.OpenPgpParser"),
    ("application/pkcs7-mime", "ai.yobix.CmsParser"),
    ("application/pkcs7-signature", "ai.yobix.CmsParser"),
    (
        "application/rss+xml",
        "org.apache.tika.parser.feed.FeedParser",
    ),
    (
        "application/rtf",
        "org.apache.tika.parser.microsoft.rtf.RTFParser",
    ),
    (
        "application/timestamped-data",
        "org.apache.tika.parser.crypto.TSDParser",
    ),
    (
        "application/vnd.adobe.indesign-idml-package",
        "org.apache.tika.parser.indesign.IDMLParser",
    ),
    (
        "application/vnd.apple.iwork",
        "org.apache.tika.parser.iwork.IWorkPackageParser",
    ),
    (
        "application/vnd.apple.unknown.13",
        "ai.yobix.IworkArchiveParser",
    ),
    (
        "application/vnd.geogebra.file",
        "org.apache.tika.parser.geogebra.GeoGebraParser",
    ),
    (
        "application/vnd.geogebra.slides",
        "org.apache.tika.parser.geogebra.GeoGebraParser",
    ),
    (
        "application/vnd.geogebra.tool",
        "org.apache.tika.parser.geogebra.GeoGebraParser",
    ),
    (
        "application/vnd.mif",
        "org.apache.tika.parser.mif.MIFParser",
    ),
    (
        "application/vnd.ms-excel.addin.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-excel.sheet.2",
        "org.apache.tika.parser.microsoft.OldExcelParser",
    ),
    (
        "application/vnd.ms-excel.sheet.3",
        "org.apache.tika.parser.microsoft.OldExcelParser",
    ),
    (
        "application/vnd.ms-excel.sheet.4",
        "org.apache.tika.parser.microsoft.OldExcelParser",
    ),
    (
        "application/vnd.ms-excel.sheet.binary.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-excel.sheet.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-excel.template.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-excel.workspace.3",
        "org.apache.tika.parser.microsoft.OldExcelParser",
    ),
    (
        "application/vnd.ms-excel.workspace.4",
        "org.apache.tika.parser.microsoft.OldExcelParser",
    ),
    (
        "application/vnd.ms-htmlhelp",
        "org.apache.tika.parser.microsoft.chm.ChmParser",
    ),
    (
        "application/vnd.ms-outlook-pst",
        "org.apache.tika.parser.microsoft.pst.OutlookPSTParser",
    ),
    (
        "application/vnd.ms-powerpoint.addin.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-powerpoint.presentation.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-powerpoint.slide.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-powerpoint.slideshow.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-powerpoint.template.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-spreadsheetml",
        "org.apache.tika.parser.microsoft.xml.SpreadsheetMLParser",
    ),
    (
        "application/vnd.ms-tnef",
        "org.apache.tika.parser.microsoft.TNEFParser",
    ),
    (
        "application/vnd.ms-visio.drawing",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-visio.drawing.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-visio.stencil",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-visio.stencil.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-visio.template",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-visio.template.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-word.document.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-word.template.macroenabled.12",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.ms-word2006ml",
        "org.apache.tika.parser.microsoft.ooxml.xwpf.ml2006.Word2006MLParser",
    ),
    (
        "application/vnd.ms-wordml",
        "org.apache.tika.parser.microsoft.xml.WordMLParser",
    ),
    (
        "application/vnd.ms-xpsdocument",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.oasis.opendocument.chart",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.chart-template",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.formula",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.formula-template",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.graphics",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.graphics-template",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.image",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.image-template",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.presentation",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.presentation-template",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.spreadsheet",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.spreadsheet-template",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.text",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.text-master",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.text-template",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.text-web",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.oasis.opendocument.tika.flat.document",
        "org.apache.tika.parser.odf.FlatOpenDocumentParser",
    ),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.slide",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.slideshow",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.template",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.template",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    (
        "application/vnd.sun.xml.writer",
        "org.apache.tika.parser.odf.OpenDocumentParser",
    ),
    (
        "application/vnd.wordperfect; version=5.0",
        "org.apache.tika.parser.wordperfect.WordPerfectParser",
    ),
    (
        "application/vnd.wordperfect; version=5.1",
        "org.apache.tika.parser.wordperfect.WordPerfectParser",
    ),
    (
        "application/vnd.wordperfect; version=6.x",
        "org.apache.tika.parser.wordperfect.WordPerfectParser",
    ),
    ("application/warc", "org.apache.tika.parser.warc.WARCParser"),
    (
        "application/warc+gz",
        "org.apache.tika.parser.warc.WARCParser",
    ),
    (
        "application/x-7z-compressed",
        "org.apache.tika.parser.pkg.SevenZParser",
    ),
    (
        "application/x-activemime",
        "org.apache.tika.parser.microsoft.activemime.ActiveMimeParser",
    ),
    (
        "application/x-archive",
        "org.apache.tika.parser.pkg.PackageParser",
    ),
    (
        "application/x-arj",
        "org.apache.tika.parser.pkg.PackageParser",
    ),
    (
        "application/x-bplist",
        "org.apache.tika.parser.apple.PListParser",
    ),
    (
        "application/x-brotli",
        "org.apache.tika.parser.pkg.CompressorParser",
    ),
    (
        "application/x-bzip",
        "org.apache.tika.parser.pkg.CompressorParser",
    ),
    (
        "application/x-compress",
        "org.apache.tika.parser.pkg.CompressorParser",
    ),
    (
        "application/x-cpio",
        "org.apache.tika.parser.pkg.PackageParser",
    ),
    ("application/x-dbf", "org.apache.tika.parser.dbf.DBFParser"),
    (
        "application/x-elf",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-fictionbook+xml",
        "ai.yobix.FictionBookParser",
    ),
    (
        "application/x-font-adobe-metric",
        "org.apache.tika.parser.font.AdobeFontMetricParser",
    ),
    (
        "application/x-font-ttf",
        "org.apache.tika.parser.font.TrueTypeParser",
    ),
    (
        "application/x-geopackage",
        "org.apache.tika.parser.geopkg.GeoPkgParser",
    ),
    (
        "application/x-httpresponse",
        "org.apache.tika.parser.http.HttpParser",
    ),
    (
        "application/x-hwp-v5",
        "org.apache.tika.parser.hwp.HwpV5Parser",
    ),
    (
        "application/x-internet-archive",
        "org.apache.tika.parser.warc.WARCParser",
    ),
    ("application/x-ipynb+json", "ai.yobix.NotebookParser"),
    (
        "application/x-java-pack200",
        "org.apache.tika.parser.pkg.CompressorParser",
    ),
    (
        "application/x-lz4",
        "org.apache.tika.parser.pkg.CompressorParser",
    ),
    (
        "application/x-lzma",
        "org.apache.tika.parser.pkg.CompressorParser",
    ),
    (
        "application/x-mach-o",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-mach-o-bundle",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-mach-o-core",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-mach-o-dsym",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-mach-o-dylib",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-mach-o-dylib-stub",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-mach-o-dylinker",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-mach-o-executable",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-mach-o-fvmlib",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-mach-o-kext-bundle",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-mach-o-object",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-mach-o-preload",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-mach-o-universal",
        "org.apache.tika.parser.executable.UniversalExecutableParser",
    ),
    (
        "application/x-matlab-data",
        "org.apache.tika.parser.mat.MatParser",
    ),
    ("application/x-mobipocket-ebook", "ai.yobix.MobiParser"),
    (
        "application/x-ms-owner",
        "org.apache.tika.parser.microsoft.MSOwnerFileParser",
    ),
    (
        "application/x-msaccess",
        "org.apache.tika.parser.microsoft.JackcessParser",
    ),
    (
        "application/x-msdownload",
        "org.apache.tika.parser.executable.ExecutableParser",
    ),
    (
        "application/x-plist",
        "org.apache.tika.parser.apple.PListParser",
    ),
    ("application/x-prt", "org.apache.tika.parser.prt.PRTParser"),
    (
        "application/x-rar-compressed",
        "org.apache.tika.parser.pkg.RarParser",
    ),
    (
        "application/x-sas-data",
        "org.apache.tika.parser.sas.SAS7BDATParser",
    ),
    (
        "application/x-sqlite3",
        "org.apache.tika.parser.sqlite3.SQLite3Parser",
    ),
    ("application/x-subrip", "ai.yobix.SubtitleParser"),
    (
        "application/x-tar",
        "org.apache.tika.parser.pkg.PackageParser",
    ),
    (
        "application/x-tika-msoffice",
        "org.apache.tika.parser.microsoft.OfficeParser",
    ),
    (
        "application/x-tika-unix-dump",
        "org.apache.tika.parser.pkg.PackageParser",
    ),
    ("application/x-tmx", "org.apache.tika.parser.tmx.TMXParser"),
    (
        "application/x-wacz",
        "org.apache.tika.parser.wacz.WACZParser",
    ),
    (
        "application/x-xliff+xml",
        "org.apache.tika.parser.xliff.XLIFF12Parser",
    ),
    (
        "application/x-xliff+zip",
        "org.apache.tika.parser.xliff.XLZParser",
    ),
    (
        "application/x-xz",
        "org.apache.tika.parser.pkg.CompressorParser",
    ),
    (
        "application/xhtml+xml",
        "org.apache.tika.parser.html.JSoupParser",
    ),
    ("application/xml", "org.apache.tika.parser.xml.DcXMLParser"),
    ("application/zip", "org.apache.tika.parser.pkg.ZipParser"),
    (
        "application/zlib",
        "org.apache.tika.parser.pkg.CompressorParser",
    ),
    ("audio/basic", "org.apache.tika.parser.audio.AudioParser"),
    ("audio/midi", "org.apache.tika.parser.audio.MidiParser"),
    ("audio/mpeg", "org.apache.tika.parser.mp3.Mp3Parser"),
    ("audio/opus", "org.apache.tika.parser.ogg.OpusParser"),
    ("audio/speex", "org.apache.tika.parser.ogg.SpeexParser"),
    ("audio/vnd.wave", "org.apache.tika.parser.audio.AudioParser"),
    ("audio/vorbis", "org.apache.tika.parser.ogg.VorbisParser"),
    ("audio/x-aiff", "org.apache.tika.parser.audio.AudioParser"),
    ("audio/x-flac", "org.apache.tika.parser.ogg.FlacParser"),
    ("audio/x-oggflac", "org.apache.tika.parser.ogg.FlacParser"),
    ("image/avif", "org.apache.tika.parser.image.HeifParser"),
    ("image/bmp", "org.apache.tika.parser.image.ImageParser"),
    ("image/emf", "org.apache.tika.parser.microsoft.EMFParser"),
    ("image/gif", "org.apache.tika.parser.image.ImageParser"),
    ("image/heif", "org.apache.tika.parser.image.HeifParser"),
    (
        "image/heif-sequence",
        "org.apache.tika.parser.image.HeifParser",
    ),
    ("image/icns", "org.apache.tika.parser.image.ICNSParser"),
    ("image/jp2", "org.apache.tika.parser.image.ImageParser"),
    ("image/jpeg", "org.apache.tika.parser.image.JpegParser"),
    ("image/jpx", "org.apache.tika.parser.image.ImageParser"),
    ("image/jxl", "org.apache.tika.parser.image.JXLParser"),
    ("image/png", "org.apache.tika.parser.image.ImageParser"),
    ("image/tiff", "org.apache.tika.parser.image.TiffParser"),
    (
        "image/vnd.adobe.photoshop",
        "ai.yobix.RecognizedImageParser",
    ),
    (
        "image/vnd.dgn; version=8",
        "org.apache.tika.parser.dgn.DGN8Parser",
    ),
    ("image/vnd.djvu", "ai.yobix.DjvuParser"),
    ("image/vnd.dwg", "org.apache.tika.parser.dwg.DWGParser"),
    (
        "image/vnd.microsoft.icon",
        "org.apache.tika.parser.image.ImageParser",
    ),
    (
        "image/vnd.wap.wbmp",
        "org.apache.tika.parser.image.ImageParser",
    ),
    ("image/vnd.zbrush.pcx", "ai.yobix.RecognizedImageParser"),
    ("image/webp", "ai.yobix.RecognizedImageParser"),
    ("image/wmf", "org.apache.tika.parser.microsoft.WMFParser"),
    ("image/x-bpg", "org.apache.tika.parser.image.BPGParser"),
    (
        "image/x-canon-cr2",
        "org.apache.tika.parser.image.RawTiffParser",
    ),
    ("image/x-jbig2", "org.apache.tika.parser.image.ImageParser"),
    ("image/x-portable-anymap", "ai.yobix.RecognizedImageParser"),
    (
        "image/x-raw-adobe",
        "org.apache.tika.parser.image.RawTiffParser",
    ),
    (
        "image/x-raw-nikon",
        "org.apache.tika.parser.image.RawTiffParser",
    ),
    (
        "image/x-raw-pentax",
        "org.apache.tika.parser.image.RawTiffParser",
    ),
    (
        "image/x-raw-samsung",
        "org.apache.tika.parser.image.RawTiffParser",
    ),
    (
        "image/x-raw-sony",
        "org.apache.tika.parser.image.RawTiffParser",
    ),
    ("image/x-tga", "ai.yobix.RecognizedImageParser"),
    ("image/x-xcf", "org.apache.tika.parser.image.ImageParser"),
    ("message/rfc822", "org.apache.tika.parser.mail.RFC822Parser"),
    ("message/x-emlx", "ai.yobix.EmlxParser"),
    (
        "model/vnd.dwfx+xps",
        "org.apache.tika.parser.microsoft.ooxml.OOXMLParser",
    ),
    ("text/html", "org.apache.tika.parser.html.JSoupParser"),
    (
        "text/markdown",
        "org.apache.tika.parser.markdown.MarkdownParser",
    ),
    ("text/plain", "org.apache.tika.parser.csv.TextAndCSVParser"),
    (
        "text/vnd.iptc.anpa",
        "org.apache.tika.parser.iptc.IptcAnpaParser",
    ),
    ("text/vtt", "ai.yobix.SubtitleParser"),
    (
        "text/x-c++src",
        "org.apache.tika.parser.code.SourceCodeParser",
    ),
    (
        "text/x-groovy",
        "org.apache.tika.parser.code.SourceCodeParser",
    ),
    (
        "text/x-java-source",
        "org.apache.tika.parser.code.SourceCodeParser",
    ),
    ("text/x-ssa", "ai.yobix.SubtitleParser"),
    ("video/3gpp", "org.apache.tika.parser.mp4.MP4Parser"),
    ("video/3gpp2", "org.apache.tika.parser.mp4.MP4Parser"),
    ("video/quicktime", "org.apache.tika.parser.mp4.MP4Parser"),
    ("video/theora", "org.apache.tika.parser.ogg.TheoraParser"),
    ("video/x-flv", "org.apache.tika.parser.video.FLVParser"),
];
