//! 有界的逐工作流加密与压缩层。
//!
//! 每次写入最多形成一个 16 KiB 记录；压缩只保留更短的结果。AEAD 使用独立方向密钥、
//! 单调 nonce 和记录序号。主连接仍由 TLS 保护，密钥在受保护的 StartWork 控制帧中传递。

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use ring::{
    aead, hmac,
    rand::{SecureRandom, SystemRandom},
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Decoder, Encoder, FramedRead, FramedWrite};
use tokio_util::io::{SinkWriter, StreamReader};

const MAX_PLAIN: usize = 16 * 1024;
const MAX_RECORD: usize = MAX_PLAIN + 512;
const TAG_LEN: usize = 16;

/// 可被 `copy_bidirectional` 使用的字节流。
pub trait AsyncIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncIo for T {}

/// 供 TCP 工作流使用的类型擦除流。
pub type BoxAsyncIo = Box<dyn AsyncIo>;

/// 创建一条新工作流的随机 256 位密钥。
pub fn random_key_hex() -> Result<String, String> {
    let mut key = [0u8; 32];
    if SystemRandom::new().fill(&mut key).is_err() {
        return Err("secure random generator failed".into());
    }
    use std::fmt::Write as FmtWrite;
    let mut hex = String::with_capacity(64);
    for byte in key {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

fn parse_key(hex: &str) -> io::Result<[u8; 32]> {
    if hex.len() != 64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid stream key",
        ));
    }
    let mut key = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks_exact(2).enumerate() {
        let pair = match std::str::from_utf8(chunk) {
            Ok(pair) => pair,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid stream key",
                ));
            }
        };
        let byte = match u8::from_str_radix(pair, 16) {
            Ok(byte) => byte,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid stream key",
                ));
            }
        };
        let slot = match key.get_mut(i) {
            Some(slot) => slot,
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid stream key",
                ));
            }
        };
        *slot = byte;
    }
    Ok(key)
}

fn direction_key(master: &[u8; 32], label: &[u8]) -> io::Result<aead::LessSafeKey> {
    let key = hmac::Key::new(hmac::HMAC_SHA256, master);
    let derived = hmac::sign(&key, label);
    let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, derived.as_ref())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "stream key derivation failed"))?;
    Ok(aead::LessSafeKey::new(unbound))
}

struct RecordCodec {
    key: Option<aead::LessSafeKey>,
    compression: bool,
    sequence: u64,
}

impl RecordCodec {
    fn new(key: Option<aead::LessSafeKey>, compression: bool) -> Self {
        Self {
            key,
            compression,
            sequence: 0,
        }
    }

    fn nonce(sequence: u64) -> aead::Nonce {
        let mut nonce = [0u8; 12];
        if let Some(tail) = nonce.get_mut(4..) {
            tail.copy_from_slice(&sequence.to_be_bytes());
        }
        aead::Nonce::assume_unique_for_key(nonce)
    }
}

impl Encoder<&[u8]> for RecordCodec {
    type Error = io::Error;

    fn encode(&mut self, input: &[u8], dst: &mut BytesMut) -> Result<(), Self::Error> {
        if input.len() > MAX_PLAIN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "record exceeds 16 KiB",
            ));
        }
        let compressed = if self.compression {
            Some(lz4_flex::block::compress(input))
        } else {
            None
        };
        let (flag, payload): (u8, &[u8]) = match compressed.as_deref() {
            Some(bytes) if bytes.len() < input.len() => (1, bytes),
            _ => (0, input),
        };
        let mut body = Vec::with_capacity(payload.len() + 3 + TAG_LEN);
        body.put_u16(input.len() as u16);
        body.put_u8(flag);
        body.extend_from_slice(payload);
        if let Some(key) = &self.key {
            if self.sequence == u64::MAX {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "record sequence exhausted",
                ));
            }
            let seq = self.sequence;
            if key
                .seal_in_place_append_tag(Self::nonce(seq), aead::Aad::empty(), &mut body)
                .is_err()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "record encryption failed",
                ));
            }
            let len = body.len() as u32;
            dst.put_u32(len);
            dst.put_u64(seq);
            dst.extend_from_slice(&body);
            self.sequence += 1;
        } else {
            let len = body.len() as u32;
            dst.put_u32(len);
            dst.extend_from_slice(&body);
        }
        Ok(())
    }
}

impl Decoder for RecordCodec {
    type Item = Bytes;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }
        let mut prefix = src.as_ref();
        let len = usize::try_from(prefix.get_u32()).unwrap_or(usize::MAX);
        if len > MAX_RECORD || len < 3 + if self.key.is_some() { TAG_LEN } else { 0 } {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid record length",
            ));
        }
        let needed = 4 + len + if self.key.is_some() { 8 } else { 0 };
        if src.len() < needed {
            return Ok(None);
        }
        src.advance(4);
        let mut body = if let Some(key) = &self.key {
            let seq = src.get_u64();
            if seq != self.sequence || seq == u64::MAX {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "record sequence mismatch",
                ));
            }
            let mut encrypted = src.split_to(len).to_vec();
            let plain =
                match key.open_in_place(Self::nonce(seq), aead::Aad::empty(), &mut encrypted) {
                    Ok(plain) => plain,
                    Err(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "record authentication failed",
                        ));
                    }
                };
            self.sequence += 1;
            BytesMut::from(&*plain)
        } else {
            src.split_to(len)
        };
        let plain_len = usize::from(body.get_u16());
        if plain_len > MAX_PLAIN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "decompressed record exceeds 16 KiB",
            ));
        }
        let flag = body.get_u8();
        let payload = body.freeze();
        let plain = match flag {
            0 if payload.len() == plain_len => payload,
            1 if self.compression => match lz4_flex::block::decompress(&payload, plain_len) {
                Ok(plain) => Bytes::from(plain),
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid compressed record",
                    ));
                }
            },
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "record flags or length are invalid",
                ))
            }
        };
        Ok(Some(plain))
    }
}

/// 双向流式包装；关闭两个开关时原样返回原连接。
pub fn wrap(
    stream: BoxAsyncIo,
    encrypt: bool,
    compress: bool,
    key_hex: &str,
    edge_side: bool,
) -> io::Result<BoxAsyncIo> {
    if !encrypt && !compress {
        return Ok(Box::new(stream));
    }
    let master = if encrypt {
        Some(parse_key(key_hex)?)
    } else {
        None
    };
    let (reader, writer) = tokio::io::split(stream);
    let (read_key, write_key) = match master.as_ref() {
        Some(master) if edge_side => (
            Some(direction_key(master, b"client-to-edge")?),
            Some(direction_key(master, b"edge-to-client")?),
        ),
        Some(master) => (
            Some(direction_key(master, b"edge-to-client")?),
            Some(direction_key(master, b"client-to-edge")?),
        ),
        None => (None, None),
    };
    let read = StreamReader::new(FramedRead::with_capacity(
        reader,
        RecordCodec::new(read_key, compress),
        4096,
    ));
    let write = SinkWriter::new(FramedWrite::with_capacity(
        writer,
        RecordCodec::new(write_key, compress),
        4096,
    ));
    Ok(Box::new(TransformIo {
        read,
        write,
        pending_write: None,
    }))
}

struct TransformIo<R, W> {
    read: R,
    write: W,
    pending_write: Option<usize>,
}

impl<R: AsyncRead + Unpin, W> AsyncRead for TransformIo<R, W> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().read).poll_read(cx, buf)
    }
}

impl<R, W: AsyncWrite + Unpin> AsyncWrite for TransformIo<R, W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if let Some(written) = this.pending_write {
            return match Pin::new(&mut this.write).poll_flush(cx) {
                Poll::Ready(Ok(())) => {
                    this.pending_write = None;
                    Poll::Ready(Ok(written))
                }
                Poll::Ready(Err(error)) => {
                    this.pending_write = None;
                    Poll::Ready(Err(error))
                }
                Poll::Pending => Poll::Pending,
            };
        }
        let input = buf.get(..buf.len().min(MAX_PLAIN)).unwrap_or(&[]);
        match Pin::new(&mut this.write).poll_write(cx, input) {
            Poll::Ready(Ok(written)) if written > 0 => {
                this.pending_write = Some(written);
                match Pin::new(&mut this.write).poll_flush(cx) {
                    Poll::Ready(Ok(())) => {
                        this.pending_write = None;
                        Poll::Ready(Ok(written))
                    }
                    Poll::Ready(Err(error)) => {
                        this.pending_write = None;
                        Poll::Ready(Err(error))
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
            other => other,
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.write).poll_flush(cx) {
            Poll::Ready(result) => {
                this.pending_write = None;
                Poll::Ready(result)
            }
            Poll::Pending => Poll::Pending,
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.write).poll_shutdown(cx) {
            Poll::Ready(result) => {
                this.pending_write = None;
                Poll::Ready(result)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<R, W> Unpin for TransformIo<R, W> {}

#[cfg(test)]
#[cfg_attr(coverage, coverage(off))]
mod tests {
    use super::*;
    use bytes::BufMut;
    use std::collections::VecDeque;
    use std::task::Waker;

    enum WriteStep {
        Pending,
        Ready(usize),
        Failed,
    }

    enum FlushStep {
        Pending,
        Ready,
        Failed,
    }

    struct ScriptedWriter {
        writes: VecDeque<WriteStep>,
        flushes: VecDeque<FlushStep>,
        shutdowns: VecDeque<FlushStep>,
    }

    impl tokio::io::AsyncWrite for ScriptedWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            match self
                .get_mut()
                .writes
                .pop_front()
                .unwrap_or(WriteStep::Ready(buf.len()))
            {
                WriteStep::Pending => Poll::Pending,
                WriteStep::Ready(n) => Poll::Ready(Ok(n)),
                WriteStep::Failed => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "scripted write failure",
                ))),
            }
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match self
                .get_mut()
                .flushes
                .pop_front()
                .unwrap_or(FlushStep::Ready)
            {
                FlushStep::Pending => Poll::Pending,
                FlushStep::Ready => Poll::Ready(Ok(())),
                FlushStep::Failed => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "scripted flush failure",
                ))),
            }
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match self
                .get_mut()
                .shutdowns
                .pop_front()
                .unwrap_or(FlushStep::Ready)
            {
                FlushStep::Pending => Poll::Pending,
                FlushStep::Ready => Poll::Ready(Ok(())),
                FlushStep::Failed => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "scripted shutdown failure",
                ))),
            }
        }
    }

    fn scripted_io(writer: ScriptedWriter) -> TransformIo<tokio::io::Empty, ScriptedWriter> {
        TransformIo {
            read: tokio::io::empty(),
            write: writer,
            pending_write: None,
        }
    }

    fn frame(body: &[u8]) -> BytesMut {
        let mut bytes = BytesMut::new();
        bytes.put_u32(body.len() as u32);
        bytes.extend_from_slice(body);
        bytes
    }

    fn error_kind(result: io::Result<Option<Bytes>>) -> io::ErrorKind {
        result.expect_err("malformed record must fail").kind()
    }

    #[test]
    fn invalid_stream_keys_are_rejected() {
        assert!(parse_key("bad").is_err());
        assert!(parse_key(&"zz".repeat(32)).is_err());
        assert!(parse_key(&"é".repeat(32)).is_err());
        assert_eq!(parse_key(&"00".repeat(32)).unwrap(), [0; 32]);
    }

    #[test]
    fn record_codec_bounds_records_and_selects_compression_only_when_smaller() {
        let plain = vec![b'x'; 4096];
        let mut compressed = RecordCodec::new(None, true);
        let mut encoded = BytesMut::new();
        compressed.encode(&plain, &mut encoded).unwrap();
        assert_eq!(encoded.get(6), Some(&1));
        assert_eq!(compressed.decode(&mut encoded).unwrap().unwrap(), plain);

        let noisy: Vec<u8> = (0..2048)
            .scan(0x1234_5678u32, |state, _| {
                *state ^= *state << 13;
                *state ^= *state >> 17;
                *state ^= *state << 5;
                Some(*state as u8)
            })
            .collect();
        let mut codec = RecordCodec::new(None, true);
        let mut encoded = BytesMut::new();
        codec.encode(&noisy, &mut encoded).unwrap();
        assert_eq!(encoded.get(6), Some(&0));
        assert_eq!(codec.decode(&mut encoded).unwrap().unwrap(), noisy);

        let mut no_compression = RecordCodec::new(None, false);
        let mut encoded = BytesMut::new();
        no_compression.encode(&plain, &mut encoded).unwrap();
        assert_eq!(encoded.get(6), Some(&0));
        assert_eq!(no_compression.decode(&mut encoded).unwrap().unwrap(), plain);
        assert_eq!(
            no_compression
                .encode(&vec![0; MAX_PLAIN + 1], &mut BytesMut::new())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn decoder_rejects_incomplete_oversized_and_malformed_records() {
        let mut codec = RecordCodec::new(None, false);
        assert!(codec.decode(&mut BytesMut::new()).unwrap().is_none());

        let mut incomplete_prefix = BytesMut::from(&[0, 0][..]);
        assert!(codec.decode(&mut incomplete_prefix).unwrap().is_none());

        let mut oversized = BytesMut::new();
        oversized.put_u32((MAX_RECORD + 1) as u32);
        assert_eq!(
            error_kind(codec.decode(&mut oversized)),
            io::ErrorKind::InvalidData
        );

        let mut undersized = BytesMut::new();
        undersized.put_u32(2);
        assert_eq!(
            error_kind(codec.decode(&mut undersized)),
            io::ErrorKind::InvalidData
        );

        let mut partial = frame(&[0, 1, 0, b'a']);
        partial.truncate(7);
        assert!(codec.decode(&mut partial).unwrap().is_none());

        let mut wrong_plain_length = frame(&[0, 2, 0, b'a']);
        assert_eq!(
            error_kind(codec.decode(&mut wrong_plain_length)),
            io::ErrorKind::InvalidData
        );

        let mut unknown_flag = frame(&[0, 1, 2, b'a']);
        assert_eq!(
            error_kind(codec.decode(&mut unknown_flag)),
            io::ErrorKind::InvalidData
        );

        let mut compressed_without_support = frame(&[0, 0, 1]);
        assert_eq!(
            error_kind(codec.decode(&mut compressed_without_support)),
            io::ErrorKind::InvalidData
        );

        let mut invalid_lz4 = RecordCodec::new(None, true);
        let mut malformed_compressed = frame(&[0, 4, 1, 0xff]);
        assert_eq!(
            error_kind(invalid_lz4.decode(&mut malformed_compressed)),
            io::ErrorKind::InvalidData
        );

        let mut too_large_plain = RecordCodec::new(None, true);
        let oversized_plain_len = (MAX_PLAIN as u16 + 1).to_be_bytes();
        let mut body = Vec::from(oversized_plain_len);
        body.extend_from_slice(&[1, 0]);
        let mut oversized_plain = frame(&body);
        assert_eq!(
            error_kind(too_large_plain.decode(&mut oversized_plain)),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn encrypted_codec_checks_sequence_authentication_and_sequence_exhaustion() {
        let master = parse_key(&"12".repeat(32)).unwrap();
        let key = direction_key(&master, b"test-direction").unwrap();
        let mut sender = RecordCodec::new(Some(key), false);
        let mut wire = BytesMut::new();
        sender.encode(b"secret", &mut wire).unwrap();

        let mut receiver = RecordCodec::new(
            Some(direction_key(&master, b"test-direction").unwrap()),
            false,
        );
        let mut partial = wire.clone();
        partial.truncate(7);
        assert!(receiver.decode(&mut partial).unwrap().is_none());
        assert_eq!(
            receiver.decode(&mut wire.clone()).unwrap().unwrap(),
            b"secret"[..]
        );

        let mut wrong_sequence = wire.clone();
        wrong_sequence[7] = 1;
        receiver.sequence = 0;
        assert_eq!(
            error_kind(receiver.decode(&mut wrong_sequence)),
            io::ErrorKind::InvalidData
        );

        let mut exhausted = wire.clone();
        exhausted[4..12].copy_from_slice(&u64::MAX.to_be_bytes());
        receiver.sequence = u64::MAX;
        assert_eq!(
            error_kind(receiver.decode(&mut exhausted)),
            io::ErrorKind::InvalidData
        );

        let mut auth_failure = wire;
        let last = auth_failure.len() - 1;
        auth_failure[last] ^= 0x80;
        receiver.sequence = 0;
        assert_eq!(
            error_kind(receiver.decode(&mut auth_failure)),
            io::ErrorKind::InvalidData
        );

        let mut exhausted_sender = RecordCodec::new(
            Some(direction_key(&master, b"test-direction").unwrap()),
            false,
        );
        exhausted_sender.sequence = u64::MAX;
        assert_eq!(
            exhausted_sender
                .encode(b"x", &mut BytesMut::new())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn transform_io_forwards_pending_and_error_states_without_losing_write_counts() {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let writer = ScriptedWriter {
            writes: VecDeque::from([WriteStep::Ready(3), WriteStep::Ready(0)]),
            flushes: VecDeque::from([
                FlushStep::Pending,
                FlushStep::Pending,
                FlushStep::Ready,
                FlushStep::Pending,
                FlushStep::Ready,
            ]),
            shutdowns: VecDeque::from([FlushStep::Pending, FlushStep::Ready]),
        };
        let mut io = scripted_io(writer);
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, b"payload"),
            Poll::Pending
        ));
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, b"payload"),
            Poll::Pending
        ));
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, b"payload"),
            Poll::Ready(Ok(3))
        ));
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, b"payload"),
            Poll::Ready(Ok(0))
        ));
        assert!(matches!(
            Pin::new(&mut io).poll_flush(&mut cx),
            Poll::Pending
        ));
        assert!(Pin::new(&mut io).poll_flush(&mut cx).is_ready());
        assert!(matches!(
            Pin::new(&mut io).poll_shutdown(&mut cx),
            Poll::Pending
        ));
        assert!(Pin::new(&mut io).poll_shutdown(&mut cx).is_ready());

        let writer = ScriptedWriter {
            writes: VecDeque::from([WriteStep::Ready(1), WriteStep::Ready(1)]),
            flushes: VecDeque::from([FlushStep::Failed, FlushStep::Pending, FlushStep::Failed]),
            shutdowns: VecDeque::new(),
        };
        let mut io = scripted_io(writer);
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, b"a"),
            Poll::Ready(Err(_))
        ));
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, b"b"),
            Poll::Pending
        ));
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, b"b"),
            Poll::Ready(Err(_))
        ));

        let writer = ScriptedWriter {
            writes: VecDeque::from([WriteStep::Pending, WriteStep::Failed]),
            flushes: VecDeque::new(),
            shutdowns: VecDeque::new(),
        };
        let mut io = scripted_io(writer);
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, b"a"),
            Poll::Pending
        ));
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, b"a"),
            Poll::Ready(Err(_))
        ));
    }
}

#[cfg(test)]
mod stream_tests {
    use super::{random_key_hex, wrap};
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn encrypted_compressed_stream_round_trip_and_shutdown() {
        let key = random_key_hex().unwrap();
        let (left, right) = duplex(64 * 1024);
        let mut client = wrap(Box::new(left), true, true, &key, false).unwrap();
        let mut edge = wrap(Box::new(right), true, true, &key, true).unwrap();
        let client_data = vec![b'a'; 16 * 1024];
        let edge_data = vec![b'b'; 4096];

        let (sent, received) =
            tokio::join!(async { client.write_all(&client_data).await }, async {
                let mut bytes = vec![0; client_data.len()];
                edge.read_exact(&mut bytes).await.map(|_| bytes)
            });
        assert!(sent.is_ok());
        assert_eq!(received.unwrap(), client_data);

        let (sent, received) = tokio::join!(async { edge.write_all(&edge_data).await }, async {
            let mut bytes = vec![0; edge_data.len()];
            client.read_exact(&mut bytes).await.map(|_| bytes)
        });
        assert!(sent.is_ok());
        assert_eq!(received.unwrap(), edge_data);

        let key = random_key_hex().unwrap();
        let (left, right) = duplex(4096);
        let mut client = wrap(Box::new(left), true, true, &key, false).unwrap();
        let mut edge = wrap(Box::new(right), true, true, &key, true).unwrap();
        let (sent, received) = tokio::join!(
            async {
                client.write_all(b"end").await?;
                client.flush().await?;
                client.shutdown().await
            },
            async {
                let mut bytes = Vec::new();
                edge.read_to_end(&mut bytes).await.map(|_| bytes)
            }
        );
        assert!(sent.is_ok());
        assert_eq!(received.unwrap(), b"end");
    }

    #[tokio::test]
    async fn compression_only_passthrough_and_invalid_keys_are_handled() {
        let (left, right) = duplex(4096);
        let mut client = wrap(Box::new(left), false, true, "unused", false).unwrap();
        let mut edge = wrap(Box::new(right), false, true, "unused", true).unwrap();
        let payload = b"compression-only stream".repeat(64);
        let (sent, received) = tokio::join!(client.write_all(&payload), async {
            let mut bytes = vec![0; payload.len()];
            edge.read_exact(&mut bytes).await.map(|_| bytes)
        });
        assert!(sent.is_ok());
        assert_eq!(received.unwrap(), payload);

        let (left, right) = duplex(4096);
        let mut client = wrap(Box::new(left), false, false, "unused", false).unwrap();
        let mut server = right;
        let (sent, received) = tokio::join!(client.write_all(b"unchanged"), async {
            let mut bytes = [0; 9];
            server.read_exact(&mut bytes).await.map(|_| bytes)
        });
        assert!(sent.is_ok());
        assert_eq!(received.unwrap(), *b"unchanged");
        assert!(wrap(Box::new(duplex(64).0), true, false, "bad", false).is_err());
        assert!(wrap(Box::new(duplex(64).0), true, false, &"zz".repeat(32), false).is_err());
        let split_utf8 = format!("a{}a", "é".repeat(31));
        assert_eq!(split_utf8.len(), 64);
        assert!(wrap(Box::new(duplex(64).0), true, false, &split_utf8, false).is_err());
    }
}
