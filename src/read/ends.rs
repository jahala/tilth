//! A text's first and last lines and its line count: all that the log, CSV
//! and unknown-type views show. [`Ends::of_text`] slices them out of a text
//! in memory. [`EndsCollector`] takes a text in pieces of any size, so a
//! file streamed from disk is never held in memory. Both split by the same
//! [`Shape::split`], and a test holds them to the same ends.

use std::borrow::Cow;

use super::{count_lines, LineCounter};

/// How many lines a view shows from each end of a text.
#[derive(Clone, Copy)]
pub(crate) struct Shape {
    pub(crate) head: usize,
    pub(crate) tail: usize,
}

impl Shape {
    /// How many lines of a text of `total` lines go in the head, and how
    /// many after them in the tail.
    fn split(self, total: usize) -> (usize, usize) {
        let head = total.min(self.head);
        (head, self.tail.min(total - head))
    }
}

/// A text's first and last lines, split the way `str::lines` splits it, and
/// its line count. The two ends never overlap: `tail` holds only lines after
/// `head`. They keep the text's bytes as they are; invalid UTF-8 is replaced
/// only when they are read as text. The ends of a text in memory borrow
/// it; the ends of a streamed text own the bytes they keep.
pub(crate) struct Ends<'a> {
    shape: Shape,
    /// The first `shape.head` lines, or all of them, line breaks included.
    head: Cow<'a, [u8]>,
    /// Up to `shape.tail` lines after `head`, through the end of the text.
    tail: Cow<'a, [u8]>,
    total: usize,
}

impl<'a> Ends<'a> {
    /// The ends of `text`, held in memory: slices of it, never a copy.
    pub(crate) fn of_text(text: &'a str, shape: Shape) -> Self {
        let text = text.as_bytes();
        let total = count_lines(text) as usize;
        let (_, tail) = shape.split(total);
        let head_end = match shape.head.checked_sub(1) {
            Some(last) => memchr::memchr_iter(b'\n', text)
                .nth(last)
                .map_or(text.len(), |at| at + 1),
            None => 0,
        };
        let (head, rest) = text.split_at(head_end);
        Ends {
            shape,
            head: Cow::Borrowed(head),
            tail: Cow::Borrowed(&rest[start_of_last_lines(rest, tail)..]),
            total,
        }
    }

    /// The first lines as text, line breaks included. The ends are cut at
    /// line breaks and no invalid UTF-8 sequence runs through one, so the
    /// two decoded apart read as the whole text decoded and then cut.
    pub(crate) fn head(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.head)
    }

    /// The last lines as text, line breaks included.
    pub(crate) fn tail(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.tail)
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

    /// The bytes of the whole text when these ends are all of it, and
    /// otherwise the ends themselves. The bytes are the ends' own, joined
    /// in place, so the text is never held twice.
    pub(crate) fn into_whole(self) -> Result<Vec<u8>, Self> {
        if !self.shows_all() {
            return Err(self);
        }
        let mut text = self.head.into_owned();
        text.extend_from_slice(&self.tail);
        Ok(text)
    }
}

/// Builds [`Ends`] from a text fed in order, in pieces of any size. It keeps
/// the first lines and, after them, a window that always holds the last
/// ones, so it holds no more than the lines it will hand over, plus one
/// piece, and never holds a byte twice.
pub(crate) struct EndsCollector {
    shape: Shape,
    head: Vec<u8>,
    /// How many whole lines `head` holds.
    head_lines: usize,
    /// Everything after the `shape.tail + 1`-th line break from the end of
    /// what was fed after `head`, or all of it while there are fewer
    /// breaks. Whether the text ends in a break or not, its last
    /// `shape.tail` lines after `head` are in here.
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
        let rest = self.feed_head(piece);
        self.feed_window(rest);
    }

    /// Take the lines of `piece` that `head` still wants, and give back the
    /// rest.
    fn feed_head<'p>(&mut self, piece: &'p [u8]) -> &'p [u8] {
        let wanted = self.shape.head - self.head_lines;
        if wanted == 0 {
            return piece;
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
        let (taken, rest) = piece.split_at(end);
        self.head.extend_from_slice(taken);
        self.head_lines += found;
        rest
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

    pub(crate) fn finish(self) -> Ends<'static> {
        let total = self.counter.lines() as usize;
        let (_, tail) = self.shape.split(total);
        let mut window = self.window;
        window.drain(..start_of_last_lines(&window, tail));
        Ends {
            shape: self.shape,
            head: Cow::Owned(self.head),
            tail: Cow::Owned(window),
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

    fn collect(text: &[u8], shape: Shape, piece: usize) -> Ends<'static> {
        let mut collector = EndsCollector::new(shape);
        for chunk in text.chunks(piece.max(1)) {
            collector.feed(chunk);
        }
        collector.finish()
    }

    /// The ends must be `str::lines` applied to the whole decoded text: its
    /// first lines, its last lines after those, and its count, whatever the
    /// size of the pieces the text arrives in. Ends that show every line
    /// hold the text's own bytes.
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
                    assert_eq!(
                        ends.head().lines().collect::<Vec<_>>(),
                        lines[..head],
                        "{at}"
                    );
                    assert_eq!(
                        ends.tail().lines().collect::<Vec<_>>(),
                        lines[tail..],
                        "{at}"
                    );
                    let expected = (lines.len() <= shape.head + shape.tail).then(|| text.clone());
                    assert_eq!(ends.into_whole().ok(), expected, "{at}");
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

    /// The head and the window never hold the same bytes, so the collector
    /// holds no more than it was fed, whatever the text's lines. A text of
    /// one huge line, in memory or streamed, is then held once more at most,
    /// not twice.
    #[test]
    fn the_collector_holds_each_byte_it_keeps_once() {
        let shapes = [Shape { head: 10, tail: 5 }, Shape { head: 1, tail: 1 }];
        for text in corpus() {
            for shape in shapes {
                for piece in [1, 3, 64, text.len()] {
                    let mut collector = EndsCollector::new(shape);
                    let mut fed = 0;
                    for chunk in text.chunks(piece.max(1)) {
                        collector.feed(chunk);
                        fed += chunk.len();
                        let held = collector.head.len() + collector.window.len();
                        assert!(
                            held <= fed,
                            "{:?}, shape {}+{}, piece {piece}: {held} held of {fed}",
                            String::from_utf8_lossy(&text),
                            shape.head,
                            shape.tail
                        );
                    }
                }
            }
        }
    }

    /// The ends of a text in memory are slices of it, never a copy, and
    /// they are the ends the collector finds when the text streams in.
    #[test]
    fn the_ends_of_a_text_in_memory_borrow_it() {
        let shapes = [
            Shape { head: 10, tail: 5 },
            Shape { head: 1, tail: 1 },
            Shape { head: 0, tail: 2 },
            Shape { head: 2, tail: 0 },
        ];
        for text in corpus() {
            let text = String::from_utf8_lossy(&text);
            for shape in shapes {
                let ends = Ends::of_text(&text, shape);
                let streamed = collect(text.as_bytes(), shape, 3);
                let at = format!("{text:?}, shape {}+{}", shape.head, shape.tail);
                assert!(matches!(ends.head, Cow::Borrowed(_)), "{at}");
                assert!(matches!(ends.tail, Cow::Borrowed(_)), "{at}");
                assert_eq!(ends.head(), streamed.head(), "{at}");
                assert_eq!(ends.tail(), streamed.tail(), "{at}");
                assert_eq!(ends.total(), streamed.total(), "{at}");
            }
        }
    }

    /// A line far longer than any piece is kept whole at either end.
    #[test]
    fn a_line_longer_than_a_piece_is_kept_whole() {
        let long = "y".repeat(10_000);
        let text = format!("{long}\nmid\nmid\nmid\n{long}\n");
        let ends = collect(text.as_bytes(), Shape { head: 1, tail: 1 }, 64);
        assert_eq!(ends.head(), format!("{long}\n"));
        assert_eq!(ends.tail(), format!("{long}\n"));
        assert_eq!(ends.total(), 5);
    }
}
