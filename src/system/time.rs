use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Distinct wall-clock and monotonic timestamps.
///
/// `wall_clock` may jump on NTP/timezone adjustments; `monotonic` is strictly non-decreasing and
/// suited for durations and animations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSnapshot {
    pub wall_clock: SystemTime,
    pub monotonic: Instant,
}

impl TimeSnapshot {
    /// Converts `wall_clock` into local calendar/clock fields using the OS timezone.
    #[must_use]
    pub fn local(&self) -> LocalDateTime {
        LocalDateTime::from_system_time(self.wall_clock)
    }
}

#[must_use]
pub fn now_snapshot() -> TimeSnapshot {
    TimeSnapshot {
        wall_clock: SystemTime::now(),
        monotonic: Instant::now(),
    }
}

/// Clock formatting style (`24-hour` vs `12-hour AM/PM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TimeFormat {
    #[default]
    H24,
    H12,
}

/// Broken-down local date and time in the system timezone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalDateTime {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// Day of week (`0 = Monday .. 6 = Sunday`).
    pub weekday_mon0: u8,
}

impl LocalDateTime {
    /// Returns the current local date and time.
    #[must_use]
    pub fn now() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    /// Converts a [`SystemTime`] into local broken-down date/time via `libc::localtime_r`.
    #[must_use]
    pub fn from_system_time(wall_clock: SystemTime) -> Self {
        let secs = wall_clock
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as libc::time_t)
            .unwrap_or(0);
        let mut tm = std::mem::MaybeUninit::<libc::tm>::zeroed();
        // SAFETY: `secs` and `tm` are valid stack locals for `localtime_r`.
        let ptr = unsafe { libc::localtime_r(&secs, tm.as_mut_ptr()) };
        if ptr.is_null() {
            return Self {
                year: 1970,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
                weekday_mon0: 3,
            };
        }
        // SAFETY: `localtime_r` returned a non-null pointer after initializing `tm`.
        let tm = unsafe { tm.assume_init() };
        Self {
            year: tm.tm_year + 1900,
            month: (tm.tm_mon + 1).clamp(1, 12) as u8,
            day: tm.tm_mday.clamp(1, 31) as u8,
            hour: tm.tm_hour.clamp(0, 23) as u8,
            minute: tm.tm_min.clamp(0, 59) as u8,
            second: tm.tm_sec.clamp(0, 60).min(59) as u8,
            weekday_mon0: ((tm.tm_wday + 6) % 7) as u8,
        }
    }

    /// Returns `true` when `hour >= 12`.
    #[must_use]
    pub const fn is_pm(self) -> bool {
        self.hour >= 12
    }

    /// Returns `"AM"` or `"PM"`.
    #[must_use]
    pub const fn am_pm(self) -> &'static str {
        if self.is_pm() { "PM" } else { "AM" }
    }

    /// Hour in 12-hour clock (`1..=12`).
    #[must_use]
    pub const fn hour_12(self) -> u8 {
        match self.hour % 12 {
            0 => 12,
            h => h,
        }
    }

    /// Formats `HH:MM` (24-hour) or `hh:MM AM/PM` (12-hour).
    #[must_use]
    pub fn format_hm(self, fmt: TimeFormat) -> String {
        match fmt {
            TimeFormat::H24 => format!("{:02}:{:02}", self.hour, self.minute),
            TimeFormat::H12 => format!("{:02}:{:02} {}", self.hour_12(), self.minute, self.am_pm()),
        }
    }

    /// Formats `HH:MM:SS` (24-hour) or `hh:MM:SS AM/PM` (12-hour).
    #[must_use]
    pub fn format_hms(self, fmt: TimeFormat) -> String {
        match fmt {
            TimeFormat::H24 => format!("{:02}:{:02}:{:02}", self.hour, self.minute, self.second),
            TimeFormat::H12 => format!(
                "{:02}:{:02}:{:02} {}",
                self.hour_12(),
                self.minute,
                self.second,
                self.am_pm()
            ),
        }
    }

    /// Full English weekday name (e.g. `"Sunday"`).
    #[must_use]
    pub const fn weekday_name(self) -> &'static str {
        match self.weekday_mon0 {
            0 => "Monday",
            1 => "Tuesday",
            2 => "Wednesday",
            3 => "Thursday",
            4 => "Friday",
            5 => "Saturday",
            _ => "Sunday",
        }
    }

    /// Full English month name (e.g. `"November"`).
    #[must_use]
    pub const fn month_name(self) -> &'static str {
        month_name(self.month)
    }

    /// Formats a long date such as `"Sunday, 15 November 2026"`.
    #[must_use]
    pub fn format_long_date(self) -> String {
        format!(
            "{}, {:02} {} {}",
            self.weekday_name(),
            self.day,
            self.month_name(),
            self.year
        )
    }

    /// Formats an ISO date `YYYY-MM-DD`.
    #[must_use]
    pub fn format_iso_date(self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// Full English month name for `1..=12`.
#[must_use]
pub const fn month_name(month: u8) -> &'static str {
    match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "Unknown",
    }
}

/// Returns `true` if `year` is a Gregorian leap year.
#[must_use]
pub const fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Number of days in `(year, month)` (`1..=12`).
#[must_use]
pub const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Shifts `(year, month)` (`month` in `1..=12`) by `delta_months`.
#[must_use]
pub const fn shift_year_month(year: i32, month: u8, delta_months: i32) -> (i32, u8) {
    let total = year * 12 + (month as i32 - 1) + delta_months;
    let new_year = total.div_euclid(12);
    let new_month = (total.rem_euclid(12) + 1) as u8;
    (new_year, new_month)
}

/// Day-of-week (`0 = Monday .. 6 = Sunday`) using Sakamoto's algorithm.
#[must_use]
pub const fn weekday_mon0(year: i32, month: u8, day: u8) -> u8 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if month < 3 { year - 1 } else { year };
    let sun0 = (y + y / 4 - y / 100 + y / 400 + T[(month as usize).saturating_sub(1)] + day as i32)
        .rem_euclid(7);
    ((sun0 + 6) % 7) as u8
}

/// Day-of-year (`1..=366`).
#[must_use]
pub fn day_of_year(year: i32, month: u8, day: u8) -> u16 {
    let mut sum = 0u16;
    for m in 1..month {
        sum += u16::from(days_in_month(year, m));
    }
    sum + u16::from(day)
}

/// ISO-8601 week number (`1..=53`).
#[must_use]
pub fn iso_week_number(year: i32, month: u8, day: u8) -> u8 {
    let doy = i32::from(day_of_year(year, month, day));
    let dow = i32::from(weekday_mon0(year, month, day)) + 1;
    let week = (doy - dow + 10) / 7;
    if week < 1 {
        iso_week_number(year - 1, 12, 31)
    } else if week > 52 {
        let dec31_dow = i32::from(weekday_mon0(year, 12, 31)) + 1;
        if dec31_dow < 4 { 1 } else { week as u8 }
    } else {
        week as u8
    }
}

/// Single day cell inside a [`CalendarRow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarDayCell {
    pub day: Option<u8>,
    pub is_today: bool,
}

/// One Monday-to-Sunday week row in a [`CalendarMonth`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalendarRow {
    pub iso_week: u8,
    pub days: [CalendarDayCell; 7],
}

/// Structured calendar month grid ready for UI rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarMonth {
    pub year: i32,
    pub month: u8,
    pub title: String,
    pub rows: Vec<CalendarRow>,
}

impl CalendarMonth {
    /// Builds a Monday-first calendar grid for `(year, month)` highlighting `today` if provided.
    #[must_use]
    pub fn build(year: i32, month: u8, today: Option<(i32, u8, u8)>) -> Self {
        let total_days = days_in_month(year, month);
        let first_wd = weekday_mon0(year, month, 1) as usize;
        let mut rows = Vec::with_capacity(6);
        let mut cur_day = 1u8;

        while cur_day <= total_days {
            let week_anchor_day = cur_day;
            let mut days = [CalendarDayCell {
                day: None,
                is_today: false,
            }; 7];
            let start_col = if cur_day == 1 { first_wd } else { 0 };
            for slot in &mut days[start_col..] {
                if cur_day <= total_days {
                    let is_today = today == Some((year, month, cur_day));
                    *slot = CalendarDayCell {
                        day: Some(cur_day),
                        is_today,
                    };
                    cur_day += 1;
                }
            }
            rows.push(CalendarRow {
                iso_week: iso_week_number(year, month, week_anchor_day),
                days,
            });
        }

        Self {
            year,
            month,
            title: format!("{} {}", month_name(month), year),
            rows,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_datetime_formats_24h_and_12h() {
        let dt = LocalDateTime {
            year: 2026,
            month: 11,
            day: 15,
            hour: 15,
            minute: 4,
            second: 9,
            weekday_mon0: 6,
        };
        assert_eq!(dt.format_hm(TimeFormat::H24), "15:04");
        assert_eq!(dt.format_hm(TimeFormat::H12), "03:04 PM");
        assert_eq!(dt.format_hms(TimeFormat::H12), "03:04:09 PM");
        assert_eq!(dt.format_long_date(), "Sunday, 15 November 2026");
        assert_eq!(dt.format_iso_date(), "2026-11-15");
    }

    #[test]
    fn calendar_month_builds_grid_and_shifts() {
        assert_eq!(shift_year_month(2026, 12, 1), (2027, 1));
        assert_eq!(shift_year_month(2026, 1, -1), (2025, 12));
        let cal = CalendarMonth::build(2026, 11, Some((2026, 11, 15)));
        assert_eq!(cal.title, "November 2026");
        assert!(
            cal.rows
                .iter()
                .flat_map(|r| r.days)
                .any(|c| c.day == Some(15) && c.is_today)
        );
    }
}
