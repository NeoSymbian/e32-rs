// SPDX-License-Identifier: Apache-2.0
//! Descriptors as the kernel reads them (`u32std.h` `TDesType`): word 0 = `length | type << 28`.
//! Lengths of 16-bit descriptors count characters, not bytes.

const EPTRC: u32 = 1;
const EPTR: u32 = 2;
const KMASK_DES_LENGTH: u32 = 0x0FFF_FFFF;

const fn word(des_type: u32, len: usize) -> u32 {
    assert!(len as u64 <= KMASK_DES_LENGTH as u64, "descriptor too long");
    (des_type << 28) | len as u32
}

/// `TPtrC8`: a constant 8-bit descriptor over bytes owned elsewhere. `{ len | EPtrC << 28, ptr }`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TPtrC8 {
    len_type: u32,
    ptr: *const u8,
}

impl TPtrC8 {
    /// Panics if `s` is longer than a descriptor can describe (2^28 - 1 bytes).
    pub const fn new(s: &[u8]) -> Self {
        Self {
            len_type: word(EPTRC, s.len()),
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

/// `TPtrC16`: a constant 16-bit (UTF-16) descriptor. `{ chars | EPtrC << 28, ptr }`.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TPtrC16 {
    len_type: u32,
    ptr: *const u16,
}

impl TPtrC16 {
    pub const fn new(s: &[u16]) -> Self {
        Self {
            len_type: word(EPTRC, s.len()),
            ptr: s.as_ptr(),
        }
    }
    pub const fn len(&self) -> usize {
        (self.len_type & KMASK_DES_LENGTH) as usize
    }
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// `TPtr8`: a writable 8-bit descriptor over a buffer owned elsewhere. `{ len | EPtr << 28, max_len, ptr }`.
/// The kernel writes data into the buffer and the new length into word 0.
#[repr(C)]
#[derive(Debug)]
pub struct TPtr8 {
    len_type: u32,
    max_len: i32,
    ptr: *mut u8,
}

impl TPtr8 {
    /// An empty descriptor whose capacity is the whole of `buf`.
    pub fn new(buf: &mut [u8]) -> Self {
        Self {
            len_type: word(EPTR, 0),
            max_len: buf.len() as i32,
            ptr: buf.as_mut_ptr(),
        }
    }
    /// The length the kernel left (volatile read: it is written behind the compiler's back).
    pub fn len(&self) -> usize {
        // SAFETY: an aligned u32 inside `self`.
        (unsafe { core::ptr::read_volatile(&self.len_type) } & KMASK_DES_LENGTH) as usize
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn max_len(&self) -> usize {
        self.max_len as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_words() {
        let c = TPtrC8::new(b"hello");
        assert_eq!((c.len(), c.des_type(), c.len_type), (5, 1, 0x1000_0005));
        let w: [u16; 3] = [b'C' as u16, b':' as u16, b'\\' as u16];
        let c16 = TPtrC16::new(&w);
        assert_eq!((c16.len(), c16.len_type), (3, 0x1000_0003));
        let mut buf = [0u8; 8];
        let p = TPtr8::new(&mut buf);
        assert_eq!((p.len(), p.max_len(), p.len_type), (0, 8, 0x2000_0000));
    }
}
