use std::{collections::VecDeque, sync::Mutex};

const PREFIXES: [&[u8]; 2] = [b"\x1bP0$r", b"\x1bP1$r"];
const MAX_CANDIDATE: usize = 1024;

#[derive(Debug, Default)]
struct State {
    candidate: Vec<u8>,
    discarding: bool,
    previous_escape: bool,
    remaining: usize,
}

/// Demultiplexes only responses to queries this process actually sent.
#[derive(Debug, Default)]
pub(crate) struct ReplyFilter {
    state: Mutex<State>,
}

impl ReplyFilter {
    #[cfg(any(unix, test))]
    pub(crate) fn expect_reply(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.remaining += 1;
    }

    #[cfg(any(unix, test))]
    pub(crate) fn has_pending_reply(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remaining
            > 0
    }

    pub(crate) fn process(&self, input: &[u8], output: &mut VecDeque<u8>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for &byte in input {
            if state.remaining == 0 {
                output.push_back(byte);
                continue;
            }
            if state.discarding {
                if state.previous_escape && byte == b'\\' {
                    state.discarding = false;
                    state.remaining -= 1;
                }
                state.previous_escape = byte == b'\x1b';
                continue;
            }
            state.candidate.push(byte);
            if PREFIXES
                .iter()
                .any(|prefix| state.candidate.starts_with(prefix))
            {
                if state.candidate.ends_with(b"\x1b\\") {
                    state.candidate.clear();
                    state.remaining -= 1;
                } else if state.candidate.len() >= MAX_CANDIDATE {
                    state.previous_escape = byte == b'\x1b';
                    state.candidate.clear();
                    state.discarding = true;
                }
                continue;
            }
            while !state.candidate.is_empty()
                && !PREFIXES
                    .iter()
                    .any(|prefix| prefix.starts_with(&state.candidate))
            {
                output.push_back(state.candidate.remove(0));
            }
        }
    }

    /// Recover after a quiet interval instead of swallowing input after a truncated reply.
    pub(crate) fn flush_pending(&self, output: &mut VecDeque<u8>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.discarding || state.candidate.len() >= PREFIXES[0].len() {
            state.candidate.clear();
            state.discarding = false;
            state.previous_escape = false;
            state.remaining -= 1;
        } else {
            // An isolated Escape or ambiguous Alt prefix still belongs to the keyboard.
            output.extend(state.candidate.drain(..));
        }
    }
}

#[cfg(unix)]
impl console::InputFilter for ReplyFilter {
    fn process(&self, input: &[u8], output: &mut VecDeque<u8>) {
        Self::process(self, input, output);
    }

    fn flush_pending(&self, output: &mut VecDeque<u8>) {
        Self::flush_pending(self, output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(output: &VecDeque<u8>) -> Vec<u8> {
        output.iter().copied().collect()
    }

    #[test]
    fn suppresses_only_outstanding_query_responses() {
        for response in [
            b"\x1bP1$r0m\x1b\\".as_slice(),
            b"\x1bP0$rm\x1b\\".as_slice(),
        ] {
            let filter = ReplyFilter::default();
            let mut output = VecDeque::new();
            filter.process(response, &mut output);
            assert_eq!(bytes(&output), response);
            output.clear();
            filter.expect_reply();
            filter.process(b"before", &mut output);
            filter.process(response, &mut output);
            filter.process(b"after\n", &mut output);
            assert_eq!(bytes(&output), b"beforeafter\n");
            assert!(!filter.has_pending_reply());
        }
    }

    #[test]
    fn keeps_partial_responses_across_every_read_boundary() {
        let response = b"\x1bP1$r0;38:6::1:2:3:128m\x1b\\";
        for split in 0..=response.len() {
            let filter = ReplyFilter::default();
            let mut output = VecDeque::new();
            filter.expect_reply();
            filter.process(&response[..split], &mut output);
            filter.process(&response[split..], &mut output);
            filter.process("h\u{e9}llo\n".as_bytes(), &mut output);
            assert_eq!(bytes(&output), "h\u{e9}llo\n".as_bytes());
        }
    }

    #[test]
    fn preserves_arrows_alt_keys_and_isolated_escape() {
        let filter = ReplyFilter::default();
        let mut output = VecDeque::new();
        filter.expect_reply();
        for &byte in b"\x1b[A\x1b[B\x1bptext" {
            filter.process(&[byte], &mut output);
        }
        filter.process(b"\x1b", &mut output);
        filter.flush_pending(&mut output);
        assert_eq!(bytes(&output), b"\x1b[A\x1b[B\x1bptext\x1b");
        assert!(filter.has_pending_reply());
        filter.process(b"\x1bP1$r0m\x1b\\next", &mut output);
        assert!(bytes(&output).ends_with(b"next"));
        assert!(!filter.has_pending_reply());
    }

    #[test]
    fn bounds_storage_for_oversized_responses() {
        let filter = ReplyFilter::default();
        let mut output = VecDeque::new();
        filter.expect_reply();
        filter.process(b"\x1bP1$r", &mut output);
        filter.process(&vec![b'1'; MAX_CANDIDATE * 2], &mut output);
        assert!(filter.state.lock().unwrap().candidate.is_empty());
        filter.process(b"\x1b\\typed", &mut output);
        assert_eq!(bytes(&output), b"typed");
        assert!(!filter.has_pending_reply());
    }

    #[test]
    fn quiet_flush_abandons_truncated_and_oversized_replies() {
        for payload_length in [0, 2, MAX_CANDIDATE, MAX_CANDIDATE * 2] {
            let filter = ReplyFilter::default();
            let mut output = VecDeque::new();
            filter.expect_reply();
            filter.process(b"\x1bP1$r", &mut output);
            filter.process(&vec![b'1'; payload_length], &mut output);
            filter.flush_pending(&mut output);
            filter.flush_pending(&mut output);
            assert!(output.is_empty());
            assert!(!filter.has_pending_reply());
            filter.process(b"12345\nhello\n\x1b[A", &mut output);
            assert_eq!(bytes(&output), b"12345\nhello\n\x1b[A");
        }
    }

    #[test]
    fn abandoning_one_reply_still_filters_another_outstanding_reply() {
        let filter = ReplyFilter::default();
        let mut output = VecDeque::new();
        filter.expect_reply();
        filter.expect_reply();
        filter.process(b"\x1bP1$r0;", &mut output);
        filter.flush_pending(&mut output);
        assert!(filter.has_pending_reply());
        filter.process(b"42\n\x1bP1$r0m\x1b\\after\n", &mut output);
        assert_eq!(bytes(&output), b"42\nafter\n");
        assert!(!filter.has_pending_reply());
    }

    #[test]
    fn quiet_flush_preserves_every_ambiguous_keyboard_prefix() {
        for prefix in PREFIXES {
            for length in 1..prefix.len() {
                let filter = ReplyFilter::default();
                let mut output = VecDeque::new();
                filter.expect_reply();
                filter.process(&prefix[..length], &mut output);
                filter.flush_pending(&mut output);
                filter.process(b"123\n", &mut output);
                assert_eq!(bytes(&output), [&prefix[..length], b"123\n"].concat());
                assert!(filter.has_pending_reply());
            }
        }
    }

    #[test]
    fn completed_queries_do_not_filter_future_keyboard_bytes() {
        let filter = ReplyFilter::default();
        filter.expect_reply();
        let mut output = VecDeque::new();
        filter.process(b"\x1bP1$r0m\x1b\\", &mut output);
        assert!(output.is_empty());
        filter.process(b"\x1bP1$r0m\x1b\\", &mut output);
        assert_eq!(bytes(&output), b"\x1bP1$r0m\x1b\\");
    }
}
