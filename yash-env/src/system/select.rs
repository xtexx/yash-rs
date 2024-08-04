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

//! [`SelectSystem`] and related items

use super::fd_set::FdSet;
use super::signal;
use super::Errno;
use super::Result;
use super::SigmaskOp;
use super::SignalHandling;
use super::System;
use crate::io::Fd;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::cmp::Reverse;
use std::collections::binary_heap::PeekMut;
use std::collections::BinaryHeap;
use std::ops::Deref;
use std::ops::DerefMut;
use std::rc::Rc;
use std::rc::Weak;
use std::task::Waker;
use std::time::Duration;
use std::time::Instant;

/// [System] extended with internal state to support asynchronous functions.
///
/// `SelectSystem` wraps a `System` instance and manages the internal state for
/// asynchronous I/O, signal handling, and timer functions. It coordinates
/// wakers for asynchronous I/O, signals, and timers to call `select` with the
/// appropriate arguments and wake up the wakers when the corresponding events
/// occur.
#[derive(Debug)]
pub struct SelectSystem {
    /// System instance that performs actual system calls
    system: Box<dyn System>,
    /// Helper for `select`ing on file descriptors
    io: AsyncIo,
    /// Helper for `select`ing on time
    time: AsyncTime,
    /// Helper for `select`ing on signals
    signal: AsyncSignal,
    /// Set of signals passed to `select`
    ///
    /// This is the mask the shell inherited from the parent shell minus the
    /// signals the shell wants to catch.
    wait_mask: Option<Vec<signal::Number>>,
}

impl Deref for SelectSystem {
    type Target = Box<dyn System>;
    fn deref(&self) -> &Box<dyn System> {
        &self.system
    }
}

impl DerefMut for SelectSystem {
    fn deref_mut(&mut self) -> &mut Box<dyn System> {
        &mut self.system
    }
}

impl SelectSystem {
    /// Creates a new `SelectSystem` that wraps the given `System`.
    pub fn new(system: Box<dyn System>) -> Self {
        SelectSystem {
            system,
            io: AsyncIo::new(),
            time: AsyncTime::new(),
            signal: AsyncSignal::new(),
            wait_mask: None,
        }
    }

    /// Calls `sigmask` and updates `self.wait_mask`.
    fn sigmask(&mut self, op: SigmaskOp, signal: signal::Number) -> Result<()> {
        match &mut self.wait_mask {
            None => {
                // This is the first call to sigmask. We need to get the current
                // signal mask (which is the mask inherited from the parent shell) and
                // remove the signal from it.
                let mut mask = Vec::new();
                self.system
                    .sigmask(Some((op, &[signal])), Some(&mut mask))?;
                mask.retain(|&s| s != signal);
                self.wait_mask = Some(mask);
            }
            Some(wait_mask) => {
                // We have already called sigmask. We just need to update the mask.
                self.system.sigmask(Some((op, &[signal])), None)?;
                wait_mask.retain(|&s| s != signal);
            }
        }
        Ok(())
    }

    /// Implements signal handler update.
    ///
    /// See [`SharedSystem::set_signal_handling`].
    pub fn set_signal_handling(
        &mut self,
        signal: signal::Number,
        handling: SignalHandling,
    ) -> Result<SignalHandling> {
        // The order of sigmask and sigaction is important to prevent the signal
        // from being caught. The signal must be caught only when the select
        // function temporarily unblocks the signal. This is to avoid race
        // condition.
        match handling {
            SignalHandling::Default | SignalHandling::Ignore => {
                let old_handling = self.system.sigaction(signal, handling)?;
                self.sigmask(SigmaskOp::Remove, signal)?;
                Ok(old_handling)
            }
            SignalHandling::Catch => {
                self.sigmask(SigmaskOp::Add, signal)?;
                self.system.sigaction(signal, handling)
            }
        }
    }

    /// Registers a waker to be woken when the specified file descriptor is ready for reading.
    pub fn add_reader(&mut self, fd: Fd, waker: Weak<RefCell<Option<Waker>>>) {
        self.io.wait_for_reading(fd, waker)
    }

    /// Registers a waker to be woken when the specified file descriptor is ready for writing.
    pub fn add_writer(&mut self, fd: Fd, waker: Weak<RefCell<Option<Waker>>>) {
        self.io.wait_for_writing(fd, waker)
    }

    /// Registers a waker to be woken when the specified time is reached.
    pub fn add_timeout(&mut self, target: Instant, waker: Weak<RefCell<Option<Waker>>>) {
        self.time.push(Timeout { target, waker })
    }

    /// Registers an awaiter for signals.
    ///
    /// This function returns a reference-counted
    /// `SignalStatus::Expected(None)`. The caller must set a waker to the
    /// returned `SignalStatus::Expected`. When signals are caught, the waker is
    /// woken and replaced with `SignalStatus::Caught(signals)`. The caller can
    /// replace the waker in the `SignalStatus::Expected` with another if the
    /// previous waker gets expired and the caller wants to be woken again.
    pub fn add_signal_waker(&mut self) -> Rc<RefCell<SignalStatus>> {
        self.signal.wait_for_signals()
    }

    fn wake_timeouts(&mut self) {
        if !self.time.is_empty() {
            let now = self.now();
            self.time.wake_if_passed(now);
        }
        self.time.gc();
    }

    fn wake_on_signals(&mut self) {
        let signals = self.system.caught_signals();
        if signals.is_empty() {
            self.signal.gc()
        } else {
            self.signal.wake(&signals.into())
        }
    }

    /// Implements the select function for `SharedSystem`.
    ///
    /// See [`SharedSystem::select`].
    pub fn select(&mut self, poll: bool) -> Result<()> {
        let mut readers = self.io.readers();
        let mut writers = self.io.writers();
        let timeout = if poll {
            Some(Duration::ZERO)
        } else {
            self.time
                .first_target()
                .map(|instant| instant.saturating_duration_since(self.now()))
        };

        let inner_result = self.system.select(
            &mut readers,
            &mut writers,
            timeout,
            self.wait_mask.as_deref(),
        );
        let final_result = match inner_result {
            Ok(_) => {
                self.io.wake(readers, writers);
                Ok(())
            }
            Err(Errno::EBADF) => {
                // Some of the readers and writers are invalid but we cannot
                // tell which, so we wake up everything.
                self.io.wake_all();
                Err(Errno::EBADF)
            }
            Err(Errno::EINTR) => Ok(()),
            Err(error) => Err(error),
        };
        self.io.gc();
        self.wake_timeouts();
        self.wake_on_signals();
        final_result
    }
}

/// Helper for `select`ing on file descriptors
///
/// An `AsyncIo` is a set of [`Waker`]s that are waiting for an FD to be ready for
/// reading or writing. It computes the set of FDs to pass to the `select` system
/// call and wakes the corresponding wakers when the FDs are ready.
#[derive(Clone, Debug, Default)]
struct AsyncIo {
    readers: Vec<FdAwaiter>,
    writers: Vec<FdAwaiter>,
}

#[derive(Clone, Debug)]
struct FdAwaiter {
    fd: Fd,
    waker: Weak<RefCell<Option<Waker>>>,
}

/// Wakes the waker when `FdAwaiter` is dropped.
impl Drop for FdAwaiter {
    fn drop(&mut self) {
        if let Some(waker) = self.waker.upgrade() {
            if let Some(waker) = waker.borrow_mut().take() {
                waker.wake();
            }
        }
    }
}

impl AsyncIo {
    /// Returns a new empty `AsyncIo`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a set of FDs waiting for reading.
    ///
    /// The return value should be passed to the `select` or `pselect` system
    /// call.
    pub fn readers(&self) -> FdSet {
        let mut set = FdSet::new();
        for reader in &self.readers {
            set.insert(reader.fd)
                .expect("file descriptor out of supported range");
        }
        set
    }

    /// Returns a set of FDs waiting for writing.
    ///
    /// The return value should be passed to the `select` or `pselect` system
    /// call.
    pub fn writers(&self) -> FdSet {
        let mut set = FdSet::new();
        for writer in &self.writers {
            set.insert(writer.fd)
                .expect("file descriptor out of supported range");
        }
        set
    }

    /// Adds an awaiter for reading.
    pub fn wait_for_reading(&mut self, fd: Fd, waker: Weak<RefCell<Option<Waker>>>) {
        self.readers.push(FdAwaiter { fd, waker });
    }

    /// Adds an awaiter for writing.
    pub fn wait_for_writing(&mut self, fd: Fd, waker: Weak<RefCell<Option<Waker>>>) {
        self.writers.push(FdAwaiter { fd, waker });
    }

    /// Wakes awaiters that are ready for reading/writing.
    ///
    /// FDs in `readers` and `writers` are considered ready and corresponding
    /// awaiters are woken. Once woken, awaiters are removed from `self`.
    pub fn wake(&mut self, readers: FdSet, writers: FdSet) {
        self.readers.retain(|awaiter| !readers.contains(awaiter.fd));
        self.writers.retain(|awaiter| !writers.contains(awaiter.fd));
    }

    /// Wakes and removes all awaiters.
    pub fn wake_all(&mut self) {
        self.readers.clear();
        self.writers.clear();
    }

    /// Discards `FdAwaiter`s having a defunct waker.
    pub fn gc(&mut self) {
        let is_alive = |awaiter: &FdAwaiter| awaiter.waker.strong_count() > 0;
        self.readers.retain(is_alive);
        self.writers.retain(is_alive);
    }
}

/// Helper for `select`ing on time
///
/// An `AsyncTime` is a set of [`Waker`]s that are waiting for a specific time
/// to come. It wakes the wakers when the time is reached.
#[derive(Clone, Debug, Default)]
struct AsyncTime {
    timeouts: BinaryHeap<Reverse<Timeout>>,
}

#[derive(Clone, Debug)]
struct Timeout {
    target: Instant,
    waker: Weak<RefCell<Option<Waker>>>,
}

impl PartialEq for Timeout {
    fn eq(&self, rhs: &Self) -> bool {
        self.target == rhs.target
    }
}

impl Eq for Timeout {}

impl PartialOrd for Timeout {
    fn partial_cmp(&self, rhs: &Self) -> Option<Ordering> {
        Some(self.cmp(rhs))
    }
}

impl Ord for Timeout {
    fn cmp(&self, rhs: &Self) -> Ordering {
        self.target.cmp(&rhs.target)
    }
}

/// Wakes the waker when `Timeout` is dropped.
impl Drop for Timeout {
    fn drop(&mut self) {
        if let Some(waker) = self.waker.upgrade() {
            if let Some(waker) = waker.borrow_mut().take() {
                waker.wake();
            }
        }
    }
}

impl AsyncTime {
    #[must_use]
    fn new() -> Self {
        Self::default()
    }

    #[must_use]
    fn is_empty(&self) -> bool {
        self.timeouts.is_empty()
    }

    fn push(&mut self, timeout: Timeout) {
        self.timeouts.push(Reverse(timeout))
    }

    #[must_use]
    fn first_target(&self) -> Option<Instant> {
        self.timeouts.peek().map(|timeout| timeout.0.target)
    }

    fn wake_if_passed(&mut self, now: Instant) {
        while let Some(timeout) = self.timeouts.peek_mut() {
            if !timeout.0.passed(now) {
                break;
            }
            PeekMut::pop(timeout);
        }
    }

    fn gc(&mut self) {
        self.timeouts.retain(|t| t.0.waker.strong_count() > 0);
    }
}

impl Timeout {
    fn passed(&self, now: Instant) -> bool {
        self.target <= now
    }
}

/// Helper for `select`ing on signals
///
/// An `AsyncSignal` is a set of [`Waker`]s that are waiting for a signal to be
/// caught by the current process. It wakes the wakers when signals are caught.
#[derive(Clone, Debug, Default)]
struct AsyncSignal {
    awaiters: Vec<Weak<RefCell<SignalStatus>>>,
}

#[derive(Clone, Debug)]
pub enum SignalStatus {
    Expected(Option<Waker>),
    Caught(Rc<[signal::Number]>),
}

impl AsyncSignal {
    /// Returns a new empty `AsyncSignal`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Removes internal weak pointers whose `SignalStatus` has gone.
    pub fn gc(&mut self) {
        self.awaiters.retain(|awaiter| awaiter.strong_count() > 0);
    }

    /// Adds an awaiter for signals.
    ///
    /// This function returns a reference-counted
    /// `SignalStatus::Expected(None)`. The caller must set a waker to the
    /// returned `SignalStatus::Expected`. When signals are caught, the waker is
    /// woken and replaced with `SignalStatus::Caught(signals)`. The caller can
    /// replace the waker in the `SignalStatus::Expected` with another if the
    /// previous waker gets expired and the caller wants to be woken again.
    pub fn wait_for_signals(&mut self) -> Rc<RefCell<SignalStatus>> {
        let status = Rc::new(RefCell::new(SignalStatus::Expected(None)));
        self.awaiters.push(Rc::downgrade(&status));
        status
    }

    /// Wakes awaiters for caught signals.
    ///
    /// This function wakes up all wakers in pending `SignalStatus`es and
    /// removes them from `self`.
    ///
    /// This function borrows `SignalStatus`es returned from `wait_for_signals`
    /// so you must not have conflicting borrows.
    pub fn wake(&mut self, signals: &Rc<[signal::Number]>) {
        for status in self.awaiters.drain(..) {
            let Some(status) = status.upgrade() else {
                continue;
            };
            let mut status_ref = status.borrow_mut();
            let new_status = SignalStatus::Caught(Rc::clone(signals));
            let old_status = std::mem::replace(&mut *status_ref, new_status);
            drop(status_ref);
            if let SignalStatus::Expected(Some(waker)) = old_status {
                waker.wake();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::r#virtual::{SIGCHLD, SIGUSR1};
    use super::*;
    use assert_matches::assert_matches;
    use futures_util::task::noop_waker;

    #[test]
    fn async_io_has_no_default_readers_or_writers() {
        let async_io = AsyncIo::new();
        assert_eq!(async_io.readers(), FdSet::new());
        assert_eq!(async_io.writers(), FdSet::new());
    }

    #[test]
    fn async_io_non_empty_readers_and_writers() {
        let mut async_io = AsyncIo::new();
        let waker = Rc::new(RefCell::new(Some(noop_waker())));
        async_io.wait_for_reading(Fd::STDIN, Rc::downgrade(&waker));
        async_io.wait_for_writing(Fd::STDOUT, Rc::downgrade(&waker));
        async_io.wait_for_writing(Fd::STDERR, Rc::downgrade(&waker));

        let mut expected_readers = FdSet::new();
        expected_readers.insert(Fd::STDIN).unwrap();
        let mut expected_writers = FdSet::new();
        expected_writers.insert(Fd::STDOUT).unwrap();
        expected_writers.insert(Fd::STDERR).unwrap();
        assert_eq!(async_io.readers(), expected_readers);
        assert_eq!(async_io.writers(), expected_writers);
    }

    #[test]
    fn async_io_wake() {
        let mut async_io = AsyncIo::new();
        let waker = Rc::new(RefCell::new(Some(noop_waker())));
        async_io.wait_for_reading(Fd(3), Rc::downgrade(&waker));
        async_io.wait_for_reading(Fd(4), Rc::downgrade(&waker));
        async_io.wait_for_writing(Fd(4), Rc::downgrade(&waker));
        let mut fds = FdSet::new();
        fds.insert(Fd(4)).unwrap();
        async_io.wake(fds, fds);

        let mut expected_readers = FdSet::new();
        expected_readers.insert(Fd(3)).unwrap();
        assert_eq!(async_io.readers(), expected_readers);
        assert_eq!(async_io.writers(), FdSet::new());
    }

    #[test]
    fn async_io_wake_all() {
        let mut async_io = AsyncIo::new();
        let waker = Rc::new(RefCell::new(Some(noop_waker())));
        async_io.wait_for_reading(Fd::STDIN, Rc::downgrade(&waker));
        async_io.wait_for_writing(Fd::STDOUT, Rc::downgrade(&waker));
        async_io.wait_for_writing(Fd::STDERR, Rc::downgrade(&waker));
        async_io.wake_all();
        assert_eq!(async_io.readers(), FdSet::new());
        assert_eq!(async_io.writers(), FdSet::new());
    }

    #[test]
    fn async_time_first_target() {
        let mut async_time = AsyncTime::new();
        let now = Instant::now();
        assert_eq!(async_time.first_target(), None);

        async_time.push(Timeout {
            target: now + Duration::from_secs(2),
            waker: Weak::default(),
        });
        async_time.push(Timeout {
            target: now + Duration::from_secs(1),
            waker: Weak::default(),
        });
        async_time.push(Timeout {
            target: now + Duration::from_secs(3),
            waker: Weak::default(),
        });
        assert_eq!(
            async_time.first_target(),
            Some(now + Duration::from_secs(1))
        );
    }

    #[test]
    fn async_time_wake_if_passed() {
        let mut async_time = AsyncTime::new();
        let now = Instant::now();
        let waker = Rc::new(RefCell::new(Some(noop_waker())));
        async_time.push(Timeout {
            target: now,
            waker: Rc::downgrade(&waker),
        });
        async_time.push(Timeout {
            target: now + Duration::new(1, 0),
            waker: Rc::downgrade(&waker),
        });
        async_time.push(Timeout {
            target: now + Duration::new(1, 1),
            waker: Rc::downgrade(&waker),
        });
        async_time.push(Timeout {
            target: now + Duration::new(2, 0),
            waker: Rc::downgrade(&waker),
        });
        assert_eq!(async_time.timeouts.len(), 4);

        async_time.wake_if_passed(now + Duration::new(1, 0));
        assert_eq!(
            async_time.timeouts.pop().unwrap().0.target,
            now + Duration::new(1, 1)
        );
        assert_eq!(
            async_time.timeouts.pop().unwrap().0.target,
            now + Duration::new(2, 0)
        );
        assert!(async_time.timeouts.is_empty(), "{:?}", async_time.timeouts);
    }

    #[test]
    fn async_signal_wake() {
        let mut async_signal = AsyncSignal::new();
        let status_1 = async_signal.wait_for_signals();
        let status_2 = async_signal.wait_for_signals();
        *status_1.borrow_mut() = SignalStatus::Expected(Some(noop_waker()));
        *status_2.borrow_mut() = SignalStatus::Expected(Some(noop_waker()));

        async_signal.wake(&(Rc::new([SIGCHLD, SIGUSR1]) as Rc<[signal::Number]>));
        assert_matches!(&*status_1.borrow(), SignalStatus::Caught(signals) => {
            assert_eq!(**signals, [SIGCHLD, SIGUSR1]);
        });
        assert_matches!(&*status_2.borrow(), SignalStatus::Caught(signals) => {
            assert_eq!(**signals, [SIGCHLD, SIGUSR1]);
        });
    }
}
