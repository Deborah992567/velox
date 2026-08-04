//! HTTP dates (RFC 9110 §5.6.7).
//!
//! All HTTP date header fields use the IMF-fixdate format
//! (`Sun, 06 Nov 1994 08:49:37 GMT`). When reading them back — `Date`,
//! `Last-Modified`, `If-Modified-Since`, `If-Range` — recipients must also
//! accept the two obsolete formats from RFC 7231, so the parser here handles
//! all three. Parsing is strict: a wrong weekday for the given date, a
//! non-canonical field width, or trailing junk rejects the value rather than
//! guessing, and every component is range-checked by the `time` crate before
//! a date is produced.

use time::{Month, OffsetDateTime, UtcOffset};

const WEEKDAY_SHORT: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const WEEKDAY_LONG: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const MONTH_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Format `dt` as an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`).
pub fn format_http_date(dt: OffsetDateTime) -> String {
    let dt = dt.to_offset(UtcOffset::UTC);
    let weekday = usize::from(dt.weekday().number_days_from_monday());
    let month = usize::from(u8::from(dt.month())) - 1;
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        WEEKDAY_SHORT[weekday],
        dt.day(),
        MONTH_SHORT[month],
        dt.year(),
        dt.hour(),
        dt.minute(),
        dt.second(),
    )
}

/// Parse an HTTP date in any of the three required formats.
///
/// Returns the instant in UTC, or `None` for a value that is not a valid
/// HTTP-date.
pub fn parse_http_date(input: &[u8]) -> Option<OffsetDateTime> {
    let input = input.trim_ascii();
    parse_imf_fixdate(input)
        .or_else(|| parse_rfc850(input))
        .or_else(|| parse_asctime(input))
}

/// A cursor over the byte string being parsed.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Consume `word` if it matches at the current position.
    fn word(&mut self, word: &str) -> Option<()> {
        let bytes = word.as_bytes();
        if self.bytes[self.pos..].starts_with(bytes) {
            self.pos += bytes.len();
            Some(())
        } else {
            None
        }
    }

    /// Consume exactly `n` ASCII digits and return their value.
    fn digits(&mut self, n: usize) -> Option<u64> {
        let slice = self.bytes.get(self.pos..self.pos + n)?;
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        self.pos += n;
        Some(digits_value(slice))
    }

    /// Like [`Self::digits`] but returns the value as `u8`.
    fn digits_u8(&mut self, n: usize) -> Option<u8> {
        u8::try_from(self.digits(n)?).ok()
    }

    /// Like [`Self::digits`] but returns the value as `i32`.
    fn digits_i32(&mut self, n: usize) -> Option<i32> {
        i32::try_from(self.digits(n)?).ok()
    }

    /// Consume one to `max` ASCII digits (at least one) and return their
    /// value as `u8`. Used for the space-padded day in the obsolete asctime
    /// format.
    fn digits_upto_u8(&mut self, max: usize) -> Option<u8> {
        u8::try_from(self.digits_upto(max)?).ok()
    }

    /// Consume one to `max` ASCII digits (at least one) and return their
    /// value. Used for the space-padded day in the obsolete asctime format.
    fn digits_upto(&mut self, max: usize) -> Option<u64> {
        let mut end = self.pos;
        while end < self.bytes.len() && end - self.pos < max && self.bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == self.pos {
            return None;
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Some(digits_value(slice))
    }

    /// Match one of `words` case-sensitively and return its index.
    fn word_enum(&mut self, words: &[&str]) -> Option<usize> {
        let rest = &self.bytes[self.pos..];
        let (index, word) = words.iter().enumerate().find(|(_, w)| {
            let bytes = w.as_bytes();
            rest.len() >= bytes.len() && &rest[..bytes.len()] == bytes
        })?;
        self.pos += word.len();
        Some(index)
    }

    /// Match one of `words` case-sensitively and return its index as `u8`.
    fn word_enum_u8(&mut self, words: &[&str]) -> Option<u8> {
        u8::try_from(self.word_enum(words)?).ok()
    }

    fn skip_spaces(&mut self) {
        while self.bytes.get(self.pos) == Some(&b' ') {
            self.pos += 1;
        }
    }

    const fn at_end(&self) -> bool {
        self.pos == self.bytes.len()
    }
}

fn digits_value(digits: &[u8]) -> u64 {
    digits
        .iter()
        .fold(0, |acc, &b| acc * 10 + u64::from(b - b'0'))
}

fn build_date(
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    expected_weekday: Option<u8>,
) -> Option<OffsetDateTime> {
    let month = Month::try_from(month).ok()?;
    let date = time::Date::from_calendar_date(year, month, day).ok()?;
    if expected_weekday.is_some_and(|expected| date.weekday().number_days_from_monday() != expected)
    {
        return None;
    }
    let time = time::Time::from_hms(hour, minute, second).ok()?;
    Some(date.with_time(time).assume_utc())
}

fn parse_imf_fixdate(input: &[u8]) -> Option<OffsetDateTime> {
    let mut c = Cursor::new(input);
    let weekday = c.word_enum_u8(&WEEKDAY_SHORT)?;
    c.word(", ")?;
    let day = c.digits_u8(2)?;
    c.word(" ")?;
    let month = c.word_enum_u8(&MONTH_SHORT)? + 1;
    c.word(" ")?;
    let year = c.digits_i32(4)?;
    c.word(" ")?;
    let hour = c.digits_u8(2)?;
    c.word(":")?;
    let minute = c.digits_u8(2)?;
    c.word(":")?;
    let second = c.digits_u8(2)?;
    c.word(" GMT")?;
    if !c.at_end() {
        return None;
    }
    build_date(year, month, day, hour, minute, second, Some(weekday))
}

fn parse_rfc850(input: &[u8]) -> Option<OffsetDateTime> {
    let mut c = Cursor::new(input);
    let weekday = c.word_enum_u8(&WEEKDAY_LONG)?;
    c.word(", ")?;
    let day = c.digits_u8(2)?;
    c.word("-")?;
    let month = c.word_enum_u8(&MONTH_SHORT)? + 1;
    c.word("-")?;
    let year = two_digit_year(c.digits_u8(2)?);
    c.word(" ")?;
    let hour = c.digits_u8(2)?;
    c.word(":")?;
    let minute = c.digits_u8(2)?;
    c.word(":")?;
    let second = c.digits_u8(2)?;
    c.word(" GMT")?;
    if !c.at_end() {
        return None;
    }
    build_date(year, month, day, hour, minute, second, Some(weekday))
}

fn parse_asctime(input: &[u8]) -> Option<OffsetDateTime> {
    let mut c = Cursor::new(input);
    let weekday = c.word_enum_u8(&WEEKDAY_SHORT)?;
    c.word(" ")?;
    let month = c.word_enum_u8(&MONTH_SHORT)? + 1;
    c.word(" ")?;
    c.skip_spaces();
    let day = c.digits_upto_u8(2)?;
    c.word(" ")?;
    let hour = c.digits_u8(2)?;
    c.word(":")?;
    let minute = c.digits_u8(2)?;
    c.word(":")?;
    let second = c.digits_u8(2)?;
    c.word(" ")?;
    let year = c.digits_i32(4)?;
    if !c.at_end() {
        return None;
    }
    build_date(year, month, day, hour, minute, second, Some(weekday))
}

/// Expand a two-digit year per RFC 6265: 00–68 map to 2000–2068, 69–99 to
/// 1969–1999.
fn two_digit_year(year: u8) -> i32 {
    if year >= 69 {
        1900 + i32::from(year)
    } else {
        2000 + i32::from(year)
    }
}

#[cfg(test)]
mod tests {
    use super::{format_http_date, parse_http_date};
    use time::macros::datetime;

    #[test]
    fn formats_imf_fixdate() {
        assert_eq!(
            format_http_date(datetime!(1994-11-06 08:49:37 UTC)),
            "Sun, 06 Nov 1994 08:49:37 GMT"
        );
        assert_eq!(
            format_http_date(datetime!(2026-08-02 12:00:00 UTC)),
            "Sun, 02 Aug 2026 12:00:00 GMT"
        );
    }

    #[test]
    fn parses_imf_fixdate() {
        assert_eq!(
            parse_http_date(b"Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(datetime!(1994-11-06 08:49:37 UTC))
        );
        assert_eq!(
            parse_http_date(b"Sun, 02 Aug 2026 12:00:00 GMT"),
            Some(datetime!(2026-08-02 12:00:00 UTC))
        );
    }

    #[test]
    fn parses_obsolete_formats() {
        assert_eq!(
            parse_http_date(b"Sunday, 06-Nov-94 08:49:37 GMT"),
            Some(datetime!(1994-11-06 08:49:37 UTC))
        );
        assert_eq!(
            parse_http_date(b"Sun Nov  6 08:49:37 1994"),
            Some(datetime!(1994-11-06 08:49:37 UTC))
        );
        assert_eq!(
            parse_http_date(b"Wed Nov 16 08:49:37 1994"),
            Some(datetime!(1994-11-16 08:49:37 UTC))
        );
    }

    #[test]
    fn two_digit_years_expand_correctly() {
        assert_eq!(
            parse_http_date(b"Tuesday, 06-Nov-68 08:49:37 GMT"),
            Some(datetime!(2068-11-06 08:49:37 UTC))
        );
        assert_eq!(
            parse_http_date(b"Thursday, 06-Nov-69 08:49:37 GMT"),
            Some(datetime!(1969-11-06 08:49:37 UTC))
        );
    }

    #[test]
    fn format_and_parse_roundtrip() {
        let dt = datetime!(2026-08-02 12:34:56 UTC);
        assert_eq!(parse_http_date(format_http_date(dt).as_bytes()), Some(dt));
    }

    #[test]
    fn rejects_malformed_dates() {
        for bad in [
            b"Sun 06 Nov 1994 08:49:37 GMT".as_slice(), // no comma
            b"Sun, 06 Nov 94 08:49:37 GMT",             // two-digit year
            b"Sun, 6 Nov 1994 08:49:37 GMT",            // unpadded day
            b"Sun, 06 Nov 1994 08:49:37 UTC",           // wrong zone suffix
            b"Sun, 32 Nov 1994 08:49:37 GMT",           // day out of range
            b"Sat, 06 Nov 1994 08:49:37 GMT",           // wrong weekday
            b"Sun, 06 Nov 1994 08:49:37 GMTx",          // trailing junk
            b"Mon, 06 Nov 1994 08:49:37 GMT",           // wrong weekday
            b"Sun, 06 Nov 1994 24:49:37 GMT",           // hour out of range
            b"",                                        // empty
        ] {
            assert_eq!(parse_http_date(bad), None, "must reject {bad:?}");
        }
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            parse_http_date(b"  Sun, 06 Nov 1994 08:49:37 GMT \t"),
            Some(datetime!(1994-11-06 08:49:37 UTC))
        );
    }
}
