// SPDX-License-Identifier: Apache-2.0
//! What a window server needs from the kernel (the hook calls return nothing: failures panic the caller): the display's mode info (HAL) and the raw input event queue.
//!
//! Layouts: `kernel/eka/include/videodriver.h` (`TVideoInfoV01`), `e32event.h` (`TRawEvent`); calls:
//! `kernel/sexec.cpp` (`HalFunction`, `WsRegisterThread`), `kernel/eventq.cpp` (event hook).

use crate::des::TPtr8;
use crate::{TInt, TRequestStatus};

/// `EHalGroupKernel` (`u32hal.h`).
pub const EHAL_GROUP_KERNEL: TInt = 0;
/// `EKernelHalNTickPeriod`: the nanokernel tick in microseconds, written as a raw `TInt` (`sutils.cpp:2630`).
pub const EKERNEL_HAL_NTICK_PERIOD: TInt = 11;
/// `EKernelHalFastCounterFrequency` (`u32hal.h:412`): the fast counter's frequency in Hz.
pub const EKERNEL_HAL_FAST_COUNTER_FREQUENCY: TInt = 10;
/// `EHalGroupVariant` (`u32hal.h`): NeoSymbian's own functions are 0x4e00.. (kernel `a64_variant.cpp`).
pub const EHAL_GROUP_VARIANT: TInt = 1;
/// `ENeoHalPower`: a1 = 0 restart, 1 restart to fastboot, 2 power off; needs PowerMgmt. Returns only on a failure.
pub const ENEO_HAL_POWER: TInt = 0x4e0b;
/// `ENeoHalRtcRead`: the PMIC RTC's raw seconds, written as a `TUint32`; KErrNotSupported without one.
pub const ENEO_HAL_RTC_READ: TInt = 0x4e0c;
/// `EHalGroupDisplay` (`u32hal.h`).
pub const EHAL_GROUP_DISPLAY: TInt = 4;
/// `EDisplayHalCurrentModeInfo`: fills a `TVideoInfoV01` for the current mode.
pub const EDISPLAY_HAL_CURRENT_MODE_INFO: TInt = 23;

/// `TVideoInfoV01` (`videodriver.h:53-67`), 52 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TVideoInfoV01 {
    pub i_size_in_pixels: [TInt; 2],
    pub i_size_in_twips: [TInt; 2],
    pub i_is_mono: TInt,
    pub i_is_palettized: TInt,
    pub i_bits_per_pixel: TInt,
    /// Linear address of the frame buffer. On the multiple memory model it is a global user-writable mapping.
    pub i_video_address: u32,
    pub i_offset_to_first_pixel: TInt,
    pub i_offset_between_lines: TInt,
    pub i_is_pixel_order_rgb: TInt,
    pub i_is_pixel_order_landscape: TInt,
    pub i_display_mode: TInt,
}

/// `Exec::HalFunction(TInt aGroup, TInt aFunction, TAny* a1, TAny* a2)`: the group's top 16 bits are the device.
///
/// # Safety
/// `a1`/`a2` must be what the HAL function expects (often a descriptor the kernel writes into).
pub unsafe fn hal_function(group: TInt, function: TInt, a1: *mut u8, a2: *mut u8) -> TInt {
    // SAFETY: forwarded caller contract.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::HAL_FUNCTION }>(
            group as u32,
            function as u32,
            a1 as u32,
            a2 as u32,
        ) as TInt
    }
}

/// The current display mode (`HAL::Get(EDisplayMemoryAddress)` and friends go through this, `userhal.cpp:402`).
pub fn display_mode_info() -> Result<TVideoInfoV01, TInt> {
    let mut info = TVideoInfoV01::default();
    // SAFETY: plain-old-data viewed as bytes; the kernel copies at most its length (Kern::InfoCopy).
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(
            (&mut info as *mut TVideoInfoV01).cast::<u8>(),
            core::mem::size_of::<TVideoInfoV01>(),
        )
    };
    let mut des = TPtr8::new(bytes);
    // SAFETY: a real TPtr8 over `info` (the kernel checks the descriptor type), alive for the call.
    let r = unsafe {
        hal_function(
            EHAL_GROUP_DISPLAY,
            EDISPLAY_HAL_CURRENT_MODE_INFO,
            (&mut des as *mut TPtr8).cast(),
            core::ptr::null_mut(),
        )
    };
    if r < 0 { Err(r) } else { Ok(info) }
}

/// The fast counter's frequency in Hz (`User::FastCounter` units).
pub fn fast_counter_frequency() -> Result<TInt, TInt> {
    let mut hz: TInt = 0;
    // SAFETY: the kernel writes one TInt to `a1` (kumemput32, sutils.cpp:2636).
    let r = unsafe {
        hal_function(
            EHAL_GROUP_KERNEL,
            EKERNEL_HAL_FAST_COUNTER_FREQUENCY,
            (&mut hz as *mut TInt).cast(),
            core::ptr::null_mut(),
        )
    };
    if r < 0 { Err(r) } else { Ok(hz) }
}

/// The nanokernel tick period in microseconds (`NTickCount` units).
pub fn ntick_period_us() -> Result<TInt, TInt> {
    let mut us: TInt = 0;
    // SAFETY: the kernel writes one TInt to `a1` (kumemput32).
    let r = unsafe {
        hal_function(
            EHAL_GROUP_KERNEL,
            EKERNEL_HAL_NTICK_PERIOD,
            (&mut us as *mut TInt).cast(),
            core::ptr::null_mut(),
        )
    };
    if r < 0 { Err(r) } else { Ok(us) }
}

/// NeoSymbian: restart (0), restart to fastboot (1) or power off (2). Only a failure comes back.
pub fn neo_power(action: TInt) -> TInt {
    // SAFETY: the kernel takes the action as a plain value in a1.
    unsafe {
        hal_function(
            EHAL_GROUP_VARIANT,
            ENEO_HAL_POWER,
            action as usize as *mut u8,
            core::ptr::null_mut(),
        )
    }
}

/// NeoSymbian: the board RTC's raw seconds (no fixed epoch: it counts on through power-offs).
pub fn neo_rtc_seconds() -> Result<u32, TInt> {
    let mut secs: u32 = 0;
    // SAFETY: the kernel writes one TUint32 to `a1` (kumemput32).
    let r = unsafe {
        hal_function(
            EHAL_GROUP_VARIANT,
            ENEO_HAL_RTC_READ,
            (&mut secs as *mut u32).cast(),
            core::ptr::null_mut(),
        )
    };
    if r < 0 { Err(r) } else { Ok(secs) }
}

/// `TRawEvent::TType` values (`e32event.h`) the UI handles.
pub mod raw {
    pub const EPOINTER_MOVE: u8 = 1;
    pub const EKEY_DOWN: u8 = 3;
    pub const EKEY_UP: u8 = 4;
    pub const EBUTTON1_DOWN: u8 = 10;
    pub const EBUTTON1_UP: u8 = 11;
}

/// `TRawEvent` (`e32event.h:398-409`), 32 bytes: type, tip, pointer number, device number, ticks, then a union whose
/// first two words are the position (x, y) or the scan code and repeat count.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TRawEvent {
    pub i_type: u8,
    pub i_tip: u8,
    pub i_pointer_number: u8,
    pub i_device_number: u8,
    pub i_ticks: TInt,
    pub i_u: [TInt; 6],
}

impl TRawEvent {
    /// Pointer position (pointer events).
    pub fn pos(&self) -> (TInt, TInt) {
        (self.i_u[0], self.i_u[1])
    }
    /// Scan code (key events).
    pub fn scan_code(&self) -> TInt {
        self.i_u[0]
    }
}

/// `TRawEventBuf` = `TPckgBuf<TRawEvent>`, 40 bytes. The kernel writes only the 32-byte event at +8
/// (`eventq.cpp:193-212`); the descriptor header is kept valid anyway.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TRawEventBuf {
    len_type: u32,
    max_len: TInt,
    pub event: TRawEvent,
}

impl Default for TRawEventBuf {
    fn default() -> Self {
        // EBuf (3) << 28 | 32, max 32
        Self {
            len_type: 3 << 28 | 32,
            max_len: 32,
            event: TRawEvent::default(),
        }
    }
}

/// `Exec::WsRegisterThread()`: make this process "the window server" — the first caller wins; any other process
/// calling it later is panicked (KERN-EXEC 1).
pub fn ws_register_thread() {
    // SAFETY: no arguments.
    unsafe { crate::swi::exec4::<{ crate::execs::slow::WS_REGISTER_THREAD }>(0, 0, 0, 0) };
}

/// `Exec::CaptureEventHook()`: only the window-server process may, once (KERN-EXEC 12 on a second capture).
pub fn capture_event_hook() {
    // SAFETY: no arguments.
    unsafe { crate::swi::exec4::<{ crate::execs::slow::CAPTURE_EVENT_HOOK }>(0, 0, 0, 0) };
}

/// `Exec::ReleaseEventHook()`.
pub fn release_event_hook() {
    // SAFETY: no arguments.
    unsafe { crate::swi::exec4::<{ crate::execs::slow::RELEASE_EVENT_HOOK }>(0, 0, 0, 0) };
}

/// `Exec::RequestEvent(TRawEventBuf&, TRequestStatus&)`: the next raw event is written into `buf` and completes
/// `status`.
///
/// # Safety
/// `buf` and `status` (set pending first) must stay valid and unmoved until it completes or is cancelled.
pub unsafe fn request_event(buf: *mut TRawEventBuf, status: *mut TRequestStatus) {
    // SAFETY: forwarded caller contract.
    unsafe {
        crate::swi::exec4::<{ crate::execs::slow::REQUEST_EVENT }>(buf as u32, status as u32, 0, 0)
    };
}

/// `Exec::RequestEventCancel()`: a pending `RequestEvent` completes with `KErrCancel`.
pub fn request_event_cancel() {
    // SAFETY: no arguments.
    unsafe { crate::swi::exec4::<{ crate::execs::slow::REQUEST_EVENT_CANCEL }>(0, 0, 0, 0) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn layouts_and_numbers() {
        assert_eq!(size_of::<TVideoInfoV01>(), 52);
        assert_eq!(core::mem::offset_of!(TVideoInfoV01, i_video_address), 28);
        assert_eq!(
            core::mem::offset_of!(TVideoInfoV01, i_offset_between_lines),
            36
        );
        assert_eq!(size_of::<TRawEvent>(), 32);
        assert_eq!(size_of::<TRawEventBuf>(), 40);
        assert_eq!(core::mem::offset_of!(TRawEventBuf, event), 8);
        use crate::execs::slow;
        assert_eq!(
            (slow::CAPTURE_EVENT_HOOK, slow::RELEASE_EVENT_HOOK),
            (0x48, 0x49)
        );
        assert_eq!(
            (slow::REQUEST_EVENT, slow::REQUEST_EVENT_CANCEL),
            (0x4A, 0x4B)
        );
        assert_eq!((slow::HAL_FUNCTION, slow::WS_REGISTER_THREAD), (0x4F, 0x50));
    }
}
