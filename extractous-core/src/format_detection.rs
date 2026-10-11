/// Fast format detection for optimized parsing
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum DocumentFormat {
    Pdf,
    Docx,
    Xlsx,
    Pptx,
    Html,
    Xml,
    Csv,
    Text,
    Json,
    Unknown,
}

/// Fast format detection using file extension and magic bytes
pub fn detect_format<P: AsRef<Path>>(path: P) -> DocumentFormat {
    let path = path.as_ref();

    // First try extension-based detection (fastest)
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        match ext.to_lowercase().as_str() {
            "pdf" => return DocumentFormat::Pdf,
            "docx" => return DocumentFormat::Docx,
            "xlsx" => return DocumentFormat::Xlsx,
            "pptx" => return DocumentFormat::Pptx,
            "html" | "htm" => return DocumentFormat::Html,
            "xml" => return DocumentFormat::Xml,
            "csv" => return DocumentFormat::Csv,
            "txt" | "md" | "rst" => return DocumentFormat::Text,
            "json" => return DocumentFormat::Json,
            _ => {}
        }
    }

    // Fallback to magic byte detection
    if let Ok(mut file) = std::fs::File::open(path) {
        if let Ok(format) = detect_format_from_file(&mut file) {
            return format;
        }
    }

    DocumentFormat::Unknown
}

/// Detect format from file content using magic bytes
pub fn detect_format_from_file(file: &mut std::fs::File) -> Result<DocumentFormat, std::io::Error> {
    use std::io::{Read, Seek, SeekFrom};

    let mut buffer = [0u8; 16];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut buffer)?;
    file.seek(SeekFrom::Start(0))?; // Reset position

    Ok(detect_format_from_bytes(&buffer))
}

/// Detect format from byte slice using magic bytes
pub fn detect_format_from_bytes(buffer: &[u8]) -> DocumentFormat {
    if buffer.len() < 4 {
        return DocumentFormat::Unknown;
    }

    match &buffer[0..4] {
        b"%PDF" => DocumentFormat::Pdf,
        b"PK\x03\x04" => detect_office_format(buffer), // ZIP-based formats
        b"<htm" | b"<HTM" | b"<!DO" => DocumentFormat::Html,
        b"<?xm" => DocumentFormat::Xml,
        b"{\n  " | b"{ \n" | b"{\r\n" | b"[{\"" => DocumentFormat::Json,
        _ => detect_text_format(buffer),
    }
}

/// Detect specific Office format from ZIP content
fn detect_office_format(buffer: &[u8]) -> DocumentFormat {
    // For now, we'll need to examine the ZIP content to determine the exact format
    // This is a simplified version - a full implementation would parse the ZIP directory

    // Look for Office-specific patterns in the first few KB
    if buffer.len() > 100 {
        let content = String::from_utf8_lossy(&buffer[0..100.min(buffer.len())]);
        if content.contains("word/") {
            return DocumentFormat::Docx;
        } else if content.contains("xl/") {
            return DocumentFormat::Xlsx;
        } else if content.contains("ppt/") {
            return DocumentFormat::Pptx;
        }
    }

    // Default to DOCX for unknown ZIP files (most common)
    DocumentFormat::Docx
}

/// Detect text-based formats
fn detect_text_format(buffer: &[u8]) -> DocumentFormat {
    // Check if it's valid UTF-8 text
    if let Ok(text) = std::str::from_utf8(buffer) {
        // Simple CSV detection
        if text.contains(',') && text.lines().count() > 1 {
            let first_line = text.lines().next().unwrap_or("");
            let comma_count = first_line.matches(',').count();
            if comma_count > 0 && comma_count < 20 {
                // Reasonable CSV column count
                return DocumentFormat::Csv;
            }
        }

        // Check for HTML patterns
        if text.to_lowercase().contains("<html") || text.to_lowercase().contains("<!doctype") {
            return DocumentFormat::Html;
        }

        // Check for XML patterns
        if text.trim_start().starts_with("<?xml") || text.trim_start().starts_with('<') {
            return DocumentFormat::Xml;
        }

        // Check for JSON patterns
        let trimmed = text.trim_start();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            return DocumentFormat::Json;
        }

        DocumentFormat::Text
    } else {
        DocumentFormat::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pdf_detection() {
        let pdf_header = b"%PDF-1.4\n";
        assert_eq!(detect_format_from_bytes(pdf_header), DocumentFormat::Pdf);
    }

    #[test]
    fn test_office_detection() {
        let zip_header = b"PK\x03\x04";
        assert_eq!(detect_format_from_bytes(zip_header), DocumentFormat::Docx);
    }

    #[test]
    fn test_html_detection() {
        let html_content = b"<!DOCTYPE html><html>";
        assert_eq!(detect_format_from_bytes(html_content), DocumentFormat::Html);
    }

    #[test]
    fn test_csv_detection() {
        let csv_content = b"name,age,city\nJohn,25,NYC\n";
        assert_eq!(detect_format_from_bytes(csv_content), DocumentFormat::Csv);
    }

    #[test]
    fn test_json_detection() {
        let json_content = b"{\n  \"name\": \"test\"\n}";
        assert_eq!(detect_format_from_bytes(json_content), DocumentFormat::Json);
    }
}
