// SPDX-License-Identifier: Apache-2.0
//! This process (exit, panic, rendezvous) and others ([`Process`]).

use crate::ipc::Session;
use crate::sys::{self, TIpcArgs, TPtr8, TPtrC16, ipc, ipc::TLdrInfo};
use alloc::vec::Vec;

/// Complete the `RProcess::Rendezvous` requests of whoever waits on this process with `reason` — a server
/// calls it once its server object exists, so the client that started it can connect.
pub fn rendezvous(reason: i32) {
    sys::process_rendezvous(reason);
}

/// End the current thread with `reason`; for the main thread that ends the process (`User::Exit`).
pub fn exit(reason: i32) -> ! {
    sys::user_thread_exiting(reason);
    // SAFETY: a null category is allowed for EExitKill.
    unsafe {
        sys::thread_kill(
            sys::KCURRENT_THREAD_HANDLE,
            sys::TExitType::Kill,
            reason,
            core::ptr::null(),
        )
    };
    // ThreadKill on the current thread does not return.
    loop {
        core::hint::spin_loop();
    }
}

/// Panic the current thread with a category (at most 16 bytes kept) and reason (`User::Panic`).
pub fn panic(category: &str, reason: i32) -> ! {
    let cat = &category.as_bytes()[..category.len().min(sys::KMAX_EXIT_CATEGORY_NAME)];
    let des = sys::TPtrC8::new(cat);
    // SAFETY: `des` points at `cat`, alive for the call (which does not return).
    unsafe {
        sys::thread_kill(
            sys::KCURRENT_THREAD_HANDLE,
            sys::TExitType::Panic,
            reason,
            &des,
        )
    };
    // ThreadKill on the current thread does not return.
    loop {
        core::hint::spin_loop();
    }
}

/// Another process, started with [`Process::spawn`] (`RProcess::Create` + the loader). Closed on drop (the process
/// keeps running).
#[derive(Debug)]
pub struct Process {
    handle: i32,
}

impl Process {
    /// Load the EXE `name` (`.exe` and `\sys\bin` are added by the loader, e.g. `"neoclock"`) with command line
    /// `cmdline`; it starts suspended — call [`Process::resume`]. As euser's `RLoader::LoadProcess`
    /// (`us_ksvr.cpp:5139`): one `ELoadProcess` message to `!Loader` (`sf_ldr.cpp:1010-1053`).
    pub fn spawn(name: &str, cmdline: &str) -> Result<Self, i32> {
        let loader = Session::connect(ipc::LOADER_NAME, ipc::LOADER_VERSION)?;
        let name16: Vec<u16> = name.encode_utf16().collect();
        let cmd16: Vec<u16> = cmdline.encode_utf16().collect();
        let mut info = TLdrInfo::default();
        // SAFETY: TLdrInfo is plain old data; the loader writes it back (iHandle).
        let bytes = unsafe {
            core::slice::from_raw_parts_mut(
                (&mut info as *mut TLdrInfo).cast::<u8>(),
                core::mem::size_of::<TLdrInfo>(),
            )
        };
        let mut info_des = TPtr8::new_full(bytes);
        let name_des = TPtrC16::new(&name16);
        let cmd_des = TPtrC16::new(&cmd16);
        let mut args = TIpcArgs::new();
        args.des8(0, &mut info_des)
            .desc16(1, &name_des)
            .desc16(2, &cmd_des);
        loader.send_receive(ipc::ELOAD_PROCESS, &args)?;
        // SAFETY: the loader wrote the new handle at offset 16 of `info` (through `bytes`, no longer used).
        let handle = unsafe { core::ptr::read_volatile(&info.i_handle) };
        Ok(Self { handle })
    }

    /// Start it running.
    pub fn resume(&self) {
        sys::process_resume(self.handle);
    }

    /// End it with `reason` (exit type Kill).
    pub fn kill(&self, reason: i32) {
        sys::process_kill(self.handle, sys::TExitType::Kill, reason);
    }

    /// `None` while it runs, else `(exit type, reason)` — type 0 Kill, 1 Terminate, 2 Panic.
    pub fn exit_info(&self) -> Option<(i32, i32)> {
        match sys::process_exit_type(self.handle) {
            3 => None,
            t => Some((t, sys::process_exit_reason(self.handle))),
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        sys::handle_close(self.handle);
    }
}
