// SPDX-License-Identifier: Apache-2.0
//! Raw EKA2 executive calls and the kernel ABI types they take.
//!
//! An executive call is `swi #n` with the arguments in r0-r3 and the result in r0, exactly like the 2010 user-side
//! stubs (`kernel/eka/include/u32exec.h`, `__DISPATCH`). `n` comes from [`execs`], generated from the kernel's
//! `execs.txt`. On non-ARM hosts the calls are stubs, so the pure parts can be unit-tested on the build machine.
//!
//! Layouts and constants are from the 2010 headers (`kernel/eka/include/u32std.h`, `e32cmn.h`, `e32const.h`); the
//! `size_of` tests pin them.
#![no_std]

pub mod des;
pub mod execs;
pub mod ipc;
pub mod ui;

pub use des::{TPtr8, TPtr16, TPtrC8, TPtrC16};
pub use ipc::{RMessage2, SIpcCopyInfo, TIpcArgs, TVersion};

use core::ffi::c_void;

pub type TInt = i32;

/// `KCurrentThreadHandle` = `0xffff0001 | KHandleNoClose` (`e32const.h`).
pub const KCURRENT_THREAD_HANDLE: TInt = 0xFFFF_8001_u32 as TInt;
/// `KMaxExitCategoryName`: the kernel keeps at most 16 bytes of a panic category.
pub const KMAX_EXIT_CATEGORY_NAME: usize = 16;
/// `KRequestPending` = `-KMaxTInt` (`e32const.h`).
pub const KREQUEST_PENDING: TInt = -0x7FFF_FFFF;

/// Error codes (`e32err.h`) used by this crate and its callers.
pub mod err {
    pub const KERR_NONE: i32 = 0;
    pub const KERR_NOT_FOUND: i32 = -1;
    pub const KERR_GENERAL: i32 = -2;
    pub const KERR_CANCEL: i32 = -3;
    pub const KERR_NO_MEMORY: i32 = -4;
    pub const KERR_NOT_SUPPORTED: i32 = -5;
    pub const KERR_ARGUMENT: i32 = -6;
    pub const KERR_OVERFLOW: i32 = -9;
    pub const KERR_ALREADY_EXISTS: i32 = -11;
    pub const KERR_PATH_NOT_FOUND: i32 = -12;
    pub const KERR_IN_USE: i32 = -14;
    pub const KERR_SERVER_TERMINATED: i32 = -15;
    pub const KERR_ACCESS_DENIED: i32 = -21;
    pub const KERR_EOF: i32 = -25;
    pub const KERR_BAD_NAME: i32 = -28;
    pub const KERR_TIMED_OUT: i32 = -33;
    pub const KERR_BAD_DESCRIPTOR: i32 = -38;
    pub const KERR_PERMISSION_DENIED: i32 = -46;
}

/// `TExitType` (`e32const.h`).
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TExitType {
    Kill = 0,
    Terminate = 1,
    Panic = 2,
    Pending = 3,
}

/// `TOwnerType` (`e32const.h`): who owns a new handle.
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TOwnerType {
    Process = 0,
    Thread = 1,
}

/// `TRequestStatus` (`e32cmn.h`): the kernel completes a request by writing `i_status` (4 bytes at offset 0) and
/// signalling the thread's request semaphore.
#[repr(C)]
#[derive(Debug)]
pub struct TRequestStatus {
    pub i_status: TInt,
    pub i_flags: u32,
}

impl TRequestStatus {
    pub const fn pending() -> Self {
        Self {
            i_status: KREQUEST_PENDING,
            i_flags: 0,
        }
    }
    /// Read through a volatile load: the kernel writes it behind the compiler's back.
    pub fn get(&self) -> TInt {
        // SAFETY: a plain aligned i32 inside `self`.
        unsafe { core::ptr::read_volatile(&self.i_status) }
    }
    pub fn is_pending(&self) -> bool {
        self.get() == KREQUEST_PENDING
    }
}

/// `TChunkCreate` (`u32std.h`), 24 bytes. `i_att`: `ENormal = 0` (local, grows at the top).
#[repr(C)]
#[derive(Debug, Default)]
pub struct TChunkCreate {
    pub i_att: u32,
    pub i_force_fixed: TInt,
    pub i_initial_bottom: TInt,
    pub i_initial_top: TInt,
    pub i_max_size: TInt,
    pub i_clear_byte: u8,
}

/// `KChunkClearByteDefault` (`e32cmn_private.h`).
pub const KCHUNK_CLEAR_BYTE_DEFAULT: u8 = 3;

/// `TChunkAdjust` (`u32std.h`): `ChunkAdjust` operation.
#[repr(i32)]
#[derive(Clone, Copy, Debug)]
pub enum TChunkAdjust {
    /// a1 = new committed size
    Adjust = 0,
    DoubleEnded = 1,
    Commit = 2,
    Decommit = 3,
    Allocate = 4,
}

/// The thread entry the kernel's EXE start-up code calls: `RunThread` → `(*iFunction)(iPtr)` → `User::Exit(r)`.
pub type ThreadFunction = extern "C" fn(*mut c_void) -> TInt;

/// `SThreadCreateInfo8` + `SStdEpocThreadCreateInfo8` (`u32std.h`), 64 bytes. `RunThread` reads **all** of it from the
/// new thread's stack (`iAllocator` at 48), so a shorter struct makes it read garbage.
#[repr(C)]
pub struct SThreadCreateInfo {
    pub i_handle: TInt,
    pub i_type: TInt,
    pub i_function: Option<ThreadFunction>,
    pub i_ptr: *mut c_void,
    pub i_supervisor_stack: *mut c_void,
    pub i_supervisor_stack_size: TInt,
    pub i_user_stack: *mut c_void,
    pub i_user_stack_size: TInt,
    pub i_initial_thread_priority: TInt,
    pub i_name: TPtrC8,
    pub i_total_size: TInt,
    pub i_allocator: *mut c_void,
    pub i_heap_initial_size: TInt,
    pub i_heap_max_size: TInt,
    pub i_flags: TInt,
}

/// `swi #N` with up to four word arguments. `clobber_abi("C")`: the kernel may change what a C call may change,
/// the same contract the 2010 C++ stubs (plain function calls) give their callers.
#[cfg(target_arch = "arm")]
pub mod swi {
    use core::arch::asm;

    /// # Safety
    /// `N` must be a valid executive call number and the arguments what that call expects.
    #[inline(always)]
    pub unsafe fn exec4<const N: u32>(a0: u32, a1: u32, a2: u32, a3: u32) -> u32 {
        let r: u32;
        // SAFETY: the caller guarantees the call and its arguments; the asm only issues the trap.
        unsafe {
            asm!("swi #{n}", n = const N,
                 inlateout("r0") a0 => r, in("r1") a1, in("r2") a2, in("r3") a3,
                 clobber_abi("C"));
        }
        r
    }
}

/// Host stand-in: executive calls exist only on the Symbian target.
#[cfg(not(target_arch = "arm"))]
pub mod swi {
    /// # Safety
    /// Never callable on the host.
    pub unsafe fn exec4<const N: u32>(_a0: u32, _a1: u32, _a2: u32, _a3: u32) -> u32 {
        unimplemented!("executive call {N:#x} on a non-ARM host")
    }
}

use execs::{fast, slow};
use swi::exec4;

// ---- debug, exit -------------------------------------------------------------------------------------------------

/// `Exec::DebugPrint(TAny* aDes, TInt aMode)`: print an 8-bit descriptor on the kernel's debug port.
/// Mode 0 is what `RDebug::Printf` uses.
///
/// # Safety
/// `des` must point to a valid descriptor for the duration of the call.
pub unsafe fn debug_print(des: *const TPtrC8, mode: TInt) {
    // SAFETY: forwarded caller contract.
    unsafe { exec4::<{ slow::DEBUG_PRINT }>(des as u32, mode as u32, 0, 0) };
}

/// `Exec::UserThreadExiting(TInt aReason)`: tells the kernel the thread is about to exit (what `User::Exit` does
/// first). Returns true when it is the last thread of the process.
pub fn user_thread_exiting(reason: TInt) -> bool {
    // SAFETY: takes a plain integer.
    unsafe { exec4::<{ slow::USER_THREAD_EXITING }>(reason as u32, 0, 0, 0) != 0 }
}

/// `Exec::ThreadKill(TInt aHandle, TExitType, TInt aReason, const TDesC8* aCategory)`.
///
/// # Safety
/// `category` must be null or point to a valid descriptor.
pub unsafe fn thread_kill(
    handle: TInt,
    exit_type: TExitType,
    reason: TInt,
    category: *const TPtrC8,
) {
    // SAFETY: forwarded caller contract.
    unsafe {
        exec4::<{ slow::THREAD_KILL }>(
            handle as u32,
            exit_type as u32,
            reason as u32,
            category as u32,
        )
    };
}

// ---- handles, requests, time -------------------------------------------------------------------------------------

/// `Exec::HandleClose(TInt)`.
pub fn handle_close(handle: TInt) -> TInt {
    // SAFETY: a stale handle only returns an error.
    unsafe { exec4::<{ slow::HANDLE_CLOSE }>(handle as u32, 0, 0, 0) as TInt }
}

/// Fast `Exec::WaitForAnyRequest()`: wait on the thread's request semaphore.
pub fn wait_for_any_request() {
    // SAFETY: no arguments.
    unsafe { exec4::<{ fast::WAIT_FOR_ANY_REQUEST }>(0, 0, 0, 0) };
}

/// `Exec::RequestSignal(TInt aCount)`: give back wake-ups that were consumed for other requests.
pub fn request_signal(count: TInt) {
    // SAFETY: plain integer.
    unsafe { exec4::<{ slow::REQUEST_SIGNAL }>(count as u32, 0, 0, 0) };
}

/// Fast `Exec::NTickCount()`: nanokernel ticks since boot.
pub fn ntick_count() -> u32 {
    // SAFETY: no arguments.
    unsafe { exec4::<{ fast::N_TICK_COUNT }>(0, 0, 0, 0) }
}

/// Fast `Exec::FastCounter()`: the high-resolution counter (`User::FastCounter`); wraps, see
/// [`ui::fast_counter_frequency`].
pub fn fast_counter() -> u32 {
    // SAFETY: no arguments.
    unsafe { exec4::<{ fast::FAST_COUNTER }>(0, 0, 0, 0) }
}

/// Fast `Exec::Heap()`: the current thread's euser `RAllocator*` (what a new thread shares by default).
pub fn heap() -> *mut c_void {
    // SAFETY: no arguments.
    unsafe { exec4::<{ fast::HEAP }>(0, 0, 0, 0) as usize as *mut c_void }
}

/// `Exec::TimerCreate()`: a thread-owned timer handle, or a negative error.
pub fn timer_create() -> TInt {
    // SAFETY: no arguments.
    unsafe { exec4::<{ slow::TIMER_CREATE }>(0, 0, 0, 0) as TInt }
}

/// `Exec::TimerAfter(TInt aHandle, TRequestStatus&, TInt aMicroseconds)`.
///
/// # Safety
/// `status` must stay valid (and not move) until the request completes; it must be set pending first.
pub unsafe fn timer_after(handle: TInt, status: *mut TRequestStatus, micros: TInt) {
    // SAFETY: forwarded caller contract.
    unsafe { exec4::<{ slow::TIMER_AFTER }>(handle as u32, status as u32, micros as u32, 0) };
}

/// `Exec::TimerCancel(TInt aHandle)`: completes a pending request with `KErrCancel`.
pub fn timer_cancel(handle: TInt) {
    // SAFETY: plain integer.
    unsafe { exec4::<{ slow::TIMER_CANCEL }>(handle as u32, 0, 0, 0) };
}

// ---- chunks ------------------------------------------------------------------------------------------------------

/// `TChunkCreate::EGlobal` (`u32std.h:169-185`): a global chunk. Unnamed, it is "protected": not findable by name,
/// but its handle can be passed to another process (e.g. `MessageCompleteWithHandle`), which maps it at the same
/// address. A local chunk cannot be shared.
pub const ECHUNK_GLOBAL: u32 = 0x10;

/// `Exec::ChunkCreate(TOwnerType, const TDesC8* aName, TChunkCreate&)`: a handle or a negative error.
pub fn chunk_create(owner: TOwnerType, info: &TChunkCreate) -> TInt {
    // SAFETY: `info` is a valid 24-byte TChunkCreate; a null name: unnamed (local, or protected if EGlobal).
    unsafe { exec4::<{ slow::CHUNK_CREATE }>(owner as u32, 0, info as *const _ as u32, 0) as TInt }
}

/// `Exec::ChunkAdjust(TInt aHandle, TInt aType, TInt a1, TInt a2)`.
pub fn chunk_adjust(handle: TInt, op: TChunkAdjust, a1: TInt, a2: TInt) -> TInt {
    // SAFETY: plain integers.
    unsafe {
        exec4::<{ slow::CHUNK_ADJUST }>(handle as u32, op as u32, a1 as u32, a2 as u32) as TInt
    }
}

/// `Exec::ChunkBase(TInt aHandle)`: the chunk's address in this process.
pub fn chunk_base(handle: TInt) -> *mut u8 {
    // SAFETY: plain integer.
    unsafe { exec4::<{ slow::CHUNK_BASE }>(handle as u32, 0, 0, 0) as usize as *mut u8 }
}

// ---- threads -----------------------------------------------------------------------------------------------------

/// `Exec::ThreadCreate(const TDesC8& aName, TOwnerType, SThreadCreateInfo8&)`: a handle to a suspended thread.
///
/// # Safety
/// `info` must be a fully initialised 64-byte create info whose function and pointer stay valid for the thread.
pub unsafe fn thread_create(
    name: &TPtrC8,
    owner: TOwnerType,
    info: &mut SThreadCreateInfo,
) -> TInt {
    // SAFETY: forwarded caller contract.
    unsafe {
        exec4::<{ slow::THREAD_CREATE }>(
            name as *const _ as u32,
            owner as u32,
            info as *mut _ as u32,
            0,
        ) as TInt
    }
}

/// `Exec::ThreadResume(TInt aHandle)`.
pub fn thread_resume(handle: TInt) {
    // SAFETY: plain integer.
    unsafe { exec4::<{ slow::THREAD_RESUME }>(handle as u32, 0, 0, 0) };
}

/// `Exec::ThreadLogon(TInt aHandle, TRequestStatus*, TBool aRendezvous)`: completes with the exit reason.
///
/// # Safety
/// `status` must stay valid until the request completes; set it pending first.
pub unsafe fn thread_logon(handle: TInt, status: *mut TRequestStatus) {
    // SAFETY: forwarded caller contract.
    unsafe { exec4::<{ slow::THREAD_LOGON }>(handle as u32, status as u32, 0, 0) };
}

/// `Exec::ProcessRendezvous(TInt aReason)`: complete the rendezvous requests of whoever waits on this process
/// (`RProcess::Rendezvous`), e.g. a client that started this server.
pub fn process_rendezvous(reason: TInt) {
    // SAFETY: plain integer.
    unsafe { exec4::<{ slow::PROCESS_RENDEZVOUS }>(reason as u32, 0, 0, 0) };
}

// ---- processes, time ---------------------------------------------------------------------------------------------

/// `KCurrentProcessHandle` = `0xffff0000 | KHandleNoClose` (`e32const.h`).
pub const KCURRENT_PROCESS_HANDLE: TInt = 0xFFFF_8000_u32 as TInt;

/// `Exec::ProcessResume(TInt aHandle)`: start a newly loaded process.
pub fn process_resume(handle: TInt) {
    // SAFETY: plain integer.
    unsafe { exec4::<{ slow::PROCESS_RESUME }>(handle as u32, 0, 0, 0) };
}

/// `Exec::ProcessLogon(TInt aHandle, TRequestStatus*, TBool aRendezvous)`: completes with the exit reason (or, with
/// `rendezvous`, the process's `ProcessRendezvous` reason).
///
/// # Safety
/// `status` (set pending first) must stay valid until it completes.
pub unsafe fn process_logon(handle: TInt, status: *mut TRequestStatus, rendezvous: bool) {
    // SAFETY: forwarded caller contract.
    unsafe { exec4::<{ slow::PROCESS_LOGON }>(handle as u32, status as u32, rendezvous as u32, 0) };
}

/// `Exec::ProcessKill(TInt aHandle, TExitType, TInt aReason, const TDesC8* aCategory)` with no category.
pub fn process_kill(handle: TInt, exit_type: TExitType, reason: TInt) {
    // SAFETY: plain integers, null category.
    unsafe { exec4::<{ slow::PROCESS_KILL }>(handle as u32, exit_type as u32, reason as u32, 0) };
}

/// `Exec::ProcessExitType(TInt aHandle)`: 0 Kill, 1 Terminate, 2 Panic, 3 Pending (still running).
pub fn process_exit_type(handle: TInt) -> TInt {
    // SAFETY: plain integer.
    unsafe { exec4::<{ slow::PROCESS_EXIT_TYPE }>(handle as u32, 0, 0, 0) as TInt }
}

/// `Exec::ProcessExitReason(TInt aHandle)`.
pub fn process_exit_reason(handle: TInt) -> TInt {
    // SAFETY: plain integer.
    unsafe { exec4::<{ slow::PROCESS_EXIT_REASON }>(handle as u32, 0, 0, 0) as TInt }
}

/// `Exec::SetUTCTimeAndOffset(const TInt64& aUtc, TInt aOffset, TUint aMode, TUint aChanges)` (`stimer.cpp:1675`) with
/// mode `ETimeSetTime | ETimeSetOffset` (`u32std.h:105`): sets both. Needs WriteDeviceData (else a platsec panic).
pub fn set_utc_time_and_offset(utc: i64, offset: TInt) -> TInt {
    // SAFETY: the kernel reads one TInt64 from the pointer (kumemget32) during the call.
    unsafe {
        exec4::<{ slow::SET_UTC_TIME_AND_OFFSET }>(
            &utc as *const i64 as u32,
            offset as u32,
            1 | 2,
            0,
        ) as TInt
    }
}

/// `Exec::TimeNow(TInt64& aUtc, TInt& aOffset)`: UTC in microseconds since 0001-01-01 and the UTC offset in seconds.
pub fn time_now() -> (i64, TInt) {
    let mut utc: i64 = 0;
    let mut offset: TInt = 0;
    // SAFETY: the kernel writes one TInt64 and one TInt.
    unsafe {
        exec4::<{ slow::TIME_NOW }>(
            &mut utc as *mut i64 as u32,
            &mut offset as *mut TInt as u32,
            0,
            0,
        )
    };
    (utc, offset)
}

// ---- semaphores, mutexes -----------------------------------------------------------------------------------------

/// `Exec::SemaphoreCreate(const TDesC8* aName, TInt aCount, TOwnerType)`: an unnamed semaphore handle.
pub fn semaphore_create(count: TInt, owner: TOwnerType) -> TInt {
    // SAFETY: null name, plain integers.
    unsafe { exec4::<{ slow::SEMAPHORE_CREATE }>(0, count as u32, owner as u32, 0) as TInt }
}

/// `Exec::SemaphoreWait(TInt aHandle, TInt aTimeout)`: timeout 0 = forever, -1 = poll, > 0 = microseconds.
pub fn semaphore_wait(handle: TInt, timeout: TInt) -> TInt {
    // SAFETY: plain integers.
    unsafe { exec4::<{ slow::SEMAPHORE_WAIT }>(handle as u32, timeout as u32, 0, 0) as TInt }
}

/// `Exec::SemaphoreSignal1(TInt aHandle)`.
pub fn semaphore_signal1(handle: TInt) {
    // SAFETY: plain integer.
    unsafe { exec4::<{ slow::SEMAPHORE_SIGNAL1 }>(handle as u32, 0, 0, 0) };
}

/// `Exec::MutexCreate(const TDesC8* aName, TOwnerType)`: an unnamed mutex handle.
pub fn mutex_create(owner: TOwnerType) -> TInt {
    // SAFETY: null name, plain integer.
    unsafe { exec4::<{ slow::MUTEX_CREATE }>(0, owner as u32, 0, 0) as TInt }
}

/// `Exec::MutexWait(TInt aHandle, TInt aTimeout)`: timeout 0 = forever.
pub fn mutex_wait(handle: TInt, timeout: TInt) -> TInt {
    // SAFETY: plain integers.
    unsafe { exec4::<{ slow::MUTEX_WAIT }>(handle as u32, timeout as u32, 0, 0) as TInt }
}

/// `Exec::MutexSignal(TInt aHandle)`.
pub fn mutex_signal(handle: TInt) {
    // SAFETY: plain integer.
    unsafe { exec4::<{ slow::MUTEX_SIGNAL }>(handle as u32, 0, 0, 0) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn layouts_match_the_2010_headers() {
        assert_eq!(size_of::<TRequestStatus>(), 8);
        assert_eq!(size_of::<TChunkCreate>(), 24);
        if size_of::<usize>() == 4 {
            assert_eq!(size_of::<SThreadCreateInfo>(), 64);
        }
    }

    #[test]
    fn handle_and_numbers_match_the_2010_release() {
        assert_eq!(KCURRENT_THREAD_HANDLE as u32, 0xFFFF_8001);
        assert_eq!(KREQUEST_PENDING as u32, 0x8000_0001);
        assert_eq!(slow::DEBUG_PRINT, 86);
        assert_eq!(slow::THREAD_KILL, 115);
        assert_eq!(slow::USER_THREAD_EXITING, 246);
        assert_eq!(slow::SESSION_SEND_SYNC, 77);
        assert_eq!(slow::THREAD_CREATE, 104);
        assert_eq!(slow::CHUNK_CREATE, 107);
        assert_eq!(fast::WAIT_FOR_ANY_REQUEST, execs::EXECUTIVE_FAST);
        assert_eq!(fast::HEAP, execs::EXECUTIVE_FAST | 1);
    }
}
