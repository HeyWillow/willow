//! Allocation-free HTTP/1.1 chunked transfer framing.

#![allow(
    dead_code,
    reason = "WIS streaming remains inactive until Rust owns runtime audio"
)]

use core::mem::size_of;
use std::io::{self, Write};

const CHUNK_END: &[u8] = b"\r\n";
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
const STREAM_END: &[u8] = b"0\r\n\r\n";

/// Frames each write as one HTTP chunk and emits the terminal chunk on finish.
pub(super) struct ChunkedBodyWriter<Destination> {
    destination: Destination,
    finished: bool,
}

impl<Destination> ChunkedBodyWriter<Destination> {
    pub(super) const fn new(destination: Destination) -> Self {
        Self {
            destination,
            finished: false,
        }
    }
}

impl<Destination: Write> ChunkedBodyWriter<Destination> {
    pub(super) fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.destination.write_all(STREAM_END)?;
        self.destination.flush()?;
        self.finished = true;
        Ok(())
    }
}

impl<Destination: Write> Write for ChunkedBodyWriter<Destination> {
    fn write(&mut self, payload: &[u8]) -> io::Result<usize> {
        if self.finished {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "HTTP chunked body is already finished",
            ));
        }
        if payload.is_empty() {
            return Ok(0);
        }

        let mut header = [0_u8; 2 * size_of::<usize>() + CHUNK_END.len()];
        let header = chunk_header(payload.len(), &mut header);
        self.destination.write_all(header)?;
        self.destination.write_all(payload)?;
        self.destination.write_all(CHUNK_END)?;
        Ok(payload.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.destination.flush()
    }
}

fn chunk_header(length: usize, buffer: &mut [u8]) -> &[u8] {
    let end = buffer.len();
    buffer[end - 2..].copy_from_slice(CHUNK_END);
    let mut cursor = end - CHUNK_END.len();
    let mut remaining = length;
    loop {
        cursor -= 1;
        let digit = match HEX_DIGITS.get(remaining & 0x0f) {
            Some(digit) => *digit,
            None => b'0',
        };
        buffer[cursor] = digit;
        remaining >>= 4;
        if remaining == 0 {
            return &buffer[cursor..];
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_lowercase_hex_chunks_and_terminal_marker() {
        let mut wire = Vec::new();
        {
            let mut writer = super::ChunkedBodyWriter::new(&mut wire);
            assert_eq!(std::io::Write::write(&mut writer, b"abc").ok(), Some(3));
            assert_eq!(
                std::io::Write::write(&mut writer, b"0123456789abcdef").ok(),
                Some(16)
            );
            assert!(writer.finish().is_ok());
            assert!(writer.finish().is_ok());
        }

        assert_eq!(wire, b"3\r\nabc\r\n10\r\n0123456789abcdef\r\n0\r\n\r\n");
    }

    #[test]
    fn empty_writes_do_not_emit_empty_terminal_chunks() {
        let mut wire = Vec::new();
        let mut writer = super::ChunkedBodyWriter::new(&mut wire);

        assert_eq!(std::io::Write::write(&mut writer, &[]).ok(), Some(0));
        assert!(writer.finish().is_ok());
        assert_eq!(wire, b"0\r\n\r\n");
    }

    #[test]
    fn writes_after_finish_are_rejected() {
        let mut wire = Vec::new();
        let mut writer = super::ChunkedBodyWriter::new(&mut wire);
        assert!(writer.finish().is_ok());

        let error = std::io::Write::write(&mut writer, b"late").err();
        assert!(error.is_some_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe));
    }

    #[test]
    fn partial_destinations_are_completed() {
        let mut wire = OneByteWriter::default();
        {
            let mut writer = super::ChunkedBodyWriter::new(&mut wire);
            assert_eq!(std::io::Write::write(&mut writer, b"xy").ok(), Some(2));
            assert!(writer.finish().is_ok());
        }

        assert_eq!(wire.bytes, b"2\r\nxy\r\n0\r\n\r\n");
    }

    #[test]
    fn header_storage_covers_the_full_usize_range() {
        let mut buffer = [0_u8; 2 * core::mem::size_of::<usize>() + super::CHUNK_END.len()];
        let header = super::chunk_header(usize::MAX, &mut buffer);
        let expected = format!("{:x}\r\n", usize::MAX);

        assert_eq!(std::str::from_utf8(header).ok(), Some(expected.as_str()));
    }

    #[derive(Default)]
    struct OneByteWriter {
        bytes: Vec<u8>,
    }

    impl std::io::Write for OneByteWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            let Some(byte) = buffer.first() else {
                return Ok(0);
            };
            self.bytes.push(*byte);
            Ok(1)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
