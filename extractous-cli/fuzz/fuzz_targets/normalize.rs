#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: (u16, u8, &[u8])| {
    let (skipped, chunk, text) = input;
    extractous::fuzz::normalize(skipped, chunk, text);
});
