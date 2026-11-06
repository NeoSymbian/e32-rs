// SPDX-License-Identifier: Apache-2.0
//! R3 test program (`NEOSRV.EXE`): the NeoEcho server (crate `neoecho`) as its own process. A client starts it with
//! `RProcess::Create` and waits in `RProcess::Rendezvous`; it serves until the last session closes, then exits 0.
#![no_std]

use e32::server::{Server, serve};
use e32::{dprintln, process, rt};

e32::main!(main);

fn main() -> i32 {
    let server = match Server::create(neoecho::NAME) {
        Ok(s) => s,
        Err(e) => {
            dprintln!("neosrv: create failed {}", e);
            process::rendezvous(e);
            return e;
        }
    };
    process::rendezvous(0);
    match rt::block_on(serve(&server, &mut neoecho::Echo::default())) {
        Ok(()) => {
            dprintln!("neosrv: done");
            0
        }
        Err(e) => {
            dprintln!("neosrv: receive failed {}", e);
            e
        }
    }
}
