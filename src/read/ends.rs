//! A text's first and last lines and its line count: all that the log, CSV
//! and unknown-type views show. [`EndsCollector`] takes the text in pieces
//! of any size, so the same code serves a text in memory (one piece) and a
//! file streamed from disk, which is then never held in memory.

use super::LineCounter;

/// How many lines a view shows from each end of a text.
#[derive(Clone, Copy)]
pub(crate) struct Shape {
    pub(crate) head: usize,
    pub(crate) tail: usize,
}

/// A text's first and last lines, split the way `str::lines` splits it, and
/// its line count. The two ends never overlap: `tail` holds only lines after
/// `head`.
pub(crate) struct Ends {
    shape: Shape,
    /// The first `shape.head` lines, or all of them, line breaks included.
    head: String,
    /// Up to `shape.tail` lines after `head`, through the end of the text.
    tail: String,
    total: usize,
}

impl Ends {
    /// The ends of `text`, held in memory.
    pub(crate) fn of_text(text: &str, shape: Shape) -> Self {
        let mut collector = EndsCollector::new(shape);
        collector.feed(text.as_bytes());
        collector.finish()
    }

    pub(crate) fn head_lines(&self) -> Vec<&str> {
        self.head.lines().collect()
    }

    pub(crate) fn tail_lines(&self) -> Vec<&str> {
        self.tail.lines().collect()
    }

    /// The text's line count, by the rule [`LineCounter`] keeps. It equals
    /// `str::lines().count()` on the same text.
    pub(crate) fn total(&self) -> usize {
        self.total
    }

    /// Whether the text has no more lines than the shape shows, so these
    /// ends are all of it.
    pub(crate) fn shows_all(&self) -> bool {
        self.total <= self.shape.head + self.shape.tail
    }
}

/// Builds [`Ends`] from a text fed in order, in pieces of any size. It keeps
/// the first lines and a window that always holds the last ones, so it
/// holds no more than the lines it will hand over, plus one piece.
pub(crate) struct EndsCollector {
    shape: Shape,
    head: Vec<u8>,
    /// How many whole lines `head` holds.
    head_lines: usize,
    /// Everything after the `shape.tail + 1`-th line break from the end of
    /// what was fed, or all of it while there are fewer breaks. Whether the
    /// text ends in a break or not, its last `shape.tail` lines are in here.
    window: Vec<u8>,
    /// How many line breaks `window` holds: always at most `shape.tail`.
    window_breaks: usize,
    counter: LineCounter,
}

impl EndsCollector {
    pub(crate) fn new(shape: Shape) -> Self {
        Self {
            shape,
            head: Vec::new(),
            head_lines: 0,
            window: Vec::new(),
            window_breaks: 0,
            counter: LineCounter::default(),
        }
    }

    /// Take the next piece of the text.
    pub(crate) fn feed(&mut self, piece: &[u8]) {
        self.counter.feed(piece);
        self.feed_head(piece);
        self.feed_window(piece);
    }

    fn feed_head(&mut self, piece: &[u8]) {
        let wanted = self.shape.head - self.head_lines;
        if wanted == 0 {
            return;
        }
        let mut found = 0;
        let mut end = piece.len();
        for at in memchr::memchr_iter(b'\n', piece) {
            found += 1;
            if found == wanted {
                end = at + 1;
                break;
            }
        }
        self.head.extend_from_slice(&piece[..end]);
        self.head_lines += found;
    }

    fn feed_window(&mut self, piece: &[u8]) {
        let keep = self.shape.tail + 1;
        // The piece's own breaks, from its end, up to `keep` of them.
        let mut breaks = 0;
        let mut earliest = 0;
        for at in memchr::memrchr_iter(b'\n', piece).take(keep) {
            breaks += 1;
            earliest = at;
        }
        if breaks == keep {
            // The piece alone holds the cut.
            self.window.clear();
            self.window.extend_from_slice(&piece[earliest + 1..]);
            self.window_breaks = keep - 1;
            return;
        }
        self.window.extend_from_slice(piece);
        self.window_breaks += breaks;
        if self.window_breaks >= keep {
            // Drop the front through the break that is now `keep`-th from
            // the end. Each byte is scanned here at most once before it goes,
            // so a long line costs no more than its length.
            let excess = self.window_breaks - keep + 1;
            if let Some(cut) = memchr::memchr_iter(b'\n', &self.window).nth(excess - 1) {
                self.window.drain(..=cut);
                self.window_breaks -= excess;
            }
        }
    }

    pub(crate) fn finish(self) -> Ends {
        let total = self.counter.lines() as usize;
        let head = total.min(self.shape.head);
        let tail = self.shape.tail.min(total - head);
        let mut window = self.window;
        window.drain(..start_of_last_lines(&window, tail));
        Ends {
            shape: self.shape,
            head: lossy(self.head),
            tail: lossy(window),
            total,
        }
    }
}

/// Where the last `lines` lines of `buf` start, for a `buf` that starts at a
/// line's start: after the `lines`-th break from the end, or the one before
/// it when `buf` ends in a break. The start of `buf` when it has no more
/// lines than that.
fn start_of_last_lines(buf: &[u8], lines: usize) -> usize {
    if lines == 0 {
        return buf.len();
    }
    let nth = lines + usize::from(buf.last() == Some(&b'\n'));
    memchr::memrchr_iter(b'\n', buf)
        .nth(nth - 1)
        .map_or(0, |at| at + 1)
}

/// `bytes` as text, invalid UTF-8 replaced. Decoding a piece cut at a line
/// break gives the same text as decoding the whole and cutting there, since
/// no invalid sequence runs through a `\n`.
fn lossy(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|invalid| String::from_utf8_lossy(invalid.as_bytes()).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    /// Texts that stress the line rule: final break or none, an empty last
    /// line, `\r\n`, a lone `\r`, blank lines, invalid UTF-8 and characters
    /// that a one-byte read splits.
    fn corpus() -> Vec<Vec<u8>> {
        let mut texts: Vec<Vec<u8>> = [
            "",
            "\n",
            "\n\n",
            "one",
            "one\n",
            "one\ntwo",
            "one\n\n",
            "a\r\nb\r\n",
            "a\r\nb\r\nc",
            "a\rb\r",
            "trailing\r",
            "\r\n\r\n",
            "é\nü\n漢字\n🦀",
        ]
        .iter()
        .map(|t| t.as_bytes().to_vec())
        .collect();
        texts.push(b"bad \xff\xfe byte\n\xe2\x82\nsplit \xe2\x82\xac\n".to_vec());
        for lines in [9, 10, 15, 16, 59, 60, 61, 200] {
            let mut text = String::new();
            for i in 0..lines {
                writeln!(text, "line {i}{}", "x".repeat(i % 7)).unwrap();
            }
            texts.push(text.clone().into_bytes());
            text.pop();
            texts.push(text.clone().into_bytes());
            text.push_str("\r\n\n");
            texts.push(text.into_bytes());
        }
        texts
    }

    fn collect(text: &[u8], shape: Shape, piece: usize) -> Ends {
        let mut collector = EndsCollector::new(shape);
        for chunk in text.chunks(piece.max(1)) {
            collector.feed(chunk);
        }
        collector.finish()
    }

    /// The ends must be `str::lines` applied to the whole decoded text: its
    /// first lines, its last lines after those, and its count, whatever the
    /// size of the pieces the text arrives in.
    #[test]
    fn ends_match_str_lines_in_pieces_of_any_size() {
        let shapes = [
            Shape { head: 10, tail: 5 },
            Shape { head: 50, tail: 10 },
            Shape { head: 6, tail: 3 },
            Shape { head: 1, tail: 1 },
        ];
        for text in corpus() {
            let decoded = String::from_utf8_lossy(&text);
            let lines: Vec<&str> = decoded.lines().collect();
            for shape in shapes {
                let head = lines.len().min(shape.head);
                let tail = lines.len().saturating_sub(shape.tail).max(head);
                for piece in [1, 2, 3, 5, 64, text.len()] {
                    let ends = collect(&text, shape, piece);
                    let at = format!(
                        "{decoded:?}, shape {}+{}, piece {piece}",
                        shape.head, shape.tail
                    );
                    assert_eq!(ends.total(), lines.len(), "{at}");
                    assert_eq!(ends.head_lines(), lines[..head], "{at}");
                    assert_eq!(ends.tail_lines(), lines[tail..], "{at}");
                }
            }
        }
    }

    /// The streamed line count is `LineCounter`'s, and the views have always
    /// counted with `str::lines().count()`. The two agree on every text.
    #[test]
    fn streamed_line_count_equals_str_lines_count() {
        for text in corpus() {
            let decoded = String::from_utf8_lossy(&text);
            for piece in [1, 2, 7, text.len()] {
                let mut counter = LineCounter::default();
                for chunk in text.chunks(piece.max(1)) {
                    counter.feed(chunk);
                }
                assert_eq!(
                    counter.lines() as usize,
                    decoded.lines().count(),
                    "{decoded:?}, piece {piece}"
                );
            }
        }
    }

    /// A line far longer than any piece is kept whole at either end.
    #[test]
    fn a_line_longer_than_a_piece_is_kept_whole() {
        let long = "y".repeat(10_000);
        let text = format!("{long}\nmid\nmid\nmid\n{long}\n");
        let ends = collect(text.as_bytes(), Shape { head: 1, tail: 1 }, 64);
        assert_eq!(ends.head_lines(), [long.as_str()]);
        assert_eq!(ends.tail_lines(), [long.as_str()]);
        assert_eq!(ends.total(), 5);
    }
}
