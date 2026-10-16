//! Basic-auth header encoding for the Transmission RPC client (#505).

use super::BasicCredentials;

/// Build the HTTP `Authorization: Basic` header value for `creds`.
///
/// Base64-encodes `username:password`. Extracted from `TransmissionClient::new`
/// (#505) so the credential encoding is independently testable; the resulting
/// header value is marked `set_sensitive(true)` at the call site.
pub(super) fn basic_auth_header(creds: &BasicCredentials) -> String {
    format!(
        "Basic {}",
        base64_encode(&format!("{}:{}", creds.username(), creds.password()))
    )
}

pub(super) fn base64_encode(input: &str) -> String {
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut encoder = Base64Writer::new(&mut buf);
        encoder.write_all(input.as_bytes()).unwrap();
        encoder.finish();
    }
    String::from_utf8(buf).unwrap()
}

/// Minimal Base64 encoder (avoids pulling in the `base64` crate).
struct Base64Writer<'a> {
    out: &'a mut Vec<u8>,
    buf: [u8; 3],
    buf_len: usize,
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

impl<'a> Base64Writer<'a> {
    fn new(out: &'a mut Vec<u8>) -> Self {
        Self {
            out,
            buf: [0; 3],
            buf_len: 0,
        }
    }

    fn finish(&mut self) {
        if self.buf_len > 0 {
            self.encode_block();
        }
    }

    fn encode_block(&mut self) {
        let b = &self.buf;
        let n = self.buf_len;
        self.out.push(B64[(b[0] >> 2) as usize]);
        self.out
            .push(B64[((b[0] & 0x03) << 4 | b[1] >> 4) as usize]);
        if n > 1 {
            self.out
                .push(B64[((b[1] & 0x0f) << 2 | b[2] >> 6) as usize]);
        } else {
            self.out.push(b'=');
        }
        if n > 2 {
            self.out.push(B64[(b[2] & 0x3f) as usize]);
        } else {
            self.out.push(b'=');
        }
        self.buf = [0; 3];
        self.buf_len = 0;
    }
}

impl std::io::Write for Base64Writer<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        for &byte in data {
            self.buf[self.buf_len] = byte;
            self.buf_len += 1;
            if self.buf_len == 3 {
                self.encode_block();
            }
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
