//! Engineering functions: number-system conversion, bitwise operations, CONVERT, DELTA /
//! GESTEP, the error function family, Bessel functions and complex-number (IM*) functions.
#![allow(clippy::redundant_closure_call)] // `(|| { … })()` enables `?` inside registry closures.

use super::special::{erf, erfc};
use super::{Registry, ret};
use crate::ast::Expr;
use crate::eval::{Ev, EvalValue, to_number};
use sp_core::{ErrorKind, Value};

type R = Result<f64, ErrorKind>;
use ErrorKind::{NA, Num};

// ── argument helpers ────────────────────────────────────────────────────────────────────

fn arg(ev: &mut Ev<'_>, a: &[Expr], i: usize) -> Result<Value, ErrorKind> {
    match a.get(i) {
        Some(e) => Ok(ev.eval_scalar(e)),
        None => Err(ErrorKind::Value),
    }
}

/// Number argument where logical values are rejected (`#VALUE!`).
fn strict(v: &Value) -> R {
    match v {
        Value::Bool(_) => Err(ErrorKind::Value),
        other => to_number(other),
    }
}

fn snum(ev: &mut Ev<'_>, a: &[Expr], i: usize) -> R {
    strict(&arg(ev, a, i)?)
}

/// Optional strict number: absent or empty → `default`.
fn sopt(ev: &mut Ev<'_>, a: &[Expr], i: usize, default: f64) -> R {
    match a.get(i) {
        None | Some(Expr::Missing) => Ok(default),
        Some(e) => {
            let v = ev.eval_scalar(e);
            strict(&v)
        }
    }
}

fn fin(x: f64) -> R {
    if x.is_finite() { Ok(x) } else { Err(Num) }
}

fn text_ret(r: Result<String, ErrorKind>) -> EvalValue {
    match r {
        Ok(s) => EvalValue::text(s),
        Err(e) => EvalValue::err(e),
    }
}

// ── number systems ──────────────────────────────────────────────────────────────────────

/// The digit string a conversion function sees for its `number` argument.
fn radix_input(v: &Value) -> Result<String, ErrorKind> {
    match v {
        Value::Bool(_) => Err(ErrorKind::Value),
        Value::Error(e) => Err(*e),
        Value::Empty => Ok("0".to_string()),
        Value::Text(s) => Ok(s.trim().to_string()),
        Value::Number(n) => {
            if *n < 0.0 || n.fract() != 0.0 || *n >= 1e10 {
                Err(Num)
            } else {
                Ok(format!("{n:.0}"))
            }
        }
    }
}

/// Parse a ≤10-digit two's-complement string in `radix` (bits per digit `bits`).
fn parse_radix(s: &str, radix: u32) -> Result<i64, ErrorKind> {
    if s.len() > 10 {
        return Err(Num);
    }
    if s.is_empty() {
        return Ok(0);
    }
    let mut v: i64 = 0;
    for ch in s.chars() {
        let d = ch.to_digit(radix).ok_or(Num)? as i64;
        v = v * radix as i64 + d;
    }
    let total_bits = match radix {
        2 => 10,
        8 => 30,
        _ => 40,
    };
    // Ten digits with the top bit set is negative.
    if s.len() == 10 && v >= 1i64 << (total_bits - 1) {
        v -= 1i64 << total_bits;
    }
    Ok(v)
}

fn places_arg(ev: &mut Ev<'_>, a: &[Expr], i: usize) -> Result<Option<usize>, ErrorKind> {
    match a.get(i) {
        None => Ok(None),
        Some(e) => {
            let v = ev.eval_scalar(e);
            let p = strict(&v)?.trunc();
            if !(1.0..=10.0).contains(&p) {
                return Err(Num);
            }
            Ok(Some(p as usize))
        }
    }
}

/// Format `v` in `radix` with two's complement for negatives (10 digits), checking range.
fn format_radix(v: i64, radix: u32, places: Option<usize>) -> Result<String, ErrorKind> {
    let total_bits: u32 = match radix {
        2 => 10,
        8 => 30,
        _ => 40,
    };
    let lim = 1i64 << (total_bits - 1);
    if v < -lim || v >= lim {
        return Err(Num);
    }
    let digits = |mut u: u64| -> String {
        if u == 0 {
            return "0".to_string();
        }
        let mut out = Vec::new();
        while u > 0 {
            let d = (u % radix as u64) as u32;
            out.push(
                std::char::from_digit(d, radix)
                    .unwrap_or('0')
                    .to_ascii_uppercase(),
            );
            u /= radix as u64;
        }
        out.iter().rev().collect()
    };
    if v < 0 {
        let u = (v + (1i64 << total_bits)) as u64;
        return Ok(digits(u));
    }
    let s = digits(v as u64);
    match places {
        None => Ok(s),
        Some(p) if p < s.len() => Err(Num),
        Some(p) => Ok(format!("{}{s}", "0".repeat(p - s.len()))),
    }
}

fn radix_to_radix(ev: &mut Ev<'_>, a: &[Expr], from: u32, to: u32) -> Result<String, ErrorKind> {
    let s = radix_input(&arg(ev, a, 0)?)?;
    let v = parse_radix(&s, from)?;
    let places = places_arg(ev, a, 1)?;
    format_radix(v, to, places)
}

fn radix_to_dec(ev: &mut Ev<'_>, a: &[Expr], from: u32) -> R {
    let s = radix_input(&arg(ev, a, 0)?)?;
    Ok(parse_radix(&s, from)? as f64)
}

fn dec_to_radix(ev: &mut Ev<'_>, a: &[Expr], to: u32) -> Result<String, ErrorKind> {
    let n = snum(ev, a, 0)?.trunc();
    if n.abs() > 1e13 {
        return Err(Num);
    }
    let places = places_arg(ev, a, 1)?;
    format_radix(n as i64, to, places)
}

// ── bits ────────────────────────────────────────────────────────────────────────────────

const BIT_LIMIT: f64 = 281_474_976_710_656.0; // 2^48

fn bit_arg(ev: &mut Ev<'_>, a: &[Expr], i: usize) -> Result<u64, ErrorKind> {
    let n = match a.get(i) {
        Some(e) => ev.num(e)?,
        None => return Err(ErrorKind::Value),
    };
    if n < 0.0 || n.fract() != 0.0 || n >= BIT_LIMIT {
        return Err(Num);
    }
    Ok(n as u64)
}

fn bit_shift(ev: &mut Ev<'_>, a: &[Expr], left: bool) -> R {
    let n = bit_arg(ev, a, 0)?;
    let s = match a.get(1) {
        Some(e) => ev.num(e)?.trunc(),
        None => return Err(ErrorKind::Value),
    };
    if s.abs() > 53.0 {
        return Err(Num);
    }
    let s = if left { s } else { -s } as i32;
    let r = if s >= 0 {
        (n as f64) * 2f64.powi(s)
    } else {
        (n >> (-s).min(63) as u32) as f64
    };
    if r >= BIT_LIMIT {
        return Err(Num);
    }
    Ok(r)
}

// ── CONVERT ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Cat {
    Mass,
    Distance,
    Time,
    Pressure,
    Force,
    Energy,
    Power,
    Magnetism,
    Temperature,
    Volume,
    Area,
    Information,
    Speed,
}

/// (symbol, category, factor to the category base, accepts prefixes, dimension for prefixes)
type Unit = (&'static str, Cat, f64, bool, i32);

const IN: f64 = 0.0254;
const FT: f64 = 0.3048;
const YD: f64 = 0.9144;
const MI: f64 = 1609.344;
const NMI: f64 = 1852.0;
const LY: f64 = 9_460_730_472_580_800.0;
const PICA_PT: f64 = 0.0254 / 72.0;
const GAL: f64 = 0.003_785_411_784;
const LBM: f64 = 453.592_37;
const G_ACC: f64 = 9.806_65;

const UNITS: &[Unit] = &[
    // Mass (gram).
    ("g", Cat::Mass, 1.0, true, 1),
    ("sg", Cat::Mass, 14_593.902_937_206_363, false, 1),
    ("lbm", Cat::Mass, LBM, false, 1),
    ("u", Cat::Mass, 1.660_538_782e-24, true, 1),
    ("ozm", Cat::Mass, LBM / 16.0, false, 1),
    ("grain", Cat::Mass, 0.064_798_91, false, 1),
    ("cwt", Cat::Mass, 100.0 * LBM, false, 1),
    ("shweight", Cat::Mass, 100.0 * LBM, false, 1),
    ("uk_cwt", Cat::Mass, 112.0 * LBM, false, 1),
    ("lcwt", Cat::Mass, 112.0 * LBM, false, 1),
    ("hweight", Cat::Mass, 112.0 * LBM, false, 1),
    ("stone", Cat::Mass, 14.0 * LBM, false, 1),
    ("ton", Cat::Mass, 2000.0 * LBM, false, 1),
    ("brton", Cat::Mass, 2240.0 * LBM, false, 1),
    ("LTON", Cat::Mass, 2240.0 * LBM, false, 1),
    ("uk_ton", Cat::Mass, 2240.0 * LBM, false, 1),
    // Distance (meter).
    ("m", Cat::Distance, 1.0, true, 1),
    ("mi", Cat::Distance, MI, false, 1),
    ("Nmi", Cat::Distance, NMI, false, 1),
    ("in", Cat::Distance, IN, false, 1),
    ("ft", Cat::Distance, FT, false, 1),
    ("yd", Cat::Distance, YD, false, 1),
    ("ang", Cat::Distance, 1e-10, true, 1),
    ("ell", Cat::Distance, 1.143, false, 1),
    ("ly", Cat::Distance, LY, true, 1),
    ("parsec", Cat::Distance, 3.085_677_581_281_553_2e16, true, 1),
    ("pc", Cat::Distance, 3.085_677_581_281_553_2e16, true, 1),
    ("Picapt", Cat::Distance, PICA_PT, false, 1),
    ("Pica", Cat::Distance, PICA_PT, false, 1),
    ("pica", Cat::Distance, IN / 6.0, false, 1),
    (
        "survey_mi",
        Cat::Distance,
        1_609.347_218_694_437_3,
        false,
        1,
    ),
    // Time (second).
    ("yr", Cat::Time, 31_557_600.0, false, 1),
    ("day", Cat::Time, 86_400.0, false, 1),
    ("d", Cat::Time, 86_400.0, false, 1),
    ("hr", Cat::Time, 3600.0, false, 1),
    ("mn", Cat::Time, 60.0, false, 1),
    ("min", Cat::Time, 60.0, false, 1),
    ("sec", Cat::Time, 1.0, true, 1),
    ("s", Cat::Time, 1.0, true, 1),
    // Pressure (pascal).
    ("Pa", Cat::Pressure, 1.0, true, 1),
    ("p", Cat::Pressure, 1.0, true, 1),
    ("atm", Cat::Pressure, 101_325.0, true, 1),
    ("at", Cat::Pressure, 101_325.0, true, 1),
    ("mmHg", Cat::Pressure, 133.322, true, 1),
    ("psi", Cat::Pressure, 6_894.757_293_168_362, false, 1),
    ("Torr", Cat::Pressure, 101_325.0 / 760.0, false, 1),
    // Force (newton).
    ("N", Cat::Force, 1.0, true, 1),
    ("dyn", Cat::Force, 1e-5, true, 1),
    ("dy", Cat::Force, 1e-5, true, 1),
    ("lbf", Cat::Force, LBM / 1000.0 * G_ACC, false, 1),
    ("pond", Cat::Force, G_ACC / 1000.0, true, 1),
    // Energy (joule).
    ("J", Cat::Energy, 1.0, true, 1),
    ("e", Cat::Energy, 1e-7, true, 1),
    ("c", Cat::Energy, 4.184, true, 1),
    ("cal", Cat::Energy, 4.1868, true, 1),
    ("eV", Cat::Energy, 1.602_176_487e-19, true, 1),
    ("ev", Cat::Energy, 1.602_176_487e-19, true, 1),
    ("HPh", Cat::Energy, 2_684_519.537_696_173, false, 1),
    ("hh", Cat::Energy, 2_684_519.537_696_173, false, 1),
    ("Wh", Cat::Energy, 3600.0, true, 1),
    ("wh", Cat::Energy, 3600.0, true, 1),
    ("flb", Cat::Energy, LBM / 1000.0 * G_ACC * FT, false, 1),
    ("BTU", Cat::Energy, 1_055.055_852_62, false, 1),
    ("btu", Cat::Energy, 1_055.055_852_62, false, 1),
    // Power (watt).
    ("HP", Cat::Power, 745.699_871_582_270_2, false, 1),
    ("h", Cat::Power, 745.699_871_582_270_2, false, 1),
    ("PS", Cat::Power, 735.498_75, false, 1),
    ("W", Cat::Power, 1.0, true, 1),
    ("w", Cat::Power, 1.0, true, 1),
    // Magnetism (tesla).
    ("T", Cat::Magnetism, 1.0, true, 1),
    ("ga", Cat::Magnetism, 1e-4, true, 1),
    // Temperature (kelvin; handled affinely).
    ("C", Cat::Temperature, 1.0, false, 1),
    ("cel", Cat::Temperature, 1.0, false, 1),
    ("F", Cat::Temperature, 1.0, false, 1),
    ("fah", Cat::Temperature, 1.0, false, 1),
    ("K", Cat::Temperature, 1.0, true, 1),
    ("kel", Cat::Temperature, 1.0, true, 1),
    ("Rank", Cat::Temperature, 1.0, false, 1),
    ("Reau", Cat::Temperature, 1.0, false, 1),
    // Volume (cubic meter).
    ("tsp", Cat::Volume, GAL / 768.0, false, 1),
    ("tspm", Cat::Volume, 5e-6, false, 1),
    ("tbs", Cat::Volume, GAL / 256.0, false, 1),
    ("oz", Cat::Volume, GAL / 128.0, false, 1),
    ("cup", Cat::Volume, GAL / 16.0, false, 1),
    ("pt", Cat::Volume, GAL / 8.0, false, 1),
    ("us_pt", Cat::Volume, GAL / 8.0, false, 1),
    ("uk_pt", Cat::Volume, 0.000_568_261_25, true, 1),
    ("qt", Cat::Volume, GAL / 4.0, false, 1),
    ("uk_qt", Cat::Volume, 0.001_136_522_5, false, 1),
    ("gal", Cat::Volume, GAL, false, 1),
    ("uk_gal", Cat::Volume, 0.004_546_09, false, 1),
    ("l", Cat::Volume, 0.001, true, 1),
    ("L", Cat::Volume, 0.001, true, 1),
    ("lt", Cat::Volume, 0.001, true, 1),
    ("ang3", Cat::Volume, 1e-30, true, 3),
    ("ang^3", Cat::Volume, 1e-30, true, 3),
    ("barrel", Cat::Volume, 42.0 * GAL, false, 1),
    ("bushel", Cat::Volume, 0.035_239_070_166_88, false, 1),
    ("ft3", Cat::Volume, FT * FT * FT, false, 1),
    ("ft^3", Cat::Volume, FT * FT * FT, false, 1),
    ("in3", Cat::Volume, IN * IN * IN, false, 1),
    ("in^3", Cat::Volume, IN * IN * IN, false, 1),
    ("ly3", Cat::Volume, LY * LY * LY, false, 1),
    ("ly^3", Cat::Volume, LY * LY * LY, false, 1),
    ("m3", Cat::Volume, 1.0, true, 3),
    ("m^3", Cat::Volume, 1.0, true, 3),
    ("mi3", Cat::Volume, MI * MI * MI, false, 1),
    ("mi^3", Cat::Volume, MI * MI * MI, false, 1),
    ("yd3", Cat::Volume, YD * YD * YD, false, 1),
    ("yd^3", Cat::Volume, YD * YD * YD, false, 1),
    ("Nmi3", Cat::Volume, NMI * NMI * NMI, false, 1),
    ("Nmi^3", Cat::Volume, NMI * NMI * NMI, false, 1),
    (
        "Picapt3",
        Cat::Volume,
        PICA_PT * PICA_PT * PICA_PT,
        false,
        1,
    ),
    (
        "Picapt^3",
        Cat::Volume,
        PICA_PT * PICA_PT * PICA_PT,
        false,
        1,
    ),
    ("Pica3", Cat::Volume, PICA_PT * PICA_PT * PICA_PT, false, 1),
    ("Pica^3", Cat::Volume, PICA_PT * PICA_PT * PICA_PT, false, 1),
    ("GRT", Cat::Volume, 100.0 * FT * FT * FT, false, 1),
    ("regton", Cat::Volume, 100.0 * FT * FT * FT, false, 1),
    ("MTON", Cat::Volume, 40.0 * FT * FT * FT, false, 1),
    // Area (square meter).
    ("uk_acre", Cat::Area, 4_046.856_422_4, false, 1),
    ("us_acre", Cat::Area, 4_046.872_609_874_252_2, false, 1),
    ("ang2", Cat::Area, 1e-20, true, 2),
    ("ang^2", Cat::Area, 1e-20, true, 2),
    ("ar", Cat::Area, 100.0, true, 1),
    ("ft2", Cat::Area, FT * FT, false, 1),
    ("ft^2", Cat::Area, FT * FT, false, 1),
    ("ha", Cat::Area, 10_000.0, false, 1),
    ("in2", Cat::Area, IN * IN, false, 1),
    ("in^2", Cat::Area, IN * IN, false, 1),
    ("ly2", Cat::Area, LY * LY, false, 1),
    ("ly^2", Cat::Area, LY * LY, false, 1),
    ("m2", Cat::Area, 1.0, true, 2),
    ("m^2", Cat::Area, 1.0, true, 2),
    ("Morgen", Cat::Area, 2500.0, false, 1),
    ("mi2", Cat::Area, MI * MI, false, 1),
    ("mi^2", Cat::Area, MI * MI, false, 1),
    ("Nmi2", Cat::Area, NMI * NMI, false, 1),
    ("Nmi^2", Cat::Area, NMI * NMI, false, 1),
    ("Picapt2", Cat::Area, PICA_PT * PICA_PT, false, 1),
    ("Pica2", Cat::Area, PICA_PT * PICA_PT, false, 1),
    ("Pica^2", Cat::Area, PICA_PT * PICA_PT, false, 1),
    ("Picapt^2", Cat::Area, PICA_PT * PICA_PT, false, 1),
    ("yd2", Cat::Area, YD * YD, false, 1),
    ("yd^2", Cat::Area, YD * YD, false, 1),
    // Information (bit).
    ("bit", Cat::Information, 1.0, true, 1),
    ("byte", Cat::Information, 8.0, true, 1),
    // Speed (meter per second).
    ("admkn", Cat::Speed, 6_080.0 * FT / 3600.0, false, 1),
    ("kn", Cat::Speed, NMI / 3600.0, false, 1),
    ("m/h", Cat::Speed, 1.0 / 3600.0, true, 1),
    ("m/hr", Cat::Speed, 1.0 / 3600.0, true, 1),
    ("m/s", Cat::Speed, 1.0, true, 1),
    ("m/sec", Cat::Speed, 1.0, true, 1),
    ("mph", Cat::Speed, MI / 3600.0, true, 1),
];

const PREFIXES: &[(&str, f64)] = &[
    ("Y", 1e24),
    ("Z", 1e21),
    ("E", 1e18),
    ("P", 1e15),
    ("T", 1e12),
    ("G", 1e9),
    ("M", 1e6),
    ("k", 1e3),
    ("h", 1e2),
    ("da", 1e1),
    ("e", 1e1),
    ("d", 1e-1),
    ("c", 1e-2),
    ("m", 1e-3),
    ("u", 1e-6),
    ("n", 1e-9),
    ("p", 1e-12),
    ("f", 1e-15),
    ("a", 1e-18),
    ("z", 1e-21),
    ("y", 1e-24),
];

const BIN_PREFIXES: &[(&str, i32)] = &[
    ("Yi", 80),
    ("Zi", 70),
    ("Ei", 60),
    ("Pi", 50),
    ("Ti", 40),
    ("Gi", 30),
    ("Mi", 20),
    ("ki", 10),
];

/// Resolve a unit name to (category, factor to base, symbol).
fn unit(name: &str) -> Option<(Cat, f64, &'static str)> {
    if let Some(u) = UNITS.iter().find(|u| u.0 == name) {
        return Some((u.1, u.2, u.0));
    }
    for (p, m) in BIN_PREFIXES {
        if let Some(rest) = name.strip_prefix(p)
            && let Some(u) = UNITS
                .iter()
                .find(|u| u.0 == rest && u.1 == Cat::Information)
        {
            return Some((u.1, u.2 * 2f64.powi(*m), u.0));
        }
    }
    for (p, m) in PREFIXES {
        if let Some(rest) = name.strip_prefix(p)
            && let Some(u) = UNITS.iter().find(|u| u.0 == rest && u.3)
        {
            return Some((u.1, u.2 * m.powi(u.4), u.0));
        }
    }
    None
}

fn to_kelvin(x: f64, sym: &str, f: f64) -> f64 {
    match sym {
        "C" | "cel" => x + 273.15,
        "F" | "fah" => (x - 32.0) * 5.0 / 9.0 + 273.15,
        "Rank" => x * 5.0 / 9.0,
        "Reau" => x * 1.25 + 273.15,
        _ => x * f,
    }
}

fn from_kelvin(k: f64, sym: &str, f: f64) -> f64 {
    match sym {
        "C" | "cel" => k - 273.15,
        "F" | "fah" => (k - 273.15) * 9.0 / 5.0 + 32.0,
        "Rank" => k * 9.0 / 5.0,
        "Reau" => (k - 273.15) / 1.25,
        _ => k / f,
    }
}

fn convert(ev: &mut Ev<'_>, a: &[Expr]) -> R {
    let x = snum(ev, a, 0)?;
    let from = text_arg(ev, a, 1)?;
    let to = text_arg(ev, a, 2)?;
    let (c1, f1, s1) = unit(&from).ok_or(NA)?;
    let (c2, f2, s2) = unit(&to).ok_or(NA)?;
    if c1 != c2 {
        return Err(NA);
    }
    if c1 == Cat::Temperature {
        return fin(from_kelvin(to_kelvin(x, s1, f1), s2, f2));
    }
    fin(x * f1 / f2)
}

fn text_arg(ev: &mut Ev<'_>, a: &[Expr], i: usize) -> Result<String, ErrorKind> {
    match arg(ev, a, i)? {
        Value::Error(e) => Err(e),
        Value::Text(s) => Ok(s.to_string()),
        Value::Empty => Ok(String::new()),
        other => Ok(other.to_display()),
    }
}

// ── Bessel functions ────────────────────────────────────────────────────────────────────
// I and K use the classic polynomial approximations from Abramowitz & Stegun 9.8.1–9.8.8
// with recurrences for higher orders; J and Y are described below.

fn poly(c: &[f64], t: f64) -> f64 {
    c.iter().rev().fold(0.0, |s, a| s * t + a)
}

fn bessel_i0(x: f64) -> f64 {
    let ax = x.abs();
    if ax < 3.75 {
        let t = (x / 3.75) * (x / 3.75);
        poly(
            &[
                1.0,
                3.515_622_9,
                3.089_942_4,
                1.206_749_2,
                0.265_973_2,
                0.036_076_8,
                0.004_581_3,
            ],
            t,
        )
    } else {
        let t = 3.75 / ax;
        ax.exp() / ax.sqrt()
            * poly(
                &[
                    0.398_942_28,
                    0.013_285_92,
                    0.002_253_19,
                    -0.001_575_65,
                    0.009_162_81,
                    -0.020_577_06,
                    0.026_355_37,
                    -0.016_476_33,
                    0.003_923_77,
                ],
                t,
            )
    }
}

fn bessel_i1(x: f64) -> f64 {
    let ax = x.abs();
    if ax < 3.75 {
        let t = (x / 3.75) * (x / 3.75);
        x * poly(
            &[
                0.5,
                0.878_905_94,
                0.514_988_69,
                0.150_849_34,
                0.026_587_33,
                0.003_015_32,
                0.000_324_11,
            ],
            t,
        )
    } else {
        let t = 3.75 / ax;
        let r = ax.exp() / ax.sqrt()
            * poly(
                &[
                    0.398_942_28,
                    -0.039_880_24,
                    -0.003_620_18,
                    0.001_638_01,
                    -0.010_315_55,
                    0.022_829_67,
                    -0.028_953_12,
                    0.017_876_54,
                    -0.004_200_59,
                ],
                t,
            );
        if x < 0.0 { -r } else { r }
    }
}

fn bessel_k0(x: f64) -> f64 {
    if x <= 2.0 {
        let t = (x / 2.0) * (x / 2.0);
        -(x / 2.0).ln() * bessel_i0(x)
            + poly(
                &[
                    -0.577_215_66,
                    0.422_784_20,
                    0.230_697_56,
                    0.034_885_90,
                    0.002_626_98,
                    0.000_107_50,
                    0.000_007_40,
                ],
                t,
            )
    } else {
        (-x).exp() / x.sqrt()
            * poly(
                &[
                    1.253_314_14,
                    -0.078_323_58,
                    0.021_895_68,
                    -0.010_624_46,
                    0.005_878_72,
                    -0.002_515_40,
                    0.000_532_08,
                ],
                2.0 / x,
            )
    }
}

fn bessel_k1(x: f64) -> f64 {
    if x <= 2.0 {
        let t = (x / 2.0) * (x / 2.0);
        (x * (x / 2.0).ln() * bessel_i1(x)
            + poly(
                &[
                    1.0,
                    0.154_431_44,
                    -0.672_785_79,
                    -0.181_568_97,
                    -0.019_194_02,
                    -0.001_104_04,
                    -0.000_046_86,
                ],
                t,
            ))
            / x
    } else {
        (-x).exp() / x.sqrt()
            * poly(
                &[
                    1.253_314_14,
                    0.234_986_19,
                    -0.036_556_20,
                    0.015_042_68,
                    -0.007_803_53,
                    0.003_256_14,
                    -0.000_682_45,
                ],
                2.0 / x,
            )
    }
}

const MAX_ORDER: usize = 10_000;

fn bessel_i(x: f64, n: usize) -> f64 {
    match n {
        0 => return bessel_i0(x),
        1 => return bessel_i1(x),
        _ => {}
    }
    if x == 0.0 {
        return 0.0;
    }
    // Miller backward recurrence normalised by I0.
    let ax = x.abs();
    let tox = 2.0 / ax;
    let m = 2 * (n + (40.0 * n as f64).sqrt() as usize);
    let (mut bip, mut bi, mut ans) = (0.0_f64, 1.0_f64, 0.0_f64);
    let mut j = m;
    while j > 0 {
        let bim = bip + j as f64 * tox * bi;
        bip = bi;
        bi = bim;
        if bi.abs() > 1e10 {
            ans *= 1e-10;
            bi *= 1e-10;
            bip *= 1e-10;
        }
        if j == n {
            ans = bip;
        }
        j -= 1;
    }
    let r = ans * bessel_i0(ax) / bi;
    if x < 0.0 && n % 2 == 1 { -r } else { r }
}

fn bessel_k(x: f64, n: usize) -> f64 {
    let (mut km, mut k) = (bessel_k0(x), bessel_k1(x));
    if n == 0 {
        return km;
    }
    for j in 1..n {
        let kp = km + 2.0 * j as f64 / x * k;
        km = k;
        k = kp;
        if !k.is_finite() {
            break;
        }
    }
    k
}

// J0, J1, Y0 and Y1 use the classic low-order rational approximations (Hart, *Computer
// Approximations*, 1968) for x < 8 and the asymptotic amplitude/phase forms beyond. They are
// accurate to about 1e-8, and the target's results carry exactly that error (for example
// BESSELJ(0,0) = 1.00000000283), so these reproduce its values rather than the true ones.

/// `poly` with the variable first.
fn poly_asc(y: f64, cs: &[f64]) -> f64 {
    poly(cs, y)
}

/// Amplitude terms P, Q of the large-x form of order 0.
fn pq0(y: f64) -> (f64, f64) {
    (
        poly_asc(
            y,
            &[
                1.0,
                -0.109_862_862_7e-2,
                0.273_451_040_7e-4,
                -0.207_337_063_9e-5,
                0.209_388_721_1e-6,
            ],
        ),
        poly_asc(
            y,
            &[
                -0.156_249_999_5e-1,
                0.143_048_876_5e-3,
                -0.691_114_765_1e-5,
                0.762_109_516_1e-6,
                -0.934_935_152e-7,
            ],
        ),
    )
}

/// Amplitude terms P, Q of the large-x form of order 1. For J1 (`j1_quirk`) the target's P
/// series repeats its third coefficient (six terms instead of five); reproduced so large-x
/// BESSELJ matches it. Y1 uses the plain series.
fn pq1(y: f64, j1_quirk: bool) -> (f64, f64) {
    const P: [f64; 5] = [
        1.0,
        0.183_105e-2,
        -0.351_639_649_6e-4,
        0.245_752_017_4e-5,
        -0.240_337_019e-6,
    ];
    const P_J1: [f64; 6] = [
        1.0,
        0.183_105e-2,
        -0.351_639_649_6e-4,
        -0.351_639_649_6e-4,
        0.245_752_017_4e-5,
        -0.240_337_019e-6,
    ];
    (
        poly_asc(y, if j1_quirk { &P_J1 } else { &P }),
        poly_asc(
            y,
            &[
                0.046_874_999_95,
                -0.200_269_087_3e-3,
                0.844_919_909_6e-5,
                -0.882_289_87e-6,
                0.105_787_412e-6,
            ],
        ),
    )
}

const TWO_OVER_PI: f64 = 0.636_619_772;

fn bessel_j0(x: f64) -> f64 {
    let ax = x.abs();
    if ax < 8.0 {
        let y = x * x;
        let n = poly_asc(
            y,
            &[
                57_568_490_574.0,
                -13_362_590_354.0,
                651_619_640.7,
                -11_214_424.18,
                77_392.330_17,
                -184.905_245_6,
            ],
        );
        let d = poly_asc(
            y,
            &[
                57_568_490_411.0,
                1_029_532_985.0,
                9_494_680.718,
                59_272.648_53,
                267.853_271_2,
                1.0,
            ],
        );
        return n / d;
    }
    let z = 8.0 / ax;
    let (p, q) = pq0(z * z);
    let xx = ax - 0.785_398_164;
    (TWO_OVER_PI / ax).sqrt() * (xx.cos() * p - z * xx.sin() * q)
}

fn bessel_j1(x: f64) -> f64 {
    let ax = x.abs();
    if ax < 8.0 {
        let y = x * x;
        let n = x * poly_asc(
            y,
            &[
                72_362_614_232.0,
                -7_895_059_235.0,
                242_396_853.1,
                -2_972_611.439,
                15_704.482_60,
                -30.160_366_06,
            ],
        );
        let d = poly_asc(
            y,
            &[
                144_725_228_442.0,
                2_300_535_178.0,
                18_583_304.74,
                99_447.433_94,
                376.999_139_7,
                1.0,
            ],
        );
        return n / d;
    }
    let z = 8.0 / ax;
    let (p, q) = pq1(z * z, true);
    let xx = ax - 2.356_194_491;
    let r = (TWO_OVER_PI / ax).sqrt() * (xx.cos() * p - z * xx.sin() * q);
    if x < 0.0 { -r } else { r }
}

/// Y0 for x > 0.
fn bessel_y0(x: f64) -> f64 {
    if x < 8.0 {
        let y = x * x;
        let n = poly_asc(
            y,
            &[
                -2_957_821_389.0,
                7_062_834_065.0,
                -512_359_803.6,
                10_879_881.29,
                -86_327.927_57,
                228.462_273_3,
            ],
        );
        let d = poly_asc(
            y,
            &[
                40_076_544_269.0,
                745_249_964.8,
                7_189_466.438,
                47_447.264_70,
                226.103_024_4,
                1.0,
            ],
        );
        return n / d + TWO_OVER_PI * bessel_j0(x) * x.ln();
    }
    let z = 8.0 / x;
    let (p, q) = pq0(z * z);
    let xx = x - 0.785_398_164;
    (TWO_OVER_PI / x).sqrt() * (xx.sin() * p + z * xx.cos() * q)
}

/// Y1 for x > 0.
fn bessel_y1(x: f64) -> f64 {
    if x < 8.0 {
        let y = x * x;
        let n = x * poly_asc(
            y,
            &[
                -0.490_060_494_3e13,
                0.127_527_439_0e13,
                -0.515_343_813_9e11,
                0.734_926_455_1e9,
                -0.423_792_272_6e7,
                0.851_193_793_5e4,
            ],
        );
        let d = poly_asc(
            y,
            &[
                0.249_958_057_0e14,
                0.424_441_966_4e12,
                0.373_365_036_7e10,
                0.224_590_400_2e8,
                0.102_042_605_0e6,
                0.354_963_288_5e3,
                1.0,
            ],
        );
        return n / d + TWO_OVER_PI * (bessel_j1(x) * x.ln() - 1.0 / x);
    }
    let z = 8.0 / x;
    let (p, q) = pq1(z * z, false);
    let xx = x - 2.356_194_491;
    (TWO_OVER_PI / x).sqrt() * (xx.sin() * p + z * xx.cos() * q)
}

fn bessel_j(x: f64, n: usize) -> f64 {
    match n {
        0 => return bessel_j0(x),
        1 => return bessel_j1(x),
        _ => {}
    }
    if x == 0.0 {
        return 0.0;
    }
    let ax = x.abs();
    let sign = if x < 0.0 && n % 2 == 1 { -1.0 } else { 1.0 };
    let tox = 2.0 / ax;
    if ax > n as f64 {
        // Upward recurrence is stable here.
        let (mut jm, mut j) = (bessel_j0(ax), bessel_j1(ax));
        for k in 1..n {
            let jp = k as f64 * tox * j - jm;
            jm = j;
            j = jp;
        }
        return sign * j;
    }
    // Miller backward recurrence from an even start, normalised by J0 + 2ΣJ_2k = 1.
    let m = 2 * ((n + (160.0 * n as f64).sqrt() as usize) / 2);
    let (mut bjp, mut bj, mut sum, mut ans) = (0.0_f64, 1.0_f64, 0.0_f64, 0.0_f64);
    let mut even = false;
    for j in (1..=m).rev() {
        let bjm = j as f64 * tox * bj - bjp;
        bjp = bj;
        bj = bjm;
        if bj.abs() > 1e10 {
            bj *= 1e-10;
            bjp *= 1e-10;
            ans *= 1e-10;
            sum *= 1e-10;
        }
        if even {
            sum += bj;
        }
        even = !even;
        if j == n {
            ans = bjp;
        }
    }
    let norm = 2.0 * sum - bj;
    sign * ans / norm
}

fn bessel_y(x: f64, n: usize) -> f64 {
    let y0 = bessel_y0(x);
    if n == 0 {
        return y0;
    }
    let (mut ym, mut y) = (y0, bessel_y1(x));
    let tox = 2.0 / x;
    for k in 1..n {
        let yp = k as f64 * tox * y - ym;
        ym = y;
        y = yp;
        if !y.is_finite() {
            break;
        }
    }
    y
}

fn bessel(ev: &mut Ev<'_>, a: &[Expr], kind: char) -> R {
    let x = snum(ev, a, 0)?;
    let n = snum(ev, a, 1)?.trunc();
    if n < 0.0 || n > MAX_ORDER as f64 {
        return Err(Num);
    }
    let n = n as usize;
    match kind {
        'I' => fin(bessel_i(x, n)),
        'J' => fin(bessel_j(x, n)),
        'K' => {
            if x <= 0.0 {
                return Err(Num);
            }
            fin(bessel_k(x, n))
        }
        _ => {
            if x <= 0.0 {
                return Err(Num);
            }
            fin(bessel_y(x, n))
        }
    }
}

// ── complex numbers ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
struct C {
    re: f64,
    im: f64,
}

impl C {
    fn new(re: f64, im: f64) -> Self {
        C { re, im }
    }
    fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }
    fn add(self, o: C) -> C {
        C::new(self.re + o.re, self.im + o.im)
    }
    fn sub(self, o: C) -> C {
        C::new(self.re - o.re, self.im - o.im)
    }
    fn mul(self, o: C) -> C {
        C::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }
    fn div(self, o: C) -> Result<C, ErrorKind> {
        let d = o.re * o.re + o.im * o.im;
        if d == 0.0 {
            return Err(Num);
        }
        Ok(C::new(
            (self.re * o.re + self.im * o.im) / d,
            (self.im * o.re - self.re * o.im) / d,
        ))
    }
    fn exp(self) -> C {
        let e = self.re.exp();
        C::new(e * self.im.cos(), e * self.im.sin())
    }
    fn ln(self) -> Result<C, ErrorKind> {
        if self.re == 0.0 && self.im == 0.0 {
            return Err(Num);
        }
        Ok(C::new(self.abs().ln(), self.im.atan2(self.re)))
    }
    fn sin(self) -> C {
        C::new(
            self.re.sin() * self.im.cosh(),
            self.re.cos() * self.im.sinh(),
        )
    }
    fn cos(self) -> C {
        C::new(
            self.re.cos() * self.im.cosh(),
            -self.re.sin() * self.im.sinh(),
        )
    }
    fn sinh(self) -> C {
        C::new(
            self.re.sinh() * self.im.cos(),
            self.re.cosh() * self.im.sin(),
        )
    }
    fn cosh(self) -> C {
        C::new(
            self.re.cosh() * self.im.cos(),
            self.re.sinh() * self.im.sin(),
        )
    }
    fn powf(self, n: f64) -> Result<C, ErrorKind> {
        if self.re == 0.0 && self.im == 0.0 {
            return if n > 0.0 {
                Ok(C::new(0.0, 0.0))
            } else {
                Err(Num)
            };
        }
        let r = self.abs().powf(n);
        let t = self.im.atan2(self.re) * n;
        Ok(C::new(r * t.cos(), r * t.sin()))
    }
}

fn parse_real(s: &str) -> Option<f64> {
    if s.is_empty()
        || !s
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
    {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parse "a", "bi", "a+bi", "a-bj", "i", "-i"… Returns the value and suffix (if any).
fn parse_complex(s: &str) -> Result<(C, Option<char>), ErrorKind> {
    let s = s.trim();
    if s.is_empty() {
        return Ok((C::new(0.0, 0.0), None));
    }
    if let Some(v) = parse_real(s) {
        return Ok((C::new(v, 0.0), None));
    }
    let last = s.chars().last().ok_or(Num)?;
    if last != 'i' && last != 'j' {
        return Err(Num);
    }
    let body = s.get(..s.len() - 1).ok_or(Num)?;
    // Find the split between real and imaginary parts: the last +/- not following an 'e'.
    let bytes = body.as_bytes();
    let mut split = None;
    for idx in (0..bytes.len()).rev() {
        let c = bytes.get(idx).copied().unwrap_or(b'0');
        if c == b'+' || c == b'-' {
            let prev = if idx == 0 {
                None
            } else {
                bytes.get(idx - 1).copied()
            };
            if matches!(prev, Some(b'e' | b'E')) {
                continue;
            }
            split = Some(idx);
            break;
        }
    }
    let coef = |t: &str| -> Result<f64, ErrorKind> {
        match t {
            "" | "+" => Ok(1.0),
            "-" => Ok(-1.0),
            _ => parse_real(t).ok_or(Num),
        }
    };
    let (re, im) = match split {
        None | Some(0) => (0.0, coef(body)?),
        Some(i) => {
            let r = body.get(..i).ok_or(Num)?;
            let m = body.get(i..).ok_or(Num)?;
            (parse_real(r).ok_or(Num)?, coef(m)?)
        }
    };
    Ok((C::new(re, im), Some(last)))
}

fn cnum_value(v: &Value) -> Result<(C, Option<char>), ErrorKind> {
    match v {
        Value::Error(e) => Err(*e),
        Value::Bool(_) => Err(ErrorKind::Value),
        Value::Empty => Ok((C::new(0.0, 0.0), None)),
        Value::Number(n) => Ok((C::new(*n, 0.0), None)),
        Value::Text(s) => parse_complex(s),
    }
}

fn carg(ev: &mut Ev<'_>, a: &[Expr], i: usize) -> Result<(C, Option<char>), ErrorKind> {
    cnum_value(&arg(ev, a, i)?)
}

/// Up to 15 significant digits, plain decimal for moderate exponents.
fn fmt_num(x: f64) -> String {
    if x == 0.0 {
        return "0".to_string();
    }
    let sci = format!("{x:.14e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let sign = if neg { "-" } else { "" };
    if !(-18..21).contains(&exp) {
        let (h, t) = digits.split_at(1.min(digits.len()));
        let m = if t.is_empty() {
            h.to_string()
        } else {
            format!("{h}.{t}")
        };
        let es = if exp < 0 { "-" } else { "+" };
        return format!("{sign}{m}E{es}{}", exp.abs());
    }
    let nd = digits.len() as i32;
    let body = if exp < 0 {
        format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
    } else if exp + 1 >= nd {
        format!("{digits}{}", "0".repeat((exp + 1 - nd) as usize))
    } else {
        let (h, t) = digits.split_at((exp + 1) as usize);
        format!("{h}.{t}")
    };
    format!("{sign}{body}")
}

fn fmt_complex(z: C, suffix: char) -> Result<String, ErrorKind> {
    if !z.re.is_finite() || !z.im.is_finite() {
        return Err(Num);
    }
    // Round to 15 significant digits so tiny noise does not print.
    let clean = |v: f64| -> f64 {
        if v == 0.0 {
            0.0
        } else {
            fmt_num(v).parse::<f64>().unwrap_or(v)
        }
    };
    let (re, im) = (clean(z.re), clean(z.im));
    if im == 0.0 {
        return Ok(fmt_num(re));
    }
    let im_str = if im == 1.0 {
        String::new()
    } else if im == -1.0 {
        "-".to_string()
    } else {
        fmt_num(im)
    };
    if re == 0.0 {
        return Ok(format!("{im_str}{suffix}"));
    }
    let plus = if im > 0.0 { "+" } else { "" };
    Ok(format!("{}{plus}{im_str}{suffix}", fmt_num(re)))
}

fn merge_suffix(a: Option<char>, b: Option<char>) -> Result<Option<char>, ErrorKind> {
    match (a, b) {
        (Some(x), Some(y)) if x != y => Err(ErrorKind::Value),
        (Some(x), _) | (_, Some(x)) => Ok(Some(x)),
        _ => Ok(None),
    }
}

fn im_unary(
    ev: &mut Ev<'_>,
    a: &[Expr],
    f: fn(C) -> Result<C, ErrorKind>,
) -> Result<String, ErrorKind> {
    let (z, s) = carg(ev, a, 0)?;
    fmt_complex(f(z)?, s.unwrap_or('i'))
}

fn complex_fn(ev: &mut Ev<'_>, a: &[Expr]) -> Result<String, ErrorKind> {
    let re = snum(ev, a, 0)?;
    let im = snum(ev, a, 1)?;
    let suffix = match a.get(2) {
        None | Some(Expr::Missing) => 'i',
        Some(e) => match ev.eval_scalar(e) {
            Value::Error(x) => return Err(x),
            Value::Empty => 'i',
            Value::Text(t) if t.is_empty() || &*t == "i" => 'i',
            Value::Text(t) if &*t == "j" => 'j',
            _ => return Err(ErrorKind::Value),
        },
    };
    fmt_complex(C::new(re, im), suffix)
}

/// IMSUM / IMPRODUCT over every argument (ranges included).
fn im_fold(ev: &mut Ev<'_>, a: &[Expr], product: bool) -> Result<String, ErrorKind> {
    let mut acc = if product {
        None
    } else {
        Some(C::new(0.0, 0.0))
    };
    let mut suffix: Option<char> = None;
    for e in a {
        let arr = ev.eval_array(e)?;
        for v in &arr.data {
            let (z, s) = cnum_value(v)?;
            suffix = merge_suffix(suffix, s)?;
            acc = Some(match acc {
                None => z,
                Some(x) => {
                    if product {
                        x.mul(z)
                    } else {
                        x.add(z)
                    }
                }
            });
        }
    }
    fmt_complex(acc.unwrap_or(C::new(0.0, 0.0)), suffix.unwrap_or('i'))
}

fn im_binary(
    ev: &mut Ev<'_>,
    a: &[Expr],
    f: fn(C, C) -> Result<C, ErrorKind>,
) -> Result<String, ErrorKind> {
    let (x, s1) = carg(ev, a, 0)?;
    let (y, s2) = carg(ev, a, 1)?;
    let s = merge_suffix(s1, s2)?;
    fmt_complex(f(x, y)?, s.unwrap_or('i'))
}

fn nonzero(z: C) -> Result<C, ErrorKind> {
    if z.re == 0.0 && z.im == 0.0 {
        Err(Num)
    } else {
        Ok(z)
    }
}

fn one() -> C {
    C::new(1.0, 0.0)
}

pub fn register(r: &mut Registry) {
    // Number systems.
    r.add("BIN2DEC", 1, 1, |ev, a| ret(radix_to_dec(ev, a, 2)));
    r.add("OCT2DEC", 1, 1, |ev, a| ret(radix_to_dec(ev, a, 8)));
    r.add("HEX2DEC", 1, 1, |ev, a| ret(radix_to_dec(ev, a, 16)));
    r.add("BIN2OCT", 1, 2, |ev, a| {
        text_ret(radix_to_radix(ev, a, 2, 8))
    });
    r.add("BIN2HEX", 1, 2, |ev, a| {
        text_ret(radix_to_radix(ev, a, 2, 16))
    });
    r.add("OCT2BIN", 1, 2, |ev, a| {
        text_ret(radix_to_radix(ev, a, 8, 2))
    });
    r.add("OCT2HEX", 1, 2, |ev, a| {
        text_ret(radix_to_radix(ev, a, 8, 16))
    });
    r.add("HEX2BIN", 1, 2, |ev, a| {
        text_ret(radix_to_radix(ev, a, 16, 2))
    });
    r.add("HEX2OCT", 1, 2, |ev, a| {
        text_ret(radix_to_radix(ev, a, 16, 8))
    });
    r.add("DEC2BIN", 1, 2, |ev, a| text_ret(dec_to_radix(ev, a, 2)));
    r.add("DEC2OCT", 1, 2, |ev, a| text_ret(dec_to_radix(ev, a, 8)));
    r.add("DEC2HEX", 1, 2, |ev, a| text_ret(dec_to_radix(ev, a, 16)));

    // Bits.
    r.add("BITAND", 2, 2, |ev, a| {
        ret((|| Ok((bit_arg(ev, a, 0)? & bit_arg(ev, a, 1)?) as f64))())
    });
    r.add("BITOR", 2, 2, |ev, a| {
        ret((|| Ok((bit_arg(ev, a, 0)? | bit_arg(ev, a, 1)?) as f64))())
    });
    r.add("BITXOR", 2, 2, |ev, a| {
        ret((|| Ok((bit_arg(ev, a, 0)? ^ bit_arg(ev, a, 1)?) as f64))())
    });
    r.add("BITLSHIFT", 2, 2, |ev, a| ret(bit_shift(ev, a, true)));
    r.add("BITRSHIFT", 2, 2, |ev, a| ret(bit_shift(ev, a, false)));

    // Units, steps, error function.
    r.add("CONVERT", 3, 3, |ev, a| ret(convert(ev, a)));
    r.add("DELTA", 1, 2, |ev, a| {
        ret((|| {
            let x = snum(ev, a, 0)?;
            let y = sopt(ev, a, 1, 0.0)?;
            Ok(if x == y { 1.0 } else { 0.0 })
        })())
    });
    r.add("GESTEP", 1, 2, |ev, a| {
        ret((|| {
            let x = snum(ev, a, 0)?;
            let y = sopt(ev, a, 1, 0.0)?;
            Ok(if x >= y { 1.0 } else { 0.0 })
        })())
    });
    r.add("ERF", 1, 2, |ev, a| {
        ret((|| {
            let lo = snum(ev, a, 0)?;
            match a.get(1) {
                None => fin(erf(lo)),
                Some(_) => {
                    let hi = snum(ev, a, 1)?;
                    fin(erf(hi) - erf(lo))
                }
            }
        })())
    });
    r.add("ERF.PRECISE", 1, 1, |ev, a| {
        ret((|| fin(erf(snum(ev, a, 0)?)))())
    });
    r.add("ERFC", 1, 1, |ev, a| ret((|| fin(erfc(snum(ev, a, 0)?)))()));
    r.alias("ERFC.PRECISE", "ERFC");

    // Bessel.
    r.add("BESSELI", 2, 2, |ev, a| ret(bessel(ev, a, 'I')));
    r.add("BESSELJ", 2, 2, |ev, a| ret(bessel(ev, a, 'J')));
    r.add("BESSELK", 2, 2, |ev, a| ret(bessel(ev, a, 'K')));
    r.add("BESSELY", 2, 2, |ev, a| ret(bessel(ev, a, 'Y')));

    // Complex numbers.
    r.add("COMPLEX", 2, 3, |ev, a| text_ret(complex_fn(ev, a)));
    r.add("IMREAL", 1, 1, |ev, a| ret(carg(ev, a, 0).map(|z| z.0.re)));
    r.add("IMAGINARY", 1, 1, |ev, a| {
        ret(carg(ev, a, 0).map(|z| z.0.im))
    });
    r.add(
        "IMABS",
        1,
        1,
        |ev, a| ret(carg(ev, a, 0).map(|z| z.0.abs())),
    );
    r.add("IMARGUMENT", 1, 1, |ev, a| {
        ret(carg(ev, a, 0).and_then(|(z, _)| {
            if z.re == 0.0 && z.im == 0.0 {
                Err(ErrorKind::Div0)
            } else {
                Ok(z.im.atan2(z.re))
            }
        }))
    });
    r.add("IMCONJUGATE", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| Ok(C::new(z.re, -z.im))))
    });
    r.add("IMEXP", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| Ok(z.exp())))
    });
    r.add("IMLN", 1, 1, |ev, a| text_ret(im_unary(ev, a, C::ln)));
    r.add("IMLOG10", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| {
            let l = z.ln()?;
            let k = std::f64::consts::LN_10;
            Ok(C::new(l.re / k, l.im / k))
        }))
    });
    r.add("IMLOG2", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| {
            let l = z.ln()?;
            let k = std::f64::consts::LN_2;
            Ok(C::new(l.re / k, l.im / k))
        }))
    });
    r.add("IMSQRT", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| z.powf(0.5)))
    });
    r.add("IMSIN", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| Ok(z.sin())))
    });
    r.add("IMCOS", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| Ok(z.cos())))
    });
    r.add("IMTAN", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| z.sin().div(z.cos())))
    });
    r.add("IMCOT", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| z.cos().div(nonzero(z.sin())?)))
    });
    r.add("IMSEC", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| one().div(z.cos())))
    });
    r.add("IMCSC", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| one().div(nonzero(z.sin())?)))
    });
    r.add("IMSINH", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| Ok(z.sinh())))
    });
    r.add("IMCOSH", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| Ok(z.cosh())))
    });
    r.add("IMSECH", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| one().div(z.cosh())))
    });
    r.add("IMCSCH", 1, 1, |ev, a| {
        text_ret(im_unary(ev, a, |z| one().div(nonzero(z.sinh())?)))
    });
    r.add("IMPOWER", 2, 2, |ev, a| {
        text_ret((|| {
            let (z, s) = carg(ev, a, 0)?;
            let n = snum(ev, a, 1)?;
            fmt_complex(z.powf(n)?, s.unwrap_or('i'))
        })())
    });
    r.add("IMDIV", 2, 2, |ev, a| text_ret(im_binary(ev, a, C::div)));
    r.add("IMSUB", 2, 2, |ev, a| {
        text_ret(im_binary(ev, a, |x, y| Ok(x.sub(y))))
    });
    r.add("IMSUM", 1, 255, |ev, a| text_ret(im_fold(ev, a, false)));
    r.add("IMPRODUCT", 1, 255, |ev, a| text_ret(im_fold(ev, a, true)));
}

#[cfg(test)]
mod tests {
    use crate::recalc::eval_formula;
    use sp_core::{CellAddr, ErrorKind, Value};
    use sp_model::Workbook;

    fn ev(src: &str) -> Value {
        eval_formula(&Workbook::new(), 0, CellAddr::new(0, 0), src)
    }

    fn approx(src: &str, want: f64) {
        match ev(src) {
            Value::Number(n) => assert!(
                (n - want).abs() <= 1e-9 * want.abs().max(1.0),
                "{src}: got {n}, want {want}"
            ),
            other => panic!("{src}: got {other:?}, want {want}"),
        }
    }

    fn text(src: &str, want: &str) {
        assert_eq!(ev(src), Value::text(want), "{src}");
    }

    #[test]
    fn number_systems() {
        approx("BIN2DEC(1100100)", 100.0);
        approx("BIN2DEC(\"1111111111\")", -1.0);
        text("BIN2HEX(1100100,3)", "064");
        text("DEC2BIN(-100)", "1110011100");
        text("DEC2HEX(-1213)", "FFFFFFFB43");
        text("DEC2OCT(511,10)", "0000000777");
        text("HEX2BIN(\"FE\",9)", "011111110");
        approx("HEX2DEC(\"8000000000\")", -549_755_813_888.0);
        text("OCT2HEX(\"1234566\")", "53976");
        assert_eq!(ev("DEC2BIN(512)"), Value::Error(ErrorKind::Num));
        assert_eq!(ev("HEX2DEC(\"QWERTY\")"), Value::Error(ErrorKind::Num));
        assert_eq!(ev("DEC2BIN(3,0)"), Value::Error(ErrorKind::Num));
    }

    #[test]
    fn bits_and_steps() {
        approx("BITAND(13,25)", 9.0);
        approx("BITOR(13,25)", 29.0);
        approx("BITXOR(13,25)", 20.0);
        approx("BITLSHIFT(12,44)", 211_106_232_532_992.0);
        approx("BITRSHIFT(145,-3)", 1160.0);
        assert_eq!(ev("BITLSHIFT(12,45)"), Value::Error(ErrorKind::Num));
        assert_eq!(ev("BITAND(-1,2)"), Value::Error(ErrorKind::Num));
        approx("DELTA(5,5)", 1.0);
        approx("GESTEP(-1)", 0.0);
        approx("ERF(1)", 0.842_700_792_949_714_9);
        approx("ERF(0,5)", 0.999_999_999_998_462_6);
        approx("ERFC(3)", 2.209_049_699_858_544e-5);
    }

    #[test]
    fn convert_units() {
        approx("CONVERT(1,\"lbm\",\"g\")", 453.592_37);
        approx("CONVERT(68,\"F\",\"C\")", 20.0);
        approx("CONVERT(1,\"mi\",\"km\")", 1.609_344);
        approx("CONVERT(1,\"m2\",\"cm2\")", 10_000.0);
        approx("CONVERT(1,\"kibyte\",\"bit\")", 8192.0);
        assert_eq!(ev("CONVERT(1,\"m\",\"g\")"), Value::Error(ErrorKind::NA));
        assert_eq!(ev("CONVERT(1,\"Mlbm\",\"g\")"), Value::Error(ErrorKind::NA));
    }

    #[test]
    fn bessel() {
        approx("BESSELI(1.5,12)", 6.904_955_117_003_34e-11);
        approx("BESSELK(1.5,12)", 598_740_569.415_651_7);
        approx("BESSELJ(1.9,2)", 0.329_925_727_692_387_8);
        approx("BESSELY(2.5,1)", 0.145_918_137_966_786_6);
        // The target's large-x J1 approximation (see `pq1`) shifts this from the true
        // -0.0989013945604 to its own value.
        approx("BESSELJ(20,3)", -0.098_901_384_895_778_21);
        assert_eq!(ev("BESSELK(0,1)"), Value::Error(ErrorKind::Num));
        assert_eq!(ev("BESSELJ(1,-1)"), Value::Error(ErrorKind::Num));
        let _ = ev("BESSELJ(1E300,9999)");
    }

    #[test]
    fn complex() {
        text("COMPLEX(3,4)", "3+4i");
        text("COMPLEX(0,-1,\"j\")", "-j");
        text("IMPRODUCT(\"3+4i\",\"-4+5i\")", "-32-i");
        text("IMSUM(\"3+4i\",\"-4+5i\")", "-1+9i");
        text("IMSUB(\"i\",\"i\")", "0");
        approx("IMABS(\"5+12i\")", 13.0);
        approx("IMREAL(IMDIV(\"-238+240i\",\"10+24i\"))", 5.0);
        approx("IMAGINARY(IMEXP(\"1+i\"))", 2.287_355_287_178_842);
        approx("IMREAL(IMSQRT(\"-4\"))", 0.0);
        approx("IMAGINARY(IMSQRT(\"-4\"))", 2.0);
        text("COMPLEX(1.2E-45,-3)", "1.2E-45-3i");
        assert_eq!(ev("IMLN(0)"), Value::Error(ErrorKind::Num));
        assert_eq!(ev("IMSUM(\"1+i\",\"1+j\")"), Value::Error(ErrorKind::Value));
        assert_eq!(ev("COMPLEX(1,2,\"k\")"), Value::Error(ErrorKind::Value));
    }
}
