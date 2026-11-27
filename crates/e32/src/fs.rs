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
use core::future::Future;
use core::mem::ManuallyDrop;
use core::pin::Pin;
use core::task::{Context, Poll};

use crate::ipc::{Session, TIpcArgs, TVersion};
use crate::request::{TRequestStatus, wait_for_request};
use crate::server::Message;
use crate::sys::{self, TPtr8, TPtr16, TPtrC8, TPtrC16, err};

const SERVER: &str = "!FileServer";
const VERSION: TVersion = TVersion {
    major: 2,
    minor: 0,
    build: 4011,
};

// TFsMessage (inc/F32plugin.h:42-188)
const EFS_NOTIFY_CHANGE_EX: i32 = 71;
const EFS_NOTIFY_CHANGE_CANCEL_EX: i32 = 72;
const EFS_DRIVE_LIST: i32 = 5;
const EFS_DRIVE: i32 = 6;
const EFS_VOLUME: i32 = 7;
const EFS_SET_VOLUME: i32 = 8;
const EFS_MK_DIR: i32 = 16;
const EFS_RM_DIR: i32 = 17;
const EFS_DELETE: i32 = 19;
const EFS_RENAME: i32 = 20;
const EFS_REPLACE: i32 = 21;
const EFS_ENTRY: i32 = 22;
const EFS_SET_ENTRY: i32 = 23;
const EFS_DIR_SUB_CLOSE: i32 = 27;
const EFS_DIR_OPEN: i32 = 49;
const EFS_DIR_READ_PACKED: i32 = 51;
const EFS_FINALISE_DRIVE: i32 = 95;
const EFS_FILE_SUB_CLOSE: i32 = 28;
const EFS_FILE_OPEN: i32 = 30;
const EFS_FILE_CREATE: i32 = 31;
const EFS_FILE_REPLACE: i32 = 32;
const EFS_FILE_READ: i32 = 34;
const EFS_FILE_WRITE: i32 = 35;
const EFS_FILE_FLUSH: i32 = 39;
const EFS_FILE_SET_SIZE_64: i32 = 41 | 0x1_0000;
const EFS_FILE_SET_ATT: i32 = 43;
const EFS_FILE_SET_MODIFIED: i32 = 45;
const KIPC_ARG_SLOT2_DESC: i32 = 0x4_0000;
/// `EFsFileSize | KIpcArgSlot0Desc`: the size comes back as a TInt64 in slot 0.
const EFS_FILE_SIZE: i32 = 40 | 0x1_0000;
/// Read/write at the current position (`I64LOW(KCurrentPosition64)`).
const KCURRENT_POSITION: i32 = -1;
const EFS_FILE_DUPLICATE: i32 = 96;
const EFS_FILE_ADOPT: i32 = 97;
const EFS_FILE_FULL_NAME: i32 = 105;
const EFS_FILE_SYSTEM_NAME: i32 = 74;
const EFS_QUERY_VOLUME_INFO_EXT: i32 = 127;
/// `TQueryVolumeInfoExtCmd::EFileSystemSubType`.
const EFILE_SYSTEM_SUB_TYPE: i32 = 0;
/// `KMaxFSNameLength`: a `TFSName`'s capacity.
const KMAX_FS_NAME: usize = 32;
/// `TFsFileAdopt` modes (`inc/f32file_private.h:102`): adopt as a 32-bit `RFile` (not `RFile64`).
const KFILE_ADOPT32: i32 = 0;
/// The file server's secure ID (`KFileServerUidValue`), what `KFileServerPolicy` checks a passed session against.
const FILE_SERVER_SID: u32 = 0x1000_39E3;

/// `TFileMode` bits (`f32file.h`).
pub mod mode {
    pub const SHARE_EXCLUSIVE: u32 = 0;
    pub const SHARE_READERS_ONLY: u32 = 1;
    pub const SHARE_ANY: u32 = 2;
    pub const READ: u32 = 0;
    pub const WRITE: u32 = 0x200;
    /// Opt into F32's bounded write cache when the mounted drive supports it; flush before durable close.
    pub const WRITE_BUFFERED: u32 = 0x800;
    /// `RFile64`: allow sizes beyond 2 GiB, subject to the mounted file system's limit.
    pub const BIG_FILE: u32 = 0x0004_0000;
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
    pub const LOCAL: u32 = 0x01;
    pub const ROM: u32 = 0x02;
    pub const REDIRECTED: u32 = 0x04;
    pub const SUBSTED: u32 = 0x08;
    pub const INTERNAL: u32 = 0x10;
    pub const REMOVABLE: u32 = 0x20;
    pub const REMOTE: u32 = 0x40;
    pub const LOGICALLY_REMOVABLE: u32 = 0x200;
    pub const HIDDEN: u32 = 0x400;
    pub const EXTERNAL: u32 = 0x800;
    /// `RFs::DriveList`'s default filter: hide remote and hidden drives.
    pub(super) const DEFAULT_LIST: u32 = 0x40440;
}

/// `TDriveInfo::iMediaAtt` bits (`e32const.h`).
pub mod media_att {
    pub const VARIABLE_SIZE: u32 = 0x01;
    pub const FORMATTABLE: u32 = 0x04;
    pub const WRITE_PROTECTED: u32 = 0x08;
    pub const LOCKED: u32 = 0x20;
}

/// `TMediaType` (`e32const.h`).
pub mod media {
    pub const NOT_PRESENT: u32 = 0;
    pub const RAM: u32 = 5;
    pub const FLASH: u32 = 6;
    pub const ROM: u32 = 7;
    pub const NAND_FLASH: u32 = 9;
}

/// `TNotifyType` (`f32file.h:542-600`): what a [`NotifyChange`] waits for.
pub mod notify {
    pub const ALL: u32 = 0x01;
    /// An entry was added, removed or renamed (the default).
    pub const ENTRY: u32 = 0x02;
    pub const FILE: u32 = 0x04;
    pub const DIR: u32 = 0x08;
    pub const ATTRIBUTES: u32 = 0x10;
    pub const WRITE: u32 = 0x20;
    pub const DISK: u32 = 0x40;
}

/// Longest volume label (`KMaxVolumeNameLength`).
pub const VOLUME_LABEL_MAX: usize = 11;

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
    /// `iUniqueID`: changes when the volume is formatted or another medium is mounted.
    pub unique_id: u32,
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
            unique_id: u32_at(b, 20),
            size: i64_at(b, 24),
            free: i64_at(b, 32),
            name: text_at(b, 44, n),
        })
    }
}

/// The name in a `TPckgBuf<TFSName>` (`TBuf16<32>`: length word, max-length word, 32 UTF-16 units), as
/// `EFsQueryVolumeInfoExt` writes it.
pub fn parse_fs_name_pckg(b: &[u8]) -> Option<String> {
    let n = (u32_at(b.get(..4)?, 0) & 0x0FFF_FFFF) as usize;
    (n <= KMAX_FS_NAME && b.len() >= 8 + 2 * n).then(|| text_at(b, 8, n))
}

/// A drive (`TDriveInfo`, `f32file.h:1503-1556`): 20 bytes, also the first 20 of [`Volume`]'s `TVolumeInfo`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriveInfo {
    /// [`media`] type.
    pub media: u32,
    /// `TBatteryState`: 0 not supported, 1 good, 2 low.
    pub battery: u32,
    /// [`drive_att`] bits.
    pub drive_att: u32,
    /// [`media_att`] bits.
    pub media_att: u32,
    /// `TConnectionBusType`: 0 internal, 1 USB.
    pub bus: u32,
}

impl DriveInfo {
    const SIZE: usize = 20;

    pub fn decode(b: &[u8]) -> Option<Self> {
        (b.len() >= Self::SIZE).then(|| Self {
            media: u32_at(b, 0),
            battery: u32_at(b, 4),
            drive_att: u32_at(b, 8),
            media_att: u32_at(b, 12),
            bus: u32_at(b, 16),
        })
    }

    /// Nothing on it can change: the ROM, or write-protected media.
    pub fn read_only(&self) -> bool {
        self.media == media::ROM
            || self.drive_att & drive_att::ROM != 0
            || self.media_att & media_att::WRITE_PROTECTED != 0
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

    /// Let this session's files be passed to other processes (`RFs::ShareProtected`): needed before
    /// [`File::transfer_to_server`] and [`SharedFile::transfer_to_client`]. The session stays usable as before.
    pub fn share_protected(&mut self) -> Result<(), i32> {
        self.session.share_protected()
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

    /// What drive `drive` (0 = A:) is, without needing a mounted volume (`RFs::Drive`, `cl_cli.cpp:1212`;
    /// `sf_svr.cpp:491-509`). A substituted drive reports `drive_att::SUBSTED`.
    pub fn drive(&self, drive: i32) -> Result<DriveInfo, i32> {
        let mut buf = [0u8; DriveInfo::SIZE];
        let mut d = TPtr8::new(&mut buf);
        let mut a = TIpcArgs::new();
        a.des8(0, &mut d).int(1, drive);
        self.session.send_receive(EFS_DRIVE, &a)?;
        let n = d.len();
        DriveInfo::decode(&buf[..n]).ok_or(err::KERR_GENERAL)
    }

    /// The file system mounted on drive `drive` (0 = A:): `Fat`, `Rom`, `Rofs`, `Composite`… (`RFs::FileSystemName`,
    /// `sf_sys.cpp:905-942`). A drive with none gives `KErrNotFound`.
    pub fn file_system_name(&self, drive: i32) -> Result<String, i32> {
        let mut buf = [0u16; KMAX_FS_NAME];
        let mut d = TPtr16::new(&mut buf);
        let mut a = TIpcArgs::new();
        a.des16(0, &mut d).int(1, drive).int(2, -1);
        self.session.send_receive(EFS_FILE_SYSTEM_NAME, &a)?;
        let n = d.len();
        Ok(String::from_utf16_lossy(&buf[..n]))
    }

    /// The mounted file system's sub type (`FAT12`, `FAT16`, `FAT32`), or its name when it has none (`Rom`,
    /// `Composite`…): `RFs::FileSystemSubType` (`cl_cli.cpp:3754`; `sf_svr.cpp:1167-1195`). Needs a mounted volume.
    pub fn file_system_sub_type(&self, drive: i32) -> Result<String, i32> {
        let mut buf = [0u8; 8 + 2 * KMAX_FS_NAME];
        let mut d = TPtr8::new_full(&mut buf);
        let mut a = TIpcArgs::new();
        a.int(0, drive)
            .int(1, EFILE_SYSTEM_SUB_TYPE)
            .des8(2, &mut d);
        self.session.send_receive(EFS_QUERY_VOLUME_INFO_EXT, &a)?;
        let n = d.len();
        parse_fs_name_pckg(&buf[..n]).ok_or(err::KERR_GENERAL)
    }

    /// Set drive `drive`'s volume label, at most [`VOLUME_LABEL_MAX`] characters (`RFs::SetVolumeLabel`,
    /// `sf_svr.cpp:588-625`). Needs DiskAdmin; a bad label gives `KErrBadName`.
    pub fn set_volume_label(&self, drive: i32, label: &str) -> Result<(), i32> {
        if label.encode_utf16().count() > VOLUME_LABEL_MAX {
            return Err(err::KERR_BAD_NAME);
        }
        let mut name = [0u16; KMAX_FILE_NAME];
        let n = utf16(label, &mut name)?;
        let name = TPtrC16::new(&name[..n]);
        let mut a = TIpcArgs::new();
        a.desc16(0, &name).int(1, drive);
        self.session.send_receive(EFS_SET_VOLUME, &a).map(drop)
    }

    /// Set the modified time (a `TTime`) and set/clear [`att`] bits of a file or directory by name (`RFs::SetEntry`,
    /// `cl_cli.cpp:2436`; `sf_nbs.cpp:251-283`). The entry must not be open (`KErrInUse`). Overlapping masks return
    /// `KErrArgument` rather than the FSCLIENT 21 panic the C++ API raises.
    pub fn set_entry(&self, path: &str, modified: i64, set: u32, clear: u32) -> Result<(), i32> {
        if set & clear != 0 {
            return Err(err::KERR_ARGUMENT);
        }
        let mut name = [0u16; KMAX_FILE_NAME];
        let n = utf16(path, &mut name)?;
        let name = TPtrC16::new(&name[..n]);
        let time = modified.to_le_bytes();
        let time = TPtrC8::new(&time);
        let mut a = TIpcArgs::new();
        a.desc16(0, &name)
            .desc8(1, &time)
            .int(2, set as i32)
            .int(3, clear as i32);
        self.session.send_receive(EFS_SET_ENTRY, &a).map(drop)
    }

    /// A future that completes when something of [`notify`] kind `kind` changes under `path` (`RFs::NotifyChange` with
    /// a path, `cl_cli.cpp:966-972`): a folder `X:\dir\` or a file; `?:` or `*:` for every drive. One change, then
    /// it is done: make a new one to keep watching. Dropping it while it waits cancels it.
    pub fn notify_change(&self, kind: u32, path: &str) -> Result<NotifyChange<'_>, i32> {
        let mut name = [0u16; KMAX_FILE_NAME];
        let n = utf16(path, &mut name)?;
        if n == 0 {
            return Err(err::KERR_ARGUMENT);
        }
        Ok(NotifyChange {
            fs: self,
            kind,
            name: name[..n].to_vec(),
            des: None,
            started: false,
            status: TRequestStatus::pending(),
        })
    }

    /// Every entry in directory `path` (`X:\dir\`), hidden and system ones included.
    pub fn read_dir(&self, path: &str) -> Result<Vec<Entry>, i32> {
        Dir::open(self, path, att::MATCH_ALL)?.read_all()
    }

    /// A bounded page of directory entries. `offset` counts entries, not bytes. Each call reopens the directory,
    /// so callers listing a changing directory may see changed ordering; memory use is independent of its size.
    ///
    /// # Errors
    /// Returns `KErrArgument` if `max` exceeds 256, or a file-server error.
    pub fn read_dir_page(&self, path: &str, offset: u32, max: usize) -> Result<Vec<Entry>, i32> {
        if max > 256 {
            return Err(err::KERR_ARGUMENT);
        }
        Dir::open(self, path, att::MATCH_ALL)?.read_page(offset, max)
    }

    /// Metadata for one file or directory (`RFs::Entry`). The wire layout is the ARM EABI `TEntry`:
    /// 32 fixed bytes, a 256-unit UTF-16 name, then the high size word and reserved word.
    pub fn entry(&self, path: &str) -> Result<Entry, i32> {
        let mut name = [0u16; KMAX_FILE_NAME];
        let n = utf16(path, &mut name)?;
        let name = TPtrC16::new(&name[..n]);
        let mut buf = [0u8; 552];
        let mut d = TPtr8::new_full(&mut buf);
        let mut a = TIpcArgs::new();
        a.desc16(0, &name).des8(1, &mut d);
        self.session.send_receive(EFS_ENTRY, &a)?;
        if d.len() != buf.len() {
            return Err(err::KERR_GENERAL);
        }
        let n = (u32_at(&buf, 28) & 0x0fff_ffff) as usize;
        if n > KMAX_FILE_NAME {
            return Err(err::KERR_GENERAL);
        }
        Ok(Entry {
            name: text_at(&buf, 32, n),
            att: u32_at(&buf, 0),
            size: u64::from(u32_at(&buf, 4)) | u64::from(u32_at(&buf, 544)) << 32,
            modified: i64_at(&buf, 8),
        })
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
        self.rename_op(EFS_RENAME, from, to)
    }

    /// Replace `to` with the existing file `from` (`RFs::Replace`). Close both files first. Unlike deleting the
    /// destination then renaming, this asks the file server to perform the replacement as one operation.
    pub fn replace_file(&self, from: &str, to: &str) -> Result<(), i32> {
        self.rename_op(EFS_REPLACE, from, to)
    }

    fn rename_op(&self, op: i32, from: &str, to: &str) -> Result<(), i32> {
        let (mut a16, mut b16) = ([0u16; KMAX_FILE_NAME], [0u16; KMAX_FILE_NAME]);
        let (na, nb) = (utf16(from, &mut a16)?, utf16(to, &mut b16)?);
        let (from, to) = (TPtrC16::new(&a16[..na]), TPtrC16::new(&b16[..nb]));
        let mut a = TIpcArgs::new();
        a.desc16(0, &from).desc16(1, &to);
        self.session.send_receive(op, &a).map(drop)
    }
}

/// From [`Fs::notify_change`]: `Ok` when the change happened, `Err` if the server refused the request (a bad path,
/// `KErrArgument` for an empty one or a directory watch on a file, `KErrPermissionDenied` for a protected folder).
pub struct NotifyChange<'a> {
    fs: &'a Fs,
    kind: u32,
    /// On the heap and `des` in the pinned future: the server reads the path after the send returns.
    name: Vec<u16>,
    des: Option<TPtrC16>,
    started: bool,
    status: TRequestStatus,
}

impl NotifyChange<'_> {
    /// The key the server files the request under (`sf_svr.cpp:141-215`): the status's address, as `RFs` sends it.
    fn key(&self) -> i32 {
        &self.status as *const TRequestStatus as usize as i32
    }
}

impl Future for NotifyChange<'_> {
    type Output = Result<(), i32>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: nothing is moved out; `des` and `status` stay put while the request is outstanding (pinned).
        let this = unsafe { self.get_unchecked_mut() };
        if !this.started {
            this.status = TRequestStatus::pending();
            let key = this.key();
            let des = this.des.insert(TPtrC16::new(&this.name));
            let mut a = TIpcArgs::new();
            a.int(0, this.kind as i32).desc16(1, des).int(2, key);
            // SAFETY: `des`, the name it points at and `status` live in the pinned future; Drop cancels and waits.
            let r = unsafe {
                sys::ipc::session_send(
                    this.fs.session.handle(),
                    EFS_NOTIFY_CHANGE_EX,
                    &a,
                    &mut this.status,
                )
            };
            if r != 0 {
                return Poll::Ready(Err(r));
            }
            this.started = true;
            return Poll::Pending;
        }
        match this.status.get() {
            sys::KREQUEST_PENDING => Poll::Pending,
            r if r < 0 => Poll::Ready(Err(r)),
            _ => Poll::Ready(Ok(())),
        }
    }
}

impl Drop for NotifyChange<'_> {
    fn drop(&mut self) {
        if self.started && self.status.is_pending() {
            // `RFs::NotifyChangeCancel(TRequestStatus&)` (`cl_cli.cpp:1016`): the server completes it with KErrCancel
            let mut a = TIpcArgs::new();
            a.int(0, self.key());
            let _ = self
                .fs
                .session
                .send_receive(EFS_NOTIFY_CHANGE_CANCEL_EX, &a);
            wait_for_request(&self.status);
        }
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

impl Dir<'_> {
    fn read_page(&self, mut skip: u32, max: usize) -> Result<Vec<Entry>, i32> {
        let mut out = Vec::new();
        let mut buf = [0u8; KENTRY_ARRAY_SIZE];
        while out.len() < max {
            let mut d = TPtr8::new(&mut buf);
            let mut a = TIpcArgs::new();
            a.des8(0, &mut d).int(3, self.handle);
            let r = self.fs.session.send_receive(EFS_DIR_READ_PACKED, &a);
            let n = d.len();
            if let Err(e) = r
                && e != err::KERR_EOF
            {
                return Err(e);
            }
            for entry in parse_packed(&buf[..n]) {
                if skip > 0 {
                    skip -= 1;
                } else if out.len() < max {
                    out.push(entry);
                }
            }
            if r.is_err() || n == 0 {
                break;
            }
        }
        Ok(out)
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

    /// Create a new file, failing with `KErrAlreadyExists` if it exists (`RFile::Create`); opened for writing.
    pub fn create(fs: &'a Fs, path: &str, mode: u32) -> Result<Self, i32> {
        Self::subsession(fs, EFS_FILE_CREATE, path, mode | mode::WRITE)
    }

    /// A second handle to this file on the same session (`RFile::DuplicateHandle`, `cl_file.cpp:255`): what
    /// `transfer_to_*` pass on, so this `File` stays open and independent of the receiver.
    fn duplicate_handle(&self) -> Result<i32, i32> {
        new_subsession(&self.fs.session, EFS_FILE_DUPLICATE, self.handle, 1)
    }

    /// Hand this file to a server in one request (`RFile::TransferToServer`, `cl_file.cpp:312`): the session goes in
    /// slot `fs_slot` as a handle, a duplicate of the file's handle in `file_slot`. The session must be shared
    /// ([`Fs::share_protected`]); the server takes the file with [`SharedFile::adopt_from_client`].
    pub fn transfer_to_server(
        &self,
        args: &mut TIpcArgs<'_>,
        fs_slot: usize,
        file_slot: usize,
    ) -> Result<(), i32> {
        let dup = self.duplicate_handle()?;
        args.handle(fs_slot, self.fs.session.handle())
            .int(file_slot, dup);
        Ok(())
    }

    /// The file's full name, drive included (`RFile::FullName`, `EFsFileFullName`).
    pub fn full_name(&self) -> Result<String, i32> {
        let mut buf = [0u16; KMAX_FILE_NAME];
        let mut d = TPtr16::new(&mut buf);
        let mut a = TIpcArgs::new();
        a.des16(0, &mut d).int(3, self.handle);
        self.fs.session.send_receive(EFS_FILE_FULL_NAME, &a)?;
        let n = d.len();
        Ok(String::from_utf16_lossy(&buf[..n]))
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

    /// Positional `RFile64::Read`. Invalid signed offsets are rejected before they reach the server.
    pub fn read_at64(&self, pos: u64, buf: &mut [u8]) -> Result<usize, i32> {
        let pos = i64::try_from(pos)
            .map_err(|_| err::KERR_ARGUMENT)?
            .to_le_bytes();
        let pos = TPtrC8::new(&pos);
        let max = i32::try_from(buf.len()).map_err(|_| err::KERR_ARGUMENT)?;
        let mut d = TPtr8::new(buf);
        let mut a = TIpcArgs::new();
        a.des8(0, &mut d)
            .int(1, max)
            .desc8(2, &pos)
            .int(3, self.handle);
        self.fs
            .session
            .send_receive(EFS_FILE_READ | KIPC_ARG_SLOT2_DESC, &a)?;
        Ok(d.len())
    }

    /// Write all bytes at a 64-bit offset (`RFile64::Write`). The file must have been opened for writing.
    /// Native F32 clamps positions beyond EOF to EOF; use [`File::set_size64`] first to reserve such a range.
    pub fn write_at64(&self, pos: u64, data: &[u8]) -> Result<(), i32> {
        let pos = i64::try_from(pos)
            .map_err(|_| err::KERR_ARGUMENT)?
            .to_le_bytes();
        let pos = TPtrC8::new(&pos);
        let n = i32::try_from(data.len()).map_err(|_| err::KERR_ARGUMENT)?;
        let data = TPtrC8::new(data);
        let mut a = TIpcArgs::new();
        a.desc8(0, &data)
            .int(1, n)
            .desc8(2, &pos)
            .int(3, self.handle);
        self.fs
            .session
            .send_receive(EFS_FILE_WRITE | KIPC_ARG_SLOT2_DESC, &a)
            .map(drop)
    }

    /// Commit this file's buffered data and metadata (`RFile::Flush`). Report any storage error to the caller.
    pub fn flush(&self) -> Result<(), i32> {
        let mut a = TIpcArgs::new();
        a.int(3, self.handle);
        self.fs.session.send_receive(EFS_FILE_FLUSH, &a).map(drop)
    }

    /// Set the file's length (`RFile64::SetSize`); extension may fail at the mounted file system's size limit.
    pub fn set_size64(&self, size: u64) -> Result<(), i32> {
        let bytes = i64::try_from(size)
            .map_err(|_| err::KERR_ARGUMENT)?
            .to_le_bytes();
        let d = TPtrC8::new(&bytes);
        let mut a = TIpcArgs::new();
        a.desc8(0, &d).int(3, self.handle);
        self.fs
            .session
            .send_receive(EFS_FILE_SET_SIZE_64, &a)
            .map(drop)
    }

    /// Set and clear file attribute bits (`RFile::SetAtt`). Overlapping masks return `KErrArgument`, rather than
    /// causing the FSCLIENT panic that the C++ API raises. Changes take effect on flush/close.
    pub fn set_att(&self, set: u32, clear: u32) -> Result<(), i32> {
        if set & clear != 0 {
            return Err(err::KERR_ARGUMENT);
        }
        let mut a = TIpcArgs::new();
        a.int(0, set as i32)
            .int(1, clear as i32)
            .int(3, self.handle);
        self.fs.session.send_receive(EFS_FILE_SET_ATT, &a).map(drop)
    }

    /// Set UTC modification time in Symbian `TTime` units (`RFile::SetModified`), committed on flush/close.
    pub fn set_modified(&self, ttime: i64) -> Result<(), i32> {
        let bytes = ttime.to_le_bytes();
        let d = TPtrC8::new(&bytes);
        let mut a = TIpcArgs::new();
        a.desc8(0, &d).int(3, self.handle);
        self.fs
            .session
            .send_receive(EFS_FILE_SET_MODIFIED, &a)
            .map(drop)
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

/// A subsession made from two integer arguments; its handle comes back in slot 3 (`RSubSessionBase::CreateSubSession`).
fn new_subsession(session: &Session, op: i32, a0: i32, a1: i32) -> Result<i32, i32> {
    let mut reply = [0u8; 4];
    let mut reply_des = TPtr8::new(&mut reply);
    let mut a = TIpcArgs::new();
    a.int(0, a0).int(1, a1).des8(3, &mut reply_des);
    session.send_receive(op, &a)?;
    if reply_des.len() != 4 {
        return Err(err::KERR_GENERAL);
    }
    Ok(i32::from_le_bytes(reply))
}

/// A file passed from another process, with the session it came on: both close on drop (Symbian's auto-close
/// subsession). Read and write it through [`SharedFile::file`].
pub struct SharedFile {
    fs: Fs,
    handle: i32,
}

impl SharedFile {
    fn adopt(session: Session, file_handle: i32) -> Result<Self, i32> {
        let handle = new_subsession(&session, EFS_FILE_ADOPT, file_handle, KFILE_ADOPT32)?;
        Ok(Self {
            fs: Fs { session },
            handle,
        })
    }

    /// In a server: take the file a client sent with [`File::transfer_to_server`] (`RFile::AdoptFromClient`,
    /// `cl_file.cpp:93`). The session must be a file-server one (`KFileServerPolicy`).
    pub fn adopt_from_client(msg: &Message, fs_slot: usize, file_slot: usize) -> Result<Self, i32> {
        let file_handle = msg.int(file_slot);
        let session = Session::adopt(msg.open_session(fs_slot), FILE_SERVER_SID)?;
        Self::adopt(session, file_handle)
    }

    /// In a client: take the file a server completed a request with ([`SharedFile::transfer_to_client`]):
    /// `fs_handle` is the request's result, `file_handle` what it wrote into the 4-byte descriptor
    /// (`RFile::AdoptFromServer`, `cl_file.cpp:62`).
    pub fn adopt_from_server(fs_handle: i32, file_handle: i32) -> Result<Self, i32> {
        Self::adopt(Session::adopt(fs_handle, FILE_SERVER_SID)?, file_handle)
    }

    /// In a server: answer `msg` with this file (`RFile::TransferToClient`, `cl_file.cpp:357`): a duplicate handle
    /// goes into the client's 4-byte descriptor in `file_slot`, and the request completes with the session. This file
    /// stays open here. On an error the message completes with it.
    pub fn transfer_to_client(&self, msg: Message, file_slot: usize) -> Result<(), i32> {
        let r = self
            .file()
            .duplicate_handle()
            .and_then(|dup| msg.write8(file_slot, &dup.to_le_bytes(), 0));
        match r {
            Ok(()) => {
                msg.complete_with_handle(self.fs.session.handle());
                Ok(())
            }
            Err(e) => {
                msg.complete(e);
                Err(e)
            }
        }
    }

    /// The file, for reading and writing; closing stays with the `SharedFile`.
    pub fn file(&self) -> ManuallyDrop<File<'_>> {
        ManuallyDrop::new(File {
            fs: &self.fs,
            handle: self.handle,
        })
    }
}

impl Drop for SharedFile {
    fn drop(&mut self) {
        drop(ManuallyDrop::into_inner(self.file())); // the subsession; `fs` (the session) closes after
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
        b[20..24].copy_from_slice(&0x1234_5678u32.to_le_bytes());
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
        assert_eq!(v.unique_id, 0x1234_5678);
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
        assert_eq!(
            (
                EFS_DRIVE,
                EFS_SET_VOLUME,
                EFS_SET_ENTRY,
                EFS_NOTIFY_CHANGE_EX,
                EFS_NOTIFY_CHANGE_CANCEL_EX
            ),
            (6, 8, 23, 71, 72)
        );
        // TFsMessage counted in inc/F32plugin.h: EFsFinaliseDrive 95, then EFsFileDuplicate, EFsFileAdopt; 105.
        assert_eq!(
            (
                EFS_FINALISE_DRIVE + 1,
                EFS_FILE_DUPLICATE,
                EFS_FILE_ADOPT,
                EFS_FILE_FULL_NAME
            ),
            (96, 96, 97, 105)
        );
        // EFsFileSystemName 74, EFsQueryVolumeInfoExt 127 (the same count)
        assert_eq!((EFS_FILE_SYSTEM_NAME, EFS_QUERY_VOLUME_INFO_EXT), (74, 127));
    }

    #[test]
    fn fs_name_package() {
        // TPckgBuf<TFSName>("FAT32"): length | EBuf << 28, max 32, then the units (the rest unwritten)
        let mut b = alloc::vec![0xAAu8; 8 + 64];
        b[0..4].copy_from_slice(&(5u32 | 3 << 28).to_le_bytes());
        b[4..8].copy_from_slice(&32u32.to_le_bytes());
        for (i, u) in "FAT32".encode_utf16().enumerate() {
            b[8 + 2 * i..10 + 2 * i].copy_from_slice(&u.to_le_bytes());
        }
        assert_eq!(parse_fs_name_pckg(&b).as_deref(), Some("FAT32"));
        assert_eq!(parse_fs_name_pckg(&b[..17]), None); // cut short
        assert_eq!(parse_fs_name_pckg(&b[..3]), None);
        b[0..4].copy_from_slice(&33u32.to_le_bytes()); // longer than a TFSName holds
        assert_eq!(parse_fs_name_pckg(&b), None);
    }

    #[test]
    fn drive_info() {
        let mut b = [0u8; 20];
        b[0..4].copy_from_slice(&media::ROM.to_le_bytes());
        b[8..12].copy_from_slice(&(drive_att::ROM | drive_att::INTERNAL).to_le_bytes());
        b[16..20].copy_from_slice(&1u32.to_le_bytes());
        let d = DriveInfo::decode(&b).unwrap();
        assert_eq!((d.media, d.drive_att, d.bus), (7, 0x12, 1));
        assert!(d.read_only());
        let mut c = b;
        c[0..4].copy_from_slice(&media::NAND_FLASH.to_le_bytes());
        c[8..12].copy_from_slice(&drive_att::INTERNAL.to_le_bytes());
        assert!(!DriveInfo::decode(&c).unwrap().read_only());
        c[12..16].copy_from_slice(&media_att::WRITE_PROTECTED.to_le_bytes());
        assert!(DriveInfo::decode(&c).unwrap().read_only());
        assert_eq!(DriveInfo::decode(&b[..19]), None);
        // the same bytes open a TVolumeInfo
        let mut v = alloc::vec![0u8; VOLUME_INFO_SIZE];
        v[..20].copy_from_slice(&b);
        assert_eq!(Volume::decode(&v).unwrap().drive_att, d.drive_att);
    }
}
