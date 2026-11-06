//! Smooth radial-basis curves and the dense linear solve behind them.

// Gaussian elimination reads clearer with row/column indices than with
// split_at_mut dances.
#![allow(clippy::needless_range_loop)]

#[derive(Clone, Debug)]
pub struct Rbf {
    pub(super) centers: Vec<f64>,
    pub(super) weights: Vec<f64>,
    pub(super) a: f64,
    pub(super) b: f64,
    pub(super) sigma2: f64,
}

impl Rbf {
    pub fn eval(&self, x: f64) -> f64 {
        let mut s = self.a + self.b * x;
        for (c, w) in self.centers.iter().zip(&self.weights) {
            let d = x - c;
            s += w * (-(d * d) / self.sigma2).exp();
        }
        s
    }

    pub fn derivative(&self, x: f64) -> f64 {
        let mut s = self.b;
        for (c, w) in self.centers.iter().zip(&self.weights) {
            let d = x - c;
            let phi = (-(d * d) / self.sigma2).exp();
            s += w * (-2.0 * d / self.sigma2) * phi;
        }
        s
    }

    pub fn second(&self, x: f64) -> f64 {
        let mut s = 0.0;
        for (c, w) in self.centers.iter().zip(&self.weights) {
            let d = x - c;
            let phi = (-(d * d) / self.sigma2).exp();
            s += w * (4.0 * d * d / (self.sigma2 * self.sigma2) - 2.0 / self.sigma2) * phi;
        }
        s
    }

    pub(super) fn max_weight(&self) -> f64 {
        self.weights.iter().fold(0.0f64, |m, w| m.max(w.abs()))
    }
}

/// Gauss-Jordan elimination with partial pivoting; `None` on a singular matrix.
fn solve_linear(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let mut piv = col;
        for r in col + 1..n {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if a[piv][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        let d = a[col][col];
        for j in col..n {
            a[col][j] /= d;
        }
        b[col] /= d;
        for r in 0..n {
            if r == col {
                continue;
            }
            let f = a[r][col];
            if f.abs() < 1e-18 {
                continue;
            }
            for j in col..n {
                a[r][j] -= f * a[col][j];
            }
            b[r] -= f * b[col];
        }
    }
    Some(b)
}

/// Fit RBF through control points. Duplicate/near-duplicate x are merged.
pub fn fit_rbf(points: &[(f64, f64)], sigma: f64) -> Option<Rbf> {
    fit_rbf_slope(points, sigma, None)
}

/// Fit RBF through control points with an optional imposed derivative
/// `F'(x0) = slope`.  The slope condition replaces the usual zero
/// first-moment condition, keeping the linear system square.
pub(super) fn fit_rbf_slope(
    points: &[(f64, f64)],
    sigma: f64,
    slope: Option<(f64, f64)>,
) -> Option<Rbf> {
    if points.len() < 2 || !sigma.is_finite() || sigma <= 0.0 {
        return None;
    }
    let mut pts: Vec<(f64, f64)> = points.to_vec();
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut merged: Vec<(f64, f64)> = Vec::new();
    for p in pts {
        if let Some(last) = merged.last_mut() {
            if (p.0 - last.0).abs() < 1e-3 {
                last.1 = p.1;
                continue;
            }
        }
        merged.push(p);
    }
    if merged.len() < 2 {
        return None;
    }
    let n = merged.len();
    let s2 = sigma * sigma;
    let mut mat = vec![vec![0.0; n + 2]; n + 2];
    let mut rhs = vec![0.0; n + 2];
    for i in 0..n {
        for j in 0..n {
            let d = merged[i].0 - merged[j].0;
            mat[i][j] = (-(d * d) / s2).exp();
        }
        mat[i][n] = 1.0;
        mat[i][n + 1] = merged[i].0;
        rhs[i] = merged[i].1;
    }
    for j in 0..n {
        mat[n][j] = 1.0;
    }
    match slope {
        None => {
            for j in 0..n {
                mat[n + 1][j] = merged[j].0;
            }
        }
        Some((x0, s)) => {
            for j in 0..n {
                let d = x0 - merged[j].0;
                mat[n + 1][j] = (-2.0 * d / s2) * (-(d * d) / s2).exp();
            }
            mat[n + 1][n + 1] = 1.0;
            rhs[n + 1] = s;
        }
    }
    let sol = solve_linear(mat, rhs)?;
    let weights = sol[..n].to_vec();
    let rbf = Rbf {
        centers: merged.iter().map(|p| p.0).collect(),
        weights,
        a: sol[n],
        b: sol[n + 1],
        sigma2: s2,
    };
    if rbf.weights.iter().any(|w| !w.is_finite() || w.abs() > 1e7)
        || !rbf.a.is_finite()
        || !rbf.b.is_finite()
    {
        return None;
    }
    Some(rbf)
}
