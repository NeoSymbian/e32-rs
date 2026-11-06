// SPDX-License-Identifier: Apache-2.0
//! Raw EKA2 executive calls and the kernel ABI types they take.
//!
//! An executive call is `swi #n` with the arguments in r0-r3 and the result in r0, exactly like the 2010 user-side
//! stubs (`kernel/eka/include/u32exec.h`, `__DISPATCH`). `n` comes from [`execs`], generated from the kernel's
//! `execs.txt`. On non-ARM hosts the calls are stubs, so the pure parts can be unit-tested on the build machine.
#![no_std]

pub mod execs;

pub type TInt = i32;

/// `KCurrentThreadHandle` = `0xffff0001 | KHandleNoClose` (`e32const.h`).
pub const KCURRENT_THREAD_HANDLE: TInt = 0xFFFF_8001_u32 as TInt;
/// `KMaxExitCategoryName`: the kernel keeps at most 16 bytes of a panic category.
pub const KMAX_EXIT_CATEGORY_NAME: usize = 16;

/// `TExitType` (`e32const.h`).
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TExitType {
    Kill = 0,
    Terminate = 1,
    Panic = 2,
    Pending = 3,
}

/// `TDesType` (`u32std.h`): the descriptor type sits in the top 4 bits of a descriptor's first word.
const EPTRC: u32 = 1;
const KMASK_DES_LENGTH: u32 = 0x0FFF_FFFF;

/// A `TPtrC8`: a constant 8-bit descriptor pointing at bytes owned elsewhere. Layout `{ (type << 28) | length, ptr }`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TPtrC8 {
    len_type: u32,
    ptr: *const u8,
}

impl TPtrC8 {
    /// Panics if `s` is longer than a descriptor can describe (2^28 - 1 bytes).
    pub const fn new(s: &[u8]) -> Self {
        assert!(
            s.len() as u64 <= KMASK_DES_LENGTH as u64,
            "descriptor too long"
        );
        Self {
            len_type: (EPTRC << 28) | s.len() as u32,
            ptr: s.as_ptr(),
        }
    }
    pub const fn len(&self) -> usize {
        (self.len_type & KMASK_DES_LENGTH) as usize
    }
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub const fn des_type(&self) -> u32 {
        self.len_type >> 28
    }
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

/// `Exec::DebugPrint(TAny* aDes, TInt aMode)`: print an 8-bit descriptor on the kernel's debug port.
/// Mode 0 is what `RDebug::Printf` uses.
///
/// # Safety
/// `des` must point to a valid descriptor for the duration of the call.
pub unsafe fn debug_print(des: *const TPtrC8, mode: TInt) {
    // SAFETY: forwarded caller contract.
    unsafe { swi::exec4::<{ execs::slow::DEBUG_PRINT }>(des as u32, mode as u32, 0, 0) };
}

/// `Exec::UserThreadExiting(TInt aReason)`: tells the kernel the thread is about to exit (what `User::Exit` does
/// first). Returns true when it is the last thread of the process.
pub fn user_thread_exiting(reason: TInt) -> bool {
    // SAFETY: takes a plain integer.
    unsafe { swi::exec4::<{ execs::slow::USER_THREAD_EXITING }>(reason as u32, 0, 0, 0) != 0 }
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
        swi::exec4::<{ execs::slow::THREAD_KILL }>(
            handle as u32,
            exit_type as u32,
            reason as u32,
            category as u32,
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tptrc8_encodes_type_and_length() {
        let d = TPtrC8::new(b"hello");
        assert_eq!(d.len(), 5);
        assert_eq!(d.des_type(), EPTRC);
        assert_eq!(d.len_type, 0x1000_0005);
    }

    #[test]
    fn handle_and_numbers_match_the_2010_release() {
        assert_eq!(KCURRENT_THREAD_HANDLE as u32, 0xFFFF_8001);
        assert_eq!(execs::slow::DEBUG_PRINT, 86);
        assert_eq!(execs::slow::THREAD_KILL, 115);
        assert_eq!(execs::slow::USER_THREAD_EXITING, 246);
        assert_eq!(execs::fast::WAIT_FOR_ANY_REQUEST, execs::EXECUTIVE_FAST);
    }
}
