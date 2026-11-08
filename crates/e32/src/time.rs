// SPDX-License-Identifier: Apache-2.0
//! Timers (`RTimer`) and the tick counter.

use crate::request::{TRequestStatus, wait_for_request};
use crate::sys;
use core::future::Future;
use core::marker::PhantomData;
use core::pin::Pin;
use core::task::{Context, Poll};

/// Nanokernel ticks since boot (`NTickCount`).
pub fn ticks() -> u32 {
    sys::ntick_count()
}

/// The tick period in microseconds (1000 on many boards; Syborg's differs). 0 if the HAL refuses.
pub fn tick_period_us() -> u32 {
    sys::ui::ntick_period_us().map_or(0, |us| us as u32)
}

/// The high-resolution counter (`User::FastCounter`), for timing short things; wraps at 2^32.
pub fn fast_counter() -> u32 {
    sys::fast_counter()
}

/// The fast counter's frequency in Hz; 0 if the HAL refuses.
pub fn fast_counter_hz() -> u32 {
    sys::ui::fast_counter_frequency().map_or(0, |hz| hz as u32)
}

/// Microseconds between fast-counter readings `from` and `to` at `hz`, across one wrap.
pub fn counter_us(from: u32, to: u32, hz: u32) -> u32 {
    (u64::from(to.wrapping_sub(from)) * 1_000_000 / u64::from(hz.max(1))) as u32
}

/// Milliseconds since boot, from the tick counter. Wraps with the counter (2^32 ticks).
pub fn millis() -> u64 {
    u64::from(ticks()) * u64::from(tick_period_us()) / 1000
}

/// Calendar time from the kernel's clock (`TimeNow`), local time = UTC + the kernel's UTC offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime {
    pub year: i32,
    /// 1-12
    pub month: u32,
    /// 1-31
    pub day: u32,
    /// 0 = Monday ... 6 = Sunday
    pub weekday: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// Days from Symbian's `TTime` zero to 1970-01-01. Symbian counts leap years before 1600 the Julian way
/// (`euser/us_time.cpp:2486`), so 2000-01-01 is day 730,497 (`KDaysFrom0ADTo2000AD`, `kernel/stimer.cpp:29`): 12 days
/// more than a proleptic Gregorian count. The kernel's clock and ROM timestamps use it, so dates here must too.
// shortcut: dates before 1600 come out Gregorian, not Symbian's Julian; nothing on a phone has them.
const DAYS_0_TO_1970: i64 = 719_540;

impl DateTime {
    /// From a Symbian `TTime` value: microseconds since 0000-01-01 00:00.
    pub fn from_ttime(us: i64) -> Self {
        let secs = us.div_euclid(1_000_000);
        let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
        let (year, month, day) = civil_from_days(days - DAYS_0_TO_1970);
        Self {
            year,
            month,
            day,
            // 1970-01-01 was a Thursday (3)
            weekday: (days - DAYS_0_TO_1970 + 3).rem_euclid(7) as u32,
            hour: (sod / 3600) as u32,
            minute: (sod / 60 % 60) as u32,
            second: (sod % 60) as u32,
        }
    }
}

/// A Symbian `TTime` (microseconds since 0000-01-01) from Unix seconds.
pub fn ttime_from_unix(secs: i64) -> i64 {
    (secs + DAYS_0_TO_1970 * 86_400) * 1_000_000
}

/// Unix seconds from a Symbian `TTime`.
pub fn unix_from_ttime(us: i64) -> i64 {
    us.div_euclid(1_000_000) - DAYS_0_TO_1970 * 86_400
}

/// The kernel's UTC (Unix seconds) and UTC offset (seconds).
pub fn utc_now() -> (i64, i32) {
    let (utc, offset) = sys::time_now();
    (unix_from_ttime(utc), offset)
}

/// Set the kernel's UTC (Unix seconds) and UTC offset (seconds). Needs WriteDeviceData.
pub fn set_utc_and_offset(unix: i64, offset: i32) -> Result<(), i32> {
    match sys::set_utc_time_and_offset(ttime_from_unix(unix), offset) {
        r if r < 0 => Err(r),
        _ => Ok(()),
    }
}

/// The board RTC's raw seconds: no fixed epoch, but it keeps counting while the phone is off. `None` without one.
pub fn rtc_seconds() -> Option<u32> {
    sys::ui::neo_rtc_seconds().ok()
}

/// Local time now.
pub fn now() -> DateTime {
    let (utc, offset) = sys::time_now();
    DateTime::from_ttime(utc + i64::from(offset) * 1_000_000)
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y as i32, m, d)
}

/// A kernel timer. Timer handles are owned by the thread that creates them, so a `Timer` stays on its thread.
pub struct Timer {
    handle: i32,
    _thread_bound: PhantomData<*const ()>,
}

impl Timer {
    pub fn new() -> Result<Self, i32> {
        let h = sys::timer_create();
        if h < 0 {
            Err(h)
        } else {
            Ok(Self {
                handle: h,
                _thread_bound: PhantomData,
            })
        }
    }

    /// Start the timer; `status` completes after `micros` microseconds.
    ///
    /// # Safety
    /// `status` must not move or be dropped until it completes (or [`Timer::cancel`] and a wait).
    pub unsafe fn after(&self, micros: i32, status: &mut TRequestStatus) {
        *status = TRequestStatus::pending();
        // SAFETY: forwarded caller contract.
        unsafe { sys::timer_after(self.handle, status, micros) };
    }

    /// Complete a pending request early with `KErrCancel`.
    pub fn cancel(&self) {
        sys::timer_cancel(self.handle);
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        sys::handle_close(self.handle);
    }
}

/// Block the thread for `micros` microseconds.
pub fn sleep(micros: i32) {
    let t = Timer::new().unwrap_or_else(|e| crate::process::panic("RUST-TIMER", e));
    let mut s = TRequestStatus::pending();
    // SAFETY: `s` lives on this frame until the wait below returns.
    unsafe { t.after(micros, &mut s) };
    wait_for_request(&s);
}

/// A future that completes after `micros` microseconds; for [`crate::rt::block_on`].
pub fn delay(micros: i32) -> Delay {
    Delay {
        micros,
        timer: None,
        status: TRequestStatus::pending(),
    }
}

pub struct Delay {
    micros: i32,
    timer: Option<Timer>,
    status: TRequestStatus,
}

impl Future for Delay {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        // SAFETY: nothing is moved out; `status` stays where it is while the request runs (the future is pinned).
        let this = unsafe { self.get_unchecked_mut() };
        match &this.timer {
            None => {
                let t = Timer::new().unwrap_or_else(|e| crate::process::panic("RUST-TIMER", e));
                // SAFETY: `status` is inside the pinned future; Drop cancels and waits before it goes away.
                unsafe { t.after(this.micros, &mut this.status) };
                this.timer = Some(t);
                Poll::Pending
            }
            Some(_) if this.status.is_pending() => Poll::Pending,
            Some(_) => Poll::Ready(()),
        }
    }
}

impl Drop for Delay {
    fn drop(&mut self) {
        if let Some(t) = &self.timer
            && self.status.is_pending()
        {
            t.cancel();
            wait_for_request(&self.status);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DateTime;

    #[test]
    fn counter_intervals() {
        assert_eq!(super::counter_us(100, 1_100, 1_000_000), 1_000);
        assert_eq!(super::counter_us(u32::MAX - 9, 10, 1_000), 20_000); // across the wrap
        assert_eq!(super::counter_us(0, 32_768, 32_768), 1_000_000);
        assert_eq!(super::counter_us(5, 6, 0), 1_000_000); // no frequency: no division by zero
    }

    #[test]
    fn unix_round_trip() {
        // 2026-11-08 12:00:00 UTC
        let t = super::ttime_from_unix(1_794_139_200);
        let d = DateTime::from_ttime(t);
        assert_eq!(
            (d.year, d.month, d.day, d.hour, d.minute),
            (2026, 11, 8, 12, 0)
        );
        assert_eq!(super::unix_from_ttime(t), 1_794_139_200);
        assert_eq!(super::unix_from_ttime(t + 999_999), 1_794_139_200);
    }

    #[test]
    fn ttime_to_calendar() {
        // Symbian's own anchor: 2000-01-01 (a Saturday) is day 730,497 (kernel/stimer.cpp:29)
        let y2k = DateTime::from_ttime(730_497 * 86_400 * 1_000_000);
        assert_eq!((y2k.year, y2k.month, y2k.day, y2k.weekday), (2000, 1, 1, 5));
        let epoch = DateTime::from_ttime(719_540 * 86_400 * 1_000_000);
        assert_eq!(
            (epoch.year, epoch.month, epoch.day, epoch.weekday),
            (1970, 1, 1, 3)
        );
        // 2026-11-06 is a Friday; 12:34:56
        let days = 719_540 + 20_763;
        let t = DateTime::from_ttime(((days * 86_400) + 12 * 3600 + 34 * 60 + 56) * 1_000_000);
        assert_eq!(
            (
                t.year, t.month, t.day, t.weekday, t.hour, t.minute, t.second
            ),
            (2026, 11, 6, 4, 12, 34, 56)
        );
        let leap = DateTime::from_ttime((719_540 + 11_016) * 86_400 * 1_000_000);
        assert_eq!((leap.year, leap.month, leap.day), (2000, 2, 29));
    }
}
