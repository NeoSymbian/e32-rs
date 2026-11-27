// SPDX-License-Identifier: Apache-2.0
//! Client-server IPC: `TIpcArgs`, `TVersion`, and the session executive calls.

use crate::des::{TPtr8, TPtr16, TPtrC8, TPtrC16};
use crate::{TInt, TRequestStatus};
use core::marker::PhantomData;

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

/// `TIpcArgs` (`e32cmn.h`): four argument words and their type flags, 20 bytes. Descriptor slots hold addresses,
/// so the args borrow those descriptors for `'a`: a temporary descriptor cannot outlive its statement here.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TIpcArgs<'a> {
    pub i_args: [TInt; 4],
    pub i_flags: TInt,
    _borrows: PhantomData<&'a ()>,
}

impl<'a> TIpcArgs<'a> {
    pub const fn new() -> Self {
        Self {
            i_args: [0; 4],
            i_flags: 0,
            _borrows: PhantomData,
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
    pub fn desc8(&mut self, slot: usize, d: &'a TPtrC8) -> &mut Self {
        self.set(slot, d as *const _ as usize as TInt, ArgType::DesC8)
    }
    /// The server writes into it.
    pub fn des8(&mut self, slot: usize, d: &'a mut TPtr8) -> &mut Self {
        self.set(slot, d as *mut _ as usize as TInt, ArgType::Des8)
    }
    /// The server writes into it.
    pub fn des16(&mut self, slot: usize, d: &'a mut TPtr16) -> &mut Self {
        self.set(slot, d as *mut _ as usize as TInt, ArgType::Des16)
    }
    pub fn desc16(&mut self, slot: usize, d: &'a TPtrC16) -> &mut Self {
        self.set(slot, d as *const _ as usize as TInt, ArgType::DesC16)
    }
    /// A handle the server may open in its own process (`TIpcArgs::Set(TInt, RHandleBase)`, `e32cmn.inl:6723`).
    pub fn handle(&mut self, slot: usize, h: TInt) -> &mut Self {
        self.set(slot, h, ArgType::Handle)
    }
}

/// `TIpcSessionType` (`e32const.h:3083`).
pub const EIPC_SESSION_GLOBAL_SHARABLE: TInt = 2;

/// `Exec::SessionShare(TInt& aHandle, TInt aMode)` (`sipc.cpp:1836`): make a session sharable. A thread-owned handle
/// becomes a new process-owned one, written back to `handle`; `EIPC_SESSION_GLOBAL_SHARABLE` also makes it a protected
/// object that other processes may open. The server must allow that mode, or KErrPermissionDenied.
pub fn session_share(handle: &mut TInt, mode: TInt) -> TInt {
    // SAFETY: `handle` is a valid, writable TInt for the call.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::SESSION_SHARE }>(
            handle as *mut TInt as u32,
            mode as u32,
            0,
            0,
        ) as TInt
    }
}

/// `TObjectType::ESession` (`u32std.h:227`).
pub const EOBJECT_SESSION: TInt = 8;

/// `Exec::MessageOpenObject(TInt aMsg, TObjectType, TInt aParam, TOwnerType)`: open, in this process, the object whose
/// handle the client put in argument `param` (sent as a handle argument). A new handle or a negative error.
pub fn message_open_object(msg: TInt, kind: TInt, param: TInt) -> TInt {
    // SAFETY: plain integers; the kernel validates the message and the client's handle. Owner 0 = EOwnerProcess.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::MESSAGE_OPEN_OBJECT }>(
            msg as u32,
            kind as u32,
            param as u32,
            0,
        ) as TInt
    }
}

/// `Exec::SessionSecurityInfo(TInt aSession, SSecurityInfo&)`: the secure ID of the session's server (the first word of
/// the 16-byte `SSecurityInfo`; vendor ID and capabilities follow), as `TSecurityPolicy::CheckPolicy(RSessionBase)`
/// reads it.
pub fn session_server_sid(session: TInt) -> Result<u32, TInt> {
    let mut info = [0u32; 4];
    // SAFETY: `info` is 16 writable bytes, the size of SSecurityInfo.
    let r = unsafe {
        crate::swi::exec4::<{ crate::execs::slow::SESSION_SECURITY_INFO }>(
            session as u32,
            info.as_mut_ptr() as u32,
            0,
            0,
        ) as TInt
    };
    if r < 0 { Err(r) } else { Ok(info[0]) }
}

/// `Exec::MessageClient(TInt aMessage, TOwnerType)` (`sipc.cpp:2010`): a new handle to the thread that sent `msg`
/// (`RMessagePtr2::Client`; owner 0 process, 1 thread), or an error. Close it with `handle_close`.
pub fn message_client(msg: TInt, owner: u32) -> TInt {
    // SAFETY: plain integers; the kernel checks the message handle.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::MESSAGE_CLIENT }>(msg as u32, owner, 0, 0) as TInt
    }
}

/// `Exec::MessageSecurityInfo(TInt aMessage, SSecurityInfo&)`: the security info of the process that sent `msg`
/// (`RMessagePtr2::SecureId`, `us_secure.cpp`); the secure ID is word 0. The kernel returns nothing: a bad handle
/// panics the caller, as every message exec does.
pub fn message_security_info(msg: TInt) -> [u32; 4] {
    let mut info = [0u32; 4];
    // SAFETY: `info` is 16 writable bytes, the size of SSecurityInfo.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::MESSAGE_SECURITY_INFO }>(
            msg as u32,
            info.as_mut_ptr() as u32,
            0,
            0,
        )
    };
    info
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
    pub const fn from_packed(w: TInt) -> Self {
        let w = w as u32;
        Self {
            major: w as u8,
            minor: (w >> 8) as u8,
            build: (w >> 16) as u16,
        }
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

// `Exec::SessionSend` (asynchronous) is extended the same way (`uc_exec.cia:28-38`).
#[cfg(target_arch = "arm")]
core::arch::global_asm!(
    ".section .text.e32_session_send,\"ax\",%progbits",
    ".global e32_session_send",
    ".type e32_session_send,%function",
    ".arm",
    "e32_session_send:",
    "  stmfd sp!, {{r4-r8, lr}}",
    "  movs r8, r2",
    "  ldmne r2, {{r4-r8}}",
    "  swi #{n}",
    "  ldmfd sp!, {{r4-r8, pc}}",
    ".size e32_session_send, . - e32_session_send",
    n = const crate::execs::slow::SESSION_SEND,
);

#[cfg(target_arch = "arm")]
unsafe extern "C" {
    fn e32_session_send(
        handle: TInt,
        function: TInt,
        args: *const TIpcArgs<'_>,
        status: *mut TRequestStatus,
    ) -> TInt;
}

/// `Exec::SessionSend(TInt aHandle, TInt aFunction, TAny* aArgs, TRequestStatus*)`: queue an asynchronous message
/// (the session needs message slots: -1, the global pool, or > 0). 0 if queued; the reply arrives in `status`.
///
/// # Safety
/// `args`, the descriptors it points at, and `status` (set pending first) must stay valid until completion.
#[cfg(target_arch = "arm")]
pub unsafe fn session_send(
    handle: TInt,
    function: TInt,
    args: *const TIpcArgs<'_>,
    status: *mut TRequestStatus,
) -> TInt {
    // SAFETY: forwarded caller contract.
    unsafe { e32_session_send(handle, function, args, status) }
}

/// Host stand-in.
///
/// # Safety
/// Never callable on the host.
#[cfg(not(target_arch = "arm"))]
pub unsafe fn session_send(
    _: TInt,
    _: TInt,
    _: *const TIpcArgs<'_>,
    _: *mut TRequestStatus,
) -> TInt {
    unimplemented!("SessionSend on a non-ARM host")
}

/// `TLdrInfo` (`e32ldr_private.h:67-77`), 32 bytes: what `RLoader::LoadProcess` sends in slot 0 and the loader writes
/// back (the new process handle at +16).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TLdrInfo {
    pub i_requested_uids: [u32; 3],
    pub i_owner_type: TInt,
    pub i_handle: TInt,
    pub i_secure_id: u32,
    pub i_requested_version: u32,
    pub i_min_stack_size: TInt,
}

impl Default for TLdrInfo {
    /// As `TLdrInfo::TLdrInfo()`: any UIDs, process-owned handle, any version (`KModuleVersionWild`), 8 KB stack.
    fn default() -> Self {
        Self {
            i_requested_uids: [0; 3],
            i_owner_type: 0,
            i_handle: 0,
            i_secure_id: 0,
            i_requested_version: 0xFFFF_FFFE,
            i_min_stack_size: 0x2000,
        }
    }
}

/// The loader server (`RLoader`): name, version 1.0.4016 (packed), and `ELoadProcess`.
pub const LOADER_NAME: &str = "!Loader";
pub const LOADER_VERSION: TVersion = TVersion {
    major: 1,
    minor: 0,
    build: 4016,
};
pub const ELOAD_PROCESS: TInt = 1;

#[cfg(target_arch = "arm")]
unsafe extern "C" {
    fn e32_session_send_sync(
        handle: TInt,
        function: TInt,
        args: *const TIpcArgs<'_>,
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
    args: *const TIpcArgs<'_>,
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
    _: *const TIpcArgs<'_>,
    _: *mut TRequestStatus,
) -> TInt {
    unimplemented!("SessionSendSync on a non-ARM host")
}

// ---- server side (`RServer2`, `RMessage2`; kernel `sipc.cpp`, euser `us_ksvr.cpp`) -------------------------------

/// The disconnect message the kernel sends when a client's last session handle closes (`RMessage2::EDisConnect`).
pub const EDISCONNECT: TInt = -2;

/// `TIpcSessionType` (`e32const.h`): how far a server lets sessions be shared. `RServer2::CreateGlobal` uses Sharable.
pub const EIPC_SESSION_SHARABLE: TInt = 1;

/// `RMessage2` (`e32cmn.h`), 40 bytes. `ServerReceive` writes the first 32 (`RMessageU2`, `sipc.cpp:34-78`):
/// handle, function, the client's raw argument words (descriptors arrive as client-side pointers), spare, and the
/// session cookie set by `SetSessionPtr` (0 on a connect message).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct RMessage2 {
    pub i_handle: TInt,
    pub i_function: TInt,
    pub i_args: [TInt; 4],
    pub i_spare1: TInt,
    pub i_session_ptr: usize,
    pub i_flags: TInt,
    pub i_spare3: TInt,
}

/// `SIpcCopyInfo` (`u32std.h:358`), 12 bytes: the server-side buffer of a `MessageIpcCopy`.
/// `i_local_len` counts characters: the buffer's capacity for a read, the data length for a write.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SIpcCopyInfo {
    pub i_local_ptr: *mut u8,
    pub i_local_len: TInt,
    pub i_flags: u32,
}

/// `KIpcDirWrite` (`u32std.h:157`): copy from the server into the client's descriptor (0 = read from it).
pub const KIPC_DIR_WRITE: u32 = 0x1000_0000;
/// `KChunkShiftBy1` (`u32std.h:160`): the descriptors are 16-bit (0 = 8-bit).
pub const KIPC_16BIT: u32 = 0x8000_0000;

/// `Exec::ServerCreate(const TDesC8* aName, TInt aMode)`: a (thread-owned, when named) server handle or an error.
/// A name starting with `!` needs ProtServ.
pub fn server_create(name: &TPtrC8, mode: TInt) -> TInt {
    // SAFETY: `name` is a valid descriptor for the call.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::SERVER_CREATE }>(
            name as *const _ as u32,
            mode as u32,
            0,
            0,
        ) as TInt
    }
}

/// `Exec::ServerReceive(TInt aHandle, TRequestStatus&, TAny* aMsg)`: on the next message the kernel writes 32 bytes
/// of `msg` and completes `status`. Only the thread that created the server may call it; one at a time
/// (KERN-EXEC 19).
///
/// # Safety
/// `status` (set pending first) and `msg` must stay valid and unmoved until `status` completes.
pub unsafe fn server_receive(handle: TInt, status: *mut TRequestStatus, msg: *mut RMessage2) {
    // SAFETY: forwarded caller contract.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::SERVER_RECEIVE }>(
            handle as u32,
            status as u32,
            msg as u32,
            0,
        )
    };
}

/// `Exec::ServerCancel(TInt aHandle)`: a pending receive completes with `KErrCancel`.
pub fn server_cancel(handle: TInt) {
    // SAFETY: plain integer.
    unsafe { crate::swi::exec4::<{ crate::execs::slow::SERVER_CANCEL }>(handle as u32, 0, 0, 0) };
}

/// `Exec::SetSessionPtr(TInt aMsg, const TAny* aCookie)`: attach the server's session object to the session of a
/// connect message. Panics the server (KERN-EXEC 58-60) on a null cookie, a non-connect message, or a second call.
pub fn set_session_ptr(msg: TInt, cookie: usize) {
    // SAFETY: plain integers; the kernel validates the message handle.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::SET_SESSION_PTR }>(
            msg as u32,
            cookie as u32,
            0,
            0,
        )
    };
}

/// `Exec::MessageComplete(TInt aMsg, TInt aReason)`. The handle is dead afterwards: completing it again panics
/// (KERN-EXEC 44), or worse, completes a newer message that reused the slot.
pub fn message_complete(msg: TInt, reason: TInt) {
    // SAFETY: plain integers; the kernel validates the message handle.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::MESSAGE_COMPLETE }>(
            msg as u32,
            reason as u32,
            0,
            0,
        )
    };
}

/// `Exec::MessageCompleteWithHandle(TInt aMsg, TInt aHandle)`: complete with a new handle, in the client thread, to
/// the object `handle` (in this process) refers to; the client's request completes with that handle value.
pub fn message_complete_with_handle(msg: TInt, handle: TInt) {
    // SAFETY: plain integers; the kernel validates both handles.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::MESSAGE_COMPLETE_WITH_HANDLE }>(
            msg as u32,
            handle as u32,
            0,
            0,
        )
    };
}

/// `Exec::MessageGetDesLength(TInt aMsg, TInt aParam)`: the client descriptor's length (characters) or an error.
pub fn message_get_des_length(msg: TInt, param: TInt) -> TInt {
    // SAFETY: plain integers.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::MESSAGE_GET_DES_LENGTH }>(
            msg as u32,
            param as u32,
            0,
            0,
        ) as TInt
    }
}

/// `Exec::MessageGetDesMaxLength(TInt aMsg, TInt aParam)`: the client descriptor's capacity or an error.
pub fn message_get_des_max_length(msg: TInt, param: TInt) -> TInt {
    // SAFETY: plain integers.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::MESSAGE_GET_DES_MAX_LENGTH }>(
            msg as u32,
            param as u32,
            0,
            0,
        ) as TInt
    }
}

/// `Exec::MessageKill(TInt aMsg, TExitType, TInt aReason, const TDesC8* aCategory)`: end the client thread. The
/// message is still outstanding afterwards; complete it.
pub fn message_kill(msg: TInt, exit_type: crate::TExitType, reason: TInt, category: &TPtrC8) {
    // SAFETY: `category` is a valid descriptor for the call.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::MESSAGE_KILL }>(
            msg as u32,
            exit_type as u32,
            reason as u32,
            category as *const _ as u32,
        )
    };
}

// `Exec::MessageIpcCopy(TInt aMsg, TInt aParam, SIpcCopyInfo&, TInt aOffset)` is an extended executive call: the
// kernel takes the three SIpcCopyInfo words in r4-r6 (`uc_exec.cia:52-61`, `ldmia r2,{r4-r6}`).
#[cfg(target_arch = "arm")]
core::arch::global_asm!(
    ".section .text.e32_message_ipc_copy,\"ax\",%progbits",
    ".global e32_message_ipc_copy",
    ".type e32_message_ipc_copy,%function",
    ".arm",
    "e32_message_ipc_copy:",
    "  stmfd sp!, {{r4-r6, lr}}",
    "  ldmia r2, {{r4-r6}}",
    "  swi #{n}",
    "  ldmfd sp!, {{r4-r6, pc}}",
    ".size e32_message_ipc_copy, . - e32_message_ipc_copy",
    n = const crate::execs::slow::MESSAGE_IPC_COPY,
);

#[cfg(target_arch = "arm")]
unsafe extern "C" {
    fn e32_message_ipc_copy(
        msg: TInt,
        param: TInt,
        info: *const SIpcCopyInfo,
        offset: TInt,
    ) -> TInt;
}

/// Copy between the client's descriptor `param` and a local buffer, `offset` characters into the client's data.
/// A read returns the characters copied (truncated to `info.i_local_len`); a write returns 0 or `KErrOverflow`
/// (past the client's max length). The client's length word changes only when the message completes.
///
/// # Safety
/// `info.i_local_ptr` must be valid for `i_local_len` characters (writable for a read).
#[cfg(target_arch = "arm")]
pub unsafe fn message_ipc_copy(msg: TInt, param: TInt, info: &SIpcCopyInfo, offset: TInt) -> TInt {
    // SAFETY: forwarded caller contract.
    unsafe { e32_message_ipc_copy(msg, param, info, offset) }
}

/// Host stand-in.
///
/// # Safety
/// Never callable on the host.
#[cfg(not(target_arch = "arm"))]
pub unsafe fn message_ipc_copy(_: TInt, _: TInt, _: &SIpcCopyInfo, _: TInt) -> TInt {
    unimplemented!("MessageIpcCopy on a non-ARM host")
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

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
        // RFile::TransferToServer(args, 0, 1): session handle (EHandle = 1) in slot 0, file handle int in slot 1
        let mut t = TIpcArgs::new();
        t.handle(0, 0x4000_0123).int(1, 7);
        assert_eq!((t.i_flags, t.i_args[0]), (1, 0x4000_0123));
    }

    #[test]
    fn server_layouts_and_numbers() {
        if size_of::<usize>() == 4 {
            assert_eq!(size_of::<RMessage2>(), 40);
            assert_eq!(size_of::<SIpcCopyInfo>(), 12);
        }
        use crate::execs::slow;
        assert_eq!(
            (
                slow::SERVER_RECEIVE,
                slow::SERVER_CANCEL,
                slow::SET_SESSION_PTR
            ),
            (34, 35, 36)
        );
        assert_eq!((slow::MESSAGE_COMPLETE, slow::SERVER_CREATE), (65, 126));
        assert_eq!(
            (slow::MESSAGE_GET_DES_LENGTH, slow::MESSAGE_IPC_COPY),
            (167, 169)
        );
        assert_eq!((slow::MESSAGE_KILL, slow::PROCESS_RENDEZVOUS), (173, 166));
        let mut w = [0u16; 2];
        let mut out = TPtr16::new(&mut w);
        let empty = TPtrC8::new(b"");
        let mut a = TIpcArgs::new();
        a.desc8(0, &empty).des16(1, &mut out);
        assert_eq!(a.i_flags, 6 | 5 << 3);
    }

    #[test]
    fn loader_layout() {
        assert_eq!(size_of::<TLdrInfo>(), 32);
        assert_eq!(core::mem::offset_of!(TLdrInfo, i_handle), 16);
        assert_eq!(LOADER_VERSION.packed(), 0x0FB0_0001);
        let mut info = [0u8; 32];
        let mut slot0 = TPtr8::new(&mut info);
        let name = TPtrC16::new(&[0u16; 1]);
        let cmd = TPtrC16::new(&[]);
        let mut a = TIpcArgs::new();
        a.des8(0, &mut slot0).desc16(1, &name).desc16(2, &cmd);
        assert_eq!(a.i_flags, 4 | 7 << 3 | 7 << 6);
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
        let v = TVersion::from_packed(0x0FAB_0002);
        assert_eq!((v.major, v.minor, v.build), (2, 0, 4011));
    }
}
