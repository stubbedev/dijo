//! The metrics of the standard 14 fonts as PDFBox's `Standard14Fonts` holds them: Adobe's font
//! metrics files, without their kerning pairs (`models/pdf/afm/`, written by
//! `just pdf-resources`), found by the names and aliases PDFBox maps to them.

use std::collections::HashMap;
use std::sync::LazyLock;

use super::encoding::Encoding;

/// The standard 14 fonts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Standard14 {
    TimesRoman,
    TimesBold,
    TimesItalic,
    TimesBoldItalic,
    Helvetica,
    HelveticaBold,
    HelveticaOblique,
    HelveticaBoldOblique,
    Courier,
    CourierBold,
    CourierOblique,
    CourierBoldOblique,
    Symbol,
    ZapfDingbats,
}

impl Standard14 {
    /// `Standard14Fonts.getMappedFontName`: the font a name or alias stands for.
    pub(super) fn named(name: &str) -> Option<Standard14> {
        use Standard14::*;
        Some(match name {
            "Times-Roman" | "TimesNewRoman" | "Times" => TimesRoman,
            "Times-Bold" | "TimesNewRoman,Bold" | "Times,Bold" => TimesBold,
            "Times-Italic" | "TimesNewRoman,Italic" | "Times,Italic" => TimesItalic,
            "Times-BoldItalic" | "TimesNewRoman,BoldItalic" | "Times,BoldItalic" => TimesBoldItalic,
            "Helvetica" | "Arial" | "ArialMT" => Helvetica,
            "Helvetica-Bold" | "Arial,Bold" | "Arial-BoldMT" => HelveticaBold,
            "Helvetica-Oblique" | "Arial,Italic" | "Arial-ItalicMT" => HelveticaOblique,
            "Helvetica-BoldOblique" | "Arial,BoldItalic" | "Arial-BoldItalicMT" => {
                HelveticaBoldOblique
            }
            "Courier" | "CourierCourierNew" | "CourierNew" => Courier,
            "Courier-Bold" | "CourierNew,Bold" => CourierBold,
            "Courier-Oblique" | "CourierNew,Italic" => CourierOblique,
            "Courier-BoldOblique" | "CourierNew,BoldItalic" => CourierBoldOblique,
            "Symbol" | "Symbol,Italic" | "Symbol,Bold" | "Symbol,BoldItalic" => Symbol,
            "ZapfDingbats" => ZapfDingbats,
            _ => return None,
        })
    }

    fn source(self) -> &'static str {
        use Standard14::*;
        match self {
            TimesRoman => include_str!("../../../models/pdf/afm/Times-Roman.afm"),
            TimesBold => include_str!("../../../models/pdf/afm/Times-Bold.afm"),
            TimesItalic => include_str!("../../../models/pdf/afm/Times-Italic.afm"),
            TimesBoldItalic => include_str!("../../../models/pdf/afm/Times-BoldItalic.afm"),
            Helvetica => include_str!("../../../models/pdf/afm/Helvetica.afm"),
            HelveticaBold => include_str!("../../../models/pdf/afm/Helvetica-Bold.afm"),
            HelveticaOblique => include_str!("../../../models/pdf/afm/Helvetica-Oblique.afm"),
            HelveticaBoldOblique => {
                include_str!("../../../models/pdf/afm/Helvetica-BoldOblique.afm")
            }
            Courier => include_str!("../../../models/pdf/afm/Courier.afm"),
            CourierBold => include_str!("../../../models/pdf/afm/Courier-Bold.afm"),
            CourierOblique => include_str!("../../../models/pdf/afm/Courier-Oblique.afm"),
            CourierBoldOblique => include_str!("../../../models/pdf/afm/Courier-BoldOblique.afm"),
            Symbol => include_str!("../../../models/pdf/afm/Symbol.afm"),
            ZapfDingbats => include_str!("../../../models/pdf/afm/ZapfDingbats.afm"),
        }
    }

    /// The font's metrics.
    pub(super) fn metrics(self) -> &'static FontMetrics {
        &METRICS[&self]
    }
}

static METRICS: LazyLock<HashMap<Standard14, FontMetrics>> = LazyLock::new(|| {
    use Standard14::*;
    [
        TimesRoman,
        TimesBold,
        TimesItalic,
        TimesBoldItalic,
        Helvetica,
        HelveticaBold,
        HelveticaOblique,
        HelveticaBoldOblique,
        Courier,
        CourierBold,
        CourierOblique,
        CourierBoldOblique,
        Symbol,
        ZapfDingbats,
    ]
    .into_iter()
    .map(|font| (font, FontMetrics::parse(font.source())))
    .collect()
});

/// One glyph's metrics.
#[derive(Debug, Clone)]
pub(super) struct CharMetric {
    pub(super) code: i32,
    pub(super) name: String,
    pub(super) wx: f32,
}

/// FontBox's `FontMetrics`, as far as PDFBox's text extraction reads it.
#[derive(Debug)]
pub(super) struct FontMetrics {
    pub(super) font_name: String,
    pub(super) encoding_scheme: String,
    pub(super) font_bbox: [f32; 4],
    pub(super) ascender: f32,
    pub(super) descender: f32,
    pub(super) cap_height: f32,
    pub(super) char_metrics: Vec<CharMetric>,
    widths: HashMap<String, f32>,
}

impl FontMetrics {
    fn parse(source: &str) -> FontMetrics {
        let mut metrics = FontMetrics {
            font_name: String::new(),
            encoding_scheme: String::new(),
            font_bbox: [0.0; 4],
            ascender: 0.0,
            descender: 0.0,
            cap_height: 0.0,
            char_metrics: Vec::new(),
            widths: HashMap::new(),
        };
        let number = |text: &str| {
            text.parse::<f32>().unwrap_or_else(|e| {
                panic!("a standard 14 font's metrics hold a bad number {text}: {e}")
            })
        };
        for line in source.lines() {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            match key {
                "FontName" => metrics.font_name = value.trim().to_owned(),
                "EncodingScheme" => metrics.encoding_scheme = value.trim().to_owned(),
                "FontBBox" => {
                    let values: Vec<f32> = value.split_whitespace().map(number).collect();
                    metrics.font_bbox = [values[0], values[1], values[2], values[3]];
                }
                "Ascender" => metrics.ascender = number(value.trim()),
                "Descender" => metrics.descender = number(value.trim()),
                "CapHeight" => metrics.cap_height = number(value.trim()),
                "C" => {
                    let mut metric = CharMetric {
                        code: -1,
                        name: String::new(),
                        wx: 0.0,
                    };
                    for field in line.split(';') {
                        let mut words = field.split_whitespace();
                        match (words.next(), words.next()) {
                            (Some("C"), Some(code)) => metric.code = number(code) as i32,
                            (Some("WX"), Some(wx)) => metric.wx = number(wx),
                            (Some("N"), Some(name)) => metric.name = name.to_owned(),
                            _ => {}
                        }
                    }
                    metrics.widths.insert(metric.name.clone(), metric.wx);
                    metrics.char_metrics.push(metric);
                }
                _ => {}
            }
        }
        metrics
    }

    /// `FontMetrics.getCharacterWidth`: 0 for a name the font lacks.
    pub(super) fn character_width(&self, name: &str) -> f32 {
        self.widths.get(name).copied().unwrap_or(0.0)
    }

    /// `FontMetrics.getAverageCharacterWidth`.
    pub(super) fn average_character_width(&self) -> f32 {
        let mut total = 0.0f32;
        let mut count = 0.0f32;
        for metric in &self.char_metrics {
            if metric.wx > 0.0 {
                total += metric.wx;
                count += 1.0;
            }
        }
        if total > 0.0 {
            total / count
        } else {
            0.0
        }
    }

    /// `new Type1Encoding(FontMetrics)`: the codes and names of the metrics.
    pub(super) fn encoding(&self) -> Encoding {
        let entries: Vec<(i32, String)> = self
            .char_metrics
            .iter()
            .map(|metric| (metric.code, metric.name.clone()))
            .collect();
        Encoding::in_order(&entries)
    }

    /// Whether `buildFontDescriptor` flags the font symbolic: its encoding is font specific.
    pub(super) fn is_symbolic(&self) -> bool {
        self.encoding_scheme == "FontSpecific"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_standard_14_metrics() {
        let helvetica = Standard14::named("Arial").unwrap().metrics();
        assert_eq!(helvetica.font_name, "Helvetica");
        assert_eq!(helvetica.character_width("space"), 278.0);
        assert_eq!(helvetica.character_width("nosuchglyph"), 0.0);
        assert_eq!(helvetica.font_bbox, [-166.0, -225.0, 1000.0, 931.0]);
        assert_eq!(helvetica.cap_height, 718.0);
        assert!(!helvetica.is_symbolic());
        assert!(Standard14::Symbol.metrics().is_symbolic());
        assert_eq!(Standard14::named("Helvetica,Bold"), None);
        assert_eq!(Standard14::Courier.metrics().encoding().name(65), "A");
    }
}
