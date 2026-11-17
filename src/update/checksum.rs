//! `shasum -a 256` checksum-file parsing, streaming SHA-256, and hex helpers.
//!
//! The accepted body is exactly one text-mode `shasum` line:
//! `^[0-9A-Fa-f]{64}  <name>\n?$`. Digits decode to bytes, so digest
//! comparison is case-insensitive by construction. The digest's role is in
//! `docs/update.md#trust-model`.

use std::io::{self, Write};

use sha2::{Digest as _, Sha256};

/// A SHA-256 digest in raw bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Digest(pub [u8; 32]);

/// Why a checksum file body was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ChecksumError {
    /// The body is not one `<64 hex>  <name>` line with an optional `\n`.
    Format,
    /// The line is well formed but names a different file.
    NameMismatch { found: String },
}

/// Hex digits in the digest field.
const HEX_LEN: usize = 64;
/// The text-mode separator between digest and file name.
const SEPARATOR: &[u8] = b"  ";

/// Parses a checksum file body and checks that it names `archive_name`.
pub(crate) fn parse_checksum(body: &[u8], archive_name: &str) -> Result<Digest, ChecksumError> {
    let line = body.strip_suffix(b"\n").unwrap_or(body);
    if line.len() < HEX_LEN + SEPARATOR.len() {
        return Err(ChecksumError::Format);
    }
    let (hex, rest) = line.split_at(HEX_LEN);
    let name = rest.strip_prefix(SEPARATOR).ok_or(ChecksumError::Format)?;
    let digest = decode_hex(hex).ok_or(ChecksumError::Format)?;
    // The name is printable ASCII without spaces: this rejects extra lines,
    // `\r`, a third separator space, and anything non-ASCII. A leading `*` is
    // shasum's binary-mode marker, which the release pipeline never emits.
    let structural =
        !name.is_empty() && name.first() != Some(&b'*') && name.iter().all(u8::is_ascii_graphic);
    if !structural {
        return Err(ChecksumError::Format);
    }
    if name != archive_name.as_bytes() {
        // `name` is ASCII, so this conversion is lossless.
        let found = String::from_utf8_lossy(name).into_owned();
        return Err(ChecksumError::NameMismatch { found });
    }
    Ok(Digest(digest))
}

/// A writer that hashes every byte it passes on, so the archive is hashed
/// while it downloads instead of being read back afterwards.
pub(crate) struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
}

impl<W> HashingWriter<W> {
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    /// The digest of everything written so far.
    pub(crate) fn digest(self) -> Digest {
        Digest(self.hasher.finalize().into())
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The SHA-256 of `bytes`.
#[cfg(test)]
pub(crate) fn sha256(bytes: &[u8]) -> Digest {
    Digest(Sha256::digest(bytes).into())
}

/// Formats a digest as 64 lowercase hex digits.
pub(crate) fn hex_lower(d: &Digest) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(HEX_LEN);
    for byte in d.0 {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Decodes exactly 64 hex digits of either case.
fn decode_hex(hex: &[u8]) -> Option<[u8; 32]> {
    if hex.len() != HEX_LEN {
        return None;
    }
    let mut out = [0u8; 32];
    for (byte, pair) in out.iter_mut().zip(hex.chunks_exact(2)) {
        *byte = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Some(out)
}

fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "Cometty-1.2.3-macos-universal.zip";
    /// SHA-256 of the empty string.
    const HEX: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn expected() -> Digest {
        let mut bytes = [0u8; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&HEX[i * 2..i * 2 + 2], 16).unwrap();
        }
        Digest(bytes)
    }

    fn parse(body: &str) -> Result<Digest, ChecksumError> {
        parse_checksum(body.as_bytes(), NAME)
    }

    #[test]
    fn valid_line_with_and_without_newline() {
        assert_eq!(parse(&format!("{HEX}  {NAME}\n")), Ok(expected()));
        assert_eq!(parse(&format!("{HEX}  {NAME}")), Ok(expected()));
    }

    #[test]
    fn uppercase_digits_decode_to_the_same_digest() {
        let upper = HEX.to_ascii_uppercase();
        assert_eq!(parse(&format!("{upper}  {NAME}\n")), Ok(expected()));
    }

    #[test]
    fn wrong_name_is_a_name_mismatch() {
        let other = "Cometty-1.2.4-macos-universal.zip";
        assert_eq!(
            parse(&format!("{HEX}  {other}\n")),
            Err(ChecksumError::NameMismatch {
                found: other.to_owned()
            })
        );
    }

    #[test]
    fn short_long_or_non_hex_digest_is_format() {
        assert_eq!(
            parse(&format!("{}  {NAME}\n", &HEX[..63])),
            Err(ChecksumError::Format)
        );
        assert_eq!(
            parse(&format!("{HEX}0  {NAME}\n")),
            Err(ChecksumError::Format)
        );
        let bad = format!("g{}", &HEX[1..]);
        assert_eq!(
            parse(&format!("{bad}  {NAME}\n")),
            Err(ChecksumError::Format)
        );
    }

    #[test]
    fn extra_lines_or_trailing_data_are_format() {
        let line = format!("{HEX}  {NAME}\n");
        assert_eq!(parse(&format!("{line}{line}")), Err(ChecksumError::Format));
        assert_eq!(parse(&format!("{line}\n")), Err(ChecksumError::Format));
        assert_eq!(
            parse(&format!("{HEX}  {NAME} \n")),
            Err(ChecksumError::Format)
        );
    }

    #[test]
    fn crlf_is_format() {
        assert_eq!(
            parse(&format!("{HEX}  {NAME}\r\n")),
            Err(ChecksumError::Format)
        );
        assert_eq!(
            parse(&format!("{HEX}  {NAME}\r")),
            Err(ChecksumError::Format)
        );
    }

    #[test]
    fn one_or_three_spaces_are_format() {
        assert_eq!(
            parse(&format!("{HEX} {NAME}\n")),
            Err(ChecksumError::Format)
        );
        assert_eq!(
            parse(&format!("{HEX}   {NAME}\n")),
            Err(ChecksumError::Format)
        );
    }

    #[test]
    fn binary_marker_is_format() {
        assert_eq!(
            parse(&format!("{HEX} *{NAME}\n")),
            Err(ChecksumError::Format)
        );
        assert_eq!(
            parse(&format!("{HEX}  *{NAME}\n")),
            Err(ChecksumError::Format)
        );
    }

    #[test]
    fn empty_and_missing_name_are_format() {
        assert_eq!(parse(""), Err(ChecksumError::Format));
        assert_eq!(parse(&format!("{HEX}  \n")), Err(ChecksumError::Format));
        assert_eq!(parse(&format!("{HEX}\n")), Err(ChecksumError::Format));
    }

    #[test]
    fn known_sha256_vectors() {
        assert_eq!(
            hex_lower(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex_lower(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hashing_writer_hashes_what_it_writes() {
        let data: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        let mut out = Vec::new();
        let mut w = HashingWriter::new(&mut out);
        for chunk in data.chunks(65_536) {
            w.write_all(chunk).unwrap();
        }
        assert_eq!(w.digest(), sha256(&data));
        assert_eq!(out, data);
    }

    #[test]
    fn hex_lower_round_trips() {
        assert_eq!(hex_lower(&expected()), HEX);
        let upper = HEX.to_ascii_uppercase();
        let digest = parse(&format!("{upper}  {NAME}")).unwrap();
        assert_eq!(hex_lower(&digest), HEX);
    }
}
