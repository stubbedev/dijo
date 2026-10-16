//! How Java writes numbers, in the `en_US` locale the native image formats in: the digits of
//! `Double.toString` and `Float.toString`, `java.text.DecimalFormat`, and the conversions of
//! `java.util.Formatter` (`String.format`).

pub(crate) mod bigint;
pub(crate) mod decimal;
pub(crate) mod decimal_format;
pub(crate) mod dtoa;
pub(crate) mod printf;
