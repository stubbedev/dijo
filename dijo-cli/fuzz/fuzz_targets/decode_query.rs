#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|query: &str| dijo::fuzz::decode_query(query));
