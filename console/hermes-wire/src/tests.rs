use std::vec;
use std::vec::Vec;

use pantheon_core::testing::Corruptor;

use crate::config::{self, Config};
use crate::*;

const TOKEN: [u8; TOKEN_LEN] = [0xA5; TOKEN_LEN];
const BUILD: [u8; BUILD_ID_LEN] = [0x3C; BUILD_ID_LEN];
const ICON: &[u8] = b"\x89PNG not really";

fn encode(m: &Message<'_>) -> Vec<u8> {
    let mut out = vec![0u8; m.encoded_len().unwrap()];
    let n = m.encode(&mut out).unwrap();
    assert_eq!(n, out.len(), "encoded_len agrees with encode");
    out
}

/// Header + body, the way a stream reader would split them; no trailing bytes.
fn decode(frame: &[u8]) -> Result<Message<'_>, Error> {
    let head: [u8; HEADER_LEN] = frame
        .get(..HEADER_LEN)
        .and_then(|h| h.try_into().ok())
        .ok_or(Error::Malformed("short header"))?;
    let h = decode_header(head)?;
    let body = &frame[HEADER_LEN..];
    if body.len() != h.body_len as usize {
        return Err(Error::Malformed("length disagrees with frame"));
    }
    Message::decode(h.kind, body)
}

fn job(icon: Option<&[u8]>) -> Job<'_> {
    Job {
        index: 7,
        manifest_url: "http://192.168.0.2:4000/00ff/7/package.json",
        name: "Synthetic Title",
        content_id: "UP0000-TEST00000_00-0000000000000000",
        package_type: "PS4GD",
        size: 0x1_0000_0000,
        digest: [0x11; DIGEST_LEN],
        icon,
    }
}

fn samples() -> Vec<Message<'static>> {
    vec![
        Message::Hello(Hello {
            version: VERSION,
            token: TOKEN,
            build_id: BUILD,
        }),
        Message::Job(job(Some(ICON))),
        Message::Job(job(None)),
        Message::Registered(Registered {
            index: 3,
            task_id: u64::MAX,
        }),
        Message::Failed(Failed {
            index: 1,
            stage: Stage::Register,
            code: 0x8099_0001,
        }),
        Message::Failed(Failed {
            index: 1,
            stage: Stage::Status,
            code: 0,
        }),
        Message::Status(Status {
            index: 2,
            state: TaskState::Downloading,
            bytes_done: Some(12345),
            error_code: 0,
        }),
        Message::Status(Status {
            index: 2,
            state: TaskState::Error,
            bytes_done: None,
            error_code: 0xDEAD,
        }),
        Message::Bye,
        Message::Keepalive,
    ]
}

#[test]
fn every_message_round_trips() {
    for m in samples() {
        let frame = encode(&m);
        assert_eq!(frame[4], m.kind() as u8);
        assert_eq!(decode(&frame).unwrap(), m, "{:?}", m.kind());
    }
}

#[test]
fn bye_is_a_bare_type_byte() {
    assert_eq!(encode(&Message::Bye), [1, 0, 0, 0, Kind::Bye as u8]);
    assert_eq!(
        encode(&Message::Keepalive),
        [1, 0, 0, 0, Kind::Keepalive as u8]
    );
}

#[test]
fn header_checks_type_and_length_before_the_body() {
    assert_eq!(decode_header([1, 0, 0, 0, 99]), Err(Error::UnknownType(99)));
    assert_eq!(
        decode_header([0, 0, 0, 0, Kind::Bye as u8]),
        Err(Error::Malformed("length 0 has no type byte"))
    );
    let over = (MAX_FRAME + 1).to_le_bytes();
    assert_eq!(
        decode_header([over[0], over[1], over[2], over[3], Kind::Hello as u8]),
        Err(Error::FrameTooLarge {
            kind: Kind::Hello,
            len: MAX_FRAME + 1,
            max: MAX_FRAME
        })
    );
    // Only `Job` may exceed 64 KiB, and only up to icon + 64 KiB.
    let job_ok = MAX_JOB_FRAME.to_le_bytes();
    assert!(decode_header([job_ok[0], job_ok[1], job_ok[2], job_ok[3], 2]).is_ok());
    let job_over = (MAX_JOB_FRAME + 1).to_le_bytes();
    assert!(matches!(
        decode_header([job_over[0], job_over[1], job_over[2], job_over[3], 2]),
        Err(Error::FrameTooLarge { .. })
    ));
    let max = u32::MAX.to_le_bytes();
    assert!(decode_header([max[0], max[1], max[2], max[3], 2]).is_err());
}

#[test]
fn version_mismatch_is_reported_not_guessed() {
    let mut frame = encode(&samples()[0]);
    frame[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&2u32.to_le_bytes());
    let e = decode(&frame).unwrap_err();
    assert_eq!(
        e,
        Error::VersionMismatch {
            got: 2,
            expected: 1
        }
    );
    assert_eq!(std::format!("{e}"), "payload is version 2, host expects 1");
    // A future Hello with a longer body still reports the version first.
    frame.extend_from_slice(&[0; 9]);
    let len = (frame.len() - 4) as u32;
    frame[..4].copy_from_slice(&len.to_le_bytes());
    assert!(matches!(
        decode(&frame),
        Err(Error::VersionMismatch { got: 2, .. })
    ));
}

#[test]
fn trailing_and_truncated_bodies_are_malformed() {
    for m in samples() {
        let frame = encode(&m);
        // One extra byte inside the declared length.
        let mut long = frame.clone();
        long.push(0);
        let len = (long.len() - 4) as u32;
        long[..4].copy_from_slice(&len.to_le_bytes());
        assert_eq!(
            decode(&long),
            Err(Error::Malformed("trailing bytes")),
            "{:?}",
            m.kind()
        );
        // Every shorter body, with the length field kept consistent.
        for cut in HEADER_LEN..frame.len() {
            let mut short = frame[..cut].to_vec();
            let len = (short.len() - 4) as u32;
            short[..4].copy_from_slice(&len.to_le_bytes());
            assert!(decode(&short).is_err(), "{:?} cut at {cut}", m.kind());
        }
    }
}

#[test]
fn bad_enum_values_and_flags_are_malformed() {
    let failed = encode(&samples()[4]);
    let mut bad = failed.clone();
    bad[HEADER_LEN + 4] = 9;
    assert_eq!(decode(&bad), Err(Error::Malformed("unknown failure stage")));
    let status = encode(&samples()[6]);
    let mut bad = status.clone();
    bad[HEADER_LEN + 4] = 5;
    assert_eq!(decode(&bad), Err(Error::Malformed("unknown task state")));
    let mut bad = status;
    bad[HEADER_LEN + 5] = 2;
    assert_eq!(
        decode(&bad),
        Err(Error::Malformed("flag is neither 0 nor 1"))
    );
}

#[test]
fn job_strings_respect_batch_limits_both_ways() {
    let long_name = "n".repeat(limits::NAME as usize + 1);
    let mut j = job(None);
    j.name = &long_name;
    assert_eq!(
        Message::Job(j).encoded_len(),
        Err(Error::FieldTooLong {
            field: "package name",
            len: limits::NAME + 1,
            max: limits::NAME
        })
    );
    let mut j = job(None);
    j.content_id = "";
    assert_eq!(
        Message::Job(j).encoded_len(),
        Err(Error::EmptyField("content ID"))
    );
    // Exactly at the limit is fine.
    let name = "n".repeat(limits::NAME as usize);
    let mut j = job(None);
    j.name = &name;
    let frame = encode(&Message::Job(j));
    assert_eq!(decode(&frame).unwrap(), Message::Job(j));

    // Decoder side: inflate the name length past the limit.
    let frame = encode(&Message::Job(job(None)));
    let url_len = job(None).manifest_url.len();
    let name_len_at = HEADER_LEN + 4 + 4 + url_len;
    let mut bad = frame.clone();
    bad[name_len_at..name_len_at + 4].copy_from_slice(&(limits::NAME + 1).to_le_bytes());
    assert!(matches!(
        decode(&bad),
        Err(Error::FieldTooLong {
            field: "package name",
            ..
        })
    ));
    // Invalid UTF-8 in a string.
    let mut bad = frame;
    bad[name_len_at + 4] = 0xFF;
    assert_eq!(decode(&bad), Err(Error::Malformed("string is not UTF-8")));
}

#[test]
fn oversized_icon_is_refused_before_encoding() {
    let icon = vec![0u8; MAX_ICON as usize + 1];
    let j = job(Some(&icon));
    assert!(matches!(
        Message::Job(j).encoded_len(),
        Err(Error::FieldTooLong { field: "icon", .. })
    ));
    // The largest icon fits in the largest Job frame.
    let icon = vec![0u8; MAX_ICON as usize];
    let j = job(Some(&icon));
    let frame = encode(&Message::Job(j));
    assert!(frame.len() - 4 <= MAX_JOB_FRAME as usize);
    assert_eq!(decode(&frame).unwrap(), Message::Job(j));
}

#[test]
fn encode_never_overruns_a_small_buffer() {
    for m in samples() {
        let need = m.encoded_len().unwrap();
        for size in 0..need {
            let mut out = vec![0u8; size];
            assert_eq!(m.encode(&mut out), Err(Error::BufferTooSmall));
        }
    }
}

#[test]
fn corruptor_never_panics_the_decoder() {
    let mut c = Corruptor::new(0x4845_524D);
    for m in samples() {
        c.run(&encode(&m), 2000, |bytes| {
            let _ = decode(bytes);
        });
    }
}

#[test]
fn config_template_patch_and_decode() {
    let mut image = vec![0xCCu8; 64];
    image.extend_from_slice(&config::template());
    image.extend_from_slice(&[0xCC; 64]);
    let at = config::find(&image).unwrap();
    assert_eq!(at, 64);
    let fields: [u8; config::FIELDS_LEN] = image[at + config::MARKER_LEN..at + config::LEN]
        .try_into()
        .unwrap();
    assert_eq!(
        Config::decode_fields(&fields),
        Err(Error::Malformed("config block was never patched"))
    );
    config::stamp_build_id(&mut image, &BUILD).unwrap();
    config::patch(&mut image, [192, 168, 1, 20], 40_000, &TOKEN).unwrap();
    assert_eq!(config::build_id(&image).unwrap(), BUILD);
    let fields: [u8; config::FIELDS_LEN] = image[at + config::MARKER_LEN..at + config::LEN]
        .try_into()
        .unwrap();
    assert_eq!(
        Config::decode_fields(&fields).unwrap(),
        Config {
            version: VERSION,
            host: [192, 168, 1, 20],
            port: 40_000,
            token: TOKEN,
            build_id: BUILD,
        }
    );
}

#[test]
fn config_marker_must_appear_exactly_once_and_fit() {
    assert_eq!(
        config::find(&[0; 100]),
        Err(Error::Malformed("config marker not found"))
    );
    let mut twice = config::template().to_vec();
    twice.extend_from_slice(&config::template());
    assert_eq!(
        config::find(&twice),
        Err(Error::Malformed("config marker appears more than once"))
    );
    let truncated = &config::template()[..config::LEN - 1];
    assert_eq!(
        config::find(truncated),
        Err(Error::Malformed("config block is truncated"))
    );
    let mut other = config::template();
    other[config::MARKER_LEN] = 9;
    assert!(matches!(
        config::patch(&mut other, [1, 2, 3, 4], 1, &TOKEN),
        Err(Error::VersionMismatch { got: 9, .. })
    ));
}
