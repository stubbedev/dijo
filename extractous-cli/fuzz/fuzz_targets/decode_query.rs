#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|query: &str| extractous::fuzz::decode_query(query));
