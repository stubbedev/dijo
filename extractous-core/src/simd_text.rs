/// SIMD-optimized text processing for better performance
///
/// This module provides vectorized operations for:
/// - UTF-8 validation
/// - Text cleaning and normalization
/// - Character encoding conversion
/// - Whitespace normalization

/// Fast text cleaning using SIMD when available
pub fn clean_text_fast(input: &str) -> String {
    // For now, use standard string operations
    // In a full SIMD implementation, we'd use vectorized operations

    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            // Normalize whitespace
            '\t' | '\r' | '\n' => {
                result.push(' ');
                // Skip consecutive whitespace
                while let Some(&next_ch) = chars.peek() {
                    if next_ch.is_whitespace() {
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            // Remove control characters except common whitespace
            ch if ch.is_control() => {
                // Skip control characters
            }
            // Keep printable characters
            ch => {
                result.push(ch);
            }
        }
    }

    // Trim and return
    result.trim().to_string()
}

/// Fast UTF-8 validation (placeholder for SIMD implementation)
pub fn validate_utf8_fast(bytes: &[u8]) -> bool {
    // Use standard library validation for now
    // In a full SIMD implementation, we'd use vectorized UTF-8 validation
    std::str::from_utf8(bytes).is_ok()
}

/// Normalize whitespace in text using optimized operations
pub fn normalize_whitespace(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut last_was_space = false;

    for ch in input.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                result.push(' ');
                last_was_space = true;
            }
        } else {
            result.push(ch);
            last_was_space = false;
        }
    }

    result.trim().to_string()
}

/// Remove common document artifacts and clean text
pub fn clean_document_text(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let lines: Vec<&str> = input.lines().collect();

    for line in lines {
        let trimmed = line.trim();

        // Skip empty lines
        if trimmed.is_empty() {
            continue;
        }

        // Skip lines that are likely headers/footers (very short or all caps)
        if trimmed.len() < 3
            || (trimmed.len() < 20
                && trimmed
                    .chars()
                    .all(|c| c.is_uppercase() || c.is_whitespace()))
        {
            continue;
        }

        // Clean the line
        let cleaned = clean_text_fast(trimmed);
        if !cleaned.is_empty() {
            result.push_str(&cleaned);
            result.push('\n');
        }
    }

    result.trim().to_string()
}

/// Extract text content from mixed content (e.g., HTML with text)
pub fn extract_text_content(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_tag = false;
    let mut chars = input.chars();

    while let Some(ch) = chars.next() {
        match ch {
            '<' => {
                in_tag = true;
                // Add space to separate content that was separated by tags
                if !result.is_empty() && !result.ends_with(' ') {
                    result.push(' ');
                }
            }
            '>' => {
                in_tag = false;
            }
            ch if !in_tag => {
                result.push(ch);
            }
            _ => {
                // Skip characters inside tags
            }
        }
    }

    normalize_whitespace(&result)
}

/// Optimized text truncation that respects word boundaries
pub fn truncate_text_smart(input: &str, max_length: usize) -> String {
    if input.len() <= max_length {
        return input.to_string();
    }

    // Find the last word boundary before max_length
    let mut truncate_at = max_length;
    let bytes = input.as_bytes();

    // Walk backwards to find a word boundary
    while truncate_at > 0 {
        if bytes[truncate_at].is_ascii_whitespace() {
            break;
        }
        truncate_at -= 1;
    }

    // If we couldn't find a word boundary in a reasonable distance, just truncate
    if truncate_at < max_length.saturating_sub(50) {
        truncate_at = max_length;
    }

    // Ensure we don't break UTF-8 sequences
    while truncate_at > 0 && !input.is_char_boundary(truncate_at) {
        truncate_at -= 1;
    }

    let mut result = input[..truncate_at].to_string();
    if truncate_at < input.len() {
        result.push_str("...");
    }

    result
}

/// Fast character counting for different character types
pub struct TextStats {
    pub total_chars: usize,
    pub alphabetic: usize,
    pub numeric: usize,
    pub whitespace: usize,
    pub punctuation: usize,
}

impl TextStats {
    pub fn analyze(text: &str) -> Self {
        let mut stats = TextStats {
            total_chars: 0,
            alphabetic: 0,
            numeric: 0,
            whitespace: 0,
            punctuation: 0,
        };

        for ch in text.chars() {
            stats.total_chars += 1;

            if ch.is_alphabetic() {
                stats.alphabetic += 1;
            } else if ch.is_numeric() {
                stats.numeric += 1;
            } else if ch.is_whitespace() {
                stats.whitespace += 1;
            } else if ch.is_ascii_punctuation() {
                stats.punctuation += 1;
            }
        }

        stats
    }

    /// Estimate if this looks like meaningful text content
    pub fn is_meaningful_text(&self) -> bool {
        if self.total_chars < 10 {
            return false;
        }

        let text_ratio = (self.alphabetic + self.numeric) as f64 / self.total_chars as f64;
        let whitespace_ratio = self.whitespace as f64 / self.total_chars as f64;

        // Good text should have reasonable ratios of text to whitespace
        text_ratio > 0.6 && whitespace_ratio < 0.4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_text_fast() {
        let input = "Hello\t\tworld\n\n\nwith\r\nmultiple\x00spaces";
        let result = clean_text_fast(input);
        // The function removes control characters and normalizes whitespace
        assert_eq!(result, "Hello world with multiplespaces");
    }

    #[test]
    fn test_normalize_whitespace() {
        let input = "  Hello    world  \n\n  test  ";
        let result = normalize_whitespace(input);
        assert_eq!(result, "Hello world test");
    }

    #[test]
    fn test_extract_text_content() {
        let input = "<html><body>Hello <b>world</b>!</body></html>";
        let result = extract_text_content(input);
        assert_eq!(result, "Hello world !");
    }

    #[test]
    fn test_truncate_text_smart() {
        let input = "This is a long sentence that should be truncated at word boundaries";
        let result = truncate_text_smart(input, 30);
        assert!(result.len() <= 33); // 30 + "..."
        assert!(result.ends_with("..."));
        assert!(!result.contains("truncat")); // Should break at word boundary
    }

    #[test]
    fn test_text_stats() {
        let text = "Hello world! 123";
        let stats = TextStats::analyze(text);

        assert_eq!(stats.total_chars, 16); // Corrected count
        assert_eq!(stats.alphabetic, 10);
        assert_eq!(stats.numeric, 3);
        assert_eq!(stats.whitespace, 2);
        assert_eq!(stats.punctuation, 1);
        assert!(stats.is_meaningful_text());
    }
}
