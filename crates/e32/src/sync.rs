// SPDX-License-Identifier: Apache-2.0
//! Locks on kernel objects: a mutex protecting data, and a counting semaphore. Process-owned handles, so every
//! thread of the process can use them.

use crate::sys::{self, TOwnerType};
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};

fn check(h: i32) -> Result<i32, i32> {
    if h < 0 { Err(h) } else { Ok(h) }
}

/// `RMutex` around a value.
pub struct Mutex<T> {
    handle: i32,
    data: UnsafeCell<T>,
}

// SAFETY: the kernel mutex gives exclusive access to `data`.
unsafe impl<T: Send> Send for Mutex<T> {}
// SAFETY: as above.
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    pub fn new(value: T) -> Result<Self, i32> {
        Ok(Self {
            handle: check(sys::mutex_create(TOwnerType::Process))?,
            data: UnsafeCell::new(value),
        })
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        sys::mutex_wait(self.handle, 0);
        MutexGuard { m: self }
    }
}

impl<T> Drop for Mutex<T> {
    fn drop(&mut self) {
        sys::handle_close(self.handle);
    }
}

pub struct MutexGuard<'a, T> {
    m: &'a Mutex<T>,
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: the guard holds the kernel mutex.
        unsafe { &*self.m.data.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard holds the kernel mutex.
        unsafe { &mut *self.m.data.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        sys::mutex_signal(self.m.handle);
    }
}

/// `RSemaphore`: a counting semaphore.
pub struct Semaphore {
    handle: i32,
}

impl Semaphore {
    pub fn new(count: i32) -> Result<Self, i32> {
        Ok(Self {
            handle: check(sys::semaphore_create(count, TOwnerType::Process))?,
        })
    }
    /// Take one count, waiting as long as needed.
    pub fn wait(&self) {
        sys::semaphore_wait(self.handle, 0);
    }
    /// Take one count, waiting at most `micros`; false on time-out.
    pub fn wait_timeout(&self, micros: i32) -> bool {
        sys::semaphore_wait(self.handle, micros.max(1)) == 0
    }
    pub fn signal(&self) {
        sys::semaphore_signal1(self.handle);
    }
}

// SAFETY: a kernel semaphore handle is usable from every thread of the process that owns it.
unsafe impl Send for Semaphore {}
// SAFETY: as above.
unsafe impl Sync for Semaphore {}

impl Drop for Semaphore {
    fn drop(&mut self) {
        sys::handle_close(self.handle);
    }
}
