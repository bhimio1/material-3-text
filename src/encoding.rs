//! Reading and writing text in the file's own encoding and line endings.
//!
//! Detection: a BOM wins; then valid UTF-8; then BOM-less UTF-16 (lots of
//! zero bytes on one side); then chardetng's guess for legacy encodings,
//! which falls back to Windows-1252 (a superset of Latin-1). Text is kept
//! with `\n` line breaks and converted back on save.

use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, UTF_8};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LineEnding {
    #[default]
    Lf,
    CrLf,
    Cr,
}

impl LineEnding {
    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::CrLf => "CRLF",
            LineEnding::Cr => "CR",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
            LineEnding::Cr => "\r",
        }
    }

    pub const ALL: [LineEnding; 3] = [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr];
}

/// An encoding plus whether the file starts with a byte order mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextEncoding {
    pub encoding: &'static Encoding,
    pub bom: bool,
}

impl Default for TextEncoding {
    fn default() -> Self {
        Self { encoding: UTF_8, bom: false }
    }
}

impl TextEncoding {
    pub fn new(encoding: &'static Encoding, bom: bool) -> Self {
        Self { encoding, bom }
    }

    pub fn label(&self) -> String {
        let name = match self.encoding.name() {
            "UTF-16LE" => "UTF-16 LE",
            "UTF-16BE" => "UTF-16 BE",
            "windows-1252" => "Windows-1252",
            other => other,
        };
        if self.bom && self.encoding == UTF_8 {
            format!("{name} with BOM")
        } else {
            name.to_string()
        }
    }

    fn is_utf16(&self) -> bool {
        self.encoding == UTF_16LE || self.encoding == UTF_16BE
    }
}

/// Encodings offered in the status bar menu.
pub fn choices() -> Vec<(TextEncoding, &'static str)> {
    use encoding_rs::*;
    vec![
        (TextEncoding::new(UTF_8, false), "UTF-8"),
        (TextEncoding::new(UTF_8, true), "UTF-8 with BOM"),
        (TextEncoding::new(UTF_16LE, true), "UTF-16 LE"),
        (TextEncoding::new(UTF_16BE, true), "UTF-16 BE"),
        (TextEncoding::new(WINDOWS_1252, false), "Western (Windows-1252, Latin-1)"),
        (TextEncoding::new(ISO_8859_15, false), "Western (ISO-8859-15)"),
        (TextEncoding::new(ISO_8859_2, false), "Central European (ISO-8859-2)"),
        (TextEncoding::new(WINDOWS_1251, false), "Cyrillic (Windows-1251)"),
        (TextEncoding::new(KOI8_R, false), "Cyrillic (KOI8-R)"),
        (TextEncoding::new(WINDOWS_1253, false), "Greek (Windows-1253)"),
        (TextEncoding::new(SHIFT_JIS, false), "Japanese (Shift_JIS)"),
        (TextEncoding::new(EUC_JP, false), "Japanese (EUC-JP)"),
        (TextEncoding::new(GBK, false), "Chinese Simplified (GBK)"),
        (TextEncoding::new(GB18030, false), "Chinese Simplified (GB18030)"),
        (TextEncoding::new(BIG5, false), "Chinese Traditional (Big5)"),
        (TextEncoding::new(EUC_KR, false), "Korean (EUC-KR)"),
    ]
}

#[derive(Debug)]
pub struct Decoded {
    pub text: String,
    pub encoding: TextEncoding,
    pub line_ending: LineEnding,
    /// More than one kind of line ending was found (they're unified).
    pub mixed_endings: bool,
    /// Some bytes weren't valid in the encoding.
    pub lossy: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    Binary,
}

fn utf16_guess(bytes: &[u8]) -> Option<&'static Encoding> {
    let sample = &bytes[..bytes.len().min(4096) & !1];
    if sample.len() < 4 {
        return None;
    }
    let pairs = sample.len() / 2;
    let even = sample.iter().step_by(2).filter(|b| **b == 0).count();
    let odd = sample.iter().skip(1).step_by(2).filter(|b| **b == 0).count();
    if odd * 10 > pairs * 4 && even * 20 < pairs {
        Some(UTF_16LE)
    } else if even * 10 > pairs * 4 && odd * 20 < pairs {
        Some(UTF_16BE)
    } else {
        None
    }
}

/// Detect the encoding and decode.
pub fn decode(bytes: &[u8]) -> Result<Decoded, DecodeError> {
    if let Some((enc, bom_len)) = Encoding::for_bom(bytes) {
        return Ok(finish(&bytes[bom_len..], TextEncoding::new(enc, true)));
    }
    // Zero bytes: UTF-16 without a BOM, or not text at all.
    if bytes[..bytes.len().min(65536)].contains(&0) {
        return match utf16_guess(bytes) {
            Some(enc) => Ok(finish(bytes, TextEncoding::new(enc, false))),
            None => Err(DecodeError::Binary),
        };
    }
    if std::str::from_utf8(bytes).is_ok() {
        return Ok(finish(bytes, TextEncoding::default()));
    }
    let mut det = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    det.feed(bytes, true);
    let enc = det.guess(None, chardetng::Utf8Detection::Allow);
    Ok(finish(bytes, TextEncoding::new(enc, false)))
}

/// Decode in a given encoding ("Reopen with encoding").
pub fn decode_as(bytes: &[u8], encoding: &'static Encoding) -> Decoded {
    let (body, bom) = match Encoding::for_bom(bytes) {
        Some((e, n)) if e == encoding => (&bytes[n..], true),
        _ => (bytes, false),
    };
    finish(body, TextEncoding::new(encoding, bom))
}

fn finish(body: &[u8], encoding: TextEncoding) -> Decoded {
    let (text, lossy) = encoding.encoding.decode_without_bom_handling(body);
    let (line_ending, mixed_endings) = detect_line_ending(&text);
    let text = if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.into_owned()
    };
    Decoded { text, encoding, line_ending, mixed_endings, lossy }
}

/// The most common line ending, and whether others appear too.
pub fn detect_line_ending(text: &str) -> (LineEnding, bool) {
    let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\r' if b.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                i += 1;
            }
            b'\r' => cr += 1,
            b'\n' => lf += 1,
            _ => {}
        }
        i += 1;
    }
    let kinds = [lf, crlf, cr].iter().filter(|n| **n > 0).count();
    let ending = if crlf > lf && crlf >= cr {
        LineEnding::CrLf
    } else if cr > lf && cr > crlf {
        LineEnding::Cr
    } else {
        LineEnding::Lf
    };
    (ending, kinds > 1)
}

/// Encode `text` (with `\n` breaks) for saving. Fails when a character
/// can't be represented in a legacy encoding.
pub fn encode(text: &str, enc: TextEncoding, ending: LineEnding) -> Result<Vec<u8>, String> {
    let text = if ending == LineEnding::Lf {
        std::borrow::Cow::Borrowed(text)
    } else {
        std::borrow::Cow::Owned(text.replace('\n', ending.as_str()))
    };
    let mut out = Vec::with_capacity(text.len() + 4);
    if enc.is_utf16() {
        let le = enc.encoding == UTF_16LE;
        if enc.bom {
            out.extend_from_slice(if le { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] });
        }
        for unit in text.encode_utf16() {
            out.extend_from_slice(&if le { unit.to_le_bytes() } else { unit.to_be_bytes() });
        }
        return Ok(out);
    }
    if enc.encoding == UTF_8 {
        if enc.bom {
            out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        out.extend_from_slice(text.as_bytes());
        return Ok(out);
    }
    let (bytes, _, unmappable) = enc.encoding.encode(&text);
    if unmappable {
        let bad = text
            .chars()
            .find(|c| {
                let mut b = [0u8; 4];
                enc.encoding.encode(c.encode_utf8(&mut b)).2
            })
            .map(|c| format!(" (e.g. “{c}”)"))
            .unwrap_or_default();
        return Err(format!("Some characters can't be saved as {}{bad}. Save as UTF-8 instead.", enc.label()));
    }
    out.extend_from_slice(&bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_with_crlf_round_trips() {
        let d = decode(b"a\r\nb\r\n").unwrap();
        assert_eq!(d.text, "a\nb\n");
        assert_eq!(d.line_ending, LineEnding::CrLf);
        assert!(!d.mixed_endings);
        assert_eq!(encode(&d.text, d.encoding, d.line_ending).unwrap(), b"a\r\nb\r\n");
    }

    #[test]
    fn bom_and_utf16() {
        let d = decode(b"\xEF\xBB\xBFhi").unwrap();
        assert_eq!((d.text.as_str(), d.encoding.bom), ("hi", true));
        assert_eq!(d.encoding.label(), "UTF-8 with BOM");
        assert_eq!(encode("hi", d.encoding, LineEnding::Lf).unwrap(), b"\xEF\xBB\xBFhi");

        let le: Vec<u8> = [0xFF, 0xFE].into_iter().chain("hé\n".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        let d = decode(&le).unwrap();
        assert_eq!(d.text, "hé\n");
        assert_eq!(d.encoding.encoding, UTF_16LE);
        assert_eq!(encode(&d.text, d.encoding, d.line_ending).unwrap(), le);

        let bomless: Vec<u8> = "plain ascii text here".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        let d = decode(&bomless).unwrap();
        assert_eq!(d.encoding.encoding, UTF_16BE);
        assert_eq!(d.text, "plain ascii text here");
    }

    #[test]
    fn latin1_falls_back_to_windows_1252() {
        let d = decode(b"caf\xE9 cr\xE8me br\xFBl\xE9e na\xEFve d\xE9j\xE0 vu").unwrap();
        assert_eq!(d.text, "café crème brûlée naïve déjà vu");
        assert_eq!(d.encoding.encoding, encoding_rs::WINDOWS_1252);
        assert_eq!(encode(&d.text, d.encoding, LineEnding::Lf).unwrap(), b"caf\xE9 cr\xE8me br\xFBl\xE9e na\xEFve d\xE9j\xE0 vu");
        assert!(encode("snow ☃", d.encoding, LineEnding::Lf).unwrap_err().contains('☃'));
    }

    #[test]
    fn binary_is_refused() {
        assert_eq!(decode(b"\x7fELF\x02\x01\x01\x00\x00\x00\xff\xfe").unwrap_err(), DecodeError::Binary);
        assert_eq!(decode(b"abc\0def").unwrap_err(), DecodeError::Binary);
    }

    #[test]
    fn mixed_endings_are_reported() {
        let (e, mixed) = detect_line_ending("a\nb\r\nc\n");
        assert_eq!(e, LineEnding::Lf);
        assert!(mixed);
        assert_eq!(detect_line_ending("a\rb\r").0, LineEnding::Cr);
    }

    #[test]
    fn reopen_with_encoding() {
        let d = decode_as("ü".as_bytes(), encoding_rs::WINDOWS_1252);
        assert_eq!(d.text, "Ã¼");
    }
}
