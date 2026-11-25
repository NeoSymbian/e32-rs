// SPDX-License-Identifier: Apache-2.0
//! The global allocator: a first-fit heap in a private chunk, grown with `ChunkAdjust`.
//!
//! euser's `RHeap` (the one the image header sizes) is left to euser's own runtime; Rust allocations never touch it.
//! The chunk is process-owned and its base never moves (multiple memory model), so the heap only grows upwards.

use crate::sys::{self, TChunkAdjust, TChunkCreate, TOwnerType};
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicI32, Ordering};
use linked_list_allocator::Heap;

/// Address space reserved for the heap, committed lazily: a bigger reserve costs no heap pages, but the kernel sizes the
/// chunk's page-array bookkeeping from the reservation, and the reservation must find that much contiguous address
/// space at start-up (else the RUST-HEAP panic). 64 MB: the UI server holds
/// two whole 1080x2160 screens during a fade (2 x 9.3 MB) besides Slint's own buffers; 16 MB left no room for the
/// second (smooth-ui S2b on the Pixel).
#[cfg(not(feature = "heap-256m"))]
const RESERVE: usize = 64 << 20;
/// `heap-256m`: for an app that decodes big pictures or 1080p video (Files: about 82 MB at 1080p H.264, FA12). Cargo
/// unifies features: every EXE built in the same invocation as one asking for it gets it too.
#[cfg(feature = "heap-256m")]
const RESERVE: usize = 256 << 20;
/// First commit, and the smallest growth step.
const INITIAL: usize = 64 << 10;
const PAGE: usize = 4 << 10;

struct State {
    heap: Heap,
    chunk: i32,
    committed: usize,
}

/// The state, and the kernel mutex guarding it. The handle lives outside the state so a thread can take the mutex
/// before it forms `&mut State`.
// shortcut: one kernel mutex round trip per alloc/free; ARMv8 has CAS now, so a user-side fast lock is the upgrade if
// the round trip shows in a profile.
struct ChunkHeap {
    state: UnsafeCell<State>,
    lock: AtomicI32,
}

// SAFETY: every access to the state goes through the kernel mutex (`with_lock`), after `init` on the main thread.
unsafe impl Sync for ChunkHeap {}

#[global_allocator]
static HEAP: ChunkHeap = ChunkHeap {
    state: UnsafeCell::new(State {
        heap: Heap::empty(),
        chunk: 0,
        committed: 0,
    }),
    lock: AtomicI32::new(0),
};

/// Create the chunk, the heap and its lock. Called once by `e32::main!` on the main thread, before any thread exists.
pub fn init() {
    if HEAP.lock.load(Ordering::Acquire) != 0 {
        return;
    }
    // SAFETY: single-threaded here (see above), and the heap isn't initialised yet, so nothing else uses the state.
    let st = unsafe { &mut *HEAP.state.get() };
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
    st.committed = INITIAL;
    // SAFETY: the chunk's first INITIAL bytes are committed, owned by this process and used by nothing else.
    unsafe { st.heap.init(sys::chunk_base(chunk), INITIAL) };
    // Publishes the state written above to every later `with_lock`.
    HEAP.lock.store(lock, Ordering::Release);
}

impl ChunkHeap {
    fn with_lock<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let lock = self.lock.load(Ordering::Acquire);
        sys::mutex_wait(lock, 0);
        // SAFETY: `init` ran before any allocation, and this thread holds the mutex that serialises the threads.
        let r = f(unsafe { &mut *self.state.get() });
        sys::mutex_signal(lock);
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
