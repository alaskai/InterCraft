// Minimal Server-Sent Events reader: yields (event name, data) pairs.

use std::io::BufRead;

pub struct SseReader<R> {
    inner: R,
    line: String,
}

impl<R: BufRead> SseReader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner, line: String::new() }
    }

    /// Next complete event, or None at the end of the stream.
    pub fn next_event(&mut self) -> std::io::Result<Option<(String, String)>> {
        let mut event = String::new();
        let mut data = String::new();
        loop {
            self.line.clear();
            if self.inner.read_line(&mut self.line)? == 0 {
                // Stream ended. Hand back a final event that wasn't followed by a blank line.
                return Ok((!data.is_empty()).then_some((event, data)));
            }
            let line = self.line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !data.is_empty() {
                    return Ok(Some((event, data)));
                }
                event.clear();
                continue;
            }
            if line.starts_with(':') {
                continue; // comment / keep-alive
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => event = value.to_owned(),
                "data" => {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(value);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_events_and_skips_comments() {
        let raw = ": ping\n\nevent: a\ndata: {\"x\":1}\n\nevent: b\r\ndata: one\r\ndata: two\r\n\r\ndata: last";
        let mut sse = SseReader::new(raw.as_bytes());
        assert_eq!(sse.next_event().unwrap(), Some(("a".into(), "{\"x\":1}".into())));
        assert_eq!(sse.next_event().unwrap(), Some(("b".into(), "one\ntwo".into())));
        assert_eq!(sse.next_event().unwrap(), Some(("".into(), "last".into())));
        assert_eq!(sse.next_event().unwrap(), None);
    }
}
