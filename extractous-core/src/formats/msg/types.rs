//! POI's `Types`: the MAPI property types, by id, and the length of each built-in one. A type
//! POI does not know is a custom one of no fixed length.

use super::property_table::KNOWN;

/// A MAPI property type, `Types.MAPIType`, which POI keeps one instance of per id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct Type(pub(super) i32);

/// The flag of a multi-valued type.
pub(super) const MULTIVALUED_FLAG: i32 = 0x1000;

impl Type {
    pub(super) const UNSPECIFIED: Type = Type(0x0000);
    pub(super) const UNKNOWN: Type = Type(-1);
    pub(super) const NULL: Type = Type(0x0001);
    pub(super) const SHORT: Type = Type(0x0002);
    pub(super) const LONG: Type = Type(0x0003);
    pub(super) const FLOAT: Type = Type(0x0004);
    pub(super) const DOUBLE: Type = Type(0x0005);
    pub(super) const CURRENCY: Type = Type(0x0006);
    pub(super) const APP_TIME: Type = Type(0x0007);
    pub(super) const BOOLEAN: Type = Type(0x000B);
    pub(super) const DIRECTORY: Type = Type(0x000D);
    pub(super) const LONG_LONG: Type = Type(0x0014);
    pub(super) const TIME: Type = Type(0x0040);
    pub(super) const CLS_ID: Type = Type(0x0048);
    pub(super) const BINARY: Type = Type(0x0102);
    pub(super) const ASCII_STRING: Type = Type(0x001E);
    pub(super) const UNICODE_STRING: Type = Type(0x001F);
    pub(super) const MV_LONG: Type = Type(0x1003);
    pub(super) const MV_APP_TIME: Type = Type(0x1007);
    pub(super) const MV_TIME: Type = Type(0x1040);
    pub(super) const MV_BINARY: Type = Type(0x1102);
    pub(super) const MV_ASCII_STRING: Type = Type(0x101E);

    /// The built-in types and their lengths, -1 for those of variable length.
    const BUILT_IN: [(i32, i32); 30] = [
        (0x0000, -1),
        (-1, -1),
        (0x0001, 0),
        (0x0002, 2),
        (0x0003, 4),
        (0x0004, 4),
        (0x0005, 8),
        (0x0006, 8),
        (0x0007, 8),
        (0x000A, 4),
        (0x000B, 2),
        (0x000D, -1),
        (0x0014, 8),
        (0x0040, 8),
        (0x0048, 16),
        (0x0102, -1),
        (0x001E, -1),
        (0x001F, -1),
        (0x1002, -1),
        (0x1003, -1),
        (0x1004, -1),
        (0x1005, -1),
        (0x1006, -1),
        (0x1007, -1),
        (0x1014, -1),
        (0x1040, -1),
        (0x1048, -1),
        (0x1102, -1),
        (0x101E, -1),
        (0x101F, -1),
    ];

    /// `Types.getById`: the built-in type of an id.
    pub(super) fn built_in(id: i32) -> Option<Type> {
        Self::BUILT_IN
            .iter()
            .any(|&(known, _)| known == id)
            .then_some(Type(id))
    }

    /// `getLength`: a custom type's is -1.
    pub(super) fn length(self) -> i32 {
        Self::BUILT_IN
            .iter()
            .find(|&&(known, _)| known == self.0)
            .map_or(-1, |&(_, length)| length)
    }

    /// `isPointer`.
    pub(super) fn is_pointer(self) -> bool {
        let length = self.length();
        length == -1 || length > 8
    }
}

/// `MAPIProperty.get(id).usualType`: the usual type of a property POI knows, `None` for one it
/// does not.
pub(super) fn usual_type(id: i32) -> Option<Type> {
    let id = u16::try_from(id).ok()?;
    KNOWN
        .binary_search_by_key(&id, |&(known, _)| known)
        .ok()
        .map(|index| KNOWN[index].1)
}

/// The properties Tika's path asks for by name, by id.
pub(super) mod property {
    pub(in crate::formats::msg) const MESSAGE_CLASS: i32 = 0x001A;
    pub(in crate::formats::msg) const SUBJECT: i32 = 0x0037;
    pub(in crate::formats::msg) const CLIENT_SUBMIT_TIME: i32 = 0x0039;
    pub(in crate::formats::msg) const SENT_REPRESENTING_NAME: i32 = 0x0042;
    pub(in crate::formats::msg) const MESSAGE_SUBMISSION_ID: i32 = 0x0047;
    pub(in crate::formats::msg) const IN_REPLY_TO_ID: i32 = 0x1042;
    pub(in crate::formats::msg) const CONVERSATION_TOPIC: i32 = 0x0070;
    pub(in crate::formats::msg) const CONVERSATION_INDEX: i32 = 0x0071;
    pub(in crate::formats::msg) const RECEIVED_BY_ADDRTYPE: i32 = 0x0075;
    pub(in crate::formats::msg) const TRANSPORT_MESSAGE_HEADERS: i32 = 0x007D;
    pub(in crate::formats::msg) const SENDER_NAME: i32 = 0x0C1A;
    pub(in crate::formats::msg) const SENDER_EMAIL_ADDRESS: i32 = 0x0C1F;
    pub(in crate::formats::msg) const RECIPIENT_TYPE: i32 = 0x0C15;
    pub(in crate::formats::msg) const DELIVER_TIME: i32 = 0x0010;
    pub(in crate::formats::msg) const DISPLAY_BCC: i32 = 0x0E02;
    pub(in crate::formats::msg) const DISPLAY_CC: i32 = 0x0E03;
    pub(in crate::formats::msg) const DISPLAY_TO: i32 = 0x0E04;
    pub(in crate::formats::msg) const MESSAGE_DELIVERY_TIME: i32 = 0x0E06;
    pub(in crate::formats::msg) const BODY: i32 = 0x1000;
    pub(in crate::formats::msg) const RTF_COMPRESSED: i32 = 0x1009;
    pub(in crate::formats::msg) const BODY_HTML: i32 = 0x1013;
    pub(in crate::formats::msg) const INTERNET_MESSAGE_ID: i32 = 0x1035;
    pub(in crate::formats::msg) const INTERNET_REFERENCES: i32 = 0x1039;
    pub(in crate::formats::msg) const DISPLAY_NAME: i32 = 0x3001;
    pub(in crate::formats::msg) const ADDRTYPE: i32 = 0x3002;
    pub(in crate::formats::msg) const EMAIL_ADDRESS: i32 = 0x3003;
    pub(in crate::formats::msg) const CREATION_TIME: i32 = 0x3007;
    pub(in crate::formats::msg) const LAST_MODIFICATION_TIME: i32 = 0x3008;
    pub(in crate::formats::msg) const SEARCH_KEY: i32 = 0x300B;
    pub(in crate::formats::msg) const ATTACH_DATA: i32 = 0x3701;
    pub(in crate::formats::msg) const ATTACH_EXTENSION: i32 = 0x3703;
    pub(in crate::formats::msg) const ATTACH_FILENAME: i32 = 0x3704;
    pub(in crate::formats::msg) const ATTACH_LONG_FILENAME: i32 = 0x3707;
    pub(in crate::formats::msg) const ATTACH_RENDERING: i32 = 0x3709;
    pub(in crate::formats::msg) const ATTACH_LONG_PATHNAME: i32 = 0x370D;
    pub(in crate::formats::msg) const ATTACH_MIME_TAG: i32 = 0x370E;
    pub(in crate::formats::msg) const ATTACH_CONTENT_ID: i32 = 0x3712;
    pub(in crate::formats::msg) const ATTACH_CONTENT_LOCATION: i32 = 0x3713;
    pub(in crate::formats::msg) const LANGUAGE: i32 = 0x3A0C;
    pub(in crate::formats::msg) const RECIPIENT_DISPLAY_NAME: i32 = 0x5FF6;
    pub(in crate::formats::msg) const SMTP_ADDRESS: i32 = 0x39FE;
    pub(in crate::formats::msg) const SENT_REPRESENTING_EMAIL_ADDRESS: i32 = 0x0065;
    pub(in crate::formats::msg) const RECORD_KEY: i32 = 0x0FF9;
    pub(in crate::formats::msg) const MESSAGE_CODEPAGE: i32 = 0x3FFD;
    pub(in crate::formats::msg) const INTERNET_CPID: i32 = 0x3FDE;
    pub(in crate::formats::msg) const DEFERRED_DELIVERY_TIME: i32 = 0x000F;
    pub(in crate::formats::msg) const EXPIRY_TIME: i32 = 0x0015;
    pub(in crate::formats::msg) const LATEST_DELIVERY_TIME: i32 = 0x0019;
    pub(in crate::formats::msg) const MESSAGE_DOWNLOAD_TIME: i32 = 0x0E18;
    pub(in crate::formats::msg) const ORIGINAL_DELIVERY_TIME: i32 = 0x0055;
    pub(in crate::formats::msg) const ORIGINAL_SUBMIT_TIME: i32 = 0x004E;
    pub(in crate::formats::msg) const PROVIDER_SUBMIT_TIME: i32 = 0x0048;
    pub(in crate::formats::msg) const RECEIPT_TIME: i32 = 0x002A;
    pub(in crate::formats::msg) const REPLY_TIME: i32 = 0x0030;
    pub(in crate::formats::msg) const REPORT_TIME: i32 = 0x0032;
}
