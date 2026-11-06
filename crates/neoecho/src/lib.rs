// SPDX-License-Identifier: Apache-2.0
//! NeoEcho: a small IPC service used to test `e32::server` — by NEOSRV.EXE (its own process, driven by the C++
//! client `symbian/neocli.cpp`) and by neotest (a server thread, driven by a Rust client).
//!
//! Function numbers and behaviour (the C++ client mirrors them):
//! - `ADD(int a, int b)` completes with `a + b`.
//! - `REVERSE8(DesC8 in, Des8 out)` writes `in` reversed into `out`, completes with its length.
//! - `UPPER16(DesC16 in, Des16 out)` writes `in` with ASCII letters upper-cased into `out`, completes with its length.
//! - `WRITE_AT(Des8 out, int offset)` writes `"xyz"` into `out` at `offset`: 0, or `KErrOverflow` past its capacity.
//! - `COUNT()` completes with the number of requests on this session so far, itself included.
//! - Anything else completes with `KErrNotSupported`; so does a connect with a major version other than 1.
//!
//! The server stops (its `serve` returns) when the last connected session closes.
#![no_std]

extern crate alloc;

use core::ops::ControlFlow;
use e32::server::{Message, Service};
use e32::sys::{TVersion, err};

pub const NAME: &str = "NeoEcho";
pub const VERSION: TVersion = TVersion {
    major: 1,
    minor: 0,
    build: 0,
};

pub mod op {
    pub const ADD: i32 = 1;
    pub const REVERSE8: i32 = 2;
    pub const UPPER16: i32 = 3;
    pub const WRITE_AT: i32 = 4;
    pub const COUNT: i32 = 5;
}

#[derive(Default)]
pub struct Echo {
    live: u32,
}

pub struct Session {
    requests: i32,
}

impl Service for Echo {
    type Session = Session;

    fn connect(&mut self, version: TVersion) -> Result<Session, i32> {
        if version.major != VERSION.major {
            return Err(err::KERR_NOT_SUPPORTED);
        }
        self.live += 1;
        Ok(Session { requests: 0 })
    }

    fn message(&mut self, s: &mut Session, m: Message) {
        s.requests += 1;
        let r = match m.function() {
            op::ADD => Ok(m.int(0).wrapping_add(m.int(1))),
            op::REVERSE8 => m.read8_vec(0).and_then(|mut v| {
                v.reverse();
                m.write8(1, &v, 0).map(|()| v.len() as i32)
            }),
            op::UPPER16 => m.read16_vec(0).and_then(|mut v| {
                for c in &mut v {
                    if (u16::from(b'a')..=u16::from(b'z')).contains(c) {
                        *c -= 32;
                    }
                }
                m.write16(1, &v, 0).map(|()| v.len() as i32)
            }),
            op::WRITE_AT => m.write8(0, b"xyz", m.int(1) as usize).map(|()| 0),
            op::COUNT => Ok(s.requests),
            _ => Err(err::KERR_NOT_SUPPORTED),
        };
        m.complete(r.unwrap_or_else(|e| e));
    }

    fn disconnect(&mut self, _: Session) -> ControlFlow<()> {
        self.live -= 1;
        if self.live == 0 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }
}
