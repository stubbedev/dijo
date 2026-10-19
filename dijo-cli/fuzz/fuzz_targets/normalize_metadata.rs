#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|fields: Vec<(String, Vec<String>)>| {
    dijo::fuzz::normalize_metadata(&fields.into_iter().collect());
});
