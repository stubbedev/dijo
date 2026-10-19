#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|xhtml: &[u8]| dijo::fuzz::split_xhtml(xhtml));
