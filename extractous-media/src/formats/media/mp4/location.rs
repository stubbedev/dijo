//! Tika's `ISO6709`: a location written as ISO 6709 angles, `±DD.D±DDD.D`, `±DDMM.M±DDDMM.M` or
//! `±DDMMSS.S±DDDMMSS.S`, with an optional altitude after them.

use crate::java;

/// A location in decimal degrees, and metres of altitude where it has one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Location {
    pub(super) latitude: f64,
    pub(super) longitude: f64,
    pub(super) altitude: Option<f64>,
}

/// The end of a `[+-]\d+(?:\.\d+)?` token starting at `start`, which the pattern's tokens can
/// only match whole.
fn token_end(text: &[u8], start: usize) -> Option<usize> {
    if !matches!(text.get(start), Some(b'+' | b'-')) {
        return None;
    }
    let digits = |from: usize| {
        text[from..]
            .iter()
            .position(|byte| !byte.is_ascii_digit())
            .map_or(text.len(), |length| from + length)
    };
    let integer_end = digits(start + 1);
    if integer_end == start + 1 {
        return None;
    }
    if text.get(integer_end) == Some(&b'.') {
        let fraction_end = digits(integer_end + 1);
        if fraction_end > integer_end + 1 {
            return Some(fraction_end);
        }
    }
    Some(integer_end)
}

/// `decodeAngle`: an angle of `degree_digits` digits of degrees, then minutes and seconds where
/// the integer part has two or four more digits.
fn angle(token: &str, degree_digits: usize) -> f64 {
    let number = |text: &str| java::parse_double(text).expect("the token holds digits");
    let whole = |text: &str| f64::from(java::parse_int(text).expect("the token holds digits"));
    let body = &token[1..];
    let (integer, fraction) = body.split_at(body.find('.').unwrap_or(body.len()));
    let value = if integer.len() == degree_digits + 2 {
        whole(&integer[..degree_digits])
            + number(&format!("{}{fraction}", &integer[degree_digits..])) / 60.0
    } else if integer.len() == degree_digits + 4 {
        whole(&integer[..degree_digits])
            + whole(&integer[degree_digits..degree_digits + 2]) / 60.0
            + number(&format!("{}{fraction}", &integer[degree_digits + 2..])) / 3600.0
    } else {
        number(body)
    };
    if token.starts_with('-') {
        -value
    } else {
        value
    }
}

/// `ISO6709.parse`: the first latitude and longitude in `text`, and the altitude after them.
pub(super) fn parse(text: &str) -> Option<Location> {
    let bytes = text.as_bytes();
    (0..bytes.len()).find_map(|start| {
        let latitude_end = token_end(bytes, start)?;
        let longitude_end = token_end(bytes, latitude_end)?;
        let altitude = token_end(bytes, longitude_end).map(|end| {
            java::parse_double(&text[longitude_end..end]).expect("the token holds digits")
        });
        Some(Location {
            latitude: angle(&text[start..latitude_end], 2),
            longitude: angle(&text[latitude_end..longitude_end], 3),
            altitude,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_iso_6709_locations_as_tika_does() {
        assert_eq!(
            parse("+12.3456-098.7654+010.500/"),
            Some(Location {
                latitude: 12.3456,
                longitude: -98.7654,
                altitude: Some(10.5),
            })
        );
        let minutes = parse("x+5530.5+01230/").unwrap();
        assert_eq!(minutes.latitude, 55.0 + 30.5 / 60.0);
        assert_eq!(minutes.longitude, 12.0 + 30.0 / 60.0);
        assert_eq!(minutes.altitude, None);
        let seconds = parse("+553015-0123045.5").unwrap();
        assert_eq!(seconds.latitude, 55.0 + 30.0 / 60.0 + 15.0 / 3600.0);
        assert_eq!(seconds.longitude, -(12.0 + 30.0 / 60.0 + 45.5 / 3600.0));
        assert_eq!(parse("+12.+34"), None);
        assert_eq!(parse("++1+2").map(|location| location.longitude), Some(2.0));
    }
}
