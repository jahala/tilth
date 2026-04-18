use memchr::memchr;

/// Strip ANSI escape sequences from a string.
///
/// Fast path uses SIMD (via memchr) to detect if any escape bytes exist at all.
/// Slow path handles CSI (`\x1b[...letter`), OSC (`\x1b]...BEL` or `\x1b]...ESC \\`) sequences.
pub fn strip(input: &str) -> String {
    // Fast path: no escape sequences present.
    if memchr(0x1b, input.as_bytes()).is_none() {
        return input.to_string();
    }

    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                // CSI: skip until final byte in range 0x40–0x7E.
                i += 1;
                while i < bytes.len() && !(0x40..=0x7E).contains(&bytes[i]) {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            } else if i < bytes.len() && bytes[i] == b']' {
                // OSC: skip until BEL (0x07), ST (ESC \), or end of input.
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            } else if i < bytes.len() && (0x40..=0x5F).contains(&bytes[i]) {
                // Fe escape sequence (2 bytes total): ESC + final byte
                // Includes \x1bD (IND), \x1bE (NEL), \x1bM (RI), etc.
                // [ and ] already handled above.
                i += 1;
            } else if i < bytes.len() && (0x20..=0x2F).contains(&bytes[i]) {
                // Escape sequence with intermediate bytes: ESC <intermediates> <final>
                // Example: \x1b(B (select ASCII charset)
                // Skip intermediate bytes (0x20-0x2F), then skip the final byte (0x30-0x7E)
                while i < bytes.len() && (0x20..=0x2F).contains(&bytes[i]) {
                    i += 1;
                }
                if i < bytes.len() && (0x30..=0x7E).contains(&bytes[i]) {
                    i += 1;
                }
            }
            // Any other escape: just consumed the 0x1b, continue.
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }

    // All remaining bytes are original UTF-8 content — the stripping only
    // skips escape sequences without modifying any other bytes. If somehow
    // invalid UTF-8 slips through (e.g. ESC inside a multibyte sequence),
    // degrade gracefully rather than panic.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_ansi_fast_path() {
        let s = "hello world";
        assert_eq!(strip(s), s);
    }

    #[test]
    fn strips_color_codes() {
        assert_eq!(strip("\x1b[31mred\x1b[0m"), "red");
    }

    #[test]
    fn strips_osc() {
        assert_eq!(strip("\x1b]0;title\x07text"), "text");
    }

    #[test]
    fn strips_osc_with_st() {
        assert_eq!(
            strip("\x1b]8;;http://example.com\x1b\\link\x1b]8;;\x1b\\"),
            "link"
        );
    }

    #[test]
    fn preserves_utf8() {
        assert_eq!(strip("\x1b[1m日本語\x1b[0m"), "日本語");
    }

    #[test]
    fn strips_fe_escape_sequence() {
        // \x1b(B is "select ASCII charset" — a 3-byte Fe escape sequence.
        // Bug: only the ESC byte is consumed, "(B" leaks into output.
        assert_eq!(strip("hello\x1b(Bworld"), "helloworld");
    }

    #[test]
    fn strips_designate_charset_sequence() {
        // \x1b(0 is "select DEC Special Graphics charset" — intermediate + final byte
        assert_eq!(strip("before\x1b(0after"), "beforeafter");
    }

    #[test]
    fn valid_utf8_input_produces_valid_utf8_output() {
        // Valid UTF-8 input with escape sequences around multibyte chars.
        // Stripping removes only ASCII-range escape bytes, preserving UTF-8 validity.
        let input = "\x1b[1m日本語\x1b(B test\x1b[0m";
        let result = strip(input);
        assert!(result.contains("日本語"));
        assert!(result.contains("test"));
        // Verify the result is valid UTF-8 (it's a String, so this is guaranteed,
        // but the from_utf8_lossy fallback in strip() provides defense-in-depth).
    }
}
