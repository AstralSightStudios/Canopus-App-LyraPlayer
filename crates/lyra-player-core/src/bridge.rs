use alloc::{
    borrow::ToOwned,
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

pub const PROTOCOL_VERSION: u8 = 4;
pub const MAX_CHUNK_SIZE: usize = 2048;
pub const ACK_WINDOW: u8 = 1;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_RESPONSE_CHUNKS: usize = MAX_RESPONSE_BYTES.div_ceil(MAX_CHUNK_SIZE);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchOptions {
    pub method: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub raw: bool,
    pub stream: bool,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            method: "GET",
            headers: Vec::new(),
            body: None,
            raw: false,
            stream: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BridgeEvent {
    HandshakeComplete,
    Response {
        id: String,
        status: i32,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    },
    StreamOpened {
        id: String,
        content_length: Option<u64>,
    },
    StreamChunk {
        id: String,
        bytes: Vec<u8>,
    },
    StreamEnded {
        id: String,
        total_bytes: u64,
        bytes: Vec<u8>,
    },
    Failed {
        id: String,
        message: String,
    },
    Ignored,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngestResult {
    pub event: BridgeEvent,
    pub replies: Vec<String>,
}

#[derive(Clone, Debug)]
struct ChunkedResponse {
    status: i32,
    headers: BTreeMap<String, String>,
    encoding: String,
    compression: String,
    total_bytes: usize,
    chunks: Vec<Option<Vec<u8>>>,
    ack: bool,
    frontier: usize,
}

#[derive(Clone, Debug)]
struct StreamResponse {
    encoding: String,
    next: u64,
    frames: BTreeMap<u64, StreamFrame>,
    received_bytes: u64,
}

#[derive(Clone, Debug)]
struct StreamFrame {
    bytes: Vec<u8>,
    final_frame: bool,
    total_bytes: Option<u64>,
}

#[derive(Default)]
pub struct FetchBridge {
    next_id: u32,
    chunked: BTreeMap<String, ChunkedResponse>,
    streams: BTreeMap<String, StreamResponse>,
}

impl FetchBridge {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            chunked: BTreeMap::new(),
            streams: BTreeMap::new(),
        }
    }

    pub fn handshake(&self, count: u8) -> String {
        json!({
            "tag": "__hs__",
            "count": count.min(2),
            "caps": {
                "version": PROTOCOL_VERSION,
                "stream": true,
                "chunk": true,
                "maxChunkSize": MAX_CHUNK_SIZE,
                "encodings": ["base64", "hex", "text"],
                "compressions": ["none"],
                "ack": true,
                "ackWindow": ACK_WINDOW,
            }
        })
        .to_string()
    }

    pub fn fetch(&mut self, url: &str, options: &FetchOptions) -> (String, String) {
        let id = format!("lyra-{}", self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let headers: BTreeMap<&str, &str> = options
            .headers
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let message = json!({
            "tag": "fetch",
            "id": id,
            "url": url,
            "options": {
                "method": options.method,
                "headers": headers,
                "body": options.body,
                "raw": options.raw,
                "stream": options.stream,
            }
        })
        .to_string();
        (id, message)
    }

    pub fn cancel_stream(&mut self, id: &str, reason: &str) -> String {
        self.streams.remove(id);
        json!({"tag":"fetch-stream-cancel", "id":id, "reason":reason}).to_string()
    }

    pub fn ingest(&mut self, text: &str) -> Result<IngestResult, BridgeError> {
        let message: Value = serde_json::from_str(text).map_err(|_| BridgeError::Json)?;
        let tag = message
            .get("tag")
            .and_then(Value::as_str)
            .ok_or(BridgeError::Protocol)?;
        match tag {
            "__hs__" => self.ingest_handshake(&message),
            "fetch" => self.ingest_header(&message),
            "fetch-chunk" => self.ingest_chunk(&message),
            "fetch-stream" => self.ingest_stream(&message),
            "fetch-stream-error" => {
                let id = string_field(&message, "id")?;
                self.streams.remove(id);
                Ok(result(BridgeEvent::Failed {
                    id: id.into(),
                    message: message
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("stream failed")
                        .into(),
                }))
            }
            _ => Ok(result(BridgeEvent::Ignored)),
        }
    }

    fn ingest_handshake(&self, message: &Value) -> Result<IngestResult, BridgeError> {
        let count = message.get("count").and_then(Value::as_u64).unwrap_or(0) as u8;
        let mut output = result(BridgeEvent::HandshakeComplete);
        if count < 2 {
            output.replies.push(self.handshake(count + 1));
        }
        Ok(output)
    }

    fn ingest_header(&mut self, message: &Value) -> Result<IngestResult, BridgeError> {
        let id = string_field(message, "id")?.to_owned();
        let response = message.get("resp").ok_or(BridgeError::Protocol)?;
        let ok = response.get("ok").and_then(Value::as_bool).unwrap_or(false);
        let status = response.get("status").and_then(Value::as_i64).unwrap_or(0) as i32;
        if !ok {
            return Ok(result(BridgeEvent::Failed {
                id,
                message: response
                    .get("statusText")
                    .and_then(Value::as_str)
                    .unwrap_or("fetch failed")
                    .into(),
            }));
        }
        if response.get("stream").and_then(Value::as_bool) == Some(true) {
            let content_length = response.get("contentLength").and_then(Value::as_u64);
            self.streams.insert(
                id.clone(),
                StreamResponse {
                    encoding: encoding(response),
                    next: 0,
                    frames: BTreeMap::new(),
                    received_bytes: 0,
                },
            );
            return Ok(result(BridgeEvent::StreamOpened { id, content_length }));
        }
        if response.get("chunked").and_then(Value::as_bool) == Some(true) {
            let count = usize_field(response, "chunkCount")?;
            let total_bytes = usize_field(response, "totalBytes")?;
            if count == 0
                || count > MAX_RESPONSE_CHUNKS
                || total_bytes > MAX_RESPONSE_BYTES
                || count.saturating_mul(MAX_CHUNK_SIZE) < total_bytes
            {
                return Err(BridgeError::Length);
            }
            self.chunked.insert(
                id,
                ChunkedResponse {
                    status,
                    headers: response_headers(response),
                    encoding: encoding(response),
                    compression: compression(response),
                    total_bytes,
                    chunks: vec![None; count],
                    ack: response.get("ack").and_then(Value::as_bool) == Some(true),
                    frontier: 0,
                },
            );
            return Ok(result(BridgeEvent::Ignored));
        }
        let bytes = decode(
            response.get("body").and_then(Value::as_str).unwrap_or(""),
            &encoding(response),
        )?;
        require_uncompressed(response)?;
        Ok(result(BridgeEvent::Response {
            id,
            status,
            headers: response_headers(response),
            body: bytes,
        }))
    }

    fn ingest_chunk(&mut self, message: &Value) -> Result<IngestResult, BridgeError> {
        let id = string_field(message, "id")?.to_owned();
        let seq = usize_field(message, "seq")?;
        let data = string_field(message, "data")?;
        let slot = self
            .chunked
            .get_mut(&id)
            .ok_or(BridgeError::UnknownRequest)?;
        if seq >= slot.chunks.len() {
            return Err(BridgeError::Protocol);
        }
        if slot.chunks[seq].is_none() {
            slot.chunks[seq] = Some(decode(data, &slot.encoding)?);
        }
        while slot.frontier < slot.chunks.len() && slot.chunks[slot.frontier].is_some() {
            slot.frontier += 1;
        }
        let mut replies = Vec::new();
        if slot.ack {
            replies.push(json!({"tag":"fetch-ack", "id":id, "ack":slot.frontier}).to_string());
        }
        if slot.frontier != slot.chunks.len() {
            return Ok(IngestResult {
                event: BridgeEvent::Ignored,
                replies,
            });
        }
        if slot.compression != "none" {
            return Err(BridgeError::UnsupportedCompression);
        }
        let mut body = Vec::with_capacity(slot.total_bytes);
        for part in &slot.chunks {
            body.extend_from_slice(part.as_deref().ok_or(BridgeError::Protocol)?);
        }
        if body.len() != slot.total_bytes {
            return Err(BridgeError::Length);
        }
        let status = slot.status;
        let headers = core::mem::take(&mut slot.headers);
        self.chunked.remove(&id);
        Ok(IngestResult {
            event: BridgeEvent::Response {
                id,
                status,
                headers,
                body,
            },
            replies,
        })
    }

    fn ingest_stream(&mut self, message: &Value) -> Result<IngestResult, BridgeError> {
        let id = string_field(message, "id")?.to_owned();
        let seq = message
            .get("seq")
            .and_then(Value::as_u64)
            .ok_or(BridgeError::Protocol)?;
        let stream = self
            .streams
            .get_mut(&id)
            .ok_or(BridgeError::UnknownRequest)?;
        if seq < stream.next {
            let reply = json!({"tag":"fetch-stream-ack", "id":id, "ack":stream.next}).to_string();
            return Ok(IngestResult {
                event: BridgeEvent::Ignored,
                replies: vec![reply],
            });
        }
        if seq >= stream.next + u64::from(ACK_WINDOW) {
            return Err(BridgeError::Protocol);
        }
        let bytes = decode(
            message.get("data").and_then(Value::as_str).unwrap_or(""),
            &stream.encoding,
        )?;
        let expected_crc = string_field(message, "crc32")?;
        if crc32_hex(&bytes) != expected_crc {
            return Err(BridgeError::Checksum);
        }
        stream.frames.entry(seq).or_insert(StreamFrame {
            bytes,
            final_frame: message.get("final").and_then(Value::as_bool) == Some(true),
            total_bytes: message.get("totalBytes").and_then(Value::as_u64),
        });

        let Some(frame) = stream.frames.remove(&stream.next) else {
            let reply = json!({"tag":"fetch-stream-ack", "id":id, "ack":stream.next}).to_string();
            return Ok(IngestResult {
                event: BridgeEvent::Ignored,
                replies: vec![reply],
            });
        };
        stream.next += 1;
        let ack = stream.next;
        stream.received_bytes += frame.bytes.len() as u64;
        if frame.final_frame {
            let total = frame.total_bytes.unwrap_or(stream.received_bytes);
            if total != stream.received_bytes {
                return Err(BridgeError::Length);
            }
            self.streams.remove(&id);
            return Ok(IngestResult {
                event: BridgeEvent::StreamEnded {
                    id: id.clone(),
                    total_bytes: total,
                    bytes: frame.bytes,
                },
                replies: vec![json!({"tag":"fetch-stream-ack", "id":id, "ack":ack}).to_string()],
            });
        }
        Ok(IngestResult {
            event: BridgeEvent::StreamChunk {
                id: id.clone(),
                bytes: frame.bytes,
            },
            replies: vec![json!({"tag":"fetch-stream-ack", "id":id, "ack":ack}).to_string()],
        })
    }
}

fn result(event: BridgeEvent) -> IngestResult {
    IngestResult {
        event,
        replies: Vec::new(),
    }
}

fn string_field<'a>(value: &'a Value, key: &str) -> Result<&'a str, BridgeError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(BridgeError::Protocol)
}

fn usize_field(value: &Value, key: &str) -> Result<usize, BridgeError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or(BridgeError::Protocol)
}

fn response_headers(response: &Value) -> BTreeMap<String, String> {
    response
        .get("headers")
        .and_then(Value::as_object)
        .map(|headers| {
            headers
                .iter()
                .filter_map(|(key, value)| {
                    value
                        .as_str()
                        .map(|value| (key.to_ascii_lowercase(), value.into()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn encoding(response: &Value) -> String {
    response
        .get("bodyEncoding")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if response.get("raw").and_then(Value::as_bool) == Some(true) {
                "base64"
            } else {
                "text"
            }
        })
        .into()
}

fn compression(response: &Value) -> String {
    response
        .get("compression")
        .and_then(Value::as_str)
        .unwrap_or("none")
        .into()
}

fn require_uncompressed(response: &Value) -> Result<(), BridgeError> {
    if compression(response) == "none" {
        Ok(())
    } else {
        Err(BridgeError::UnsupportedCompression)
    }
}

fn decode(input: &str, encoding: &str) -> Result<Vec<u8>, BridgeError> {
    match encoding {
        "text" => Ok(input.as_bytes().to_vec()),
        "base64" => STANDARD.decode(input).map_err(|_| BridgeError::Encoding),
        "hex" => {
            if input.len() % 2 != 0 {
                return Err(BridgeError::Encoding);
            }
            let mut output = Vec::with_capacity(input.len() / 2);
            for pair in input.as_bytes().chunks_exact(2) {
                let high = hex_digit(pair[0])?;
                let low = hex_digit(pair[1])?;
                output.push((high << 4) | low);
            }
            Ok(output)
        }
        _ => Err(BridgeError::Encoding),
    }
}

fn hex_digit(byte: u8) -> Result<u8, BridgeError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(BridgeError::Encoding),
    }
}

pub fn crc32_hex(bytes: &[u8]) -> String {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb8_8320 } else { 0 };
        }
    }
    format!("{:08x}", crc ^ 0xffff_ffff)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeError {
    Json,
    Protocol,
    UnknownRequest,
    Encoding,
    UnsupportedCompression,
    Checksum,
    Length,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_advertises_bounded_v4_streaming() {
        let value: Value = serde_json::from_str(&FetchBridge::new().handshake(0)).unwrap();
        assert_eq!(value["caps"]["version"], 4);
        assert_eq!(value["caps"]["ack"], true);
        assert_eq!(value["caps"]["ackWindow"], 1);
        assert_eq!(value["caps"]["compressions"][0], "none");
    }

    #[test]
    fn chunked_response_acks_incrementally() {
        let mut bridge = FetchBridge::new();
        bridge
            .ingest(r#"{"tag":"fetch","id":"x","resp":{"ok":true,"status":200,"body":"","raw":true,"chunked":true,"totalBytes":3,"chunkCount":2,"bodyEncoding":"base64","compression":"none","ack":true}}"#)
            .unwrap();
        let first = bridge
            .ingest(r#"{"tag":"fetch-chunk","id":"x","seq":0,"total":2,"data":"YWI="}"#)
            .unwrap();
        assert!(first.replies[0].contains(r#""ack":1"#));
        let second = bridge
            .ingest(r#"{"tag":"fetch-chunk","id":"x","seq":1,"total":2,"data":"Yw=="}"#)
            .unwrap();
        assert_eq!(
            second.event,
            BridgeEvent::Response {
                id: "x".into(),
                status: 200,
                headers: BTreeMap::new(),
                body: b"abc".to_vec()
            }
        );
    }

    #[test]
    fn final_stream_frame_preserves_payload() {
        let mut bridge = FetchBridge::new();
        bridge
            .ingest(r#"{"tag":"fetch","id":"s","resp":{"ok":true,"status":200,"stream":true,"raw":true,"bodyEncoding":"base64","contentLength":3}}"#)
            .unwrap();
        let event = bridge
            .ingest(r#"{"tag":"fetch-stream","id":"s","seq":0,"data":"YWJj","crc32":"352441c2","final":true,"totalBytes":3}"#)
            .unwrap()
            .event;
        assert_eq!(
            event,
            BridgeEvent::StreamEnded {
                id: "s".into(),
                total_bytes: 3,
                bytes: b"abc".to_vec(),
            }
        );
    }

    #[test]
    fn chunked_response_preserves_headers_and_rejects_huge_allocations() {
        let mut bridge = FetchBridge::new();
        bridge
            .ingest(r#"{"tag":"fetch","id":"x","resp":{"ok":true,"status":200,"headers":{"Set-Cookie":"sid=x; Expires=Wed, 21 Oct 2015 07:28:00 GMT"},"raw":true,"chunked":true,"totalBytes":3,"chunkCount":1,"bodyEncoding":"base64","compression":"none"}}"#)
            .unwrap();
        let result = bridge
            .ingest(r#"{"tag":"fetch-chunk","id":"x","seq":0,"data":"YWJj"}"#)
            .unwrap();
        let BridgeEvent::Response { headers, .. } = result.event else {
            panic!("expected response");
        };
        assert!(headers["set-cookie"].starts_with("sid=x"));
        assert_eq!(
            bridge.ingest(r#"{"tag":"fetch","id":"huge","resp":{"ok":true,"status":200,"chunked":true,"totalBytes":999999999,"chunkCount":999999999}}"#),
            Err(BridgeError::Length)
        );
    }

    #[test]
    fn crc_matches_protocol_example_for_empty_frame() {
        assert_eq!(crc32_hex(&[]), "00000000");
    }
}
