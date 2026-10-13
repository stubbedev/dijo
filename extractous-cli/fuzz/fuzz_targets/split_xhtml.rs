#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|xhtml: &[u8]| extractous::fuzz::split_xhtml(xhtml));
