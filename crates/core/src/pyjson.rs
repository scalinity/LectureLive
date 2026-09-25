//! JSON exactly as Python's `json.dumps` writes it with default arguments: the format of the files
//! the Python CLI shares with this app (the spend ledger, the study page cache; spec §8, §6.4).
use std::fmt::Write;

use serde_json::Value;

/// `", "` and `": "` separators, non-ASCII as `\uXXXX`, floats as Python's `repr`, keys in insertion order.
pub fn dumps(v: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, v);
    out
}

fn write_value(out: &mut String, v: &Value) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => write!(out, "{i}").unwrap(),
            (_, Some(u)) => write!(out, "{u}").unwrap(),
            _ => out.push_str(&float_repr(n.as_f64().unwrap_or(0.0))),
        },
        Value::String(s) => write_str(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(out, x);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, x)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_str(out, k);
                out.push_str(": ");
                write_value(out, x);
            }
            out.push('}');
        }
    }
}

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    write!(out, "\\u{u:04x}").unwrap();
                }
            }
        }
    }
    out.push('"');
}

/// Python's `repr(float)`: the shortest digits that round-trip, positional from 1e-4 up to 1e16, otherwise `d.ddde±XX`.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    let sci = format!("{x:e}"); // shortest round-trip digits: "5.6e-5", "-1.2345e2"
    let (mantissa, exp) = sci.split_once('e').expect("{:e} has an exponent");
    let exp: i32 = exp.parse().expect("a decimal exponent");
    let (sign, mantissa) = mantissa.strip_prefix('-').map_or(("", mantissa), |m| ("-", m));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    if (-4..16).contains(&exp) {
        let point = exp + 1; // digits before the decimal point
        let body = if point <= 0 {
            format!("0.{}{digits}", "0".repeat(point.unsigned_abs() as usize))
        } else if point as usize >= digits.len() {
            format!("{digits}{}.0", "0".repeat(point as usize - digits.len()))
        } else {
            format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
        };
        format!("{sign}{body}")
    } else {
        let m = if digits.len() == 1 { digits } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{sign}{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.unsigned_abs())
    }
}

/// Python's `round(x)`: to the nearest integer, ties to even.
pub fn round_int(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Python's `round(x, n)`: x correctly rounded to n decimals (ties to even on its exact binary value).
pub fn round_to(x: f64, n: usize) -> f64 {
    format!("{x:.n$}").parse().expect("a formatted float parses")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The Python CLI's real ledger line (read with `od -c`), with the names replaced.
    #[test]
    fn a_ledger_line_is_the_python_clis_bytes() {
        let v = json!({"at": "2026-09-24T16:50:11", "course": "Machine Learning", "lecture": "Week 06 — Optimisation", "what": "page", "usd": 0.265218, "billed": true});
        assert_eq!(dumps(&v), r#"{"at": "2026-09-24T16:50:11", "course": "Machine Learning", "lecture": "Week 06 \u2014 Optimisation", "what": "page", "usd": 0.265218, "billed": true}"#);
    }

    #[test]
    fn floats_are_pythons_repr() {
        for (x, s) in [
            (0.265218, "0.265218"),
            (5.6e-05, "5.6e-05"),
            (1e-06, "1e-06"),
            (0.0001, "0.0001"),
            (1e-05, "1e-05"),
            (12.0, "12.0"),
            (12.3, "12.3"),
            (1790.5, "1790.5"),
            (0.0, "0.0"),
            (1e16, "1e+16"),
            (9999999999999998.0, "9999999999999998.0"),
            (123456789.0, "123456789.0"),
            (-0.5, "-0.5"),
            (0.1 + 0.2, "0.30000000000000004"),
        ] {
            assert_eq!(float_repr(x), s, "{x:e}");
        }
    }

    #[test]
    fn strings_are_escaped_as_python_escapes_them() {
        assert_eq!(dumps(&json!("a\"b\\c\nd\te\u{1}\u{7f}ü😀/")), r#""a\"b\\c\nd\te\u0001\u007f\u00fc\ud83d\ude00/""#);
    }

    #[test]
    fn integers_lists_and_objects_keep_their_order() {
        assert_eq!(dumps(&json!({"n": 3, "l": [1, 2.5, null, false], "e": {}, "a": []})), r#"{"n": 3, "l": [1, 2.5, null, false], "e": {}, "a": []}"#);
    }

    #[test]
    fn rounding_is_pythons() {
        assert_eq!([round_int(2.5), round_int(3.5), round_int(0.5), round_int(-2.5), round_int(16.5), round_int(15.504)], [2, 4, 0, -2, 16, 16]);
        assert_eq!(round_to(2.675, 2), 2.67, "the binary value is below the half");
        assert_eq!(round_to(0.125, 2), 0.12, "an exact tie goes to even");
        assert_eq!(round_to(0.375, 2), 0.38);
        assert_eq!(round_to(0.0012019999, 6), 0.001202);
        assert_eq!(round_to(12.34, 1), 12.3);
    }
}
