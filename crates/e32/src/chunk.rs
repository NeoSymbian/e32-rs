// SPDX-License-Identifier: Apache-2.0
//! Memory shared between processes: an unnamed global chunk, handed over by handle (e.g. an app's drawing surface).

use crate::sys::{self, TChunkCreate, TOwnerType};

/// A chunk mapped in this process. On the multiple memory model a global chunk sits at the same address in every
/// process that has a handle to it. Closed on drop.
#[derive(Debug)]
pub struct Chunk {
    handle: i32,
    base: *mut u8,
}

impl Chunk {
    /// A new unnamed global chunk with `size` bytes committed (zeroed). Its handle can be given to another process
    /// with `Message::complete_with_handle`; a local chunk could not be shared (`sipc.cpp:2071`).
    pub fn create_global(size: usize) -> Result<Self, i32> {
        let info = TChunkCreate {
            i_att: sys::ECHUNK_GLOBAL,
            i_initial_top: size as i32,
            i_max_size: size as i32,
            ..TChunkCreate::default()
        };
        let h = sys::chunk_create(TOwnerType::Process, &info);
        if h < 0 {
            return Err(h);
        }
        Ok(Self::from_handle(h))
    }

    /// Take ownership of a chunk handle received from another process (e.g. as a request's completion value).
    pub fn from_handle(handle: i32) -> Self {
        Self {
            handle,
            base: sys::chunk_base(handle),
        }
    }

    pub fn handle(&self) -> i32 {
        self.handle
    }

    /// Where the chunk is mapped (the same address in every process that maps it).
    pub fn base(&self) -> *mut u8 {
        self.base
    }
}

impl Drop for Chunk {
    fn drop(&mut self) {
        sys::handle_close(self.handle);
    }
}
