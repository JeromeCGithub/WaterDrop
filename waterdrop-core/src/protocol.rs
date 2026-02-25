use anyhow::{Result, bail, ensure};
use bytes::{Buf, BufMut, BytesMut};

/// ASCII magic bytes that open every WaterDrop frame.
const MAGIC: &[u8; 5] = b"WDROP";
/// Protocol version understood by this build.
const VERSION: u8 = 0x01;
/// Total header size: magic(5) + version(1) + type(1) + flags(2) + length(4).
const HEADER_LEN: usize = 13;
/// Upper bound on a single frame payload to protect against malicious peers.
const MAX_PAYLOAD_LEN: usize = 64 * 1024;

const OFF_MAGIC: usize = 0;
const OFF_VERSION: usize = 5;
const OFF_TYPE: usize = 6;
const OFF_FLAGS: usize = 7;
const OFF_LENGTH: usize = 9;

/// Protocol-level message type codes (v1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Hello = 0x01,
    HelloAck = 0x02,
    PairWithCode = 0x10,
    PairWithPassword = 0x11,
    PairResult = 0x12,
    TransferOffer = 0x20,
    TransferDecision = 0x21,
    TransferDone = 0x30,
    Error = 0x7F,
}

impl TryFrom<u8> for MessageType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0x01 => Ok(Self::Hello),
            0x02 => Ok(Self::HelloAck),
            0x10 => Ok(Self::PairWithCode),
            0x11 => Ok(Self::PairWithPassword),
            0x12 => Ok(Self::PairResult),
            0x20 => Ok(Self::TransferOffer),
            0x21 => Ok(Self::TransferDecision),
            0x30 => Ok(Self::TransferDone),
            0x7F => Ok(Self::Error),
            other => bail!("unknown message type: 0x{other:02X}"),
        }
    }
}

impl From<MessageType> for u8 {
    fn from(mt: MessageType) -> u8 {
        mt as u8
    }
}

/// Decoded frame header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub version: u8,
    pub msg_type: MessageType,
    /// Reserved flags — MUST be `0x0000` in v1.
    pub flags: u16,
    pub payload_length: u32,
}

/// A fully decoded control frame (header + payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub header: Header,
    pub payload: Vec<u8>,
}

/// Attempts to decode one complete frame from the front of `buf`.
///
/// * `Ok(Some(frame))` — a full frame was present; its bytes have been consumed
///   from `buf`.
/// * `Ok(None)` — not enough bytes yet; `buf` is left untouched.  The caller
///   should read more data and try again.
/// * `Err(..)` — protocol violation (bad magic, unsupported version, unknown
///   message type, oversized payload).  The caller should close the connection.
///
/// # Errors
///
/// Returns an error on protocol violations: bad magic, unsupported version,
/// unknown message type, or payload exceeding [`MAX_PAYLOAD_LEN`].
///
/// # Panics
///
/// Cannot panic. The `expect` calls on slice conversions are guarded by the
/// `HEADER_LEN` check at the top of the function.
pub fn try_decode_frame(buf: &mut BytesMut) -> Result<Option<Frame>> {
    if buf.len() < HEADER_LEN {
        return Ok(None);
    }

    ensure!(
        &buf[OFF_MAGIC..OFF_MAGIC + MAGIC.len()] == MAGIC,
        "bad magic: expected WDROP"
    );

    let version = buf[OFF_VERSION];
    ensure!(version == VERSION, "unsupported version: 0x{version:02X}");

    let msg_type = MessageType::try_from(buf[OFF_TYPE])?;

    // These slices are exactly 2 and 4 bytes respectively (guaranteed by
    // the HEADER_LEN check above), so the conversions cannot fail.
    let flags = u16::from_be_bytes(
        buf[OFF_FLAGS..OFF_FLAGS + 2]
            .try_into()
            .expect("flags slice is exactly 2 bytes"),
    );

    let payload_len = u32::from_be_bytes(
        buf[OFF_LENGTH..OFF_LENGTH + 4]
            .try_into()
            .expect("length slice is exactly 4 bytes"),
    ) as usize;

    ensure!(
        payload_len <= MAX_PAYLOAD_LEN,
        "payload too large: {payload_len} bytes (max {MAX_PAYLOAD_LEN})"
    );

    if buf.len() < HEADER_LEN + payload_len {
        return Ok(None);
    }

    buf.advance(HEADER_LEN);
    let payload = buf.split_to(payload_len).to_vec();

    let header = Header {
        version,
        msg_type,
        flags,
        #[allow(clippy::cast_possible_truncation)] // guarded by MAX_PAYLOAD_LEN (fits in u32)
        payload_length: payload_len as u32,
    };

    Ok(Some(Frame { header, payload }))
}

/// Encodes a frame into `buf`.
///
/// Appends the 13-byte header followed by `payload` to the buffer.
pub fn encode_frame(msg_type: MessageType, payload: &[u8], buf: &mut BytesMut) {
    buf.reserve(HEADER_LEN + payload.len());
    buf.put_slice(MAGIC);
    buf.put_u8(VERSION);
    buf.put_u8(msg_type.into());
    buf.put_u16(0x0000);
    #[allow(clippy::cast_possible_truncation)] // frame payloads are bounded by MAX_PAYLOAD_LEN
    buf.put_u32(payload.len() as u32);
    buf.put_slice(payload);
}

/// Convenience wrapper that allocates and returns a new `BytesMut`.
#[must_use]
pub fn encode_frame_to_bytes(msg_type: MessageType, payload: &[u8]) -> BytesMut {
    let mut buf = BytesMut::with_capacity(HEADER_LEN + payload.len());
    encode_frame(msg_type, payload, &mut buf);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Given a frame with an empty payload, when encoded and decoded, then the header and payload match.
    #[test]
    fn given_empty_payload_when_round_tripped_then_frame_matches() {
        let encoded = encode_frame_to_bytes(MessageType::Hello, &[]);
        let mut buf = encoded;
        let frame = try_decode_frame(&mut buf).unwrap().unwrap();
        assert_eq!(frame.header.msg_type, MessageType::Hello);
        assert_eq!(frame.header.version, VERSION);
        assert_eq!(frame.header.flags, 0);
        assert_eq!(frame.header.payload_length, 0);
        assert!(frame.payload.is_empty());
        assert!(buf.is_empty());
    }

    /// Given a frame with a JSON payload, when encoded and decoded, then the header and payload match.
    #[test]
    fn given_json_payload_when_round_tripped_then_frame_matches() {
        let json = br#"{"device_id":"abc"}"#;
        let encoded = encode_frame_to_bytes(MessageType::TransferOffer, json);
        let mut buf = encoded;
        let frame = try_decode_frame(&mut buf).unwrap().unwrap();
        assert_eq!(frame.header.msg_type, MessageType::TransferOffer);
        assert_eq!(frame.header.payload_length, json.len() as u32);
        assert_eq!(frame.payload, json);
        assert!(buf.is_empty());
    }

    /// Given a buffer with only a partial header, when decoding, then None is returned and the buffer is untouched.
    #[test]
    fn given_partial_header_when_decoded_then_returns_none() {
        let full = encode_frame_to_bytes(MessageType::Hello, b"{}");
        let mut buf = BytesMut::from(&full[..7]); // only 7 of 13 header bytes
        assert!(try_decode_frame(&mut buf).unwrap().is_none());
    }

    /// Given a complete header but truncated payload, when decoding, then None is returned and the buffer is untouched.
    #[test]
    fn given_truncated_payload_when_decoded_then_returns_none() {
        let payload = b"hello world";
        let full = encode_frame_to_bytes(MessageType::HelloAck, payload);
        // Give the full header + half the payload.
        let partial_len = HEADER_LEN + payload.len() / 2;
        let mut buf = BytesMut::from(&full[..partial_len]);
        assert!(try_decode_frame(&mut buf).unwrap().is_none());
    }

    /// Given a frame with invalid magic bytes, when decoded, then an error is returned.
    #[test]
    fn given_bad_magic_when_decoded_then_returns_error() {
        let mut buf = BytesMut::from(&b"XXXXX\x01\x01\x00\x00\x00\x00\x00\x00"[..]);
        let err = try_decode_frame(&mut buf).unwrap_err();
        assert!(err.to_string().contains("bad magic"));
    }

    /// Given a frame with an unsupported version, when decoded, then an error is returned.
    #[test]
    fn given_unsupported_version_when_decoded_then_returns_error() {
        let mut buf = BytesMut::from(&b"WDROP\xFF\x01\x00\x00\x00\x00\x00\x00"[..]);
        let err = try_decode_frame(&mut buf).unwrap_err();
        assert!(err.to_string().contains("unsupported version"));
    }

    /// Given a frame with an unknown message type, when decoded, then an error is returned.
    #[test]
    fn given_unknown_message_type_when_decoded_then_returns_error() {
        let mut buf = BytesMut::from(&b"WDROP\x01\xFE\x00\x00\x00\x00\x00\x00"[..]);
        let err = try_decode_frame(&mut buf).unwrap_err();
        assert!(err.to_string().contains("unknown message type"));
    }

    /// Given every defined message type code, when converted to u8 and back, then the original variant is preserved.
    #[test]
    fn given_all_message_types_when_converted_to_u8_and_back_then_match() {
        let types = [
            (0x01, MessageType::Hello),
            (0x02, MessageType::HelloAck),
            (0x10, MessageType::PairWithCode),
            (0x11, MessageType::PairWithPassword),
            (0x12, MessageType::PairResult),
            (0x20, MessageType::TransferOffer),
            (0x21, MessageType::TransferDecision),
            (0x30, MessageType::TransferDone),
            (0x7F, MessageType::Error),
        ];
        for (code, expected) in types {
            let parsed = MessageType::try_from(code).unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(u8::from(parsed), code);
        }
    }
}
