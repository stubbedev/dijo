#![doc = include_str!("../../README.md")]
// errors module
mod errors;
pub use errors::*;

// config module
mod config;
pub use config::*;

// embedded module for embedded document extraction
mod embedded;
pub use embedded::*;

// extractor module is the main public api interface
mod extractor;
pub use extractor::*;

// tika module, not exposed outside this crate
mod tika {
    mod jni_utils;
    mod native;
    mod parse;
    mod wrappers;
    pub(crate) use parse::*;
    pub(crate) use wrappers::JInputStream;
}
