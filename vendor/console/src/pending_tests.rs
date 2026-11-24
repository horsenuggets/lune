use std::{io::Write, os::unix::net::UnixStream};

use super::*;

#[test]
fn buffered_keys_keep_unicode_arrows_and_following_input_in_order() {
    let (socket, _writer) = UnixStream::pair().unwrap();
    let pending = Mutex::new(VecDeque::from("\x1b[Bé\nnext".as_bytes().to_vec()));
    for expected in [Key::ArrowDown, Key::Char('é'), Key::Enter] {
        assert_eq!(
            read_single_key_impl(socket.as_raw_fd(), Some(&pending), None).unwrap(),
            expected
        );
    }
    assert_eq!(
        pending.lock().unwrap().iter().copied().collect::<Vec<_>>(),
        b"next"
    );
}

#[test]
fn buffered_keys_precede_new_os_input() {
    let (socket, mut writer) = UnixStream::pair().unwrap();
    writer.write_all(b"b").unwrap();
    let pending = Mutex::new(VecDeque::from(b"a".to_vec()));
    assert_eq!(
        read_single_key_impl(socket.as_raw_fd(), Some(&pending), None).unwrap(),
        Key::Char('a')
    );
    assert_eq!(
        read_single_key_impl(socket.as_raw_fd(), Some(&pending), None).unwrap(),
        Key::Char('b')
    );
}

#[test]
fn a_unicode_character_can_span_the_pending_queue_and_os_input() {
    let (socket, mut writer) = UnixStream::pair().unwrap();
    let bytes = "\u{10400}".as_bytes();
    let pending = Mutex::new(VecDeque::from(bytes[..1].to_vec()));
    writer.write_all(&bytes[1..]).unwrap();
    assert_eq!(
        read_single_key_impl(socket.as_raw_fd(), Some(&pending), None).unwrap(),
        Key::Char('\u{10400}')
    );
}

#[test]
fn an_arrow_can_span_the_pending_queue_and_os_input() {
    let (socket, mut writer) = UnixStream::pair().unwrap();
    let pending = Mutex::new(VecDeque::from(b"\x1b[".to_vec()));
    writer.write_all(b"A").unwrap();
    assert_eq!(
        read_single_key_impl(socket.as_raw_fd(), Some(&pending), None).unwrap(),
        Key::ArrowUp
    );
}

#[test]
fn raw_ctrl_c_is_preserved_as_an_interruption() {
    let (socket, _writer) = UnixStream::pair().unwrap();
    let pending = Mutex::new(VecDeque::from(b"\x03rest".to_vec()));
    assert_eq!(
        read_single_key_impl(socket.as_raw_fd(), Some(&pending), None)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(
        pending.lock().unwrap().iter().copied().collect::<Vec<_>>(),
        b"rest"
    );
}

#[test]
fn unconfigured_readers_do_not_consume_another_terminals_queue() {
    let (socket, mut writer) = UnixStream::pair().unwrap();
    writer.write_all(b"o").unwrap();
    let pending = Mutex::new(VecDeque::from(b"p".to_vec()));
    assert_eq!(
        read_single_key_impl(socket.as_raw_fd(), None, None).unwrap(),
        Key::Char('o')
    );
    assert_eq!(
        pending.lock().unwrap().iter().copied().collect::<Vec<_>>(),
        b"p"
    );
}

#[derive(Debug, Default)]
struct DropMarkedInput(Mutex<Vec<u8>>);

impl InputFilter for DropMarkedInput {
    fn process(&self, input: &[u8], output: &mut VecDeque<u8>) {
        let mut held = self.0.lock().unwrap();
        for &byte in input {
            if held.first() == Some(&b'<') {
                if byte == b'>' {
                    held.clear();
                } else {
                    held.push(byte);
                }
                continue;
            }
            if !held.is_empty() {
                output.extend(held.drain(..));
            }
            if byte == b'<' || byte == b'\x1b' {
                held.push(byte);
            } else {
                output.push_back(byte);
            }
        }
    }

    fn flush_pending(&self, output: &mut VecDeque<u8>) {
        let mut held = self.0.lock().unwrap();
        if held.as_slice() == b"\x1b" {
            output.extend(held.drain(..));
        }
    }
}

#[test]
fn filtered_os_input_drops_sequences_without_reprocessing_clean_typeahead() {
    let (socket, mut writer) = UnixStream::pair().unwrap();
    let pending = Mutex::new(VecDeque::from(b"<clean>".to_vec()));
    let filter = DropMarkedInput::default();
    writer.write_all(b"<ignored>o").unwrap();
    for character in "<clean>o".chars() {
        assert_eq!(
            read_single_key_impl(socket.as_raw_fd(), Some(&pending), Some(&filter)).unwrap(),
            Key::Char(character)
        );
    }
}

#[test]
fn a_filter_can_hold_a_prefix_until_raw_continuation_arrives() {
    let (socket, mut writer) = UnixStream::pair().unwrap();
    let pending = Mutex::new(VecDeque::new());
    let filter = DropMarkedInput::default();
    filter.process(b"<fragment", &mut pending.lock().unwrap());
    writer.write_all(">é\n".as_bytes()).unwrap();
    assert_eq!(
        read_single_key_impl(socket.as_raw_fd(), Some(&pending), Some(&filter)).unwrap(),
        Key::Char('é')
    );
    assert_eq!(
        read_single_key_impl(socket.as_raw_fd(), Some(&pending), Some(&filter)).unwrap(),
        Key::Enter
    );
}

#[test]
fn a_consumed_sequence_is_not_mistaken_for_eof() {
    let (socket, mut writer) = UnixStream::pair().unwrap();
    let pending = Mutex::new(VecDeque::new());
    let filter = DropMarkedInput::default();
    let sender = std::thread::spawn(move || {
        writer.write_all(b"<ignored>").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(80));
        writer.write_all(b"x").unwrap();
        writer
    });
    assert_eq!(
        read_single_key_impl(socket.as_raw_fd(), Some(&pending), Some(&filter)).unwrap(),
        Key::Char('x')
    );
    sender.join().unwrap();
}

#[test]
fn ambiguous_escape_is_flushed_after_a_bounded_quiet_period() {
    let (socket, mut writer) = UnixStream::pair().unwrap();
    let pending = Mutex::new(VecDeque::new());
    let filter = DropMarkedInput::default();
    writer.write_all(b"\x1b").unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        read_single_key_impl(socket.as_raw_fd(), Some(&pending), Some(&filter)).unwrap(),
        Key::Escape
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

#[test]
fn filtered_unicode_can_arrive_in_separate_raw_reads() {
    let (socket, mut writer) = UnixStream::pair().unwrap();
    let pending = Mutex::new(VecDeque::new());
    let filter = DropMarkedInput::default();
    let sender = std::thread::spawn(move || {
        writer.write_all(b"<ignored>\xf0").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        writer.write_all(b"\x90\x90\x80").unwrap();
        writer
    });
    assert_eq!(
        read_single_key_impl(socket.as_raw_fd(), Some(&pending), Some(&filter)).unwrap(),
        Key::Char('\u{10400}')
    );
    sender.join().unwrap();
}

#[test]
fn filtered_secure_input_preserves_password_secrecy_and_following_input() {
    use std::fs::File;
    use std::os::fd::FromRawFd;
    use std::time::{Duration, Instant};

    let mut master_fd = -1;
    let mut slave_fd = -1;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let mut master = unsafe { File::from_raw_fd(master_fd) };
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    let mut saved = mem::MaybeUninit::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), saved.as_mut_ptr()) },
        0
    );
    let saved = unsafe { saved.assume_init() };
    let observer = slave.try_clone().unwrap();
    let sender = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let mut mode = mem::MaybeUninit::uninit();
            assert_eq!(
                unsafe { libc::tcgetattr(observer.as_raw_fd(), mode.as_mut_ptr()) },
                0
            );
            if unsafe { mode.assume_init() }.c_lflag & libc::ECHO == 0 {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        master.write_all(b"<ignored>ret\nnext").unwrap();
        master
    });
    let pending = Mutex::new(VecDeque::from(b"sec".to_vec()));
    let filter = DropMarkedInput::default();
    assert_eq!(
        read_secure_pending_inner(slave.as_raw_fd(), &pending, Some(&filter)).unwrap(),
        "secret"
    );
    assert_eq!(
        pending.lock().unwrap().iter().copied().collect::<Vec<_>>(),
        b"next"
    );
    let master = sender.join().unwrap();
    assert!(!select_or_poll_term_fd(master.as_raw_fd(), 0).unwrap());
    let mut restored = mem::MaybeUninit::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), restored.as_mut_ptr()) },
        0
    );
    let restored = unsafe { restored.assume_init() };
    assert_eq!(
        restored.c_lflag & (libc::ICANON | libc::ECHO),
        saved.c_lflag & (libc::ICANON | libc::ECHO)
    );
}

#[test]
fn attaching_a_filter_preserves_terminal_output_identity() {
    let terminal = Term::buffered_stderr();
    let target = format!("{:?}", terminal.target());
    let style = terminal.style().apply_to("text").to_string();
    let filtered = terminal.with_filtered_input(
        std::sync::Arc::new(Mutex::new(VecDeque::new())),
        std::sync::Arc::new(DropMarkedInput::default()),
    );
    assert_eq!(format!("{:?}", filtered.target()), target);
    assert_eq!(filtered.style().apply_to("text").to_string(), style);
    assert_eq!(format!("{:?}", filtered.clone().target()), target);
}
