//! Wire framing for the daemon IPC: `u32` little-endian length prefix
//! followed by a UTF-8 JSON body. Shared by both the client and server
//! so the two ends can't drift in their framing rules.

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::protocol::MAX_FRAME_BYTES;

pub async fn read_frame<T, R>(stream: &mut R) -> Result<T>
where
    T: serde::de::DeserializeOwned,
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .context("reading frame length")?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(anyhow!("frame too large: {len} bytes"));
    }
    let mut body = vec![0u8; len];
    stream
        .read_exact(&mut body)
        .await
        .context("reading frame body")?;
    serde_json::from_slice(&body).context("parsing frame body")
}

pub async fn write_frame<T, W>(stream: &mut W, value: &T) -> Result<()>
where
    T: serde::Serialize,
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(value)?;
    let len = body.len() as u32;
    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::protocol::{Request, Response};
    use std::path::PathBuf;
    use tokio::io::duplex;

    /// A frame written by `write_frame` is decoded byte-for-byte by
    /// `read_frame` on the other end.
    #[tokio::test]
    async fn round_trip_request() {
        let (mut a, mut b) = duplex(64 * 1024);
        let req = Request::TaskSend {
            to: "02ab".into(),
            text: "hello".into(),
        };
        write_frame(&mut a, &req).await.unwrap();
        let parsed: Request = read_frame(&mut b).await.unwrap();
        match parsed {
            Request::TaskSend { to, text } => {
                assert_eq!(to, "02ab");
                assert_eq!(text, "hello");
            }
            other => panic!("expected TaskSend, got {other:?}"),
        }
    }

    /// Round-trip every Response variant with non-trivial payloads, so
    /// future schema changes can't silently break the wire format.
    #[tokio::test]
    async fn round_trip_responses() {
        let cases = vec![
            Response::Info {
                name: "alice".into(),
                pubkey: "02ff".into(),
                capabilities: vec!["text".into()],
                uptime_secs: 7,
                socket_path: PathBuf::from("/tmp/lmao.sock"),
                storage_enabled: true,
            },
            Response::ShutdownAck,
            Response::Error {
                message: "nope".into(),
            },
        ];
        for resp in cases {
            let (mut a, mut b) = duplex(64 * 1024);
            write_frame(&mut a, &resp).await.unwrap();
            let parsed: Response = read_frame(&mut b).await.unwrap();
            // serde JSON is order-preserving and our types are PartialEq-free,
            // so re-serialize and compare the strings.
            assert_eq!(
                serde_json::to_string(&resp).unwrap(),
                serde_json::to_string(&parsed).unwrap(),
            );
        }
    }

    /// A length prefix above MAX_FRAME_BYTES is rejected before the body
    /// is even read — protects the daemon from malicious or corrupt
    /// clients trying to balloon allocations.
    #[tokio::test]
    async fn rejects_oversized_frame() {
        let (mut a, mut b) = duplex(8);
        // Writer task: write a length larger than MAX_FRAME_BYTES, then
        // sit idle. read_frame should bail without ever issuing the
        // body read.
        let writer = tokio::spawn(async move {
            let bogus = (MAX_FRAME_BYTES as u32).saturating_add(1);
            a.write_all(&bogus.to_le_bytes()).await.unwrap();
            // Keep `a` alive so the reader's read_exact(len_buf) returns Ok.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let err = read_frame::<Request, _>(&mut b).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("frame too large"),
            "expected oversize error, got: {msg}"
        );
        writer.await.unwrap();
    }

    /// A truncated stream (writer drops before sending the length prefix)
    /// surfaces as a "reading frame length" error rather than a panic.
    #[tokio::test]
    async fn handles_truncated_length_prefix() {
        let (a, mut b) = duplex(8);
        drop(a); // close the write half immediately
        let err = read_frame::<Request, _>(&mut b).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("reading frame length"),
            "expected length-read error, got: {msg}"
        );
    }

    /// A frame whose body isn't valid JSON for the target type returns
    /// a parse error from serde_json, not garbage.
    #[tokio::test]
    async fn rejects_malformed_body() {
        let (mut a, mut b) = duplex(64);
        let bogus_body = b"not-json-at-all";
        let len = bogus_body.len() as u32;
        a.write_all(&len.to_le_bytes()).await.unwrap();
        a.write_all(bogus_body).await.unwrap();
        a.flush().await.unwrap();
        drop(a);

        let err = read_frame::<Request, _>(&mut b).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("parsing frame body"),
            "expected parse error, got: {msg}"
        );
    }

    /// Length prefix is little-endian. Hand-craft the bytes and verify
    /// the wire layout the way an external tool (e.g., a non-Rust client)
    /// would have to.
    #[tokio::test]
    async fn length_prefix_is_little_endian() {
        let (mut a, mut b) = duplex(64);
        let body = serde_json::to_vec(&Request::Info).unwrap();
        // 6 bytes for `{"kind":"info"}`-ish — keep it small enough that
        // a hand-rolled little-endian header is unambiguous.
        let len = body.len();
        let mut header = [0u8; 4];
        header[0] = (len & 0xff) as u8;
        header[1] = ((len >> 8) & 0xff) as u8;
        header[2] = ((len >> 16) & 0xff) as u8;
        header[3] = ((len >> 24) & 0xff) as u8;
        a.write_all(&header).await.unwrap();
        a.write_all(&body).await.unwrap();
        a.flush().await.unwrap();
        drop(a);

        let parsed: Request = read_frame(&mut b).await.unwrap();
        assert!(matches!(parsed, Request::Info));
    }

    /// Two frames written back-to-back can be read back in order on the
    /// same stream — confirming framing is self-delimiting.
    #[tokio::test]
    async fn two_frames_back_to_back() {
        let (mut a, mut b) = duplex(64 * 1024);
        write_frame(&mut a, &Request::Info).await.unwrap();
        write_frame(&mut a, &Request::Discover).await.unwrap();
        drop(a);

        let r1: Request = read_frame(&mut b).await.unwrap();
        let r2: Request = read_frame(&mut b).await.unwrap();
        assert!(matches!(r1, Request::Info));
        assert!(matches!(r2, Request::Discover));
    }
}
