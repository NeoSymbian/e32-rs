// SPDX-License-Identifier: Apache-2.0
//! Client-server IPC: `TIpcArgs`, `TVersion`, and the session executive calls.

use crate::des::{TPtr8, TPtrC8, TPtrC16};
use crate::{TInt, TRequestStatus};

/// Argument types (`e32cmn.h` `TIpcArgs::TArgType`), 3 bits per slot at `slot * 3` in `iFlags`.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgType {
    Int = 0,
    Handle = 1,
    Des8 = 4,
    Des16 = 5,
    DesC8 = 6,
    DesC16 = 7,
}

/// `TIpcArgs` (`e32cmn.h`): four argument words and their type flags, 20 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TIpcArgs {
    pub i_args: [TInt; 4],
    pub i_flags: TInt,
}

impl TIpcArgs {
    pub const fn new() -> Self {
        Self {
            i_args: [0; 4],
            i_flags: 0,
        }
    }
    fn set(&mut self, slot: usize, value: TInt, t: ArgType) -> &mut Self {
        let shift = slot * 3;
        self.i_args[slot] = value;
        self.i_flags = (self.i_flags & !(7 << shift)) | ((t as TInt) << shift);
        self
    }
    pub fn int(&mut self, slot: usize, v: TInt) -> &mut Self {
        self.set(slot, v, ArgType::Int)
    }
    /// The descriptor must outlive the send.
    pub fn desc8(&mut self, slot: usize, d: &TPtrC8) -> &mut Self {
        self.set(slot, d as *const _ as usize as TInt, ArgType::DesC8)
    }
    /// The descriptor must outlive the send; the server writes into it.
    pub fn des8(&mut self, slot: usize, d: &mut TPtr8) -> &mut Self {
        self.set(slot, d as *mut _ as usize as TInt, ArgType::Des8)
    }
    /// The descriptor must outlive the send.
    pub fn desc16(&mut self, slot: usize, d: &TPtrC16) -> &mut Self {
        self.set(slot, d as *const _ as usize as TInt, ArgType::DesC16)
    }
}

/// `TVersion` (`e32cmn.h`) `{ TInt8 major, TInt8 minor, TInt16 build }` packed into one word, as `CreateSession`
/// sends it in the connect message.
#[derive(Clone, Copy, Debug)]
pub struct TVersion {
    pub major: u8,
    pub minor: u8,
    pub build: u16,
}

impl TVersion {
    pub const fn packed(self) -> TInt {
        (self.major as u32 | (self.minor as u32) << 8 | (self.build as u32) << 16) as TInt
    }
}

/// The connect message (`RMessage2::EConnect`).
pub const ECONNECT: TInt = -1;

/// `Exec::SessionCreate(const TDesC8& aServer, TInt aMsgSlots, const TSecurityPolicy*, TInt aType)`: an unsharable
/// session (thread-owned handle) on the global message pool when `slots` is -1. A handle or a negative error.
pub fn session_create(server: &TPtrC8, slots: TInt) -> TInt {
    // SAFETY: `server` is a valid descriptor; null policy; type 0 = EIpcSession_Unsharable.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::SESSION_CREATE }>(
            server as *const _ as u32,
            slots as u32,
            0,
            0,
        ) as TInt
    }
}

// `Exec::SessionSendSync` is an extended executive call: the kernel takes iArgs[0..3] and iFlags in r4-r8, so the
// stub loads them from the TIpcArgs, as the 2010 `uc_exec.cia` does (`ldmneia r2,{r4-r8}`). r6 and r11 cannot be
// inline-asm operands, hence a standalone function.
#[cfg(target_arch = "arm")]
core::arch::global_asm!(
    ".section .text.e32_session_send_sync,\"ax\",%progbits",
    ".global e32_session_send_sync",
    ".type e32_session_send_sync,%function",
    ".arm",
    "e32_session_send_sync:",
    "  stmfd sp!, {{r4-r8, lr}}",
    "  movs r8, r2",
    "  ldmne r2, {{r4-r8}}",
    "  swi #{n}",
    "  ldmfd sp!, {{r4-r8, pc}}",
    ".size e32_session_send_sync, . - e32_session_send_sync",
    n = const crate::execs::slow::SESSION_SEND_SYNC,
);

#[cfg(target_arch = "arm")]
unsafe extern "C" {
    fn e32_session_send_sync(
        handle: TInt,
        function: TInt,
        args: *const TIpcArgs,
        status: *mut TRequestStatus,
    ) -> TInt;
}

/// `Exec::SessionSendSync(TInt aHandle, TInt aFunction, TAny* aArgs, TRequestStatus*)`: queue a message on the
/// thread's synchronous slot. On success the reply arrives in `status`.
///
/// # Safety
/// `args`, the descriptors it points at, and `status` (set pending first) must stay valid until completion.
#[cfg(target_arch = "arm")]
pub unsafe fn session_send_sync(
    handle: TInt,
    function: TInt,
    args: *const TIpcArgs,
    status: *mut TRequestStatus,
) -> TInt {
    // SAFETY: forwarded caller contract.
    unsafe { e32_session_send_sync(handle, function, args, status) }
}

/// Host stand-in.
///
/// # Safety
/// Never callable on the host.
#[cfg(not(target_arch = "arm"))]
pub unsafe fn session_send_sync(
    _: TInt,
    _: TInt,
    _: *const TIpcArgs,
    _: *mut TRequestStatus,
) -> TInt {
    unimplemented!("SessionSendSync on a non-ARM host")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_server_flag_encodings() {
        // RFile::Replace: name DesC16, mode int, -, reply Des8 -> 0x807 (fileserver research, us_mes.cpp)
        let name = TPtrC16::new(&[0u16; 1]);
        let mut buf = [0u8; 4];
        let mut reply = TPtr8::new(&mut buf);
        let mut a = TIpcArgs::new();
        a.desc16(0, &name).int(1, 0x200).des8(3, &mut reply);
        assert_eq!(a.i_flags, 0x807);
        // RFile::Write: DesC8, length, position -> 6
        let data = TPtrC8::new(b"x");
        let mut w = TIpcArgs::new();
        w.desc8(0, &data).int(1, 1).int(2, -1);
        assert_eq!(w.i_flags, 6);
        assert_eq!(core::mem::size_of::<TIpcArgs>(), 20);
    }

    #[test]
    fn file_server_version() {
        assert_eq!(
            TVersion {
                major: 2,
                minor: 0,
                build: 4011
            }
            .packed(),
            0x0FAB_0002
        );
    }
}
