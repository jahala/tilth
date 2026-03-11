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
            }
            // Any other escape: just consumed the 0x1b, continue.
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }

    // All remaining bytes are original UTF-8 content — the stripping only
    // skips escape sequences without modifying any other bytes.
    String::from_utf8(out).expect("ANSI stripping preserved valid UTF-8")
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
        assert_eq!(strip("\x1b]8;;http://example.com\x1b\\link\x1b]8;;\x1b\\"), "link");
    }

    #[test]
    fn preserves_utf8() {
        assert_eq!(strip("\x1b[1m日本語\x1b[0m"), "日本語");
    }
}
