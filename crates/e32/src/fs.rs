// SPDX-License-Identifier: Apache-2.0
//! A client for Symbian's file server (`RFs` / `RFile`), speaking its IPC protocol directly.
//!
//! Opcodes are `TFsMessage` (`userlibandfileserver/fileserver/inc/F32plugin.h`), argument layouts are those of the
//! 2010 `efsrv` client (`sfsrv/cl_file.cpp`, `kernel/eka/euser/us_mes.cpp`). A file is a subsession: created on the
//! session with a 4-byte reply holding its handle, then addressed by putting that handle in argument slot 3. So is a
//! directory listing (`RDir`, `sfsrv/cl_dir.cpp`); its packed entries are `TEntry`s cut to their name
//! (`sfile/sf_dir.cpp:49-95`, `inc/f32file.h:1429-1464`).

use alloc::string::String;
use alloc::vec::Vec;

use crate::ipc::{Session, TIpcArgs, TVersion};
use crate::sys::{TPtr8, TPtrC8, TPtrC16, err};

const SERVER: &str = "!FileServer";
const VERSION: TVersion = TVersion {
    major: 2,
    minor: 0,
    build: 4011,
};

// TFsMessage (inc/F32plugin.h:42-188)
const EFS_DRIVE_LIST: i32 = 5;
const EFS_VOLUME: i32 = 7;
const EFS_MK_DIR: i32 = 16;
const EFS_RM_DIR: i32 = 17;
const EFS_DELETE: i32 = 19;
const EFS_RENAME: i32 = 20;
const EFS_DIR_SUB_CLOSE: i32 = 27;
const EFS_DIR_OPEN: i32 = 49;
const EFS_DIR_READ_PACKED: i32 = 51;
const EFS_FINALISE_DRIVE: i32 = 95;
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

/// `TEntry::iAtt` bits (`f32file.h`).
pub mod att {
    pub const READ_ONLY: u32 = 0x01;
    pub const HIDDEN: u32 = 0x02;
    pub const SYSTEM: u32 = 0x04;
    pub const DIR: u32 = 0x10;
    /// Set by the server on packed entries (`KEntryAttPacked`); not a property of the file.
    pub(super) const PACKED: u32 = 0x0100_0000;
    /// `KEntryAttMatchMask`: list hidden, system and directory entries too.
    pub const MATCH_ALL: u32 = HIDDEN | SYSTEM | DIR;
}

/// `TDriveInfo::iDriveAtt` bits (`e32const.h`).
pub mod drive_att {
    pub const ROM: u32 = 0x02;
    pub const REDIRECTED: u32 = 0x04;
    pub const INTERNAL: u32 = 0x10;
    pub const REMOVABLE: u32 = 0x20;
    pub const REMOTE: u32 = 0x40;
    pub const HIDDEN: u32 = 0x400;
    /// `RFs::DriveList`'s default filter: hide remote and hidden drives.
    pub(super) const DEFAULT_LIST: u32 = 0x40440;
}

/// Longest file name the server accepts (`KMaxFileName`).
const KMAX_FILE_NAME: usize = 256;
/// `KEntryArraySize`: one packed directory batch.
const KENTRY_ARRAY_SIZE: usize = 1024;
/// sizeof(TVolumeInfo) with 8-byte-aligned TInt64 (ARM EABI; the fields end at 572), checked on Syborg.
const VOLUME_INFO_SIZE: usize = 576;

/// A path as the UTF-16 the server wants. Over-long names give `KErrBadName`.
fn utf16(path: &str, buf: &mut [u16; KMAX_FILE_NAME]) -> Result<usize, i32> {
    let mut n = 0;
    for u in path.encode_utf16() {
        *buf.get_mut(n).ok_or(err::KERR_BAD_NAME)? = u;
        n += 1;
    }
    Ok(n)
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn i64_at(b: &[u8], at: usize) -> i64 {
    u32_at(b, at) as i64 | (u32_at(b, at + 4) as i64) << 32
}

/// `n` UTF-16 units at `at` as a string (unpaired surrogates become U+FFFD).
fn text_at(b: &[u8], at: usize, n: usize) -> String {
    let units = (0..n).map(|i| u16::from_le_bytes([b[at + 2 * i], b[at + 2 * i + 1]]));
    char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// One directory entry (`TEntry`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// [`att`] bits.
    pub att: u32,
    pub size: u64,
    /// Last modified, as a `TTime`: microseconds since 0000-01-01 (UTC).
    pub modified: i64,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.att & att::DIR != 0
    }
}

/// The entries in one `EFsDirReadPacked` reply: each is the first 32 bytes of a `TEntry` (att, size, modified,
/// uids, name length), the name padded to 4 bytes, then `iSizeHigh` and `iReserved` (`f32file_private.h:313-316`).
pub fn parse_packed(mut b: &[u8]) -> Vec<Entry> {
    let mut out = Vec::new();
    while b.len() >= 32 {
        let n = (u32_at(b, 28) & 0x0FFF_FFFF) as usize;
        let name_end = 32 + (2 * n).next_multiple_of(4);
        if b.len() < name_end + 8 {
            break;
        }
        out.push(Entry {
            name: text_at(b, 32, n),
            att: u32_at(b, 0) & !att::PACKED,
            size: u32_at(b, 4) as u64 | (u32_at(b, name_end) as u64) << 32,
            modified: i64_at(b, 8),
        });
        b = &b[name_end + 8..];
    }
    out
}

/// A drive's volume (`TVolumeInfo`, `f32file.h:1590-1648`): its `TDriveInfo`, then sizes and the label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    /// `TMediaType` (e.g. 5 RAM, 6 flash, 7 ROM, 9 NAND).
    pub media: u32,
    /// [`drive_att`] bits.
    pub drive_att: u32,
    pub media_att: u32,
    pub size: i64,
    pub free: i64,
    pub name: String,
}

impl Volume {
    /// `b` is what the server wrote: at least the 44 fixed bytes and the label.
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < 44 {
            return None;
        }
        let n = (u32_at(b, 40) & 0x0FFF_FFFF) as usize;
        if b.len() < 44 + 2 * n {
            return None;
        }
        Some(Self {
            media: u32_at(b, 0),
            drive_att: u32_at(b, 8),
            media_att: u32_at(b, 12),
            size: i64_at(b, 24),
            free: i64_at(b, 32),
            name: text_at(b, 44, n),
        })
    }
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

    /// One `TDriveList` byte per drive A..Z: 0 when absent, else its attribute byte (`sf_svr.cpp:380-466`).
    pub fn drives(&self) -> Result<[u8; 26], i32> {
        let mut list = [0u8; 26];
        let mut d = TPtr8::new(&mut list);
        let mut a = TIpcArgs::new();
        a.des8(0, &mut d).int(1, drive_att::DEFAULT_LIST as i32);
        self.session.send_receive(EFS_DRIVE_LIST, &a)?;
        Ok(list)
    }

    /// Size, free space and label of drive `drive` (0 = A:). The server reads the client's `TPckg<TVolumeInfo>` into
    /// its own and writes that back, so the descriptor must go out full length: an empty one comes back empty, a
    /// longer one fails with KErrOverflow (`sf_svr.cpp:522-575`).
    pub fn volume(&self, drive: i32) -> Result<Volume, i32> {
        let mut buf = [0u8; VOLUME_INFO_SIZE];
        let mut d = TPtr8::new_full(&mut buf);
        let mut a = TIpcArgs::new();
        a.des8(0, &mut d).int(1, drive).int(2, 0);
        self.session.send_receive(EFS_VOLUME, &a)?;
        let n = d.len();
        Volume::decode(&buf[..n]).ok_or(err::KERR_GENERAL)
    }

    /// Every entry in directory `path` (`X:\dir\`), hidden and system ones included.
    pub fn read_dir(&self, path: &str) -> Result<Vec<Entry>, i32> {
        Dir::open(self, path, att::MATCH_ALL)?.read_all()
    }

    fn path_op(&self, op: i32, path: &str, arg1: i32) -> Result<(), i32> {
        let mut name = [0u16; KMAX_FILE_NAME];
        let n = utf16(path, &mut name)?;
        let name = TPtrC16::new(&name[..n]);
        let mut a = TIpcArgs::new();
        a.desc16(0, &name).int(1, arg1);
        self.session.send_receive(op, &a).map(drop)
    }

    /// Make directory `path`, which ends with `\\`; its parent must exist (`RFs::MkDir`).
    pub fn mkdir(&self, path: &str) -> Result<(), i32> {
        self.path_op(EFS_MK_DIR, path, 0)
    }

    /// Remove empty directory `path` (`RFs::RmDir`).
    pub fn rmdir(&self, path: &str) -> Result<(), i32> {
        self.path_op(EFS_RM_DIR, path, 0)
    }

    /// Delete file `path` (`RFs::Delete`).
    pub fn delete(&self, path: &str) -> Result<(), i32> {
        self.path_op(EFS_DELETE, path, 0)
    }

    /// Mark every writable drive's volume clean before power goes (`RFs::FinaliseDrives`, `sfsrv/cl_cli.cpp:3302`):
    /// each drive but ROM and redirected ones gets `EFsFinaliseDrive` with `EFinal_RW` (0: the next write simply
    /// marks it in use again). Needs DiskAdmin. The first error, if any, after trying them all.
    pub fn finalise_drives(&self) -> Result<(), i32> {
        let mut first = Ok(());
        for (drive, &att) in self.drives()?.iter().enumerate() {
            if att == 0 || u32::from(att) & (drive_att::ROM | drive_att::REDIRECTED) != 0 {
                continue;
            }
            let mut a = TIpcArgs::new();
            a.int(0, drive as i32).int(1, 0);
            if let Err(e) = self.session.send_receive(EFS_FINALISE_DRIVE, &a) {
                first = first.and(Err(e));
            }
        }
        first
    }

    /// Rename or move a file or directory within one drive (`RFs::Rename`).
    pub fn rename(&self, from: &str, to: &str) -> Result<(), i32> {
        let (mut a16, mut b16) = ([0u16; KMAX_FILE_NAME], [0u16; KMAX_FILE_NAME]);
        let (na, nb) = (utf16(from, &mut a16)?, utf16(to, &mut b16)?);
        let (from, to) = (TPtrC16::new(&a16[..na]), TPtrC16::new(&b16[..nb]));
        let mut a = TIpcArgs::new();
        a.desc16(0, &from).desc16(1, &to);
        self.session.send_receive(EFS_RENAME, &a).map(drop)
    }
}

/// An open directory listing (`RDir`), closed on drop.
struct Dir<'a> {
    fs: &'a Fs,
    handle: i32,
}

impl<'a> Dir<'a> {
    /// `EFsDirOpen`: name, attribute mask, a `TPckgC<TUidType>` of three null UIDs (match any), reply handle in slot 3
    /// (`cl_dir.cpp:59-105`).
    fn open(fs: &'a Fs, path: &str, mask: u32) -> Result<Self, i32> {
        let mut name = [0u16; KMAX_FILE_NAME];
        let n = utf16(path, &mut name)?;
        let name = TPtrC16::new(&name[..n]);
        let uids = TPtrC8::new(&[0u8; 12]);
        let mut reply = [0u8; 4];
        let mut reply_des = TPtr8::new(&mut reply);
        let mut a = TIpcArgs::new();
        a.desc16(0, &name)
            .int(1, mask as i32)
            .desc8(2, &uids)
            .des8(3, &mut reply_des);
        fs.session.send_receive(EFS_DIR_OPEN, &a)?;
        if reply_des.len() != 4 {
            return Err(err::KERR_GENERAL);
        }
        Ok(Self {
            fs,
            handle: i32::from_le_bytes(reply),
        })
    }

    /// Batches until the server says `KErrEof`, whose batch may still hold entries (`sf_dir.cpp:194-212`).
    fn read_all(&self) -> Result<Vec<Entry>, i32> {
        let mut all = Vec::new();
        let mut buf = [0u8; KENTRY_ARRAY_SIZE];
        loop {
            let mut d = TPtr8::new(&mut buf);
            let mut a = TIpcArgs::new();
            a.des8(0, &mut d).int(3, self.handle);
            let r = self.fs.session.send_receive(EFS_DIR_READ_PACKED, &a);
            let n = d.len();
            match r {
                Ok(_) | Err(err::KERR_EOF) => all.extend(parse_packed(&buf[..n])),
                Err(e) => return Err(e),
            }
            if r.is_err() || n == 0 {
                return Ok(all);
            }
        }
    }
}

impl Drop for Dir<'_> {
    fn drop(&mut self) {
        let mut a = TIpcArgs::new();
        a.int(3, self.handle);
        let _ = self.fs.session.send_receive(EFS_DIR_SUB_CLOSE, &a);
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
        assert_eq!(utf16("C:\\ü", &mut b), Ok(4));
        assert_eq!(b[3], 0xFC);
        assert_eq!(utf16(&"x".repeat(256), &mut b), Ok(256));
        assert_eq!(utf16(&"x".repeat(257), &mut b), Err(err::KERR_BAD_NAME));
    }

    /// A packed entry as `sf_dir.cpp` writes it: 32 bytes, the name padded to 4, iSizeHigh, iReserved.
    fn packed(name: &str, att: u32, size: u64, modified: i64) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let mut b = Vec::new();
        b.extend((att | 0x0100_0000).to_le_bytes());
        b.extend((size as u32).to_le_bytes());
        b.extend(modified.to_le_bytes());
        b.extend([0u8; 12]);
        b.extend((units.len() as u32).to_le_bytes());
        units.iter().for_each(|u| b.extend(u.to_le_bytes()));
        b.resize(b.len().next_multiple_of(4), 0xAA); // padding is not zeroed
        b.extend(((size >> 32) as u32).to_le_bytes());
        b.extend(0xDEADu32.to_le_bytes());
        b
    }

    #[test]
    fn packed_entries() {
        let mut buf = packed("sys", att::DIR, 0, 63_000_000_000_000_000);
        buf.extend(packed("big.bin", att::READ_ONLY, 5 << 32 | 7, -1));
        buf.extend(packed("Ünï", 0, 3, 0)); // odd UTF-16 length: padded
        let e = parse_packed(&buf);
        assert_eq!(e.len(), 3);
        assert_eq!(
            e[0],
            Entry {
                name: "sys".into(),
                att: att::DIR,
                size: 0,
                modified: 63_000_000_000_000_000,
            }
        );
        assert!(e[0].is_dir() && !e[1].is_dir());
        assert_eq!((e[1].name.as_str(), e[1].size), ("big.bin", 5 << 32 | 7));
        assert_eq!(e[2].name, "Ünï");
        // a truncated tail is ignored, not misread
        assert_eq!(parse_packed(&buf[..buf.len() - 3]).len(), 2);
        assert!(parse_packed(&[]).is_empty());
    }

    #[test]
    fn volume_info() {
        let mut b = vec![0u8; VOLUME_INFO_SIZE];
        b[0..4].copy_from_slice(&6u32.to_le_bytes()); // flash
        b[8..12].copy_from_slice(&(drive_att::INTERNAL | 1).to_le_bytes());
        b[24..32].copy_from_slice(&(3i64 << 30).to_le_bytes());
        b[32..40].copy_from_slice(&(1i64 << 30).to_le_bytes());
        b[40..44].copy_from_slice(&4u32.to_le_bytes());
        for (i, u) in "DATA".encode_utf16().enumerate() {
            b[44 + 2 * i..46 + 2 * i].copy_from_slice(&u.to_le_bytes());
        }
        let v = Volume::decode(&b).unwrap();
        assert_eq!(
            (v.media, v.drive_att, v.size, v.free),
            (6, 0x11, 3 << 30, 1 << 30)
        );
        assert_eq!(v.name, "DATA");
        assert_eq!(Volume::decode(&b[..43]), None);
        assert_eq!(Volume::decode(&b[..47]), None); // label cut short
    }

    #[test]
    fn version_and_opcodes() {
        assert_eq!(VERSION.packed(), 0x0FAB_0002);
        assert_eq!(
            (EFS_FILE_REPLACE, EFS_FILE_WRITE, EFS_FILE_SIZE),
            (32, 35, 0x10028)
        );
        assert_eq!(
            (
                EFS_DRIVE_LIST,
                EFS_VOLUME,
                EFS_MK_DIR,
                EFS_RM_DIR,
                EFS_DELETE,
                EFS_RENAME
            ),
            (5, 7, 16, 17, 19, 20)
        );
        assert_eq!(
            (EFS_DIR_SUB_CLOSE, EFS_DIR_OPEN, EFS_DIR_READ_PACKED),
            (27, 49, 51)
        );
    }
}
