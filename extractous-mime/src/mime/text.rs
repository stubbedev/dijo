//! Tika's `TextDetector`: whether the first bytes read as text, by their byte statistics alone.

/// Whether the first `bytes_to_test` bytes of `data` are mostly ASCII or look like UTF-8, as
/// `TextDetector` and `TextStatistics` decide; no bytes are not text.
pub(crate) fn is_text(data: &[u8], bytes_to_test: usize) -> bool {
    let mut counts = [0usize; 256];
    let sample = &data[..data.len().min(bytes_to_test)];
    for &byte in sample {
        counts[usize::from(byte)] += 1;
    }
    let total = sample.len();
    let count = |range: std::ops::Range<usize>| counts[range].iter().sum::<usize>();
    let control = count(0..0x20);
    let safe = [0x09, 0x0A, 0x0D, 0x0C, 0x1B]
        .iter()
        .map(|&b| counts[b])
        .sum::<usize>();
    let mostly_ascii = total > 0
        && (control - safe) * 100 < total * 2
        && (count(0x20..0x80) + safe) * 100 > total * 90;
    if mostly_ascii {
        return true;
    }
    let leading = [count(0xC0..0xE0), count(0xE0..0xF0), count(0xF0..0xF8)];
    let utf8 = count(0x20..0x80) + leading.iter().sum::<usize>();
    let expected_continuation: usize = leading.iter().enumerate().map(|(i, &n)| (i + 1) * n).sum();
    let continuation = count(0x80..0xC0);
    utf8 > 0
        && continuation <= expected_continuation
        && continuation + 3 >= expected_continuation
        && count(0xF8..0x100) == 0
        && (control - safe) * 100 < utf8 * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ascii_and_utf8_as_text() {
        assert!(is_text(b"Hello,\tworld\r\n", 512));
        assert!(is_text("Blåbærgrød på dansk".as_bytes(), 512));
        assert!(!is_text(b"", 512));
    }

    #[test]
    fn rejects_binary() {
        assert!(!is_text(&[0, 1, 2, 3, 4, 5, 6, 7], 512));
        assert!(!is_text(&[0xFF; 20], 512));
        assert!(!is_text(b"caf\xE9 au lait \xE9\xE9\xE9", 512));
    }

    #[test]
    fn tests_only_the_first_bytes() {
        let mut data = b"plain text ".repeat(50);
        data.extend([0u8; 600]);
        assert!(is_text(&data, 512));
        assert!(!is_text(&data, 2048));
    }
}
