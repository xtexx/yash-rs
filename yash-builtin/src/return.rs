// This file is part of yash, an extended POSIX shell.
// Copyright (C) 2021 WATANABE Yuki
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

//! Return built-in.
//!
//! The **`return`** built-in quits the currently executing innermost function
//! or script.
//!
//! # Syntax
//!
//! ```sh
//! return [-n] [exit_status]
//! ```
//!
//! # Semantics
//!
//! `return exit_status` makes the shell return from the currently executing
//! function or script with the specified exit status.
//!
//! # Options
//!
//! The **`-n`** (**`--no-return`**) option makes the built-in not actually quit
//! a function or script. This option will be helpful when you want to set the
//! exit status to an arbitrary value without any other side effect.
//!
//! # Operands
//!
//! The optional ***exit_status*** operand, if given, should be a non-negative
//! decimal integer and will be the exit status of the built-in.
//!
//! # Exit status
//!
//! The *exit_status* operand will be the exit status of the built-in.
//!
//! If the operand is not given, the exit status will be the current exit status
//! (`$?`). If the built-in is invoked in a trap executed in a function or
//! script and the built-in returns from that function or script, the exit
//! status will be the value of `$?` before entering the trap.
//!
//! # Errors
//!
//! If the *exit_status* operand is given but not a valid non-negative integer,
//! it is a syntax error. In that case, an error message is printed, and the
//! exit status will be 2 ([`ExitStatus::ERROR`]).
//!
//! This implementation treats an *exit_status* value greater than 2147483647 as
//! a syntax error.
//!
//! TODO: What if there is no function or script to return from?
//!
//! # Portability
//!
//! POSIX only requires the return built-in to quit a function or dot script.
//! The behavior for other kinds of scripts is a non-standard extension.
//!
//! The `-n` (`--no-return`) option is a non-standard extension.
//!
//! The behavior is unspecified in POSIX if *exit_status* is greater than 255.
//! The current implementation passes such a value as is in the result, but this
//! behavior may change in the future.
//!
//! # Implementation notes
//!
//! This implementation of the built-in does not actually quit the current
//! function or dot script, but returns a [`Result`] having a
//! [`Divert::Return`]. The caller is responsible for handling the divert value
//! and returning from the function or script.
//!
//! - If an operand specifies an exit status, the divert value will contain the
//! specified exit status. The caller should use it as the exit status of the
//! process.
//! - If no operand is given, the divert value will contain no exit status. The
//! built-in's exit status is the current value of `$?`, and the caller should
//! use it as the exit status of the function or script. However, if the
//! built-in is invoked in a trap executed in the function or script, the caller
//! should use the value of `$?` before entering trap.

use std::future::Future;
use std::ops::ControlFlow::Break;
use std::pin::Pin;
use yash_env::builtin::Result;
use yash_env::semantics::Divert;
use yash_env::semantics::ExitStatus;
use yash_env::semantics::Field;
use yash_env::Env;

/// Implementation of the return built-in.
///
/// See the [module-level documentation](self) for details.
pub async fn builtin_body(env: &mut Env, args: Vec<Field>) -> Result {
    // TODO: POSIX does not require the return built-in to support XBD Utility
    // Syntax Guidelines. That means the built-in does not have to recognize the
    // "--" separator. We should reject the separator in the POSIXly-correct
    // mode.
    // TODO Reject returning from an interactive session
    let mut i = args.iter().peekable();
    let no_return = i.next_if(|field| field.value == "-n").is_some();
    let exit_status = match i.next() {
        Some(field) => Some(ExitStatus(field.value.parse().expect("TODO"))),
        None => None,
    };
    if no_return {
        Result::new(exit_status.unwrap_or(env.exit_status))
    } else {
        let mut result = Result::new(env.exit_status);
        result.set_divert(Break(Divert::Return(exit_status)));
        result
    }
}

/// Implementation of the return built-in.
///
/// This function calls [`builtin_body`] and wraps the result in a `Box`.
pub fn builtin_main(
    env: &mut yash_env::Env,
    args: Vec<Field>,
) -> Pin<Box<dyn Future<Output = Result> + '_>> {
    Box::pin(builtin_body(env, args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;
    use yash_env::semantics::ExitStatus;

    #[test]
    fn return_without_arguments_with_exit_status_0() {
        let mut env = Env::new_virtual();
        let actual_result = builtin_body(&mut env, vec![]).now_or_never().unwrap();
        let mut expected_result = Result::default();
        expected_result.set_divert(Break(Divert::Return(None)));
        assert_eq!(actual_result, expected_result);
    }

    #[test]
    fn return_without_arguments_with_non_zero_exit_status() {
        let mut env = Env::new_virtual();
        env.exit_status = ExitStatus(42);
        let actual_result = builtin_body(&mut env, vec![]).now_or_never().unwrap();
        let mut expected_result = Result::new(ExitStatus(42));
        expected_result.set_divert(Break(Divert::Return(None)));
        assert_eq!(actual_result, expected_result);
    }

    #[test]
    fn returns_exit_status_specified_without_n_option() {
        let mut env = Env::new_virtual();
        let args = Field::dummies(["42"]);
        let actual_result = builtin_body(&mut env, args).now_or_never().unwrap();
        let mut expected_result = Result::default();
        expected_result.set_divert(Break(Divert::Return(Some(ExitStatus(42)))));
        assert_eq!(actual_result, expected_result);
    }

    #[test]
    fn returns_exit_status_12_with_n_option() {
        let mut env = Env::new_virtual();
        let args = Field::dummies(["-n", "12"]);
        let result = builtin_body(&mut env, args).now_or_never().unwrap();
        assert_eq!(result, Result::new(ExitStatus(12)));
    }

    #[test]
    fn returns_exit_status_47_with_n_option() {
        let mut env = Env::new_virtual();
        let args = Field::dummies(["-n", "47"]);
        let result = builtin_body(&mut env, args).now_or_never().unwrap();
        assert_eq!(result, Result::new(ExitStatus(47)));
    }
}
