// SPDX-License-Identifier: Apache-2.0
//! The global allocator: a first-fit heap in a private chunk, grown with `ChunkAdjust`.
//!
//! euser's `RHeap` (the one the image header sizes) is left to euser's own runtime; Rust allocations never touch it.
//! The chunk is process-owned and its base never moves (multiple memory model), so the heap only grows upwards.

use crate::sys::{self, TChunkAdjust, TChunkCreate, TOwnerType};
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::{self, NonNull};
use linked_list_allocator::Heap;

/// Address space reserved for the heap (committed lazily).
const RESERVE: usize = 16 << 20;
/// First commit, and the smallest growth step.
const INITIAL: usize = 64 << 10;
const PAGE: usize = 4 << 10;

struct State {
    heap: Heap,
    chunk: i32,
    committed: usize,
    // shortcut: one kernel mutex round trip per alloc/free (ARMv5TE has no CAS for a user-side fast lock); use an
    // LDREX/STREX lock once there is an ARMv7 build.
    lock: i32,
}

struct ChunkHeap(UnsafeCell<State>);

// SAFETY: every access to the state goes through the kernel mutex (`with_lock`), after `init` on the main thread.
unsafe impl Sync for ChunkHeap {}

#[global_allocator]
static HEAP: ChunkHeap = ChunkHeap(UnsafeCell::new(State {
    heap: Heap::empty(),
    chunk: 0,
    committed: 0,
    lock: 0,
}));

/// Create the chunk, the heap and its lock. Called once by `e32::main!` on the main thread, before any thread exists.
pub fn init() {
    // SAFETY: single-threaded here (see above).
    let st = unsafe { &mut *HEAP.0.get() };
    if st.chunk > 0 {
        return;
    }
    let info = TChunkCreate {
        i_att: 0, // ENormal, local
        i_initial_top: INITIAL as i32,
        i_max_size: RESERVE as i32,
        i_clear_byte: sys::KCHUNK_CLEAR_BYTE_DEFAULT,
        ..Default::default()
    };
    let chunk = sys::chunk_create(TOwnerType::Process, &info);
    let lock = sys::mutex_create(TOwnerType::Process);
    if chunk < 0 || lock < 0 {
        crate::process::panic("RUST-HEAP", if chunk < 0 { chunk } else { lock });
    }
    st.chunk = chunk;
    st.lock = lock;
    st.committed = INITIAL;
    // SAFETY: the chunk's first INITIAL bytes are committed, owned by this process and used by nothing else.
    unsafe { st.heap.init(sys::chunk_base(chunk), INITIAL) };
}

impl ChunkHeap {
    fn with_lock<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        // SAFETY: `init` ran before any allocation; the mutex serialises the threads.
        let st = unsafe { &mut *self.0.get() };
        sys::mutex_wait(st.lock, 0);
        let r = f(st);
        sys::mutex_signal(st.lock);
        r
    }
}

fn round_up(n: usize, to: usize) -> usize {
    n.div_ceil(to) * to
}

unsafe impl GlobalAlloc for ChunkHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.with_lock(|st| {
            if let Ok(p) = st.heap.allocate_first_fit(layout) {
                return p.as_ptr();
            }
            // Grow by at least what this request needs (plus alignment slack), in INITIAL steps.
            let need = round_up(layout.size() + layout.align() + 64, PAGE).max(INITIAL);
            let target = st.committed + need;
            if target > RESERVE
                || sys::chunk_adjust(st.chunk, TChunkAdjust::Adjust, target as i32, 0) != 0
            {
                return ptr::null_mut();
            }
            // SAFETY: the bytes just committed directly follow the heap's current top.
            unsafe { st.heap.extend(need) };
            st.committed = target;
            st.heap
                .allocate_first_fit(layout)
                .map_or(ptr::null_mut(), |p| p.as_ptr())
        })
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        self.with_lock(|st| {
            if let Some(p) = NonNull::new(p) {
                // SAFETY: `p` came from `alloc` with this layout (GlobalAlloc contract).
                unsafe { st.heap.deallocate(p, layout) };
            }
        })
    }
}
