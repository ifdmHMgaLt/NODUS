// A small parser/evaluator matching the expression language of Graphwar:
// numbers, x, y, y', + - * / ^, sqrt log ln abs sin cos tan exp, e and pi.
// Graphwar's own parser is unusual; this one accepts the same practical syntax
// and is used to reproduce every shot that appears in the room.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Func {
    Sqrt,
    Log,
    Ln,
    Abs,
    Sin,
    Cos,
    Tan,
    Exp,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Const(f64),
    X,
    Y,
    Dy,
    Neg(Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Pow(Box<Expr>, Box<Expr>),
    Call(Func, Box<Expr>),
}

impl Expr {
    pub fn parse(src: &str) -> Result<Expr, String> {
        // Opponents (and the bundled AI) send formulas with the locale decimal
        // comma, e.g. "(-5,41)".  There are no multi-argument functions here,
        // so every comma is a decimal point.
        let src = src.replace(',', ".");
        let mut p = Parser::new(&src)?;
        let e = p.parse_add()?;
        if p.pos != p.toks.len() {
            return Err(format!("unexpected token at {}", p.pos));
        }
        Ok(e)
    }

    pub fn eval(&self, x: f64, y: f64, dy: f64) -> f64 {
        match self {
            Expr::Const(v) => *v,
            Expr::X => x,
            Expr::Y => y,
            Expr::Dy => dy,
            Expr::Neg(a) => -a.eval(x, y, dy),
            Expr::Add(a, b) => a.eval(x, y, dy) + b.eval(x, y, dy),
            Expr::Sub(a, b) => a.eval(x, y, dy) - b.eval(x, y, dy),
            Expr::Mul(a, b) => a.eval(x, y, dy) * b.eval(x, y, dy),
            Expr::Div(a, b) => a.eval(x, y, dy) / b.eval(x, y, dy),
            Expr::Pow(a, b) => a.eval(x, y, dy).powf(b.eval(x, y, dy)),
            Expr::Call(f, a) => {
                let v = a.eval(x, y, dy);
                match f {
                    Func::Sqrt => v.sqrt(),
                    Func::Log => v.log10(),
                    Func::Ln => v.ln(),
                    Func::Abs => v.abs(),
                    Func::Sin => v.sin(),
                    Func::Cos => v.cos(),
                    Func::Tan => v.tan(),
                    Func::Exp => v.exp(),
                }
            }
        }
    }

    pub fn eval_x(&self, x: f64) -> f64 {
        self.eval(x, 0.0, 0.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    X,
    Y,
    Dy,
    Fn(Func),
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    LParen,
    RParen,
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn new(src: &str) -> Result<Self, String> {
        Ok(Self {
            toks: tokenize(src)?,
            pos: 0,
        })
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn eat(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_add(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_mul()?;
        loop {
            match self.peek() {
                Some(Tok::Plus) => {
                    self.pos += 1;
                    let rhs = self.parse_mul()?;
                    lhs = Expr::Add(Box::new(lhs), Box::new(rhs));
                }
                Some(Tok::Minus) => {
                    self.pos += 1;
                    let rhs = self.parse_mul()?;
                    lhs = Expr::Sub(Box::new(lhs), Box::new(rhs));
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn starts_primary(&self) -> bool {
        matches!(
            self.peek(),
            Some(Tok::Num(_))
                | Some(Tok::X)
                | Some(Tok::Y)
                | Some(Tok::Dy)
                | Some(Tok::Fn(_))
                | Some(Tok::LParen)
        )
    }

    fn parse_mul(&mut self) -> Result<Expr, String> {
        let mut lhs = self.parse_unary()?;
        loop {
            match self.peek() {
                Some(Tok::Star) => {
                    self.pos += 1;
                    let rhs = self.parse_unary()?;
                    lhs = Expr::Mul(Box::new(lhs), Box::new(rhs));
                }
                Some(Tok::Slash) => {
                    self.pos += 1;
                    let rhs = self.parse_unary()?;
                    lhs = Expr::Div(Box::new(lhs), Box::new(rhs));
                }
                _ if self.starts_primary() => {
                    // implicit multiplication: 2x, 3(x+1), xsin(x)
                    let rhs = self.parse_unary()?;
                    lhs = Expr::Mul(Box::new(lhs), Box::new(rhs));
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(Tok::Plus) => {
                self.pos += 1;
                self.parse_unary()
            }
            Some(Tok::Minus) => {
                self.pos += 1;
                Ok(Expr::Neg(Box::new(self.parse_unary()?)))
            }
            _ => self.parse_power(),
        }
    }

    fn parse_power(&mut self) -> Result<Expr, String> {
        let base = self.parse_atom()?;
        if matches!(self.peek(), Some(Tok::Caret)) {
            self.pos += 1;
            let exp = self.parse_unary()?;
            Ok(Expr::Pow(Box::new(base), Box::new(exp)))
        } else {
            Ok(base)
        }
    }

    fn parse_atom(&mut self) -> Result<Expr, String> {
        match self.eat() {
            Some(Tok::Num(v)) => Ok(Expr::Const(v)),
            Some(Tok::X) => Ok(Expr::X),
            Some(Tok::Y) => Ok(Expr::Y),
            Some(Tok::Dy) => Ok(Expr::Dy),
            Some(Tok::LParen) => {
                let e = self.parse_add()?;
                match self.eat() {
                    Some(Tok::RParen) => Ok(e),
                    _ => Err("missing )".into()),
                }
            }
            Some(Tok::Fn(f)) => {
                if matches!(self.peek(), Some(Tok::LParen)) {
                    self.pos += 1;
                    let e = self.parse_add()?;
                    if !matches!(self.eat(), Some(Tok::RParen)) {
                        return Err("missing ) after function argument".into());
                    }
                    return Ok(Expr::Call(f, Box::new(e)));
                }
                // Graphwar also accepts `sin x`, `sqrt 4`, `ln e`: the argument
                // is the next atom, and such a call binds tighter than `^`
                // (`sin x^2` is `(sin x)^2` in the game).
                let e = self.parse_atom()?;
                Ok(Expr::Call(f, Box::new(e)))
            }
            other => Err(format!("unexpected token {:?}", other)),
        }
    }
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let s = src.to_ascii_lowercase();
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if !c.is_ascii() {
            // The game's regex scanner never matches non-ASCII, so it skips
            // the whole character; advance by its UTF-8 length.
            let ch = s[i..].chars().next().unwrap();
            i += ch.len_utf8();
            continue;
        }
        if c.is_ascii_digit() || c == '.' {
            let start = i;
            let mut seen_dot = false;
            while i < b.len() {
                let d = b[i] as char;
                if d.is_ascii_digit() {
                    i += 1;
                } else if d == '.' && !seen_dot {
                    seen_dot = true;
                    i += 1;
                } else {
                    break;
                }
            }
            let num = &s[start..i];
            let v: f64 = num.parse().map_err(|_| format!("bad number {num}"))?;
            out.push(Tok::Num(v));
            continue;
        }
        if c == '\'' {
            // A stray apostrophe is not a token (the game's regex simply skips
            // it); `y'` was consumed together with its y above.
            i += 1;
            continue;
        }
        if c == 'y' && i + 1 < b.len() && b[i + 1] == b'\'' {
            out.push(Tok::Dy);
            i += 2;
            continue;
        }
        let rest = &s[i..];
        let names: &[(&str, Tok)] = &[
            ("sqrt", Tok::Fn(Func::Sqrt)),
            ("log", Tok::Fn(Func::Log)),
            ("abs", Tok::Fn(Func::Abs)),
            ("sin", Tok::Fn(Func::Sin)),
            ("sen", Tok::Fn(Func::Sin)),
            ("cos", Tok::Fn(Func::Cos)),
            ("tan", Tok::Fn(Func::Tan)),
            ("exp", Tok::Fn(Func::Exp)),
            ("ln", Tok::Fn(Func::Ln)),
            ("tg", Tok::Fn(Func::Tan)),
            ("pi", Tok::Num(std::f64::consts::PI)),
        ];
        let mut matched = false;
        for (name, tok) in names {
            if rest.starts_with(name) {
                out.push(tok.clone());
                i += name.len();
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }
        if c == 'x' {
            out.push(Tok::X);
            i += 1;
            continue;
        }
        if c == 'y' {
            out.push(Tok::Y);
            i += 1;
            continue;
        }
        if c == 'e' {
            out.push(Tok::Num(std::f64::consts::E));
            i += 1;
            continue;
        }
        let tok = match c {
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            '^' => Tok::Caret,
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            // The game's tokenizer is a regex scanner: characters it does not
            // know (π, Cyrillic, stray punctuation) are dropped, not rejected.
            _ => {
                i += 1;
                continue;
            }
        };
        out.push(tok);
        i += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str, x: f64) -> f64 {
        Expr::parse(s).unwrap().eval_x(x)
    }

    #[test]
    fn basics() {
        assert!((ev("2+3*4", 0.0) - 14.0).abs() < 1e-12);
        assert!((ev("2x", 5.0) - 10.0).abs() < 1e-12);
        assert!((ev("(x-3)^2", 5.0) - 4.0).abs() < 1e-12);
        assert!((ev("-x^2", 3.0) + 9.0).abs() < 1e-12);
        assert!((ev("2^3^2", 0.0) - 512.0).abs() < 1e-9);
        assert!((ev("sin(pi/2)", 0.0) - 1.0).abs() < 1e-12);
        assert!((ev("exp(0-((x-1)^2)/2)", 2.0) - (-0.5f64).exp()).abs() < 1e-12);
        assert!((ev("sqrt(abs(x))", -9.0) - 3.0).abs() < 1e-12);
    }

    #[test]
    fn implicit_mul() {
        assert!((ev("3(x+1)", 2.0) - 9.0).abs() < 1e-12);
        assert!((ev("x sin(x)", 0.0)).abs() < 1e-12);
    }

    #[test]
    fn decimal_comma() {
        // Graphwar's AI writes formulas in the local locale.
        assert!((ev("(2*x)+(-5,41)", 1.0) + 3.41).abs() < 1e-12);
        assert!((ev("(((-0,54)*x)+(-12,73))", 0.0) + 12.73).abs() < 1e-9);
        let e = Expr::parse("28cos((cos((cos(x)*x)))*(-5,41))").unwrap();
        assert!(e.eval_x(0.3).is_finite());
    }

    #[test]
    fn functions_without_parentheses() {
        // The game's tokenizer happily accepts `sin x`, `sqrt 4`, `ln e`.
        assert!((ev("sin x", 0.0)).abs() < 1e-12);
        assert!((ev("cos 0", 0.0) - 1.0).abs() < 1e-12);
        assert!((ev("sqrt 4+1", 0.0) - 3.0).abs() < 1e-12);
        assert!((ev("ln e", 0.0) - 1.0).abs() < 1e-12);
        assert!((ev("2sin x", std::f64::consts::FRAC_PI_2) - 2.0).abs() < 1e-12);
        // A no-parenthesis call binds tighter than `^`: (sin x)^2, not sin(x^2).
        let x = 1.0f64;
        assert!((ev("sin x^2", x) - x.sin().powi(2)).abs() < 1e-12);
        // Implicit multiplication still splits the argument: sin(2)*x.
        assert!((ev("sin 2x", 3.0) - 2.0f64.sin() * 3.0).abs() < 1e-12);
        assert!((ev("sen x", 0.0)).abs() < 1e-12);
        assert!((ev("tg 0", 0.0)).abs() < 1e-12);
    }

    #[test]
    fn unknown_characters_are_skipped() {
        // The game scans with a regex and drops anything it does not know.
        assert!((Expr::parse("x#y").unwrap().eval(2.0, 3.0, 0.0) - 6.0).abs() < 1e-12);
        assert!((ev("sin(x)π", 0.5) - 0.5f64.sin()).abs() < 1e-12);
        // A second apostrophe is dropped, leaving y'.
        assert!((Expr::parse("y''").unwrap().eval(0.0, 0.0, 7.0) - 7.0).abs() < 1e-12);
    }
}
