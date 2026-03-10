use std::collections::VecDeque;

const MAX_LINES: usize = 200;

/// A ring buffer that holds the last N log lines for a server.
pub struct LogBuffer {
    lines: VecDeque<String>,
    capacity: usize,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self {
            lines: VecDeque::with_capacity(MAX_LINES),
            capacity: MAX_LINES,
        }
    }

    pub fn push(&mut self, line: String) {
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn lines(&self) -> Vec<String> {
        self.lines.iter().cloned().collect()
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_retrieve() {
        let mut buf = LogBuffer::new();
        buf.push("line 1".into());
        buf.push("line 2".into());
        assert_eq!(buf.lines(), vec!["line 1", "line 2"]);
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn evicts_oldest_when_full() {
        let mut buf = LogBuffer::new();
        for i in 0..250 {
            buf.push(format!("line {}", i));
        }
        assert_eq!(buf.len(), MAX_LINES);
        assert_eq!(buf.lines()[0], "line 50");
        assert_eq!(buf.lines()[MAX_LINES - 1], "line 249");
    }

    #[test]
    fn clear_empties_buffer() {
        let mut buf = LogBuffer::new();
        buf.push("hello".into());
        buf.clear();
        assert!(buf.is_empty());
    }
}
