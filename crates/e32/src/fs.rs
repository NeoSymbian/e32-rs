// SPDX-License-Identifier: Apache-2.0
//! A client for Symbian's file server (`RFs` / `RFile`), speaking its IPC protocol directly.
//!
//! Opcodes are `TFsMessage` (`userlibandfileserver/fileserver/inc/F32plugin.h`), argument layouts are those of the
//! 2010 `efsrv` client (`sfsrv/cl_file.cpp`, `kernel/eka/euser/us_mes.cpp`). A file is a subsession: created on the
//! session with a 4-byte reply holding its handle, then addressed by putting that handle in argument slot 3.

use crate::ipc::{Session, TIpcArgs, TVersion};
use crate::sys::{TPtr8, TPtrC8, TPtrC16, err};

const SERVER: &str = "!FileServer";
const VERSION: TVersion = TVersion {
    major: 2,
    minor: 0,
    build: 4011,
};

const EFS_FILE_SUB_CLOSE: i32 = 28;
const EFS_FILE_OPEN: i32 = 30;
const EFS_FILE_REPLACE: i32 = 32;
const EFS_FILE_READ: i32 = 34;
const EFS_FILE_WRITE: i32 = 35;
/// `EFsFileSize | KIpcArgSlot0Desc`: the size comes back as a TInt64 in slot 0.
const EFS_FILE_SIZE: i32 = 40 | 0x1_0000;
/// Read/write at the current position (`I64LOW(KCurrentPosition64)`).
const KCURRENT_POSITION: i32 = -1;

/// `TFileMode` bits (`f32file.h`).
pub mod mode {
    pub const SHARE_EXCLUSIVE: u32 = 0;
    pub const SHARE_READERS_ONLY: u32 = 1;
    pub const SHARE_ANY: u32 = 2;
    pub const READ: u32 = 0;
    pub const WRITE: u32 = 0x200;
}

/// Longest file name the server accepts (`KMaxFileName`).
const KMAX_FILE_NAME: usize = 256;

/// An ASCII path as the UTF-16 the server wants. Non-ASCII or over-long names give `KErrBadName`.
fn utf16(path: &str, buf: &mut [u16; KMAX_FILE_NAME]) -> Result<usize, i32> {
    if path.len() > KMAX_FILE_NAME || !path.is_ascii() {
        return Err(err::KERR_BAD_NAME);
    }
    for (d, s) in buf.iter_mut().zip(path.bytes()) {
        *d = s as u16;
    }
    Ok(path.len())
}

/// A file-server session (`RFs`). Use absolute names (`C:\…`): the default session path is the process's private
/// directory, which usually does not exist.
pub struct Fs {
    session: Session,
}

impl Fs {
    pub fn connect() -> Result<Self, i32> {
        Ok(Self {
            session: Session::connect(SERVER, VERSION)?,
        })
    }
}

/// An open file (`RFile`), closed on drop.
pub struct File<'a> {
    fs: &'a Fs,
    handle: i32,
}

impl<'a> File<'a> {
    fn subsession(fs: &'a Fs, op: i32, path: &str, mode: u32) -> Result<Self, i32> {
        let mut name = [0u16; KMAX_FILE_NAME];
        let n = utf16(path, &mut name)?;
        let name = TPtrC16::new(&name[..n]);
        let mut reply = [0u8; 4];
        let mut reply_des = TPtr8::new(&mut reply);
        let mut a = TIpcArgs::new();
        a.desc16(0, &name)
            .int(1, mode as i32)
            .des8(3, &mut reply_des);
        fs.session.send_receive(op, &a)?;
        if reply_des.len() != 4 {
            return Err(err::KERR_GENERAL);
        }
        Ok(Self {
            fs,
            handle: i32::from_le_bytes(reply),
        })
    }

    /// Create `path`, or empty it if it exists (`RFile::Replace`); opened for writing.
    pub fn replace(fs: &'a Fs, path: &str, mode: u32) -> Result<Self, i32> {
        Self::subsession(fs, EFS_FILE_REPLACE, path, mode | mode::WRITE)
    }

    /// Open an existing file (`RFile::Open`).
    pub fn open(fs: &'a Fs, path: &str, mode: u32) -> Result<Self, i32> {
        Self::subsession(fs, EFS_FILE_OPEN, path, mode)
    }

    /// Write all of `data` at the current position.
    pub fn write(&self, data: &[u8]) -> Result<(), i32> {
        let d = TPtrC8::new(data);
        let mut a = TIpcArgs::new();
        a.desc8(0, &d)
            .int(1, data.len() as i32)
            .int(2, KCURRENT_POSITION)
            .int(3, self.handle);
        self.fs.session.send_receive(EFS_FILE_WRITE, &a).map(drop)
    }

    /// Read up to `buf.len()` bytes from byte `pos`; how many were read (0 at the end).
    pub fn read_at(&self, pos: i32, buf: &mut [u8]) -> Result<usize, i32> {
        let max = buf.len() as i32;
        let mut d = TPtr8::new(buf);
        let mut a = TIpcArgs::new();
        a.des8(0, &mut d)
            .int(1, max)
            .int(2, pos)
            .int(3, self.handle);
        self.fs.session.send_receive(EFS_FILE_READ, &a)?;
        Ok(d.len())
    }

    pub fn size(&self) -> Result<i64, i32> {
        let mut buf = [0u8; 8];
        let mut d = TPtr8::new(&mut buf);
        let mut a = TIpcArgs::new();
        a.des8(0, &mut d).int(3, self.handle);
        self.fs.session.send_receive(EFS_FILE_SIZE, &a)?;
        Ok(i64::from_le_bytes(buf))
    }
}

impl Drop for File<'_> {
    fn drop(&mut self) {
        let mut a = TIpcArgs::new();
        a.int(3, self.handle);
        let _ = self.fs.session.send_receive(EFS_FILE_SUB_CLOSE, &a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_become_utf16() {
        let mut b = [0u16; KMAX_FILE_NAME];
        assert_eq!(utf16("C:\\a.txt", &mut b), Ok(8));
        assert_eq!(&b[..3], &[b'C' as u16, b':' as u16, b'\\' as u16]);
        assert_eq!(utf16("C:\\ü", &mut b), Err(err::KERR_BAD_NAME));
        assert_eq!(utf16(&"x".repeat(257), &mut b), Err(err::KERR_BAD_NAME));
    }

    #[test]
    fn version_and_opcodes() {
        assert_eq!(VERSION.packed(), 0x0FAB_0002);
        assert_eq!(
            (EFS_FILE_REPLACE, EFS_FILE_WRITE, EFS_FILE_SIZE),
            (32, 35, 0x10028)
        );
    }
}
