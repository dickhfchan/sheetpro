//! Cell values and error kinds.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

/// Spreadsheet error values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ErrorKind {
    Null,
    Div0,
    Value,
    Ref,
    Name,
    Num,
    NA,
    GettingData,
    Spill,
    Calc,
    Circ,
}

impl ErrorKind {
    pub const ALL: [ErrorKind; 11] = [
        ErrorKind::Null,
        ErrorKind::Div0,
        ErrorKind::Value,
        ErrorKind::Ref,
        ErrorKind::Name,
        ErrorKind::Num,
        ErrorKind::NA,
        ErrorKind::GettingData,
        ErrorKind::Spill,
        ErrorKind::Calc,
        ErrorKind::Circ,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Null => "#NULL!",
            ErrorKind::Div0 => "#DIV/0!",
            ErrorKind::Value => "#VALUE!",
            ErrorKind::Ref => "#REF!",
            ErrorKind::Name => "#NAME?",
            ErrorKind::Num => "#NUM!",
            ErrorKind::NA => "#N/A",
            ErrorKind::GettingData => "#GETTING_DATA",
            ErrorKind::Spill => "#SPILL!",
            ErrorKind::Calc => "#CALC!",
            ErrorKind::Circ => "#CIRC!",
        }
    }

    pub fn parse(s: &str) -> Option<ErrorKind> {
        let up = s.to_ascii_uppercase();
        ErrorKind::ALL.into_iter().find(|e| e.as_str() == up)
    }

    /// Code used by `ERROR.TYPE`.
    pub fn type_code(self) -> f64 {
        match self {
            ErrorKind::Null => 1.0,
            ErrorKind::Div0 => 2.0,
            ErrorKind::Value => 3.0,
            ErrorKind::Ref => 4.0,
            ErrorKind::Name => 5.0,
            ErrorKind::Num => 6.0,
            ErrorKind::NA => 7.0,
            ErrorKind::GettingData => 8.0,
            ErrorKind::Spill => 9.0,
            ErrorKind::Calc => 14.0,
            ErrorKind::Circ => 4.0,
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A scalar cell value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum Value {
    #[default]
    Empty,
    Number(f64),
    Text(Arc<str>),
    Bool(bool),
    Error(ErrorKind),
}

impl Value {
    pub fn text(s: impl Into<Arc<str>>) -> Self {
        Value::Text(s.into())
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Value::Empty)
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Value as it appears with the General format (no styles applied).
    pub fn to_display(&self) -> String {
        match self {
            Value::Empty => String::new(),
            Value::Number(n) => format_general(*n),
            Value::Text(s) => s.to_string(),
            Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
            Value::Error(e) => e.as_str().to_string(),
        }
    }

    /// Ordering class used by sorting and comparison: numbers < text < logicals < errors.
    pub fn type_rank(&self) -> u8 {
        match self {
            Value::Number(_) => 0,
            Value::Text(_) => 1,
            Value::Bool(_) => 2,
            Value::Error(_) => 3,
            Value::Empty => 4,
        }
    }
}

/// Render a number the way the General format does: up to 11 significant characters,
/// switching to scientific notation for very large/small magnitudes.
pub fn format_general(n: f64) -> String {
    if !n.is_finite() {
        return ErrorKind::Num.as_str().to_string();
    }
    if n == 0.0 {
        return "0".to_string();
    }
    let a = n.abs();
    if (1e-9..1e11).contains(&a) {
        // Fixed notation, trimmed to at most 10 significant digits.
        let int_digits = if a >= 1.0 {
            a.log10().floor() as i32 + 1
        } else {
            0
        };
        let decimals = (10 - int_digits).clamp(0, 10) as usize;
        let s = format!("{:.*}", decimals, n);
        let s = if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        };
        if s == "-0" { "0".to_string() } else { s }
    } else {
        let s = format!("{:.5E}", n);
        // Rust gives "1.23450E15"; normalise to "1.2345E+15".
        match s.split_once('E') {
            Some((mant, exp)) => {
                let mant = if mant.contains('.') {
                    mant.trim_end_matches('0').trim_end_matches('.')
                } else {
                    mant
                };
                let (sign, digits) = match exp.strip_prefix('-') {
                    Some(d) => ('-', d),
                    None => ('+', exp),
                };
                format!("{mant}E{sign}{digits:0>2}")
            }
            None => s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_format() {
        assert_eq!(format_general(1.0), "1");
        assert_eq!(format_general(-2.5), "-2.5");
        assert_eq!(format_general(1.0 / 3.0), "0.3333333333");
        assert_eq!(format_general(123456.789), "123456.789");
        assert_eq!(format_general(1e15), "1E+15");
        assert_eq!(format_general(1.5e-12), "1.5E-12");
        assert_eq!(format_general(0.1 + 0.2), "0.3");
    }

    #[test]
    fn errors_roundtrip() {
        for e in ErrorKind::ALL {
            assert_eq!(ErrorKind::parse(e.as_str()), Some(e));
        }
    }
}
