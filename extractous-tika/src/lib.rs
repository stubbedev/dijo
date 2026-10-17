//! extractous's bridge to Apache Tika, compiled to native code with GraalVM and run through JNI,
//! for what no native parser reads yet, with the `tika` feature; and, with or without it, the
//! process's settings, which Tika's isolate is set up with.

use extractous_base::*;
#[cfg(feature = "tika")]
use extractous_native as bundle;

pub mod isolate;

#[cfg(feature = "tika")]
pub mod tika {
    mod jni_utils;
    pub mod native;
    mod parse;
    mod wrappers;
    pub use extractous_base::tika::{EmbeddedEvents, EmbeddedOutcome};
    pub use parse::*;
    pub use wrappers::JInputStream;
}

#[cfg(feature = "tika")]
pub mod unpack;
