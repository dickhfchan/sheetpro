//! Special mathematical functions used by the statistical and engineering functions.
//!
//! Implemented from textbook definitions: Lanczos approximation for the gamma function,
//! power series / continued fractions for the incomplete gamma and beta functions and for
//! the error function, and bracketed bisection for distribution inverses. Every loop has a
//! fixed iteration cap; non-convergence yields NaN, which callers map to `#NUM!`.

use std::f64::consts::PI;

/// ln(sqrt(2π)).
pub const LN_SQRT_2PI: f64 = 0.918_938_533_204_672_8;
const EPS: f64 = 1e-16;
const TINY: f64 = 1e-300;
const MAX_ITER: usize = 100_000;

/// Lanczos coefficients for g = 7, n = 9.
const LANCZOS_G: f64 = 7.0;
const LANCZOS: [f64; 9] = [
    0.999_999_999_999_809_9,
    676.520_368_121_885_1,
    -1_259.139_216_722_402_8,
    771.323_428_777_653_1,
    -176.615_029_162_140_6,
    12.507_343_278_686_905,
    -0.138_571_095_265_720_12,
    9.984_369_578_019_572e-6,
    1.505_632_735_149_311_6e-7,
];

fn lanczos_sum(x: f64) -> f64 {
    // x is the shifted argument (z - 1).
    let mut a = 0.0;
    for (i, c) in LANCZOS.iter().enumerate().rev() {
        if i == 0 {
            a += c;
        } else {
            a += c / (x + i as f64);
        }
    }
    a
}

/// ln|Γ(x)|. Returns +∞ at the poles (0, -1, -2, …).
pub fn ln_gamma(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x.is_infinite() {
        return f64::INFINITY;
    }
    if x < 0.5 {
        let s = (PI * x).sin();
        if s == 0.0 || x == x.floor() {
            return f64::INFINITY;
        }
        return (PI / s.abs()).ln() - ln_gamma(1.0 - x);
    }
    if x == 1.0 || x == 2.0 {
        return 0.0;
    }
    if x > 1e15 {
        // Stirling: the Lanczos sum is 1 to machine precision here.
        return (x - 0.5) * x.ln() - x + LN_SQRT_2PI + 1.0 / (12.0 * x);
    }
    let z = x - 1.0;
    let t = z + LANCZOS_G + 0.5;
    LN_SQRT_2PI + (z + 0.5) * t.ln() - t + lanczos_sum(z).ln()
}

/// Γ(x). NaN at the poles; ±∞ on overflow.
pub fn gamma(x: f64) -> f64 {
    if x.is_nan() || (x <= 0.0 && x == x.floor()) {
        return f64::NAN;
    }
    if x == x.floor() && x <= 171.0 {
        let mut f = 1.0;
        let mut k = 2.0;
        while k < x {
            f *= k;
            k += 1.0;
        }
        return f;
    }
    if x < 0.5 {
        let s = (PI * x).sin();
        let g = gamma(1.0 - x);
        if !g.is_finite() || s == 0.0 {
            return if g.is_finite() { f64::NAN } else { 0.0 };
        }
        return PI / (s * g);
    }
    if x > 171.7 {
        return f64::INFINITY;
    }
    let z = x - 1.0;
    let t = z + LANCZOS_G + 0.5;
    let half = t.powf((z + 0.5) / 2.0);
    (2.0 * PI).sqrt() * half * (half * (-t).exp()) * lanczos_sum(z)
}

/// ln B(a, b).
pub fn ln_beta(a: f64, b: f64) -> f64 {
    ln_gamma(a) + ln_gamma(b) - ln_gamma(a + b)
}

/// ln(n!) for n ≥ 0 (exact product below 171).
pub fn ln_factorial(n: f64) -> f64 {
    if n < 171.0 && n == n.floor() {
        let mut f = 1.0_f64;
        let mut k = 2.0;
        while k <= n {
            f *= k;
            k += 1.0;
        }
        return f.ln();
    }
    ln_gamma(n + 1.0)
}

/// ln C(n, k) for 0 ≤ k ≤ n.
pub fn ln_choose(n: f64, k: f64) -> f64 {
    ln_factorial(n) - ln_factorial(k) - ln_factorial(n - k)
}

/// C(n, k) for integer 0 ≤ k ≤ n, exact-ish product when small.
pub fn choose(n: f64, k: f64) -> f64 {
    if k < 0.0 || k > n {
        return 0.0;
    }
    let k = k.min(n - k);
    if k <= 200.0 {
        let mut r = 1.0;
        let mut i = 1.0;
        while i <= k {
            r = r * (n - k + i) / i;
            i += 1.0;
        }
        return if r < 9e15 { r.round() } else { r };
    }
    ln_choose(n, k).exp()
}

// ── error function ──────────────────────────────────────────────────────────────────────

fn erf_series(x: f64) -> f64 {
    // erf(x) = 2/√π e^{-x²} Σ (2x²)^n x / (1·3·…·(2n+1))
    let x2 = 2.0 * x * x;
    let mut term = x;
    let mut sum = x;
    let mut n = 0.0;
    for _ in 0..500 {
        n += 1.0;
        term *= x2 / (2.0 * n + 1.0);
        sum += term;
        if term.abs() < sum.abs() * EPS {
            break;
        }
    }
    2.0 / PI.sqrt() * (-x * x).exp() * sum
}

fn erfc_cf(x: f64) -> f64 {
    // erfc(x) = e^{-x²}/√π · 1/(x + (1/2)/(x + 1/(x + (3/2)/(x + …)))), x > 0.
    let mut f = x;
    let mut c = x;
    let mut d = 0.0;
    for j in 1..5000 {
        let a = j as f64 / 2.0;
        d = x + a * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = x + a / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let delta = c * d;
        f *= delta;
        if (delta - 1.0).abs() < EPS {
            break;
        }
    }
    (-x * x).exp() / (PI.sqrt() * f)
}

/// Error function.
pub fn erf(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    let ax = x.abs();
    let r = if ax < 2.5 {
        erf_series(ax)
    } else if ax > 6.0 {
        1.0
    } else {
        1.0 - erfc_cf(ax)
    };
    if x < 0.0 { -r } else { r }
}

/// Complementary error function.
pub fn erfc(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x < 0.0 {
        return 2.0 - erfc(-x);
    }
    if x < 2.5 {
        1.0 - erf_series(x)
    } else if x > 27.3 {
        0.0
    } else {
        erfc_cf(x)
    }
}

// ── normal distribution ─────────────────────────────────────────────────────────────────

/// Standard normal density.
pub fn norm_pdf(z: f64) -> f64 {
    (-0.5 * z * z - LN_SQRT_2PI).exp()
}

/// Standard normal CDF.
pub fn norm_cdf(z: f64) -> f64 {
    0.5 * erfc(-z / std::f64::consts::SQRT_2)
}

/// Standard normal quantile; NaN outside (0, 1).
pub fn norm_inv(p: f64) -> f64 {
    if !(p > 0.0 && p < 1.0) {
        return f64::NAN;
    }
    if p == 0.5 {
        return 0.0;
    }
    // Solve the smaller tail: Φ(-t) = q with t > 0.
    let (q, sign) = if p < 0.5 { (p, -1.0) } else { (1.0 - p, 1.0) };
    let t = bisect(|t| q - norm_cdf(-t), 0.0, 40.0);
    // One Newton polish step on the tail equation.
    let t = {
        let f = norm_cdf(-t) - q;
        let d = norm_pdf(t);
        if d > 0.0 { t + f / d } else { t }
    };
    sign * t
}

// ── incomplete gamma ────────────────────────────────────────────────────────────────────

fn gamma_front(a: f64, x: f64) -> f64 {
    (a * x.ln() - x - ln_gamma(a)).exp()
}

/// Regularized lower incomplete gamma P(a, x), a > 0, x ≥ 0.
pub fn gamma_p(a: f64, x: f64) -> f64 {
    if a.is_nan() || a <= 0.0 || x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return 0.0;
    }
    if x.is_infinite() {
        return 1.0;
    }
    if x < a + 1.0 {
        gamma_series(a, x)
    } else {
        1.0 - gamma_cf(a, x)
    }
}

/// Regularized upper incomplete gamma Q(a, x) = 1 - P(a, x).
pub fn gamma_q(a: f64, x: f64) -> f64 {
    if a.is_nan() || a <= 0.0 || x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return 1.0;
    }
    if x.is_infinite() {
        return 0.0;
    }
    if x < a + 1.0 {
        1.0 - gamma_series(a, x)
    } else {
        gamma_cf(a, x)
    }
}

fn gamma_series(a: f64, x: f64) -> f64 {
    let mut ap = a;
    let mut del = 1.0 / a;
    let mut sum = del;
    let mut converged = false;
    for _ in 0..MAX_ITER {
        ap += 1.0;
        del *= x / ap;
        sum += del;
        if del.abs() < sum.abs() * EPS {
            converged = true;
            break;
        }
    }
    if !converged {
        return f64::NAN;
    }
    (sum * gamma_front(a, x)).min(1.0)
}

fn gamma_cf(a: f64, x: f64) -> f64 {
    let mut b = x + 1.0 - a;
    let mut c = 1.0 / TINY;
    let mut d = 1.0 / b;
    let mut h = d;
    let mut converged = false;
    for i in 1..MAX_ITER {
        let i = i as f64;
        let an = -i * (i - a);
        b += 2.0;
        d = an * d + b;
        if d.abs() < TINY {
            d = TINY;
        }
        c = b + an / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            converged = true;
            break;
        }
    }
    if !converged {
        return f64::NAN;
    }
    (gamma_front(a, x) * h).min(1.0)
}

// ── incomplete beta ─────────────────────────────────────────────────────────────────────

/// Regularized incomplete beta I_x(a, b), with `y = 1 - x` supplied for precision.
pub fn beta_reg2(x: f64, y: f64, a: f64, b: f64) -> f64 {
    if a.is_nan()
        || b.is_nan()
        || a <= 0.0
        || b <= 0.0
        || x.is_nan()
        || y.is_nan()
        || x < 0.0
        || y < 0.0
    {
        return f64::NAN;
    }
    if x == 0.0 {
        return 0.0;
    }
    if y == 0.0 {
        return 1.0;
    }
    let front = (a * x.ln() + b * y.ln() - ln_beta(a, b)).exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        front * beta_cf(x, a, b) / a
    } else {
        1.0 - front * beta_cf(y, b, a) / b
    }
}

/// Regularized incomplete beta I_x(a, b).
pub fn beta_reg(x: f64, a: f64, b: f64) -> f64 {
    beta_reg2(x, 1.0 - x, a, b)
}

fn beta_cf(x: f64, a: f64, b: f64) -> f64 {
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < TINY {
        d = TINY;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..MAX_ITER {
        let m = m as f64;
        let m2 = 2.0 * m;
        let aa = m * (b - m) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        h *= d * c;
        let aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < TINY {
            d = TINY;
        }
        c = 1.0 + aa / c;
        if c.abs() < TINY {
            c = TINY;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            return h;
        }
    }
    f64::NAN
}

// ── root finding ────────────────────────────────────────────────────────────────────────

/// Solve `h(x) = 0` on `[lo, hi]` for an increasing `h` with `h(lo) ≤ 0 ≤ h(hi)`.
/// Bisects geometrically on wide positive brackets so tiny and huge roots both converge.
pub fn bisect(h: impl Fn(f64) -> f64, mut lo: f64, mut hi: f64) -> f64 {
    for _ in 0..3000 {
        let mid = if lo > 0.0 && hi > 2.0 * lo {
            (lo.sqrt()) * (hi.sqrt())
        } else if lo == 0.0 && hi > 1.0 {
            1.0_f64.min(hi / 2.0)
        } else if lo == 0.0 && hi > 1e-300 {
            hi * 1e-3
        } else {
            lo + (hi - lo) / 2.0
        };
        if !(mid > lo && mid < hi) {
            break;
        }
        let v = h(mid);
        if v.is_nan() {
            return f64::NAN;
        }
        if v == 0.0 {
            return mid;
        }
        if v < 0.0 {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo + (hi - lo) / 2.0
}

/// Quantile of a distribution on `(0, ∞)` (or `(0, upper]`) given its CDF and survival
/// function: the x with CDF(x) = p. Uses the survival function above the median for
/// precision in the upper tail.
pub fn quantile_pos(cdf: impl Fn(f64) -> f64, sf: impl Fn(f64) -> f64, p: f64, upper: f64) -> f64 {
    if !(0.0..=1.0).contains(&p) {
        return f64::NAN;
    }
    if p == 0.0 {
        return 0.0;
    }
    let h = |x: f64| {
        if p <= 0.5 {
            cdf(x) - p
        } else {
            (1.0 - p) - sf(x)
        }
    };
    // Expand the upper bracket until it contains the root.
    let mut hi = upper.min(1.0);
    let mut n = 0;
    while hi < upper {
        let v = h(hi);
        if v.is_nan() {
            return f64::NAN;
        }
        if v >= 0.0 {
            break;
        }
        hi = (hi * 4.0).min(upper);
        n += 1;
        if n > 600 {
            return f64::NAN;
        }
    }
    let v = h(hi);
    if v.is_nan() || v < 0.0 {
        return if p == 1.0 { hi } else { f64::NAN };
    }
    bisect(h, 0.0, hi)
}

/// The x > 0 with `tail(x) = q` for a decreasing tail function on `(0, ∞)`.
pub fn solve_tail(tail: impl Fn(f64) -> f64, q: f64) -> f64 {
    let h = |x: f64| q - tail(x);
    let mut hi = 1.0_f64;
    let mut n = 0;
    loop {
        let v = h(hi);
        if v.is_nan() {
            return f64::NAN;
        }
        if v >= 0.0 {
            break;
        }
        hi *= 4.0;
        n += 1;
        if n > 600 || !hi.is_finite() {
            return f64::NAN;
        }
    }
    bisect(h, 0.0, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol * b.abs().max(1.0)
    }

    #[test]
    fn gamma_values() {
        assert!(close(gamma(5.0), 24.0, 1e-15));
        assert!(close(gamma(0.5), PI.sqrt(), 1e-14));
        assert!(close(gamma(-0.5), -2.0 * PI.sqrt(), 1e-14));
        assert!(close(gamma(2.5), 1.329_340_388_179_137, 1e-14));
        assert!(gamma(0.0).is_nan());
        assert!(gamma(-3.0).is_nan());
        assert!(close(ln_gamma(10.0), 12.801_827_480_081_469, 1e-14));
        assert!(close(ln_gamma(0.1), 2.252_712_651_734_206, 1e-14));
    }

    #[test]
    fn erf_values() {
        assert!(close(erf(0.5), 0.520_499_877_813_046_5, 1e-15));
        assert!(close(erf(1.0), 0.842_700_792_949_714_9, 1e-15));
        assert!(close(erfc(3.0), 2.209_049_699_858_544e-5, 1e-13));
        assert!(close(erfc(-1.0), 1.842_700_792_949_715, 1e-15));
        assert!(close(erfc(1.8) / 0.010_909_498_364_269_283, 1.0, 1e-13));
    }

    #[test]
    fn normal() {
        assert!(close(norm_cdf(1.96), 0.975_002_104_851_780, 1e-14));
        assert!(close(
            norm_cdf(-10.0) / 7.619_853_024_160_527e-24,
            1.0,
            1e-12
        ));
        assert!(close(norm_inv(0.975), 1.959_963_984_540_054, 1e-13));
        assert!(close(norm_inv(1e-7), -5.199_337_582_192_816, 1e-12));
    }

    #[test]
    fn incomplete() {
        // P(1, x) = 1 - e^{-x}
        assert!(close(gamma_p(1.0, 2.0), 1.0 - (-2.0f64).exp(), 1e-15));
        assert!(close(gamma_q(3.0, 10.0), 0.002_769_395_715_511_5, 1e-12));
        // I_x(1, 1) = x; I_x(2, 3)
        assert!(close(beta_reg(0.3, 1.0, 1.0), 0.3, 1e-15));
        assert!(close(beta_reg(0.4, 2.0, 3.0), 0.5248, 1e-14));
    }

    #[test]
    fn hostile_inputs_terminate() {
        assert!(gamma_p(1e300, 1e300).is_nan() || gamma_p(1e300, 1e300) <= 1.0);
        let _ = beta_reg(0.5, 1e300, 1e-300);
        let _ = quantile_pos(
            |x| gamma_p(1e-300, x),
            |x| gamma_q(1e-300, x),
            0.5,
            f64::MAX,
        );
        assert!(norm_inv(0.0).is_nan());
    }
}
