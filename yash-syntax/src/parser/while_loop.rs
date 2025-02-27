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

//! Syntax parser for while and until loops

use super::core::Parser;
use super::core::Result;
use super::error::Error;
use super::error::SyntaxError;
use super::lex::Keyword::{Until, While};
use super::lex::TokenId::Token;
use crate::syntax::CompoundCommand;

impl Parser<'_, '_> {
    /// Parses a while loop.
    ///
    /// The next token must be the `while` reserved word.
    ///
    /// # Panics
    ///
    /// If the first token is not `while`.
    pub async fn while_loop(&mut self) -> Result<CompoundCommand> {
        let open = self.take_token_raw().await?;
        assert_eq!(open.id, Token(Some(While)));

        let condition = self.maybe_compound_list_boxed().await?;

        // TODO allow empty condition if not POSIXly-correct
        if condition.0.is_empty() {
            let cause = SyntaxError::EmptyWhileCondition.into();
            let location = self.take_token_raw().await?.word.location;
            return Err(Error { cause, location });
        }

        let body = match self.do_clause().await? {
            Some(body) => body,
            None => {
                let opening_location = open.word.location;
                let cause = SyntaxError::UnclosedWhileClause { opening_location }.into();
                let location = self.take_token_raw().await?.word.location;
                return Err(Error { cause, location });
            }
        };

        Ok(CompoundCommand::While { condition, body })
    }

    /// Parses an until loop.
    ///
    /// The next token must be the `until` reserved word.
    ///
    /// # Panics
    ///
    /// If the first token is not `until`.
    pub async fn until_loop(&mut self) -> Result<CompoundCommand> {
        let open = self.take_token_raw().await?;
        assert_eq!(open.id, Token(Some(Until)));

        let condition = self.maybe_compound_list_boxed().await?;

        // TODO allow empty condition if not POSIXly-correct
        if condition.0.is_empty() {
            let cause = SyntaxError::EmptyUntilCondition.into();
            let location = self.take_token_raw().await?.word.location;
            return Err(Error { cause, location });
        }

        let body = match self.do_clause().await? {
            Some(body) => body,
            None => {
                let opening_location = open.word.location;
                let cause = SyntaxError::UnclosedUntilClause { opening_location }.into();
                let location = self.take_token_raw().await?.word.location;
                return Err(Error { cause, location });
            }
        };

        Ok(CompoundCommand::Until { condition, body })
    }
}

#[cfg(test)]
mod tests {
    use super::super::error::ErrorCause;
    use super::super::lex::Lexer;
    use super::super::lex::TokenId::EndOfInput;
    use super::*;
    use crate::alias::{AliasSet, HashEntry};
    use crate::source::Location;
    use crate::source::Source;
    use assert_matches::assert_matches;
    use futures_util::FutureExt;

    #[test]
    fn parser_while_loop_short() {
        let mut lexer = Lexer::with_code("while true; do :; done");
        let mut parser = Parser::new(&mut lexer);

        let result = parser.compound_command().now_or_never().unwrap();
        let compound_command = result.unwrap().unwrap();
        assert_matches!(compound_command, CompoundCommand::While { condition, body } => {
            assert_eq!(condition.to_string(), "true");
            assert_eq!(body.to_string(), ":");
        });

        let next = parser.peek_token().now_or_never().unwrap().unwrap();
        assert_eq!(next.id, EndOfInput);
    }

    #[test]
    fn parser_while_loop_long() {
        let mut lexer = Lexer::with_code("while false; true& do foo; bar& done");
        let mut parser = Parser::new(&mut lexer);

        let result = parser.compound_command().now_or_never().unwrap();
        let compound_command = result.unwrap().unwrap();
        assert_matches!(compound_command, CompoundCommand::While { condition, body } => {
            assert_eq!(condition.to_string(), "false; true&");
            assert_eq!(body.to_string(), "foo; bar&");
        });

        let next = parser.peek_token().now_or_never().unwrap().unwrap();
        assert_eq!(next.id, EndOfInput);
    }

    #[test]
    fn parser_while_loop_unclosed() {
        let mut lexer = Lexer::with_code("while :");
        let mut parser = Parser::new(&mut lexer);

        let result = parser.compound_command().now_or_never().unwrap();
        let e = result.unwrap_err();
        assert_matches!(e.cause,
            ErrorCause::Syntax(SyntaxError::UnclosedWhileClause { opening_location }) => {
            assert_eq!(*opening_location.code.value.borrow(), "while :");
            assert_eq!(opening_location.code.start_line_number.get(), 1);
            assert_eq!(*opening_location.code.source, Source::Unknown);
            assert_eq!(opening_location.range, 0..5);
        });
        assert_eq!(*e.location.code.value.borrow(), "while :");
        assert_eq!(e.location.code.start_line_number.get(), 1);
        assert_eq!(*e.location.code.source, Source::Unknown);
        assert_eq!(e.location.range, 7..7);
    }

    #[test]
    fn parser_while_loop_empty_posix() {
        let mut lexer = Lexer::with_code(" while do :; done");
        let mut parser = Parser::new(&mut lexer);

        let result = parser.compound_command().now_or_never().unwrap();
        let e = result.unwrap_err();
        assert_eq!(
            e.cause,
            ErrorCause::Syntax(SyntaxError::EmptyWhileCondition)
        );
        assert_eq!(*e.location.code.value.borrow(), " while do :; done");
        assert_eq!(e.location.code.start_line_number.get(), 1);
        assert_eq!(*e.location.code.source, Source::Unknown);
        assert_eq!(e.location.range, 7..9);
    }

    #[test]
    fn parser_while_loop_aliasing() {
        let mut lexer = Lexer::with_code(" while :; DO :; done");
        #[allow(clippy::mutable_key_type)]
        let mut aliases = AliasSet::new();
        let origin = Location::dummy("");
        aliases.insert(HashEntry::new(
            "DO".to_string(),
            "do".to_string(),
            false,
            origin.clone(),
        ));
        aliases.insert(HashEntry::new(
            "while".to_string(),
            ";;".to_string(),
            false,
            origin,
        ));
        let mut parser = Parser::config().aliases(&aliases).input(&mut lexer);

        let result = parser.compound_command().now_or_never().unwrap();
        let compound_command = result.unwrap().unwrap();
        assert_eq!(compound_command.to_string(), "while :; do :; done");

        let next = parser.peek_token().now_or_never().unwrap().unwrap();
        assert_eq!(next.id, EndOfInput);
    }

    #[test]
    fn parser_until_loop_short() {
        let mut lexer = Lexer::with_code("until true; do :; done");
        let mut parser = Parser::new(&mut lexer);

        let result = parser.compound_command().now_or_never().unwrap();
        let compound_command = result.unwrap().unwrap();
        assert_matches!(compound_command, CompoundCommand::Until { condition, body } => {
            assert_eq!(condition.to_string(), "true");
            assert_eq!(body.to_string(), ":");
        });

        let next = parser.peek_token().now_or_never().unwrap().unwrap();
        assert_eq!(next.id, EndOfInput);
    }

    #[test]
    fn parser_until_loop_long() {
        let mut lexer = Lexer::with_code("until false; true& do foo; bar& done");
        let mut parser = Parser::new(&mut lexer);

        let result = parser.compound_command().now_or_never().unwrap();
        let compound_command = result.unwrap().unwrap();
        assert_matches!(compound_command, CompoundCommand::Until { condition, body } => {
            assert_eq!(condition.to_string(), "false; true&");
            assert_eq!(body.to_string(), "foo; bar&");
        });

        let next = parser.peek_token().now_or_never().unwrap().unwrap();
        assert_eq!(next.id, EndOfInput);
    }

    #[test]
    fn parser_until_loop_unclosed() {
        let mut lexer = Lexer::with_code("until :");
        let mut parser = Parser::new(&mut lexer);

        let result = parser.compound_command().now_or_never().unwrap();
        let e = result.unwrap_err();
        assert_matches!(e.cause,
            ErrorCause::Syntax(SyntaxError::UnclosedUntilClause { opening_location }) => {
            assert_eq!(*opening_location.code.value.borrow(), "until :");
            assert_eq!(opening_location.code.start_line_number.get(), 1);
            assert_eq!(*opening_location.code.source, Source::Unknown);
            assert_eq!(opening_location.range, 0..5);
        });
        assert_eq!(*e.location.code.value.borrow(), "until :");
        assert_eq!(e.location.code.start_line_number.get(), 1);
        assert_eq!(*e.location.code.source, Source::Unknown);
        assert_eq!(e.location.range, 7..7);
    }

    #[test]
    fn parser_until_loop_empty_posix() {
        let mut lexer = Lexer::with_code("  until do :; done");
        let mut parser = Parser::new(&mut lexer);

        let result = parser.compound_command().now_or_never().unwrap();
        let e = result.unwrap_err();
        assert_eq!(
            e.cause,
            ErrorCause::Syntax(SyntaxError::EmptyUntilCondition)
        );
        assert_eq!(*e.location.code.value.borrow(), "  until do :; done");
        assert_eq!(e.location.code.start_line_number.get(), 1);
        assert_eq!(*e.location.code.source, Source::Unknown);
        assert_eq!(e.location.range, 8..10);
    }

    #[test]
    fn parser_until_loop_aliasing() {
        let mut lexer = Lexer::with_code(" until :; DO :; done");
        #[allow(clippy::mutable_key_type)]
        let mut aliases = AliasSet::new();
        let origin = Location::dummy("");
        aliases.insert(HashEntry::new(
            "DO".to_string(),
            "do".to_string(),
            false,
            origin.clone(),
        ));
        aliases.insert(HashEntry::new(
            "until".to_string(),
            ";;".to_string(),
            false,
            origin,
        ));
        let mut parser = Parser::config().aliases(&aliases).input(&mut lexer);

        let result = parser.compound_command().now_or_never().unwrap();
        let compound_command = result.unwrap().unwrap();
        assert_eq!(compound_command.to_string(), "until :; do :; done");

        let next = parser.peek_token().now_or_never().unwrap().unwrap();
        assert_eq!(next.id, EndOfInput);
    }
}
