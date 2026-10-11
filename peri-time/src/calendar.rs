use std::{fmt, time::SystemTime};

/// The calendar convention selected by the deployment, not by a stored timestamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalendarConvention {
    HostLocal,
    Utc,
}

impl CalendarConvention {
    /// Native hosts retain their local calendar; Emscripten uses UTC.
    pub const fn deployment_default() -> Self {
        #[cfg(target_os = "emscripten")]
        {
            Self::Utc
        }
        #[cfg(not(target_os = "emscripten"))]
        {
            Self::HostLocal
        }
    }
}

/// A date without a time or timezone, obtained by interpreting a wall-clock instant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CalendarDate {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl fmt::Display for CalendarDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// Interpret one fixed wall-clock value using the selected calendar convention.
pub fn calendar_date(at: SystemTime, convention: CalendarConvention) -> CalendarDate {
    use chrono::{DateTime, Datelike, Local, Utc};
    let utc: DateTime<Utc> = at.into();
    match convention {
        CalendarConvention::HostLocal => {
            let local = utc.with_timezone(&Local);
            CalendarDate {
                year: local.year(),
                month: local.month(),
                day: local.day(),
            }
        }
        CalendarConvention::Utc => CalendarDate {
            year: utc.year(),
            month: utc.month(),
            day: utc.day(),
        },
    }
}

/// Serialize a wall-clock value in RFC 3339 with an explicit UTC offset.
pub fn format_utc_rfc3339(at: SystemTime) -> String {
    let utc: chrono::DateTime<chrono::Utc> = at.into();
    utc.to_rfc3339()
}

#[cfg(test)]
#[path = "calendar_test.rs"]
mod tests;
