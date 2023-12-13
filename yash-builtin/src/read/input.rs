// This file is part of yash, an extended POSIX shell.
// Copyright (C) 2023 WATANABE Yuki
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Reading input

use thiserror::Error;
use yash_env::system::Errno;
use yash_env::Env;
use yash_semantics::expansion::attr::AttrChar;
use yash_semantics::expansion::attr::Origin;
use yash_syntax::source::pretty::AnnotationType;
use yash_syntax::source::pretty::Message;
use yash_syntax::syntax::Fd;

/// Error reading from the standard input
///
/// This error is returned by [`read`] when an error occurs while reading from
/// the standard input.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("error reading from the standard input: {errno}")]
pub struct Error {
    #[from]
    pub errno: Errno,
}

impl Error {
    /// Converts this error to a message.
    #[must_use]
    pub fn to_message(&self) -> Message {
        Message {
            r#type: AnnotationType::Error,
            title: self.to_string().into(),
            annotations: vec![],
        }
    }
}

impl<'a> From<&'a Error> for Message<'a> {
    #[inline]
    fn from(error: &'a Error) -> Self {
        error.to_message()
    }
}

fn quoted(value: char) -> AttrChar {
    AttrChar {
        value,
        origin: Origin::SoftExpansion,
        is_quoted: true,
        is_quoting: false,
    }
}

fn quoting(value: char) -> AttrChar {
    AttrChar {
        value,
        origin: Origin::SoftExpansion,
        is_quoted: false,
        is_quoting: true,
    }
}

fn plain(value: char) -> AttrChar {
    AttrChar {
        value,
        origin: Origin::SoftExpansion,
        is_quoted: false,
        is_quoting: false,
    }
}

/// Reads a line from the standard input.
///
/// This function reads a line from the standard input and returns a vector of
/// [`AttrChar`]s representing the line. The line is terminated by a newline
/// character, which is not included in the returned vector.
///
/// If `is_raw` is `true`, the read line is not subject to backslash processing.
/// Otherwise, backslash-newline pairs are treated as line continuations, and
/// other backslashes are treated as quoting characters. On encountering a line
/// continuation, this function removes the backslash-newline pair and continues
/// reading the next line. When reading the second and subsequent lines, this
/// function displays the value of the `PS2` variable as a prompt if the shell
/// is interactive and the input is from a terminal.
pub async fn read(env: &mut Env, is_raw: bool) -> Result<Vec<AttrChar>, Error> {
    let mut result = Vec::new();

    loop {
        // TODO Read in bulk if the standard input is seekable
        match read_char(env).await? {
            None | Some('\n') => break,

            // Backslash escape
            Some('\\') if !is_raw => {
                let c = read_char(env).await?;
                if c == Some('\n') {
                    // Line continuation
                    // TODO Display $PS2
                    continue;
                }
                result.push(quoting('\\'));
                match c {
                    None => break,
                    Some(c) => result.push(quoted(c)),
                }
            }

            // Plain character
            Some(c) => result.push(plain(c)),
        }
    }

    Ok(result)
}

/// Reads one character from the standard input.
///
/// This function reads a single UTF-8-encoded character from the standard
/// input. If the standard input is empty, this function returns `Ok(None)`.
/// If the input is not a valid UTF-8 sequence, this function returns an error.
async fn read_char(env: &mut Env) -> Result<Option<char>, Error> {
    // Any character is at most 4 bytes in UTF-8.
    let mut buffer = [0; 4];
    let mut len = 0;
    loop {
        // Read from the standard input byte by byte so that we don't consume
        // more than one character.
        let byte = std::slice::from_mut(&mut buffer[len]);
        let count = env.system.read_async(Fd::STDIN, byte).await?;
        if count == 0 {
            // End of input
            return if len == 0 {
                Ok(None)
            } else {
                // The input ended in the middle of a UTF-8 sequence.
                Err(Errno::EILSEQ.into())
            };
        }
        debug_assert_eq!(count, 1);
        len += 1;

        match std::str::from_utf8(&buffer[..len]) {
            Ok(s) => {
                let mut chars = s.chars();
                // Since the buffer is not empty, there must be a character.
                let c = chars.next().unwrap();
                // And it must be the only character.
                debug_assert_eq!(chars.next(), None);
                return Ok(Some(c));
            }
            Err(e) => match e.error_len() {
                None => {
                    // The bytes in the buffer are incomplete for a UTF-8
                    // character. Read more bytes.
                    continue;
                }
                Some(_) => return Err(Errno::EILSEQ.into()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::in_virtual_system;
    use std::cell::RefCell;
    use yash_env::system::r#virtual::FileBody;
    use yash_env::system::r#virtual::SystemState;

    fn set_stdin<B: Into<Vec<u8>>>(system: &RefCell<SystemState>, bytes: B) {
        let state = system.borrow_mut();
        let stdin = state.file_system.get("/dev/stdin").unwrap();
        stdin.borrow_mut().body = FileBody::new(bytes);
    }

    fn attr_chars(s: &str) -> Vec<AttrChar> {
        s.chars().map(plain).collect()
    }

    #[test]
    fn empty_input() {
        in_virtual_system(|mut env, _| async move {
            let result = read(&mut env, false).await;
            assert_eq!(result, Ok(vec![]));
        })
    }

    #[test]
    fn non_empty_input() {
        in_virtual_system(|mut env, system| async move {
            set_stdin(&system, "foo\nbar\n");

            let result = read(&mut env, false).await;
            assert_eq!(result, Ok(attr_chars("foo")));

            let result = read(&mut env, false).await;
            assert_eq!(result, Ok(attr_chars("bar")));

            let result = read(&mut env, false).await;
            assert_eq!(result, Ok(vec![]));
        })
    }

    #[test]
    fn input_without_newline() {
        in_virtual_system(|mut env, system| async move {
            set_stdin(&system, "newline");

            let result = read(&mut env, false).await;
            assert_eq!(result, Ok(attr_chars("newline")));

            let result = read(&mut env, false).await;
            assert_eq!(result, Ok(vec![]));
        })
    }

    #[test]
    fn multibyte_characters() {
        in_virtual_system(|mut env, system| async move {
            set_stdin(&system, "©⁉😀\n");

            let result = read(&mut env, false).await;
            assert_eq!(result, Ok(attr_chars("©⁉😀")));

            let result = read(&mut env, false).await;
            assert_eq!(result, Ok(vec![]));
        })
    }

    #[test]
    fn raw_mode() {
        in_virtual_system(|mut env, system| async move {
            set_stdin(&system, "\\foo\\\nbar\\\nbaz\n");

            let result = read(&mut env, true).await;
            assert_eq!(result, Ok(attr_chars("\\foo\\")));
        })
    }

    #[test]
    fn no_raw_mode() {
        in_virtual_system(|mut env, system| async move {
            set_stdin(&system, "\\foo\\\nbar\\\nbaz\n");

            let result = read(&mut env, false).await;
            assert_eq!(
                result,
                Ok(vec![
                    quoting('\\'),
                    quoted('f'),
                    plain('o'),
                    plain('o'),
                    plain('b'),
                    plain('a'),
                    plain('r'),
                    plain('b'),
                    plain('a'),
                    plain('z'),
                ]),
            );
        })
    }

    #[test]
    fn orphan_backslash() {
        in_virtual_system(|mut env, system| async move {
            set_stdin(&system, "foo\\");

            let result = read(&mut env, false).await;
            assert_eq!(
                result,
                Ok(vec![plain('f'), plain('o'), plain('o'), quoting('\\'),]),
            );
        })
    }

    #[test]
    fn broken_utf8() {
        in_virtual_system(|mut env, system| async move {
            set_stdin(&system, *b"\xFF");

            let result = read(&mut env, false).await;
            assert_eq!(result, Err(Errno::EILSEQ.into()));
        });

        in_virtual_system(|mut env, system| async move {
            set_stdin(&system, *b"\xCF\xD0");

            let result = read(&mut env, false).await;
            assert_eq!(result, Err(Errno::EILSEQ.into()));
        });

        in_virtual_system(|mut env, system| async move {
            set_stdin(&system, *b"\xCF");

            let result = read(&mut env, false).await;
            assert_eq!(result, Err(Errno::EILSEQ.into()));
        });
    }
}
