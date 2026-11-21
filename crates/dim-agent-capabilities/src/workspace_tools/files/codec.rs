//! Text file encoding, BOM and line endings (TS `text-file-codec.ts`, ADR agent-019).

use super::super::file_reads::{Bom, EncodingSource, Eol, TextEncoding, TextFileBinding};
use super::failed;
use dim_agent_types::ToolResult;
use encoding_rs::{DecoderResult, EncoderResult, Encoding};
use serde_json::json;

const UTF8_BOM: &[u8] = &[0xef, 0xbb, 0xbf];
const UTF16LE_BOM: &[u8] = &[0xff, 0xfe];
const UTF16BE_BOM: &[u8] = &[0xfe, 0xff];

pub(super) struct CodecError {
    code: &'static str,
    message: String,
}
impl CodecError {
    pub(super) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub(super) fn into_result(self) -> ToolResult {
        failed(self.message, json!({ "code": self.code }))
    }
}

pub(super) struct ResolvedEncoding {
    pub encoding: TextEncoding,
    pub source: EncodingSource,
    pub bom: Bom,
}

/// BOM first, then the explicit encoding, then the UTF-8 policy.
pub(super) fn resolve_encoding(
    head: &[u8],
    explicit: Option<&str>,
) -> Result<ResolvedEncoding, CodecError> {
    let bom = detect_bom(head);
    let explicit_encoding = explicit
        .map(|name| resolve_explicit(name, bom.as_ref().map(|(encoding, _)| encoding)))
        .transpose()?;
    if let Some((encoding, bom)) = bom {
        if explicit_encoding
            .as_ref()
            .is_some_and(|explicit| kind(explicit) != kind(&encoding))
        {
            return Err(CodecError::new(
                "text_encoding_conflict",
                format!(
                    "Explicit encoding \"{}\" conflicts with the file BOM.",
                    explicit.unwrap_or_default()
                ),
            ));
        }
        return Ok(ResolvedEncoding {
            encoding,
            source: EncodingSource::Bom,
            bom,
        });
    }
    Ok(match explicit_encoding {
        Some(encoding) => ResolvedEncoding {
            encoding,
            source: EncodingSource::User,
            bom: Bom::None,
        },
        None => ResolvedEncoding {
            encoding: TextEncoding::Utf8,
            source: EncodingSource::Utf8Policy,
            bom: Bom::None,
        },
    })
}

fn kind(encoding: &TextEncoding) -> &'static str {
    match encoding {
        TextEncoding::Utf8 => "utf8",
        TextEncoding::Utf16Le => "utf16le",
        TextEncoding::Utf16Be => "utf16be",
        TextEncoding::Legacy { .. } => "legacy",
    }
}

fn resolve_explicit(input: &str, bom: Option<&TextEncoding>) -> Result<TextEncoding, CodecError> {
    let name = input.trim();
    if name.is_empty() {
        return Err(CodecError::new(
            "text_encoding_required",
            "read encoding must be a non-empty supported encoding name.",
        ));
    }
    let canonical: String = name
        .to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    match canonical.as_str() {
        "utf8" | "utf8sig" | "utf8bom" | "cp65001" => return Ok(TextEncoding::Utf8),
        "utf16le" | "ucs2" | "ucs2le" => return Ok(TextEncoding::Utf16Le),
        "utf16be" | "ucs2be" => return Ok(TextEncoding::Utf16Be),
        "utf16" => {
            return match bom {
                Some(encoding @ (TextEncoding::Utf16Le | TextEncoding::Utf16Be)) => {
                    Ok(encoding.clone())
                }
                _ => Err(CodecError::new(
                    "text_encoding_required",
                    "UTF-16 without a BOM is ambiguous; specify utf-16le or utf-16be.",
                )),
            };
        }
        _ => {}
    }
    let encoding = match canonical.as_str() {
        "cp936" => Some(encoding_rs::GBK),
        "cp932" => Some(encoding_rs::SHIFT_JIS),
        "cp949" => Some(encoding_rs::EUC_KR),
        "cp950" => Some(encoding_rs::BIG5),
        _ => Encoding::for_label(name.as_bytes()),
    };
    match encoding {
        Some(encoding) if encoding == encoding_rs::UTF_8 => Ok(TextEncoding::Utf8),
        Some(encoding) if encoding == encoding_rs::UTF_16LE => Ok(TextEncoding::Utf16Le),
        Some(encoding) if encoding == encoding_rs::UTF_16BE => Ok(TextEncoding::Utf16Be),
        Some(encoding) if encoding != encoding_rs::REPLACEMENT => Ok(TextEncoding::Legacy {
            name: name.to_lowercase(),
            encoding,
        }),
        _ => Err(CodecError::new(
            "text_encoding_required",
            format!("Unsupported text encoding: {input}."),
        )),
    }
}

pub(in crate::workspace_tools) fn deserialize_legacy_encoding<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<(String, &'static Encoding), D::Error> {
    use serde::{Deserialize, de::Error};
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Legacy {
        name: String,
    }
    let Legacy { name } = Legacy::deserialize(deserializer)?;
    match resolve_explicit(&name, None).map_err(|error| D::Error::custom(error.message))? {
        TextEncoding::Legacy { name, encoding } => Ok((name, encoding)),
        _ => Err(D::Error::custom("expected a legacy encoding")),
    }
}

fn detect_bom(head: &[u8]) -> Option<(TextEncoding, Bom)> {
    if head.starts_with(UTF8_BOM) {
        Some((TextEncoding::Utf8, Bom::Utf8))
    } else if head.starts_with(UTF16LE_BOM) {
        Some((TextEncoding::Utf16Le, Bom::Utf16Le))
    } else if head.starts_with(UTF16BE_BOM) {
        Some((TextEncoding::Utf16Be, Bom::Utf16Be))
    } else {
        None
    }
}

pub(super) fn bom_bytes(bom: Bom) -> &'static [u8] {
    match bom {
        Bom::None => &[],
        Bom::Utf8 => UTF8_BOM,
        Bom::Utf16Le => UTF16LE_BOM,
        Bom::Utf16Be => UTF16BE_BOM,
    }
}

pub(super) fn encoding_rs_of(encoding: &TextEncoding) -> &'static Encoding {
    match encoding {
        TextEncoding::Utf8 => encoding_rs::UTF_8,
        TextEncoding::Utf16Le => encoding_rs::UTF_16LE,
        TextEncoding::Utf16Be => encoding_rs::UTF_16BE,
        TextEncoding::Legacy { encoding, .. } => encoding,
    }
}

/// Strict incremental decode; `Err` on malformed input.
pub(super) fn decode_chunk(
    decoder: &mut encoding_rs::Decoder,
    mut input: &[u8],
    last: bool,
    out: &mut String,
) -> Result<(), ()> {
    loop {
        let needed = decoder
            .max_utf8_buffer_length_without_replacement(input.len())
            .unwrap_or(input.len() * 3 + 16);
        out.reserve(needed);
        let (result, read) = decoder.decode_to_string_without_replacement(input, out, last);
        input = &input[read..];
        match result {
            DecoderResult::InputEmpty => return Ok(()),
            DecoderResult::OutputFull => {}
            DecoderResult::Malformed(..) => return Err(()),
        }
    }
}

/// Decodes the full raw bytes with the binding `read` established.
pub(super) fn decode_text(bytes: &[u8], binding: &TextFileBinding) -> Result<String, CodecError> {
    let body = bytes.get(bom_bytes(binding.bom).len()..).unwrap_or(&[]);
    let mut decoder = encoding_rs_of(&binding.encoding).new_decoder_without_bom_handling();
    let mut text = String::new();
    decode_chunk(&mut decoder, body, true, &mut text).map_err(|()| match &binding.encoding {
        TextEncoding::Legacy { name, .. } => CodecError::new(
            "text_encoding_required",
            format!("File bytes are not valid {name} text."),
        ),
        _ => CodecError::new(
            "text_encoding_required",
            "File bytes no longer match the encoding established by read.",
        ),
    })?;
    Ok(text)
}

/// Encodes with the binding's encoding and BOM; fails before any write when
/// the text is not representable.
pub(super) fn encode_text(text: &str, binding: &TextFileBinding) -> Result<Vec<u8>, CodecError> {
    let mut bytes = bom_bytes(binding.bom).to_vec();
    match &binding.encoding {
        TextEncoding::Utf8 => bytes.extend_from_slice(text.as_bytes()),
        TextEncoding::Utf16Le => text
            .encode_utf16()
            .for_each(|unit| bytes.extend_from_slice(&unit.to_le_bytes())),
        TextEncoding::Utf16Be => text
            .encode_utf16()
            .for_each(|unit| bytes.extend_from_slice(&unit.to_be_bytes())),
        TextEncoding::Legacy { name, encoding } => {
            let not_representable = || {
                CodecError::new(
                    "text_not_representable",
                    format!("New text cannot be represented in {name}."),
                )
            };
            let start = bytes.len();
            let mut encoder = encoding.new_encoder();
            let mut input = text;
            loop {
                let needed = encoder
                    .max_buffer_length_from_utf8_without_replacement(input.len())
                    .unwrap_or(input.len() * 4 + 16);
                bytes.reserve(needed);
                let (result, read) =
                    encoder.encode_from_utf8_to_vec_without_replacement(input, &mut bytes, true);
                input = &input[read..];
                match result {
                    EncoderResult::InputEmpty => break,
                    EncoderResult::OutputFull => {}
                    EncoderResult::Unmappable(_) => return Err(not_representable()),
                }
            }
            let mut decoder = encoding.new_decoder_without_bom_handling();
            let mut round_trip = String::new();
            if decode_chunk(&mut decoder, &bytes[start..], true, &mut round_trip).is_err()
                || round_trip != text
            {
                return Err(not_representable());
            }
        }
    }
    Ok(bytes)
}

pub(super) fn detect_eol(text: &str) -> Eol {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count() - crlf;
    match (crlf, lf) {
        (0, 0) => Eol::None,
        (_, 0) => Eol::Crlf,
        (0, _) => Eol::Lf,
        _ => Eol::Mixed,
    }
}

/// `write` over an existing uniform file adopts the file's line endings.
pub(super) fn normalize_eol(text: &str, eol: Eol) -> String {
    match eol {
        Eol::Lf => text.replace("\r\n", "\n"),
        Eol::Crlf => text.replace("\r\n", "\n").replace('\n', "\r\n"),
        Eol::None | Eol::Mixed => text.to_owned(),
    }
}
