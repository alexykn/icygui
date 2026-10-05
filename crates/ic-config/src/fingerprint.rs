//! SHA-256 certificate fingerprints, as users paste them and as the config
//! stores them (`TlsConfig::pinned_sha256`).

use crate::error::{ConfigError, excerpt};

/// The label `openssl x509 -noout -fingerprint -sha256` prints before the
/// value (`sha256 Fingerprint=` in OpenSSL 3, `SHA256 Fingerprint=` before).
const OPENSSL_LABEL: &str = "sha256 fingerprint=";

/// Parses a SHA-256 fingerprint.
///
/// Accepts 64 hex digits in either case, either run together
/// (`ab12…`) or as 32 two-digit bytes separated by colons (`AB:12:…`, as
/// browsers and `openssl` show them) or by spaces. Surrounding whitespace
/// and the `sha256 Fingerprint=` label `openssl` prints are ignored.
///
/// # Errors
///
/// [`ConfigError::InvalidFingerprint`] when the text is empty, contains
/// something other than hex digits and separators, has a separated group
/// that isn't exactly two digits, or doesn't encode exactly 32 bytes.
pub fn parse_fingerprint(text: &str) -> Result<[u8; 32], ConfigError> {
    let body = strip_label(text.trim());
    if body.is_empty() {
        return Err(invalid("it is empty"));
    }
    let separator = if body.contains(':') {
        Some(':')
    } else if body.contains(char::is_whitespace) {
        Some(' ')
    } else {
        None
    };
    match separator {
        None => decode_hex(body),
        Some(separator) => {
            let groups: Vec<&str> = if separator == ':' {
                body.split(':').collect()
            } else {
                body.split_whitespace().collect()
            };
            if let Some(group) = groups.iter().find(|group| group.chars().count() != 2) {
                return Err(if group.is_empty() {
                    invalid("it has an empty byte between two separators")
                } else {
                    invalid(format!(
                        "each byte must be two hex digits, but `{}` isn't",
                        excerpt(group, 16)
                    ))
                });
            }
            decode_hex(&groups.concat())
        }
    }
}

/// Formats a fingerprint the way icygui shows and stores it: 32 uppercase
/// hex bytes separated by colons (`AB:12:…`). [`parse_fingerprint`]
/// reads it back.
pub fn format_fingerprint(fingerprint: &[u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut text = String::with_capacity(fingerprint.len() * 3);
    for (index, byte) in fingerprint.iter().enumerate() {
        if index > 0 {
            text.push(':');
        }
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

fn strip_label(text: &str) -> &str {
    text.get(..OPENSSL_LABEL.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(OPENSSL_LABEL))
        .and_then(|_| text.get(OPENSSL_LABEL.len()..))
        .map_or(text, str::trim_start)
}

fn decode_hex(digits: &str) -> Result<[u8; 32], ConfigError> {
    if let Some(bad) = digits.chars().find(|c| !c.is_ascii_hexdigit()) {
        return Err(invalid(format!("`{bad}` is not a hex digit")));
    }
    // Only ASCII digits are left, so the length counts digits.
    if digits.len() != 64 {
        return Err(invalid(format!(
            "expected 64 hex digits (32 bytes), found {}",
            digits.len()
        )));
    }
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let pair = digits
            .get(index * 2..index * 2 + 2)
            .ok_or_else(|| invalid("it is too short"))?;
        *byte = u8::from_str_radix(pair, 16)
            .map_err(|_| invalid(format!("`{pair}` is not a hex byte")))?;
    }
    Ok(bytes)
}

fn invalid(reason: impl Into<String>) -> ConfigError {
    ConfigError::InvalidFingerprint(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    fn expected() -> [u8; 32] {
        let mut bytes = [0_u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&HEX[index * 2..index * 2 + 2], 16).unwrap();
        }
        bytes
    }

    fn colon_separated() -> String {
        format_fingerprint(&expected())
    }

    fn reason(text: &str) -> String {
        match parse_fingerprint(text) {
            Err(ConfigError::InvalidFingerprint(reason)) => reason,
            other => panic!("expected an invalid fingerprint for {text:?}, got {other:?}"),
        }
    }

    #[test]
    fn parses_plain_hex_in_either_case() {
        assert_eq!(parse_fingerprint(HEX).unwrap(), expected());
        assert_eq!(parse_fingerprint(&HEX.to_uppercase()).unwrap(), expected());
    }

    #[test]
    fn parses_colon_and_space_separated_bytes() {
        let colons = colon_separated();
        assert_eq!(parse_fingerprint(&colons).unwrap(), expected());
        assert_eq!(
            parse_fingerprint(&colons.to_lowercase()).unwrap(),
            expected()
        );
        assert_eq!(
            parse_fingerprint(&colons.replace(':', " ")).unwrap(),
            expected()
        );
    }

    #[test]
    fn ignores_whitespace_and_the_openssl_label() {
        let colons = colon_separated();
        assert_eq!(
            parse_fingerprint(&format!("  {colons}\n")).unwrap(),
            expected()
        );
        assert_eq!(
            parse_fingerprint(&format!("sha256 Fingerprint={colons}")).unwrap(),
            expected()
        );
        assert_eq!(
            parse_fingerprint(&format!("SHA256 Fingerprint={colons}\n")).unwrap(),
            expected()
        );
    }

    #[test]
    fn formats_as_uppercase_colon_hex() {
        let text = colon_separated();
        assert_eq!(text.len(), 95);
        assert!(text.starts_with("A1:B2:C3:D4:"));
        assert!(text.ends_with(":8F:90"));
        assert_eq!(format_fingerprint(&[0; 32]), ["00"; 32].join(":"));
    }

    #[test]
    fn rejects_empty_text() {
        assert_eq!(reason(""), "it is empty");
        assert_eq!(reason("   "), "it is empty");
        assert_eq!(reason("sha256 Fingerprint="), "it is empty");
    }

    #[test]
    fn rejects_wrong_lengths() {
        assert_eq!(
            reason(&HEX[..62]),
            "expected 64 hex digits (32 bytes), found 62"
        );
        assert_eq!(
            reason(&format!("{HEX}00")),
            "expected 64 hex digits (32 bytes), found 66"
        );
        let short = colon_separated()[..92].to_owned();
        assert_eq!(
            reason(&short),
            "expected 64 hex digits (32 bytes), found 62"
        );
    }

    #[test]
    fn rejects_non_hex_characters() {
        let mut text = HEX.to_owned();
        text.replace_range(10..11, "g");
        assert_eq!(reason(&text), "`g` is not a hex digit");
        assert_eq!(reason(&HEX.replacen('a', "ä", 1)), "`ä` is not a hex digit");
        // A base64 SSH-style fingerprint is not hex.
        assert!(parse_fingerprint("SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU").is_err());
    }

    #[test]
    fn rejects_malformed_groups() {
        let colons = colon_separated();
        assert_eq!(
            reason(&format!("{colons}:")),
            "it has an empty byte between two separators"
        );
        assert_eq!(
            reason(&colons.replacen(':', "::", 1)),
            "it has an empty byte between two separators"
        );
        // Bytes regrouped into fours have the right digits but the wrong shape.
        let regrouped = colons
            .replace(':', "")
            .as_bytes()
            .chunks(4)
            .map(|c| std::str::from_utf8(c).unwrap())
            .collect::<Vec<_>>()
            .join(":");
        assert_eq!(
            reason(&regrouped),
            "each byte must be two hex digits, but `A1B2` isn't"
        );
        // Mixed separators leave a group with a space in it.
        assert!(parse_fingerprint(&colons.replacen(':', " ", 1)).is_err());
        // A huge group is quoted only in part.
        assert_eq!(
            reason(&format!("{}:AB", "C".repeat(100_000))),
            format!(
                "each byte must be two hex digits, but `{}…` isn't",
                "C".repeat(16)
            )
        );
    }
}
