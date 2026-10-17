//! Tika's `MpegStream` over the input as `Mp3Parser` reads it: a pushback stream in which ID3v2
//! tags are looked for at the start and MPEG audio frames are then found by their sync byte and
//! header, and skipped by the length their header gives.

/// `PushbackInputStream` over the input: bytes pushed back are read again first, the first of
/// a pushed array first. A byte Java pushes back for the end of the stream, `-1`, comes back as
/// `0xFF`.
pub(super) struct Stream<'a> {
    data: &'a [u8],
    position: usize,
    pushed: Vec<u8>,
}

impl<'a> Stream<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            pushed: Vec::new(),
        }
    }

    /// `read()`: the next byte, or `None` at the end.
    pub(super) fn read(&mut self) -> Option<u8> {
        if let Some(byte) = self.pushed.pop() {
            return Some(byte);
        }
        let byte = self.data.get(self.position).copied();
        if byte.is_some() {
            self.position += 1;
        }
        byte
    }

    /// `unread(bytes)`.
    pub(super) fn unread(&mut self, bytes: &[u8]) {
        self.pushed.extend(bytes.iter().rev());
    }

    /// Up to `count` bytes read, as many as there are.
    pub(super) fn read_up_to(&mut self, count: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(count);
        while bytes.len() < count {
            match self.read() {
                Some(byte) => bytes.push(byte),
                None => break,
            }
        }
        bytes
    }

    /// `IOUtils.skip`: up to `count` bytes skipped; how many.
    fn skip(&mut self, count: usize) -> usize {
        let from_pushed = count.min(self.pushed.len());
        self.pushed.truncate(self.pushed.len() - from_pushed);
        let from_data = (count - from_pushed).min(self.data.len() - self.position);
        self.position += from_data;
        from_pushed + from_data
    }
}

const MPEG_V1: u32 = 3;
const MPEG_V2: u32 = 2;
const MPEG_V2_5: u32 = 0;
const LAYER_1: u32 = 3;
const LAYER_2: u32 = 2;
const LAYER_3: u32 = 1;
const HEADER_SIZE: usize = 4;

const BIT_RATE_MPEG1_L1: [i32; 15] = [
    0, 32000, 64000, 96000, 128000, 160000, 192000, 224000, 256000, 288000, 320000, 352000, 384000,
    416000, 448000,
];
const BIT_RATE_MPEG1_L2: [i32; 15] = [
    0, 32000, 48000, 56000, 64000, 80000, 96000, 112000, 128000, 160000, 192000, 224000, 256000,
    320000, 384000,
];
const BIT_RATE_MPEG1_L3: [i32; 15] = [
    0, 32000, 40000, 48000, 56000, 64000, 80000, 96000, 112000, 128000, 160000, 192000, 224000,
    256000, 320000,
];
const BIT_RATE_MPEG2_L1: [i32; 15] = [
    0, 32000, 48000, 56000, 64000, 80000, 96000, 112000, 128000, 144000, 160000, 176000, 192000,
    224000, 256000,
];
const BIT_RATE_MPEG2_L2: [i32; 15] = [
    0, 8000, 16000, 24000, 32000, 40000, 48000, 56000, 64000, 80000, 96000, 112000, 128000, 144000,
    160000,
];

/// An MPEG audio frame's header, `AudioFrame`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct AudioFrame {
    version: u32,
    layer: u32,
    pub(super) bit_rate: i32,
    pub(super) sample_rate: i32,
    pub(super) channels: i32,
    length: i32,
    /// Milliseconds, as a `float`.
    pub(super) duration: f32,
}

impl AudioFrame {
    /// `generateVersionStr`.
    pub(super) fn version(&self) -> String {
        let layer = match self.layer {
            LAYER_3 => "III",
            LAYER_2 => "II",
            LAYER_1 => "I",
            _ => "(reserved)",
        };
        let version = match self.version {
            MPEG_V2_5 => "2.5",
            MPEG_V2 => "2",
            MPEG_V1 => "1",
            _ => "(reserved)",
        };
        format!("MPEG 3 Layer {layer} Version {version}")
    }

    /// `createHeader`: the frame the three bytes after a sync byte describe, if they do.
    fn from_header(bytes: [u8; 3]) -> Option<Self> {
        let value = u32::from(bytes[0]) << 16 | u32::from(bytes[1]) << 8 | u32::from(bytes[2]);
        let bits = |from: u32, to: u32| (value >> from) & ((1 << (to - from + 1)) - 1);
        if bits(21, 23) != 7 {
            return None;
        }
        let version = bits(19, 20);
        let layer = bits(17, 18);
        let bit_rate_code = bits(12, 15) as usize;
        let sample_rate_code = bits(10, 11) as usize;
        let padding = bits(9, 9) as i32;
        if version == 1 || layer == 0 || bit_rate_code == 0 || bit_rate_code == 15 {
            return None;
        }
        if sample_rate_code == 3 {
            return None;
        }
        let bit_rates = match (version, layer) {
            (MPEG_V1, LAYER_1) => &BIT_RATE_MPEG1_L1,
            (MPEG_V1, LAYER_2) => &BIT_RATE_MPEG1_L2,
            (MPEG_V1, _) => &BIT_RATE_MPEG1_L3,
            (_, LAYER_1) => &BIT_RATE_MPEG2_L1,
            _ => &BIT_RATE_MPEG2_L2,
        };
        let bit_rate = bit_rates[bit_rate_code];
        let sample_rates = match version {
            MPEG_V1 => [44100, 48000, 32000],
            MPEG_V2 => [22050, 24000, 16000],
            _ => [11025, 12000, 8000],
        };
        let sample_rate = sample_rates[sample_rate_code];
        let low_sampling = layer == LAYER_3 && version != MPEG_V1;
        let (length, samples) = if layer == LAYER_1 {
            ((12 * bit_rate / sample_rate + padding) * 4, 384)
        } else if low_sampling {
            (72 * bit_rate / sample_rate + padding, 576)
        } else {
            (144 * bit_rate / sample_rate + padding, 1152)
        };
        Some(Self {
            version,
            layer,
            bit_rate,
            sample_rate,
            channels: if bits(6, 7) < 3 { 2 } else { 1 },
            length,
            duration: (1000.0f32 / sample_rate as f32) * samples as f32,
        })
    }
}

/// The frame scan over a stream: the end of the stream once a read finds it, and the frame
/// whose header was read last.
pub(super) struct Frames<'s, 'a> {
    stream: &'s mut Stream<'a>,
    end_of_stream: bool,
    current: Option<AudioFrame>,
}

impl<'s, 'a> Frames<'s, 'a> {
    pub(super) fn new(stream: &'s mut Stream<'a>) -> Self {
        Self {
            stream,
            end_of_stream: false,
            current: None,
        }
    }

    /// `nextByte`: the next byte, or 0 once the stream has ended.
    fn next_byte(&mut self) -> u8 {
        if self.end_of_stream {
            return 0;
        }
        match self.stream.read() {
            Some(byte) => byte,
            None => {
                self.end_of_stream = true;
                0
            }
        }
    }

    /// `nextFrame`: the next frame whose header is valid, past a sync byte.
    pub(super) fn next_frame(&mut self) -> Option<AudioFrame> {
        let mut frame = None;
        while !self.end_of_stream && frame.is_none() {
            while !self.end_of_stream && self.next_byte() != 0xFF {}
            if self.end_of_stream {
                break;
            }
            let header = [self.next_byte(), self.next_byte(), self.next_byte()];
            if self.end_of_stream {
                break;
            }
            frame = AudioFrame::from_header(header);
            if frame.is_none() {
                self.stream.unread(&header);
            }
        }
        self.current = frame.clone();
        frame
    }

    /// `skipFrame`: whether the rest of the current frame was there to skip.
    pub(super) fn skip_frame(&mut self) -> bool {
        let Some(frame) = self.current.take() else {
            return false;
        };
        let to_skip = usize::try_from(frame.length)
            .expect("a frame header gives a positive length")
            - HEADER_SIZE;
        self.stream.skip(to_skip) == to_skip
    }

    /// `isMetadataFrame`: whether the 40 bytes after the header name a Xing, Info or VBRI
    /// header, read and pushed back.
    pub(super) fn is_metadata_frame(&mut self) -> bool {
        let payload = self.stream.read_up_to(40);
        self.stream.unread(&payload);
        payload
            .windows(4)
            .any(|window| matches!(window, b"Xing" | b"Info" | b"VBRI"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_frame_headers_as_tika_does() {
        let frame = AudioFrame::from_header([0xFB, 0x90, 0x64]).unwrap();
        assert_eq!(frame.version(), "MPEG 3 Layer III Version 1");
        assert_eq!(frame.bit_rate, 128_000);
        assert_eq!(frame.sample_rate, 44_100);
        assert_eq!(frame.channels, 2);
        assert_eq!(frame.length, 417);
        assert!(AudioFrame::from_header([0xEB, 0x90, 0x64]).is_none());
    }

    #[test]
    fn pushes_bytes_back_in_order() {
        let mut stream = Stream::new(b"abc");
        let read = stream.read_up_to(2);
        stream.unread(&read);
        assert_eq!(stream.read_up_to(5), b"abc");
        assert_eq!(stream.skip(3), 0);
    }
}
