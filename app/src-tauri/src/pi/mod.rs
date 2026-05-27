pub mod chunk_sink;
pub mod defaults;
pub mod events;
pub mod eviction;
pub mod manager;
pub mod rpc;
pub mod session;
pub mod transport;

#[cfg(any(test, feature = "test-mocks"))]
pub mod mock;

/// Truncate `s` to at most `max_bytes`, snapping down to the nearest UTF-8
/// char boundary so the slice never lands inside a multi-byte codepoint.
pub(crate) fn preview(s: &str, max_bytes: usize) -> &str {
    let mut end = s.len().min(max_bytes);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::preview;

    #[test]
    fn preview_handles_multibyte_boundary() {
        let s = "a—b"; // '—' is 3 bytes (0xE2 0x80 0x94)
        assert_eq!(preview(s, 1), "a");
        assert_eq!(preview(s, 2), "a"); // would split '—', snaps back
        assert_eq!(preview(s, 3), "a"); // still mid-'—'
        assert_eq!(preview(s, 4), "a—");
        assert_eq!(preview(s, 100), "a—b");
    }

    #[test]
    fn preview_empty_and_zero() {
        assert_eq!(preview("", 10), "");
        assert_eq!(preview("hello", 0), "");
    }
}
