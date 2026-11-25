// SPDX-License-Identifier: Apache-2.0
//! Known-answer tests of the ARMv8 CRC32 and crypto instructions, through the `core::arch` intrinsics: AArch32 on the
//! device, AArch64 on an Apple-silicon host (`cargo test -p neocpu`, the same code and answers). Inputs pass through
//! `black_box` so the compiler can't fold the answers in.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::hint::black_box;

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;
#[cfg(target_arch = "arm")]
use core::arch::arm::*;

pub type Outcome = Result<(), String>;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0))
        .collect()
}

fn check(what: &str, got: &[u8], want: &str) -> Outcome {
    if hex(got) == want {
        Ok(())
    } else {
        Err(format!("{what}: {} != {want}", hex(got)))
    }
}

// ---- CRC32 (crc32b/crc32w) and CRC32C (crc32cb/crc32cw) -------------------------------------------------------------

/// "123456789", as two little-endian words and a byte, then byte by byte: CRC-32 cbf43926, CRC-32C e3069283.
pub fn crc32() -> Outcome {
    let msg = black_box(*b"123456789");
    let w0 = u32::from_le_bytes([msg[0], msg[1], msg[2], msg[3]]);
    let w1 = u32::from_le_bytes([msg[4], msg[5], msg[6], msg[7]]);
    // SAFETY: plain register intrinsics; the target has `crc` (+v8).
    let (crc_w, crc_b, crcc_w, crcc_b) = unsafe {
        let words = !__crc32b(__crc32w(__crc32w(!0, w0), w1), msg[8]);
        let bytes = !msg.iter().fold(!0, |c, &b| __crc32b(c, b));
        let cwords = !__crc32cb(__crc32cw(__crc32cw(!0, w0), w1), msg[8]);
        let cbytes = !msg.iter().fold(!0, |c, &b| __crc32cb(c, b));
        (words, bytes, cwords, cbytes)
    };
    check("crc32w", &crc_w.to_be_bytes(), "cbf43926")?;
    check("crc32b", &crc_b.to_be_bytes(), "cbf43926")?;
    check("crc32cw", &crcc_w.to_be_bytes(), "e3069283")?;
    check("crc32cb", &crcc_b.to_be_bytes(), "e3069283")
}

// ---- AES-128 (aese/aesmc/aesd/aesimc) -------------------------------------------------------------------------------

type Block = [u8; 16];

/// SubWord through AESE: with all four columns equal, ShiftRows changes nothing, so lane 0 is SubBytes of the word.
fn sub_word(w: u32) -> u32 {
    // SAFETY: register intrinsics; the target has `aes` and `neon`.
    unsafe {
        let x = vaeseq_u8(vreinterpretq_u8_u32(vdupq_n_u32(w)), vdupq_n_u8(0));
        vgetq_lane_u32::<0>(vreinterpretq_u32_u8(x))
    }
}

/// FIPS-197 5.2 key expansion; words little-endian, so RotWord is a right rotation and Rcon goes in the low byte.
fn expand(key: &Block) -> [Block; 11] {
    const RCON: [u32; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];
    let mut w = [0u32; 44];
    for (i, c) in key.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_le_bytes(*c);
    }
    for i in 4..44 {
        let mut t = w[i - 1];
        if i % 4 == 0 {
            t = sub_word(t.rotate_right(8)) ^ RCON[i / 4 - 1];
        }
        w[i] = w[i - 4] ^ t;
    }
    let mut rk = [[0u8; 16]; 11];
    for (r, k) in rk.iter_mut().enumerate() {
        for j in 0..4 {
            k[4 * j..4 * j + 4].copy_from_slice(&w[4 * r + j].to_le_bytes());
        }
    }
    rk
}

fn encrypt(rk: &[Block; 11], pt: &Block) -> Block {
    let mut out = [0u8; 16];
    // SAFETY: 16-byte loads/stores of local arrays; register intrinsics otherwise.
    unsafe {
        let mut s = vld1q_u8(pt.as_ptr());
        for k in &rk[..9] {
            s = vaesmcq_u8(vaeseq_u8(s, vld1q_u8(k.as_ptr())));
        }
        s = veorq_u8(
            vaeseq_u8(s, vld1q_u8(rk[9].as_ptr())),
            vld1q_u8(rk[10].as_ptr()),
        );
        vst1q_u8(out.as_mut_ptr(), s);
    }
    out
}

/// The equivalent inverse cipher: AESD(s, k) = InvSubBytes(InvShiftRows(s ^ k)), and InvMixColumns is linear, so the
/// middle rounds take InvMixColumns of their round keys.
fn decrypt(rk: &[Block; 11], ct: &Block) -> Block {
    let mut out = [0u8; 16];
    // SAFETY: as `encrypt`.
    unsafe {
        let mut s = vaesdq_u8(vld1q_u8(ct.as_ptr()), vld1q_u8(rk[10].as_ptr()));
        for k in rk[1..10].iter().rev() {
            s = vaesdq_u8(vaesimcq_u8(s), vaesimcq_u8(vld1q_u8(k.as_ptr())));
        }
        s = veorq_u8(s, vld1q_u8(rk[0].as_ptr()));
        vst1q_u8(out.as_mut_ptr(), s);
    }
    out
}

/// FIPS-197 appendix B and C.1, both directions.
pub fn aes128() -> Outcome {
    const CASES: [(&str, &str, &str); 2] = [
        (
            "2b7e151628aed2a6abf7158809cf4f3c",
            "3243f6a8885a308d313198a2e0370734",
            "3925841d02dc09fbdc118597196a0b32",
        ),
        (
            "000102030405060708090a0b0c0d0e0f",
            "00112233445566778899aabbccddeeff",
            "69c4e0d86a7b0430d8cdb78070b4c55a",
        ),
    ];
    for (key, pt, ct) in CASES {
        let block = |s: &str| -> Block { black_box(unhex(s).try_into().unwrap_or([0; 16])) };
        let rk = expand(&block(key));
        check("aes encrypt", &encrypt(&rk, &block(pt)), ct)?;
        check("aes decrypt", &decrypt(&rk, &block(ct)), pt)?;
    }
    Ok(())
}

// ---- SHA-1 and SHA-256 -------------------------------------------------------------------------------------------

/// MD padding: 0x80, zeros, the bit length big-endian, to a multiple of 64 bytes.
fn pad(msg: &[u8]) -> Vec<u8> {
    let mut m = msg.to_vec();
    m.push(0x80);
    while m.len() % 64 != 56 {
        m.push(0);
    }
    m.extend_from_slice(&((msg.len() as u64) * 8).to_be_bytes());
    m
}

/// Four big-endian message words.
fn words(block: &[u8; 64]) -> [uint32x4_t; 4] {
    // SAFETY: each load reads 16 of the block's 64 bytes.
    unsafe {
        core::array::from_fn(|i| {
            vreinterpretq_u32_u8(vrev32q_u8(vld1q_u8(block[16 * i..].as_ptr())))
        })
    }
}

fn sha1(msg: &[u8]) -> [u8; 20] {
    const K: [u32; 4] = [0x5a827999, 0x6ed9eba1, 0x8f1bbcdc, 0xca62c1d6];
    let mut h = [
        0x67452301u32,
        0xefcdab89,
        0x98badcfe,
        0x10325476,
        0xc3d2e1f0,
    ];
    for block in pad(msg).as_chunks::<64>().0 {
        let mut w = words(block);
        // SAFETY: register intrinsics (`sha2` implies the SHA-1 ones on Arm) and loads/stores of `h`.
        unsafe {
            let (abcd0, e0) = (vld1q_u32(h.as_ptr()), h[4]);
            let (mut abcd, mut e) = (abcd0, e0);
            // 20 groups of 4 rounds: choose, parity, majority, parity
            for i in 0..20 {
                let wk = vaddq_u32(w[i % 4], vdupq_n_u32(K[i / 5]));
                let next_e = vsha1h_u32(vgetq_lane_u32::<0>(abcd));
                abcd = match i / 5 {
                    0 => vsha1cq_u32(abcd, e, wk),
                    2 => vsha1mq_u32(abcd, e, wk),
                    _ => vsha1pq_u32(abcd, e, wk),
                };
                e = next_e;
                if i < 16 {
                    let t = vsha1su0q_u32(w[i % 4], w[(i + 1) % 4], w[(i + 2) % 4]);
                    w[i % 4] = vsha1su1q_u32(t, w[(i + 3) % 4]);
                }
            }
            vst1q_u32(h.as_mut_ptr(), vaddq_u32(abcd, abcd0));
            h[4] = h[4].wrapping_add(e);
        }
    }
    let mut out = [0u8; 20];
    for (o, v) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        o.copy_from_slice(&v.to_be_bytes());
    }
    out
}

const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(msg: &[u8]) -> [u8; 32] {
    let mut h = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    for block in pad(msg).as_chunks::<64>().0 {
        let mut w = words(block);
        // SAFETY: register intrinsics and loads/stores of `h` and `K256` (16 bytes each, in bounds).
        unsafe {
            let (abcd0, efgh0) = (vld1q_u32(h.as_ptr()), vld1q_u32(h[4..].as_ptr()));
            let (mut abcd, mut efgh) = (abcd0, efgh0);
            for i in 0..16 {
                let wk = vaddq_u32(w[i % 4], vld1q_u32(K256[4 * i..].as_ptr()));
                let prev = abcd;
                abcd = vsha256hq_u32(abcd, efgh, wk);
                efgh = vsha256h2q_u32(efgh, prev, wk);
                if i < 12 {
                    let t = vsha256su0q_u32(w[i % 4], w[(i + 1) % 4]);
                    w[i % 4] = vsha256su1q_u32(t, w[(i + 2) % 4], w[(i + 3) % 4]);
                }
            }
            vst1q_u32(h.as_mut_ptr(), vaddq_u32(abcd, abcd0));
            vst1q_u32(h[4..].as_mut_ptr(), vaddq_u32(efgh, efgh0));
        }
    }
    let mut out = [0u8; 32];
    for (o, v) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        o.copy_from_slice(&v.to_be_bytes());
    }
    out
}

const ABC: &[u8] = b"abc";
const MSG448: &[u8] = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";

/// FIPS 180-2 appendix A: "abc" (one block) and the 448-bit message (two blocks).
pub fn sha1_kat() -> Outcome {
    check(
        "sha1 abc",
        &sha1(black_box(ABC)),
        "a9993e364706816aba3e25717850c26c9cd0d89d",
    )?;
    check(
        "sha1 448",
        &sha1(black_box(MSG448)),
        "84983e441c3bd26ebaae4aa1f95129e5e54670f1",
    )
}

/// FIPS 180-2 appendix B: the same two messages.
pub fn sha256_kat() -> Outcome {
    check(
        "sha256 abc",
        &sha256(black_box(ABC)),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    )?;
    check(
        "sha256 448",
        &sha256(black_box(MSG448)),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
    )
}

/// A test's name and body.
pub type Test = (&'static str, fn() -> Outcome);

pub const TESTS: [Test; 4] = [
    ("crc32", crc32),
    ("aes", aes128),
    ("sha1", sha1_kat),
    ("sha256", sha256_kat),
];

#[cfg(test)]
mod tests {
    #[test]
    fn known_answers() {
        for (name, test) in super::TESTS {
            assert_eq!(test(), Ok(()), "{name}");
        }
    }
}
