//! A port of Tika's `AdaptiveProbe`: how much of the input the detectors look at. Reading goes
//! on in steps of the content target until the text left once markup is stripped reaches it, the
//! input ends, or the raw cap is read, so that a page heavy with markup is still judged on
//! enough of its text.

use super::strip::{strip_tags_resumable, Cursor};

/// The text to judge a charset on.
pub(super) const CONTENT_TARGET: usize = 16 * 1024;
/// The most raw input a probe reads.
pub(super) const RAW_CAP: usize = 512 * 1024;

/// The probe of `input`: its first bytes, as many as Tika's probe reads.
pub(super) fn probe(input: &[u8], content_target: usize, raw_cap: usize) -> &[u8] {
    let mut cursor = Cursor::new();
    let mut stripped = Vec::new();
    let mut total = 0;
    while total < raw_cap {
        let want = (raw_cap - total).min(content_target);
        let read = want.min(input.len() - total);
        total += read;
        let end_of_input = read < want;
        strip_tags_resumable(input, total, &mut stripped, &mut cursor, end_of_input);
        let content = if cursor.tags() > 0 {
            cursor.content_length()
        } else {
            total
        };
        if content >= content_target || end_of_input {
            break;
        }
    }
    &input[..total]
}
