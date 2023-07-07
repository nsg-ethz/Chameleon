// BgpSim: BGP Network Simulator written in Rust
// Copyright (C) 2022-2023 Tibor Schneider <sctibor@ethz.ch>
//
// This program is free software; you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation; either version 2 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License along
// with this program; if not, write to the Free Software Foundation, Inc.,
// 51 Franklin Street, Fifth Floor, Boston, MA 02110-1301 USA.

//! Module for parsing cisco tables.

use itertools::Itertools;
use thiserror::Error;

pub struct Assert<const N: usize>;
impl<const N: usize> Assert<N> {
    pub const NON_ZERO: usize = N - 1;
}

/// Parse a table using the given header field names. The first line must be the table header. It
/// returns a vector for each non-empty line, containing a tuple of flags (the thing before the
/// first column), and a vector containing all other elements.
pub fn parse_table<'a, const N: usize>(
    table: &'a str,
    headers: [&'static str; N],
) -> Result<Vec<(&'a str, [&'a str; N])>, TableParseError> {
    _ = Assert::<N>::NON_ZERO;

    let mut lines = table.lines();
    let header = lines
        .next()
        .ok_or_else(|| TableParseError::InvalidHeader(String::new()))?;

    if header.split_whitespace().join(" ") != headers.iter().join(" ") {
        return Err(TableParseError::InvalidHeader(header.to_string()));
    }

    let positions = headers.map(|h| header.find(h).unwrap());
    let mut idx = 0;
    let ranges = headers.map(|_| {
        let range = if idx + 1 == N {
            (positions[idx], None)
        } else {
            (positions[idx], Some(positions[idx + 1]))
        };
        idx += 1;
        range
    });

    let mut results = Vec::new();
    for row in lines {
        if row.is_empty() {
            continue;
        }
        let flags = &row[..positions[0]];

        let cells = ranges.map(|r| {
            match r {
                (low, Some(high)) => &row[low..high],
                (low, None) => &row[low..],
            }
            .trim()
        });
        results.push((flags, cells))
    }

    Ok(results)
}

/// Parse a table using the given header field names. The first line must be the table header. It
/// returns a vector for each non-empty line, containing a tuple of flags (the thing before the
/// first column), and a vector containing all other elements.
///
/// In this function, you need to specify for each column, if it is right or left aligned. A `true`
/// means that the column is right-aligned, while a `false` indicates the column to be
/// right-aligned.
pub fn parse_table_with_alignment<'a, const N: usize>(
    table: &'a str,
    headers: [(&'static str, bool); N],
) -> Result<Vec<[&'a str; N]>, TableParseError> {
    // make sure that N is at least 1.
    _ = Assert::<N>::NON_ZERO;

    let mut lines = table.lines();
    let header = lines
        .next()
        .ok_or_else(|| TableParseError::InvalidHeader(String::new()))?;

    if header.split_whitespace().join(" ") != headers.iter().map(|(h, _)| h).join(" ") {
        return Err(TableParseError::InvalidHeader(header.to_string()));
    }

    // the positions points to either the start of the header in left-aligned columns, and the end
    // of right-aligned columns
    let positions =
        headers.map(|(h, right)| header.find(h).unwrap() + if right { h.len() - 1 } else { 0 });
    let mut idx = 0;
    let ranges = headers.map(|_| {
        let range = if idx + 1 == N {
            if idx == 0 {
                // special case: just take the entire column
                (positions[idx], None)
            } else {
                match (
                    (positions[idx - 1], headers[idx - 1]),
                    (positions[idx], headers[idx]),
                ) {
                    // Current element is left-aligned. Normal stuff.
                    (_, (cur_p, (_, false))) => (cur_p, None),
                    // both are right-aligned. Just go from the last element to the end.
                    ((prev_p, (_, true)), (_, (_, true))) => (prev_p + 1, None),
                    // Last is left aligned, but furrent is right aligned. Get the center point and
                    // use that one to the end.
                    ((prev_p, (prev_h, false)), (cur_p, (cur_h, true))) => {
                        let prev_end = prev_p + prev_h.len();
                        let cur_beg = cur_p - cur_h.len();
                        let mid = (prev_end + cur_beg) / 2;
                        (mid, None)
                    }
                }
            }
        } else if idx == 0 {
            match (
                (positions[idx], headers[idx]),
                (positions[idx + 1], headers[idx + 1]),
            ) {
                // if the current element is right-aligned, go to the start of that element
                ((cur_p, (_, true)), _) => (0, Some(cur_p + 1)),
                // if next is left aligned, simply go up to that point
                ((cur_p, _), (next_p, (_, false))) => (cur_p, Some(next_p)),
                // If current is left aligned, and the next is right-aligned, find the mid point.
                ((cur_p, (cur_h, false)), (next_p, (next_h, true))) => {
                    let cur_end = cur_p + cur_h.len();
                    let next_beg = next_p - next_h.len();
                    let mid = (cur_end + next_beg) / 2;
                    (cur_p, Some(mid))
                }
            }
        } else {
            match (
                (positions[idx - 1], headers[idx - 1]),
                (positions[idx], headers[idx]),
                (positions[idx + 1], headers[idx + 1]),
            ) {
                // cur_p is left-aligned: It depends on the next one
                // if the next one is also left-aligned, do the normal thing
                (_, (cur_p, (_, false)), (next_p, (_, false))) => (cur_p, Some(next_p)),
                // if the next is right-aligned, find the mid
                (_, (cur_p, (cur_h, false)), (next_p, (next_h, true))) => {
                    let cur_end = cur_p + cur_h.len();
                    let next_beg = next_p - next_h.len();
                    let mid = (cur_end + next_beg) / 2;
                    (cur_p, Some(mid))
                }
                // cur_p is right-aligned: It depends on the previous one
                // if the previous one is right-aligned, just to the normal thing
                ((prev_p, (_, true)), (cur_p, (_, true)), _) => (prev_p + 1, Some(cur_p + 1)),
                ((prev_p, (prev_h, false)), (cur_p, (cur_h, true)), _) => {
                    let prev_end = prev_p + prev_h.len();
                    let cur_beg = cur_p - cur_h.len();
                    let mid = (prev_end + cur_beg) / 2;
                    (mid, Some(cur_p + 1))
                }
            }
        };
        idx += 1;
        range
    });

    let mut results = Vec::new();
    for row in lines {
        if row.is_empty() {
            continue;
        }
        let cells = ranges.map(|r| {
            match r {
                (low, Some(high)) => &row[low..high],
                (low, None) => &row[low..],
            }
            .trim()
        });
        results.push(cells)
    }

    Ok(results)
}

/// Error while parsing a Cisco table
#[derive(Debug, Error)]
pub enum TableParseError {
    /// Invalid header line.
    #[error("Invalid header line: {0}")]
    InvalidHeader(String),
    /// Route row is too short
    #[error("A row is too short to be parsed: {0}")]
    RowTooShort(String),
}
