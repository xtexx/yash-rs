// This file is part of yash, an extended POSIX shell.
// Copyright (C) 2020 WATANABE Yuki
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

//! Syntax parser for the shell language.
//!
//! TODO Elaborate

mod core;
mod fill;
mod lex;

use self::lex::Operator::*;
use self::lex::TokenId::*;
use super::syntax::*;

pub use self::core::AsyncFnMut;
pub use self::core::AsyncFnOnce;
pub use self::core::Error;
pub use self::core::ErrorCause;
pub use self::core::Parser;
pub use self::core::Result;
pub use self::fill::Fill;
pub use self::fill::MissingHereDoc;
pub use self::lex::Lexer;
pub use self::lex::Token;

impl Parser<'_> {
    /// Parses a redirection.
    ///
    /// If the current token is not a redirection operator, an [unknown](ErrorCause::Unknown) error
    /// is returned.
    pub async fn redirection(&mut self) -> Result<Redir<MissingHereDoc>> {
        // TODO IO_NUMBER
        let operator = match self.peek_token().await {
            Ok(token) => match token.id {
                // TODO <, <>, >, >>, >|, <&, >&, >>|, <<<
                Operator(op) if op == LessLess || op == LessLessDash => {
                    self.take_token().await.unwrap()
                }
                _ => {
                    return Err(Error {
                        cause: ErrorCause::Unknown,
                        location: token.word.location.clone(),
                    })
                }
            },
            Err(_) => return Err(self.take_token().await.unwrap_err()),
        };

        let operand = self.take_token().await?;
        match operand.id {
            Token => (),
            Operator(_) => {
                return Err(Error {
                    cause: ErrorCause::MissingHereDocDelimiter,
                    location: operator.word.location,
                })
            }
            // TODO what if the operand is missing (end of input)
            // TODO IoNumber => reject if posixly-correct,
        }

        Ok(Redir {
            fd: None,
            body: RedirBody::HereDoc(MissingHereDoc),
        })
    }

    /// Parses a simple command.
    pub async fn simple_command(&mut self) -> Result<SimpleCommand<MissingHereDoc>> {
        // TODO Support assignments and redirections. Stop on a delimiter token.
        let mut words = vec![];
        loop {
            let token = self.take_token().await;
            if let Err(Error {
                cause: ErrorCause::EndOfInput,
                ..
            }) = token
            {
                break;
            }
            words.push(token?.word);
        }
        Ok(SimpleCommand {
            words,
            redirs: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Source;
    use futures::executor::block_on;

    #[test]
    fn parser_redirection_lessless() {
        let mut lexer = Lexer::with_source(Source::Unknown, "<<end ");
        let mut parser = Parser::new(&mut lexer);

        let redir = block_on(parser.redirection()).unwrap();
        assert_eq!(redir.fd, None);
        assert_eq!(redir.body, RedirBody::HereDoc(MissingHereDoc));
        // TODO pending here-doc content
    }

    #[test]
    fn parser_redirection_lesslessdash() {
        let mut lexer = Lexer::with_source(Source::Unknown, "<<-end ");
        let mut parser = Parser::new(&mut lexer);

        let redir = block_on(parser.redirection()).unwrap();
        assert_eq!(redir.fd, None);
        assert_eq!(redir.body, RedirBody::HereDoc(MissingHereDoc));
        // TODO pending here-doc content
    }

    #[test]
    fn parser_redirection_not_operator() {
        let mut lexer = Lexer::with_source(Source::Unknown, "x");
        let mut parser = Parser::new(&mut lexer);

        let e = block_on(parser.redirection()).unwrap_err();
        assert_eq!(e.cause, ErrorCause::Unknown);
        assert_eq!(e.location.line.value, "x");
        assert_eq!(e.location.line.number.get(), 1);
        assert_eq!(e.location.line.source, Source::Unknown);
        assert_eq!(e.location.column.get(), 1);
    }

    #[test]
    fn parser_redirection_not_heredoc_delimiter() {
        let mut lexer = Lexer::with_source(Source::Unknown, "<< <<");
        let mut parser = Parser::new(&mut lexer);

        let e = block_on(parser.redirection()).unwrap_err();
        assert_eq!(e.cause, ErrorCause::MissingHereDocDelimiter);
        assert_eq!(e.location.line.value, "<< <<");
        assert_eq!(e.location.line.number.get(), 1);
        assert_eq!(e.location.line.source, Source::Unknown);
        assert_eq!(e.location.column.get(), 1);
    }

    #[test]
    fn parser_redirection_eof_heredoc_delimiter() {
        let mut lexer = Lexer::with_source(Source::Unknown, "<<");
        let mut parser = Parser::new(&mut lexer);

        let e = block_on(parser.redirection()).unwrap_err();
        assert_eq!(e.cause, ErrorCause::EndOfInput);
        assert_eq!(e.location.line.value, "<<");
        assert_eq!(e.location.line.number.get(), 1);
        assert_eq!(e.location.line.source, Source::Unknown);
        assert_eq!(e.location.column.get(), 3);
    }
}
