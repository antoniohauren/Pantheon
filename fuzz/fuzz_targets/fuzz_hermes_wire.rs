//! Decodes arbitrary bytes as one Hermes frame (header, then body); anything
//! that decodes must encode back to the same bytes.

#![no_main]

use hermes_wire::{HEADER_LEN, Message, decode_header};

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Some((head, body)) = data.split_first_chunk::<HEADER_LEN>() else {
        return;
    };
    let Ok(header) = decode_header(*head) else {
        return;
    };
    let Some(body) = body.get(..header.body_len as usize) else {
        return;
    };
    if let Ok(message) = Message::decode(header.kind, body) {
        let mut out = vec![0u8; message.encoded_len().expect("decoded frame has a length")];
        message.encode(&mut out).expect("decoded frame encodes");
        assert_eq!(&out[..HEADER_LEN], head);
        assert_eq!(&out[HEADER_LEN..], body);
    }
});
