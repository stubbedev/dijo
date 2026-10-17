//! What Tika's path gives back of a document embedded in one a native parser reads: the events
//! of its body, which the native parser writes into the document embedding it, and how its parse
//! ended.

mod events;

use crate::errors::Error;
use crate::Metadata;
pub use events::{decode as decode_events, Event};

/// What Tika made of a document embedded in one a native parser reads: the SAX events of its
/// body as `ai.yobix.EventRecorder` encodes them, and how its parse ended.
pub struct EmbeddedEvents {
    pub events: Vec<u8>,
    pub outcome: EmbeddedOutcome,
}

/// How the parse of an embedded document ended.
pub enum EmbeddedOutcome {
    /// It ended, with the document's metadata and, if its parser failed in a way Tika records
    /// in the embedding document's metadata, that failure as Tika formats it there.
    Parsed {
        metadata: Metadata,
        recorded_exception: Option<String>,
    },
    /// It failed in a way that fails the embedding document too, after writing the events.
    Failed(Error),
}
