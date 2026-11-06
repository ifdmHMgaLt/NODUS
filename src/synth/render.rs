//! Turning a fitted curve into Graphwar's formula syntax.

use crate::protocol::{FST_ODE, SND_ODE};

use super::rbf::Rbf;

// ---------------- formula rendering ----------------

fn num(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let mut s = format!("{:.8}", v);
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" {
        s.clear();
        s.push('0');
    }
    s
}

fn x_minus(c: f64) -> String {
    if c.abs() < 5e-8 {
        "x".into()
    } else if c > 0.0 {
        format!("(x-{})", num(c))
    } else {
        format!("(x+{})", num(-c))
    }
}

fn gauss_core(c: f64, s2: f64) -> String {
    format!("exp(0-(({})^2)/{})", x_minus(c), num(s2))
}

struct Term {
    coef: f64,
    core: String,
}

/// Graphwar's parser has no unary minus, so a leading negative term is
/// written as `0-...`.
fn render_terms(constant: f64, linear: f64, terms: Vec<Term>) -> String {
    let mut items: Vec<(f64, String)> = Vec::new();
    if constant.abs() > 5e-10 {
        items.push((constant, String::new()));
    }
    if linear.abs() > 5e-10 {
        items.push((linear, "x".to_string()));
    }
    for t in terms {
        if t.coef.abs() > 5e-10 {
            items.push((t.coef, t.core));
        }
    }
    if items.is_empty() {
        return "0".into();
    }
    let mut out = String::new();
    for (i, (coef, core)) in items.iter().enumerate() {
        let sign = if *coef < 0.0 { '-' } else { '+' };
        let mag = num(coef.abs());
        let body = if core.is_empty() {
            mag
        } else if (coef.abs() - 1.0).abs() < 5e-10 {
            core.clone()
        } else {
            format!("{mag}*{core}")
        };
        if i == 0 {
            if *coef < 0.0 {
                out.push_str("0-");
                out.push_str(&body);
            } else {
                out.push_str(&body);
            }
        } else {
            out.push(sign);
            out.push_str(&body);
        }
    }
    out
}

pub fn formula_for(mode: i32, rbf: &Rbf) -> String {
    match mode {
        FST_ODE => {
            let s2 = rbf.sigma2;
            let mut terms = Vec::new();
            for (c, w) in rbf.centers.iter().zip(&rbf.weights) {
                let coef = -2.0 * w / s2;
                let core = format!("{}*{}", x_minus(*c), gauss_core(*c, s2));
                terms.push(Term { coef, core });
            }
            // d/dx (a + b*x + sum ...) = b + sum ...
            render_terms(rbf.b, 0.0, terms)
        }
        SND_ODE => {
            let s2 = rbf.sigma2;
            let mut terms = Vec::new();
            for (c, w) in rbf.centers.iter().zip(&rbf.weights) {
                let q2 = 4.0 * w / (s2 * s2);
                let core2 = format!("({})^2*{}", x_minus(*c), gauss_core(*c, s2));
                terms.push(Term {
                    coef: q2,
                    core: core2,
                });
                let q0 = -2.0 * w / s2;
                terms.push(Term {
                    coef: q0,
                    core: gauss_core(*c, s2),
                });
            }
            render_terms(0.0, 0.0, terms)
        }
        _ => {
            let s2 = rbf.sigma2;
            let mut terms = Vec::new();
            for (c, w) in rbf.centers.iter().zip(&rbf.weights) {
                terms.push(Term {
                    coef: *w,
                    core: gauss_core(*c, s2),
                });
            }
            render_terms(rbf.a, rbf.b, terms)
        }
    }
}
