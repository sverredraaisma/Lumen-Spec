//! Hex, lowercase, no separators.
//!
//! The line protocol fixes the spelling rather than accepting anything and
//! normalising, so a runner and an adapter can compare datagrams as strings and
//! a failure diff reads as bytes rather than as a formatting argument.

/// Why a hex string could not be read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum HexError {
    /// Hex encodes whole bytes; an odd count means a digit went missing.
    OddLength(usize),
    /// Not a hex digit, or an uppercase one — see the module note.
    BadDigit { byte: char, at: usize },
}

impl std::fmt::Display for HexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HexError::OddLength(n) => write!(f, "hex string has an odd length ({n})"),
            HexError::BadDigit { byte, at } => {
                write!(f, "`{byte}` at index {at} is not a lowercase hex digit")
            }
        }
    }
}

/// Decode a lowercase hex string. An empty string decodes to no bytes, which is
/// a legal datagram to hand a receiver and one worth testing.
pub fn decode(text: &str) -> Result<Vec<u8>, HexError> {
    if text.len() % 2 != 0 {
        return Err(HexError::OddLength(text.len()));
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for i in (0..bytes.len()).step_by(2) {
        out.push(nibble(bytes[i], i)? << 4 | nibble(bytes[i + 1], i + 1)?);
    }
    Ok(out)
}

fn nibble(byte: u8, at: usize) -> Result<u8, HexError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(HexError::BadDigit {
            byte: byte as char,
            at,
        }),
    }
}

/// Encode as lowercase hex.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from_digit(u32::from(b >> 4), 16).unwrap());
        out.push(char::from_digit(u32::from(b & 0x0F), 16).unwrap());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_byte_value() {
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&all)).unwrap(), all);
    }

    #[test]
    fn empty_is_a_valid_encoding_of_no_bytes() {
        assert_eq!(decode("").unwrap(), Vec::<u8>::new());
        assert_eq!(encode(&[]), "");
    }

    #[test]
    fn rejects_an_odd_length() {
        assert_eq!(decode("abc"), Err(HexError::OddLength(3)));
    }

    #[test]
    fn rejects_uppercase_so_string_comparison_is_meaningful() {
        // The protocol fixes lowercase. Accepting both would mean every
        // comparison had to normalise first, and one that forgot would fail a
        // conforming adapter for cosmetic reasons.
        assert_eq!(decode("4C"), Err(HexError::BadDigit { byte: 'C', at: 1 }));
    }

    #[test]
    fn rejects_a_non_hex_byte_and_says_where() {
        assert_eq!(decode("4cz0"), Err(HexError::BadDigit { byte: 'z', at: 2 }));
    }

    #[test]
    fn errors_render_readably() {
        assert!(decode("abc")
            .unwrap_err()
            .to_string()
            .contains("odd length"));
        assert!(decode("zz").unwrap_err().to_string().contains("index 0"));
    }
}
