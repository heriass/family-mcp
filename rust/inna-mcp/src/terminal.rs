//! The sign-in's phone prompt, as packages/inna-mcp/src/cli.ts reads it with node:readline into a
//! sink: the first line of standard input, or on a terminal one line typed in raw mode without
//! echo. Adapted from rust/kronan-mcp/src/terminal.rs. Everything here blocks.

use std::io::{IsTerminal, Read, Write};

use rustix::termios::{
    InputModes, LocalModes, OptionalActions, OutputModes, SpecialCodeIndex, Termios, tcgetattr,
    tcsetattr,
};

use crate::error::{Fail, Result};

const END_OF_TEXT: char = '\u{3}';

const END_OF_TRANSMISSION: char = '\u{4}';

const BACKSPACE: char = '\u{8}';

const KILL_LINE: char = '\u{15}';

const ESCAPE: char = '\u{1b}';

const DELETE: char = '\u{7f}';

/// Restores the terminal mode on every path out of the prompt.
struct Raw {
    saved: Termios,
}

impl Raw {
    /// libuv's raw mode, as readline's `setRawMode(true)` sets it.
    fn enter() -> Option<Self> {
        let stdin = std::io::stdin();
        let saved = tcgetattr(&stdin).ok()?;
        let mut raw = saved.clone();
        raw.input_modes -= InputModes::BRKINT
            | InputModes::ICRNL
            | InputModes::INPCK
            | InputModes::ISTRIP
            | InputModes::IXON;
        raw.output_modes |= OutputModes::ONLCR;
        raw.local_modes -=
            LocalModes::ECHO | LocalModes::ICANON | LocalModes::IEXTEN | LocalModes::ISIG;
        raw.special_codes[SpecialCodeIndex::VMIN] = 1;
        raw.special_codes[SpecialCodeIndex::VTIME] = 0;
        tcsetattr(&stdin, OptionalActions::Now, &raw).ok()?;
        Some(Self { saved })
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        // The terminal may already be gone; the prompt's outcome stands either way.
        let _ = tcsetattr(std::io::stdin(), OptionalActions::Now, &self.saved);
    }
}

/// What a key does to the line being typed.
enum Key {
    Insert(char),
    Done,
    Closed,
    Ignored,
}

/// readline's keys for a line typed at the end, with no cursor movement: Enter ends it, Ctrl-C
/// cancels (its `SIGINT` event), Ctrl-D on an empty line closes the input, Backspace removes one
/// character and Ctrl-U the whole line. Other control keys and escape sequences change nothing.
struct Keys {
    line: String,
    escape: Escape,
}

/// Where an escape sequence (an arrow or function key) stands.
#[derive(Clone, Copy, PartialEq)]
enum Escape {
    None,
    Started,
    /// After `ESC [`: parameters until a final byte in `@`..=`~`.
    Control,
    /// After `ESC O`: one more character.
    Single,
}

impl Keys {
    fn key(&mut self, char: char) -> Key {
        match self.escape {
            Escape::Started => {
                self.escape = match char {
                    '[' => Escape::Control,
                    'O' => Escape::Single,
                    _ => Escape::None,
                };
                return Key::Ignored;
            }
            Escape::Control => {
                if ('@'..='~').contains(&char) {
                    self.escape = Escape::None;
                }
                return Key::Ignored;
            }
            Escape::Single => {
                self.escape = Escape::None;
                return Key::Ignored;
            }
            Escape::None => {}
        }

        match char {
            '\r' | '\n' => Key::Done,
            END_OF_TEXT => Key::Closed,
            END_OF_TRANSMISSION if self.line.is_empty() => Key::Closed,
            BACKSPACE | DELETE => {
                self.line.pop();
                Key::Ignored
            }
            KILL_LINE => {
                self.line.clear();
                Key::Ignored
            }
            ESCAPE => {
                self.escape = Escape::Started;
                Key::Ignored
            }
            '\t' => Key::Insert(char),
            _ if char.is_control() => Key::Ignored,
            _ => Key::Insert(char),
        }
    }
}

/// `prompt`, then one line typed on the terminal, unechoed; `None` when it is cancelled or the
/// input closes.
fn read_hidden_line(prompt: &str) -> Result<Option<String>> {
    // Like `setRawMode` throwing in createInterface, before cli.ts writes its prompt: no prompt
    // is shown and nothing is read while the terminal would echo it.
    let raw = Raw::enter().ok_or(Fail::Unknown)?;
    // The prompt shows only once the terminal is raw, as cli.ts writes it after createInterface,
    // so a key typed as soon as it shows is read unechoed.
    show(prompt);
    let line = hidden_line(&mut std::io::stdin().lock());
    drop(raw);
    line
}

fn show(prompt: &str) {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
}

/// The keys read from `input` up to the line's end; `None` when they cancel it or the input ends.
/// A failed read fails the prompt, as readline's error does: on Linux a terminal that hangs up
/// fails the read (EIO), where macOS ends the input.
fn hidden_line(input: &mut impl Read) -> Result<Option<String>> {
    let mut keys = Keys {
        line: String::new(),
        escape: Escape::None,
    };
    let mut chunk = [0u8; 4096];

    loop {
        let read = match input.read(&mut chunk) {
            Ok(0) => return Ok(None),
            Ok(read) => read,
            // A signal the sign-in handles arrived while the read waited.
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(Fail::Unknown),
        };

        // Each chunk is decoded on its own, as Node decodes each `data` Buffer.
        for char in String::from_utf8_lossy(&chunk[..read]).chars() {
            match keys.key(char) {
                Key::Insert(char) => keys.line.push(char),
                Key::Done => return Ok(Some(std::mem::take(&mut keys.line))),
                Key::Closed => return Ok(None),
                Key::Ignored => {}
            }
        }
    }
}

/// The first line of piped input, ended by `\n` or `\r`, or the text before its end; `None` when
/// the input ends empty.
fn read_first_line() -> Option<String> {
    let mut line = Vec::new();
    let mut ended = true;

    for byte in std::io::stdin().lock().bytes() {
        match byte {
            Ok(b'\n' | b'\r') => {
                ended = false;
                break;
            }
            Ok(byte) => line.push(byte),
            Err(_) => break,
        }
    }
    (!(ended && line.is_empty())).then(|| String::from_utf8_lossy(&line).into_owned())
}

/// `prompt` on standard error, then the phone number as typed, before `trim`: `None` when the
/// owner cancelled or the input closed.
pub fn read_phone(prompt: &str) -> Result<Option<String>> {
    match std::io::stdin().is_terminal() {
        true => read_hidden_line(prompt),
        false => {
            show(prompt);
            Ok(read_first_line())
        }
    }
}

/// The terminal mode before the prompt, for a signal that ends the sign-in while the prompt's
/// read blocks and its `Raw` guard can no longer run.
pub struct Saved(Termios);

impl Saved {
    /// The current mode when standard input is a terminal.
    pub fn current() -> Option<Self> {
        let stdin = std::io::stdin();
        stdin
            .is_terminal()
            .then(|| tcgetattr(&stdin).ok().map(Saved))
            .flatten()
    }

    pub fn restore(&self) {
        let _ = tcsetattr(std::io::stdin(), OptionalActions::Now, &self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(input: &str) -> Option<String> {
        let mut keys = Keys {
            line: String::new(),
            escape: Escape::None,
        };

        for char in input.chars() {
            match keys.key(char) {
                Key::Insert(char) => keys.line.push(char),
                Key::Done => return Some(keys.line),
                Key::Closed => return None,
                Key::Ignored => {}
            }
        }
        panic!("the line never ended: {input:?}");
    }

    #[test]
    fn keys_edit_the_line_as_readline_does_at_its_end() {
        assert_eq!(typed("5550000\r").as_deref(), Some("5550000"));
        assert_eq!(typed("55x\u{7f}50000\n").as_deref(), Some("5550000"));
        assert_eq!(typed("12\u{15}5550000\r").as_deref(), Some("5550000"));
        assert_eq!(
            typed("5\u{1b}[D\u{1b}[1;5C\u{1b}OA550000\u{4}\r").as_deref(),
            Some("5550000")
        );
        assert_eq!(typed("\u{1}\t1\r").as_deref(), Some("\t1"));
        assert_eq!(typed("555\u{3}"), None);
        assert_eq!(typed("\u{4}"), None);
        assert_eq!(typed("1\u{8}\u{4}"), None);
    }

    /// Reads `chunks` in turn, then fails as a hung-up Linux terminal does.
    struct HungUp(Vec<std::io::Result<&'static [u8]>>);

    impl Read for HungUp {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            match self.0.is_empty() {
                true => Err(std::io::Error::from_raw_os_error(5)),
                false => self.0.remove(0).map(|chunk| {
                    buffer[..chunk.len()].copy_from_slice(chunk);
                    chunk.len()
                }),
            }
        }
    }

    #[test]
    fn a_failed_read_fails_the_prompt_and_an_ended_input_cancels_it() {
        let interrupted = || Err(std::io::ErrorKind::Interrupted.into());
        let line = |chunks| hidden_line(&mut HungUp(chunks));
        assert!(matches!(line(vec![Ok(b"5550000")]), Err(Fail::Unknown)));
        assert!(matches!(line(vec![Ok(b"555"), Ok(b"")]), Ok(None)));
        assert!(matches!(
            line(vec![Ok(b"555"), interrupted(), Ok(b"0000\r")]),
            Ok(Some(line)) if line == "5550000"
        ));
    }
}
