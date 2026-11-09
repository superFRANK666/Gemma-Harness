use anyhow::{bail, Context, Result};

const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// Decode bytes rather than individual HTTP chunks: a UTF-8 codepoint can straddle chunks.
#[derive(Default)]
pub(crate) struct SseDecoder {
    pending: Vec<u8>,
    data: Vec<u8>,
    started: bool,
}

impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.pending.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            self.line(&line, &mut events)?;
        }
        if self.pending.len() + self.data.len() > MAX_EVENT_BYTES {
            bail!("SSE event exceeds 8 MiB");
        }
        Ok(events)
    }

    fn line(&mut self, line: &[u8], events: &mut Vec<String>) -> Result<()> {
        let line = if !self.started {
            self.started = true;
            line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line)
        } else {
            line
        };
        if line.is_empty() {
            if !self.data.is_empty() {
                self.data.pop(); // SSE joins multiple data lines with a newline.
                events.push(
                    String::from_utf8(std::mem::take(&mut self.data))
                        .context("Invalid UTF-8 in SSE event")?,
                );
            }
        } else if line.starts_with(b"data:") {
            let mut value = &line[5..];
            if value.first() == Some(&b' ') {
                value = &value[1..];
            }
            self.data.extend_from_slice(value);
            self.data.push(b'\n');
            if self.data.len() > MAX_EVENT_BYTES {
                bail!("SSE event exceeds 8 MiB");
            }
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<Vec<String>> {
        let mut events = Vec::new();
        if !self.pending.is_empty() {
            let mut last = std::mem::take(&mut self.pending);
            if last.last() == Some(&b'\r') {
                last.pop();
            }
            self.line(&last, &mut events)?;
        }
        self.line(&[], &mut events)?;
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_utf8_crlf_comments_and_multiline() {
        let input = ": heartbeat\r\nevent: response.output_text.delta\r\ndata: {\"text\":\"你好🙂\",\r\ndata: \"ok\":true}\r\n\r\ndata: [DONE]\n\n";
        let mut decoder = SseDecoder::default();
        let mut output = Vec::new();
        for byte in input.as_bytes() {
            output.extend(decoder.push(&[*byte]).unwrap());
        }
        output.extend(decoder.finish().unwrap());
        assert_eq!(
            output,
            vec!["{\"text\":\"你好🙂\",\n\"ok\":true}", "[DONE]"]
        );
    }

    #[test]
    fn accepts_final_event_without_blank_line() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b"data: {\"ok\":true}").unwrap().is_empty());
        assert_eq!(decoder.finish().unwrap(), vec!["{\"ok\":true}"]);
    }

    #[test]
    fn accepts_utf8_bom() {
        let mut decoder = SseDecoder::default();
        assert_eq!(
            decoder.push(b"\xef\xbb\xbfdata: [DONE]\n\n").unwrap(),
            vec!["[DONE]"]
        );
    }

    #[test]
    fn rejects_unbounded_line() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(&vec![b'x'; MAX_EVENT_BYTES + 1]).is_err());
    }
}
