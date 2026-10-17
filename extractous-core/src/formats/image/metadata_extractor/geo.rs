//! metadata-extractor's `GeoLocation` and `GpsDirectory.getGeoLocation`.

use super::descriptions::decimal;
use super::exif::tags::{GPS_LATITUDE, GPS_LATITUDE_REF, GPS_LONGITUDE, GPS_LONGITUDE_REF};
use super::{Directory, Rational};
use crate::java::equals_ignore_case;

/// A latitude and longitude in decimal degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Location {
    pub(crate) latitude: f64,
    pub(crate) longitude: f64,
}

/// `GeoLocation.degreesMinutesSecondsToDecimal`.
pub(crate) fn to_decimal(
    degrees: Rational,
    minutes: Rational,
    seconds: Rational,
    negative: bool,
) -> Option<f64> {
    let decimal = degrees.double_value().abs()
        + minutes.double_value() / 60.0
        + seconds.double_value() / 3600.0;
    if decimal.is_nan() {
        return None;
    }
    Some(if negative { -decimal } else { decimal })
}

/// `GpsDirectory.getGeoLocation`: none unless both coordinates have three values and a
/// reference.
pub(crate) fn location(directory: &Directory) -> Option<Location> {
    let latitudes = directory.rationals(GPS_LATITUDE)?;
    let longitudes = directory.rationals(GPS_LONGITUDE)?;
    let ([lat_d, lat_m, lat_s], [lon_d, lon_m, lon_s]) = (latitudes, longitudes) else {
        return None;
    };
    let latitude_ref = directory.string(GPS_LATITUDE_REF)?;
    let longitude_ref = directory.string(GPS_LONGITUDE_REF)?;
    Some(Location {
        latitude: to_decimal(
            *lat_d,
            *lat_m,
            *lat_s,
            equals_ignore_case(&latitude_ref, "S"),
        )?,
        longitude: to_decimal(
            *lon_d,
            *lon_m,
            *lon_s,
            equals_ignore_case(&longitude_ref, "W"),
        )?,
    })
}

/// `GeoLocation.decimalToDegreesMinutesSecondsString`.
pub(crate) fn degrees_minutes_seconds(decimal_degrees: f64) -> String {
    let degrees = decimal_degrees as i32;
    let minutes = ((decimal_degrees % 1.0) * 60.0).abs();
    let seconds = (minutes % 1.0) * 60.0;
    format!(
        "{}° {}' {}\"",
        decimal("0.##", f64::from(degrees)),
        decimal("0.##", f64::from(minutes as i32)),
        decimal("0.##", seconds)
    )
}
