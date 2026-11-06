// Exact-enough reproduction of Graphwar's shot physics.
//
// The game works in a 770x450 pixel plane.  The mathematical plane is 50 units
// wide, so 1 unit = 15.4 pixels.  Team 2 fires in a mirrored coordinate system;
// Frame hides that detail.

// The simulator entry points take the whole game state explicitly.
#![allow(clippy::too_many_arguments)]

use crate::expr::Expr;

pub const PLANE_LEN: f64 = 770.0;
pub const PLANE_H: f64 = 450.0;
pub const GAME_LEN: f64 = 50.0;
pub const SCALE: f64 = PLANE_LEN / GAME_LEN; // 15.4
pub const SOLDIER_R: f64 = 7.0;
pub const R_GAME: f64 = SOLDIER_R / SCALE;
pub const STEP: f64 = 0.01;
pub const MAX_STEPS: usize = 20_000;
pub const MAX_STEP_DIST2: f64 = 0.001;
pub const MIN_X_STEP: f64 = 0.00001;
pub const EXPLOSION_R: f64 = 12.0;

pub const TEAM1: i32 = 1;
pub const TEAM2: i32 = 2;

#[derive(Clone, Copy, Debug)]
pub struct Circle {
    pub x: f64,
    pub y: f64,
    pub r: f64,
}

impl Circle {
    pub fn new(x: f64, y: f64, r: f64) -> Self {
        Self { x, y, r }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Surface {
    pub black: Vec<Circle>,
    pub white: Vec<Circle>,
}

impl Surface {
    pub fn solid_real(&self, x: f64, y: f64) -> bool {
        if !x.is_finite() || !y.is_finite() {
            return true;
        }
        let ix = x as i32;
        let iy = y as i32;
        if ix < 0 || ix >= PLANE_LEN as i32 || iy < 0 || iy >= PLANE_H as i32 {
            return true;
        }
        let px = ix as f64;
        let py = iy as f64;
        let mut black = false;
        for c in &self.black {
            let dx = px - c.x;
            let dy = py - c.y;
            let rr = c.r + 0.8;
            if dx * dx + dy * dy <= rr * rr {
                black = true;
                break;
            }
        }
        if !black {
            return false;
        }
        for c in &self.white {
            let dx = px - c.x;
            let dy = py - c.y;
            let rr = (c.r - 1.0).max(0.0);
            if dx * dx + dy * dy <= rr * rr {
                return false;
            }
        }
        true
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub team: i32,
}

impl Frame {
    pub fn new(team: i32) -> Self {
        Self { team }
    }

    pub fn to_game(&self, sx: f64, sy: f64) -> (f64, f64) {
        let u = if self.team == TEAM1 {
            (sx - PLANE_LEN / 2.0) / SCALE
        } else {
            (PLANE_LEN / 2.0 - sx) / SCALE
        };
        let v = (PLANE_H / 2.0 - sy) / SCALE;
        (u, v)
    }

    pub fn to_real(&self, u: f64, v: f64) -> (f64, f64) {
        let x = if self.team == TEAM1 {
            PLANE_LEN / 2.0 + SCALE * u
        } else {
            PLANE_LEN / 2.0 - SCALE * u
        };
        let y = PLANE_H / 2.0 - SCALE * v;
        (x, y)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SoldierRef {
    pub player: usize,
    pub soldier: usize,
    pub team: i32,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub player: usize,
    pub soldier: usize,
    pub team: i32,
    pub u: f64,
    pub v: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    Terrain,
    Bounds,
    TooSteep,
    Invalid,
    MaxSteps,
}

#[derive(Clone, Debug)]
pub struct ShotPath {
    pub end_u: f64,
    pub end_v: f64,
    pub end_real: (f64, f64),
    pub reason: EndReason,
    pub hits: Vec<Hit>,
    pub first_block_u: Option<f64>,
    pub first_ally_u: Option<f64>,
    pub samples: Vec<(f64, f64)>, // real pixels
    pub length: f64,
}

impl ShotPath {
    fn new(frame: &Frame, u: f64, v: f64) -> Self {
        Self {
            end_u: u,
            end_v: v,
            end_real: frame.to_real(u, v),
            reason: EndReason::MaxSteps,
            hits: Vec::new(),
            first_block_u: None,
            first_ally_u: None,
            samples: Vec::new(),
            length: 0.0,
        }
    }
}

/// Java's iterative tangent lookup used in normal mode.  The first guess is
/// taken at the soldier centre (exactly like Graphwar), then the muzzle point
/// is refined.
pub fn find_start_angle(f: &dyn Fn(f64) -> f64, x: f64, radius: f64) -> f64 {
    let mut angle = ((f(x + STEP) - f(x)) / STEP).atan();
    let mut error = 10_000.0f64;
    let mut loops = 0;
    while error > std::f64::consts::PI / 360.0 && loops < 100 {
        let fx = x + radius * angle.cos();
        let tangent = (f(fx + STEP) - f(fx)) / STEP;
        let new_angle = tangent.atan();
        error = (new_angle - angle).abs();
        angle = new_angle;
        loops += 1;
    }
    angle
}

fn rk4_start_angle(f: &dyn Fn(f64, f64, f64) -> f64, x: f64, y: f64, radius: f64) -> f64 {
    let mut angle = 0.0f64;
    let h = STEP;
    for _ in 0..100 {
        let fx = x + radius * angle.cos();
        let fy = y + radius * angle.sin();
        let k1 = f(fx, fy, 0.0);
        let k2 = f(fx + 0.5 * h, fy + 0.5 * h * k1, 0.0);
        let k3 = f(fx + 0.5 * h, fy + 0.5 * h * k2, 0.0);
        let k4 = f(fx + h, fy + h * k3, 0.0);
        let next_y = fy + (h / 6.0) * (k1 + 2.0 * k2 + 2.0 * k3 + k4);
        let tangent = (next_y - fy) / h;
        let new_angle = tangent.atan();
        let err = (new_angle - angle).abs();
        angle = new_angle;
        if err <= std::f64::consts::PI / 360.0 {
            break;
        }
    }
    angle
}

struct Walker<'a> {
    frame: Frame,
    surface: &'a Surface,
    soldiers: &'a [SoldierRef],
    shooter_player: usize,
    shooter_soldier: usize,
    shooter_team: i32,
    path: ShotPath,
    last_u: f64,
    last_v: f64,
    count: usize,
}

impl<'a> Walker<'a> {
    fn new(
        frame: Frame,
        surface: &'a Surface,
        soldiers: &'a [SoldierRef],
        shooter_player: usize,
        shooter_soldier: usize,
        shooter_team: i32,
        u0: f64,
        v0: f64,
    ) -> Self {
        let mut path = ShotPath::new(&frame, u0, v0);
        let (rx, ry) = frame.to_real(u0, v0);
        path.samples.push((rx, ry));
        Self {
            frame,
            surface,
            soldiers,
            shooter_player,
            shooter_soldier,
            shooter_team,
            path,
            last_u: u0,
            last_v: v0,
            count: 0,
        }
    }

    /// `Some(true)` when solid (the shot ends), `None` when invalid,
    /// `Some(false)` otherwise.
    fn step(&mut self, u: f64, v: f64) -> Option<bool> {
        self.count += 1;
        let (rx, ry) = self.frame.to_real(u, v);
        let du = u - self.last_u;
        let dv = v - self.last_v;
        self.path.length += (du * du + dv * dv).sqrt();
        self.last_u = u;
        self.last_v = v;
        self.path.end_u = u;
        self.path.end_v = v;
        self.path.end_real = (rx, ry);
        if self.count.is_multiple_of(50) {
            self.path.samples.push((rx, ry));
        }

        if v.is_finite() {
            for s in self.soldiers {
                if s.player == self.shooter_player && s.soldier == self.shooter_soldier {
                    continue;
                }
                let dx = s.x - rx;
                let dy = s.y - ry;
                let inside = dx * dx + dy * dy < SOLDIER_R * SOLDIER_R;
                let fresh = !self
                    .path
                    .hits
                    .iter()
                    .any(|h| h.player == s.player && h.soldier == s.soldier);
                if inside && fresh {
                    let (hu, hv) = self.frame.to_game(s.x, s.y);
                    if s.team == self.shooter_team && self.path.first_ally_u.is_none() {
                        self.path.first_ally_u = Some(hu);
                    }
                    self.path.hits.push(Hit {
                        player: s.player,
                        soldier: s.soldier,
                        team: s.team,
                        u: hu,
                        v: hv,
                    });
                }
            }
        }

        if !v.is_finite() {
            self.path.reason = EndReason::Invalid;
            return None;
        }

        if self.surface.solid_real(rx, ry) {
            let in_bounds = (0.0..PLANE_LEN).contains(&rx) && (0.0..PLANE_H).contains(&ry);
            self.path.reason = if in_bounds {
                EndReason::Terrain
            } else {
                EndReason::Bounds
            };
            if self.path.first_block_u.is_none() {
                self.path.first_block_u = Some(u);
            }
            return Some(true);
        }
        Some(false)
    }

    fn finish(&mut self, reason: EndReason) -> ShotPath {
        self.path.reason = reason;
        self.path.samples.push(self.path.end_real);
        self.path.clone()
    }
}

pub fn simulate_normal_fn(
    f: &dyn Fn(f64) -> f64,
    frame: Frame,
    shooter_real: (f64, f64),
    surface: &Surface,
    soldiers: &[SoldierRef],
    shooter_player: usize,
    shooter_soldier: usize,
    shooter_team: i32,
) -> ShotPath {
    let (x0, y0) = frame.to_game(shooter_real.0, shooter_real.1);
    let angle = find_start_angle(f, x0, R_GAME);
    let (mut x_prev, mut y_prev) = if angle.is_finite() {
        (x0 + R_GAME * angle.cos(), y0 + R_GAME * angle.sin())
    } else {
        (x0, y0)
    };
    let offset = -f(x_prev) + y_prev;

    let mut walker = Walker::new(
        frame,
        surface,
        soldiers,
        shooter_player,
        shooter_soldier,
        shooter_team,
        x_prev,
        y_prev,
    );

    for _ in 1..MAX_STEPS {
        let mut step = STEP;
        let mut x = x_prev + step;
        let mut y = f(x) + offset;
        let mut too_steep = false;
        loop {
            let dx = x - x_prev;
            let dy = y - y_prev;
            if dx * dx + dy * dy <= MAX_STEP_DIST2 {
                break;
            }
            if x - x_prev > MIN_X_STEP {
                step /= 2.0;
                x = x_prev + step;
                y = f(x) + offset;
            } else {
                too_steep = true;
                break;
            }
        }
        if too_steep {
            if let Some(done) = walker.step(x, y) {
                if done {
                    break;
                }
            }
            return walker.finish(EndReason::TooSteep);
        }
        match walker.step(x, y) {
            Some(true) => break,
            None => break,
            Some(false) => {}
        }
        x_prev = x;
        y_prev = y;
    }
    if walker.path.reason == EndReason::MaxSteps {
        walker.finish(EndReason::MaxSteps)
    } else {
        walker.path.samples.push(walker.path.end_real);
        walker.path.clone()
    }
}

pub fn simulate_normal_expr(
    expr: &Expr,
    frame: Frame,
    shooter_real: (f64, f64),
    surface: &Surface,
    soldiers: &[SoldierRef],
    shooter_player: usize,
    shooter_soldier: usize,
    shooter_team: i32,
) -> ShotPath {
    let f = |x: f64| expr.eval_x(x);
    simulate_normal_fn(
        &f,
        frame,
        shooter_real,
        surface,
        soldiers,
        shooter_player,
        shooter_soldier,
        shooter_team,
    )
}

pub fn simulate_first_order_fn(
    f: &dyn Fn(f64, f64, f64) -> f64,
    frame: Frame,
    shooter_real: (f64, f64),
    surface: &Surface,
    soldiers: &[SoldierRef],
    shooter_player: usize,
    shooter_soldier: usize,
    shooter_team: i32,
) -> ShotPath {
    let (x0, y0) = frame.to_game(shooter_real.0, shooter_real.1);
    let angle = rk4_start_angle(f, x0, y0, R_GAME);
    let (mut x_prev, mut y_prev) = if angle.is_finite() {
        (x0 + R_GAME * angle.cos(), y0 + R_GAME * angle.sin())
    } else {
        (x0, y0)
    };
    let mut walker = Walker::new(
        frame,
        surface,
        soldiers,
        shooter_player,
        shooter_soldier,
        shooter_team,
        x_prev,
        y_prev,
    );

    for _ in 1..MAX_STEPS {
        let h = STEP;
        let k1 = f(x_prev, y_prev, 0.0);
        let k2 = f(x_prev + 0.5 * h, y_prev + 0.5 * h * k1, 0.0);
        let k3 = f(x_prev + 0.5 * h, y_prev + 0.5 * h * k2, 0.0);
        let k4 = f(x_prev + h, y_prev + h * k3, 0.0);
        let mut hh = h;
        let mut x = x_prev + hh;
        let mut y = y_prev + (hh / 6.0) * (k1 + 2.0 * k2 + 2.0 * k3 + k4);
        let mut too_steep = false;
        loop {
            let dx = x - x_prev;
            let dy = y - y_prev;
            if dx * dx + dy * dy <= MAX_STEP_DIST2 {
                break;
            }
            if x - x_prev > MIN_X_STEP {
                hh /= 2.0;
                let k1 = f(x_prev, y_prev, 0.0);
                let k2 = f(x_prev + 0.5 * hh, y_prev + 0.5 * hh * k1, 0.0);
                let k3 = f(x_prev + 0.5 * hh, y_prev + 0.5 * hh * k2, 0.0);
                let k4 = f(x_prev + hh, y_prev + hh * k3, 0.0);
                x = x_prev + hh;
                y = y_prev + (hh / 6.0) * (k1 + 2.0 * k2 + 2.0 * k3 + k4);
            } else {
                too_steep = true;
                break;
            }
        }
        if too_steep {
            walker.step(x, y);
            return walker.finish(EndReason::TooSteep);
        }
        match walker.step(x, y) {
            Some(true) => break,
            None => break,
            Some(false) => {}
        }
        x_prev = x;
        y_prev = y;
    }
    if walker.path.reason == EndReason::MaxSteps {
        walker.finish(EndReason::MaxSteps)
    } else {
        walker.path.samples.push(walker.path.end_real);
        walker.path.clone()
    }
}

pub fn simulate_first_order(
    expr: &Expr,
    frame: Frame,
    shooter_real: (f64, f64),
    surface: &Surface,
    soldiers: &[SoldierRef],
    shooter_player: usize,
    shooter_soldier: usize,
    shooter_team: i32,
) -> ShotPath {
    simulate_first_order_fn(
        &|x, y, _dy| expr.eval(x, y, 0.0),
        frame,
        shooter_real,
        surface,
        soldiers,
        shooter_player,
        shooter_soldier,
        shooter_team,
    )
}

pub fn simulate_second_order_fn(
    f: &dyn Fn(f64, f64, f64) -> f64,
    angle: f64,
    frame: Frame,
    shooter_real: (f64, f64),
    surface: &Surface,
    soldiers: &[SoldierRef],
    shooter_player: usize,
    shooter_soldier: usize,
    shooter_team: i32,
) -> ShotPath {
    let (x0, y0) = frame.to_game(shooter_real.0, shooter_real.1);
    let (mut x_prev, mut y_prev) = if angle.is_finite() {
        (x0 + R_GAME * angle.cos(), y0 + R_GAME * angle.sin())
    } else {
        (x0, y0)
    };
    let mut dy = if angle.is_finite() { angle.tan() } else { 0.0 };
    let mut walker = Walker::new(
        frame,
        surface,
        soldiers,
        shooter_player,
        shooter_soldier,
        shooter_team,
        x_prev,
        y_prev,
    );

    for _ in 1..MAX_STEPS {
        let h = STEP;
        let mut x1 = x_prev;
        let mut y1 = y_prev;
        let mut y2 = dy;
        let k11 = y2;
        let k12 = f(x1, y1, y2);
        x1 = x_prev + h / 2.0;
        y1 = y_prev + (h / 2.0) * k11;
        y2 = dy + (h / 2.0) * k12;
        let k21 = y2;
        let k22 = f(x1, y1, y2);
        y1 = y_prev + (h / 2.0) * k21;
        y2 = dy + (h / 2.0) * k22;
        let k31 = y2;
        let k32 = f(x1, y1, y2);
        x1 = x_prev + h;
        y1 = y_prev + h * k31;
        y2 = dy + h * k32;
        let k41 = y2;
        let k42 = f(x1, y1, y2);

        let mut hh = h;
        let mut x = x_prev + hh;
        let mut y = y_prev + (hh / 6.0) * (k11 + 2.0 * k21 + 2.0 * k31 + k41);
        let mut d = dy + (hh / 6.0) * (k12 + 2.0 * k22 + 2.0 * k32 + k42);
        let mut too_steep = false;
        loop {
            let dx = x - x_prev;
            let dyv = y - y_prev;
            if dx * dx + dyv * dyv <= MAX_STEP_DIST2 {
                break;
            }
            if x - x_prev > MIN_X_STEP {
                hh /= 2.0;
                let mut xx = x_prev;
                let mut yy = y_prev;
                let mut dd = dy;
                let a1 = dd;
                let b1 = f(xx, yy, dd);
                xx = x_prev + hh / 2.0;
                yy = y_prev + (hh / 2.0) * a1;
                dd = dy + (hh / 2.0) * b1;
                let a2 = dd;
                let b2 = f(xx, yy, dd);
                yy = y_prev + (hh / 2.0) * a2;
                dd = dy + (hh / 2.0) * b2;
                let a3 = dd;
                let b3 = f(xx, yy, dd);
                xx = x_prev + hh;
                yy = y_prev + hh * a3;
                dd = dy + hh * b3;
                let a4 = dd;
                let b4 = f(xx, yy, dd);
                x = x_prev + hh;
                y = y_prev + (hh / 6.0) * (a1 + 2.0 * a2 + 2.0 * a3 + a4);
                d = dy + (hh / 6.0) * (b1 + 2.0 * b2 + 2.0 * b3 + b4);
            } else {
                too_steep = true;
                break;
            }
        }
        if too_steep {
            walker.step(x, y);
            return walker.finish(EndReason::TooSteep);
        }
        match walker.step(x, y) {
            Some(true) => break,
            None => break,
            Some(false) => {}
        }
        x_prev = x;
        y_prev = y;
        dy = d;
    }
    if walker.path.reason == EndReason::MaxSteps {
        walker.finish(EndReason::MaxSteps)
    } else {
        walker.path.samples.push(walker.path.end_real);
        walker.path.clone()
    }
}

pub fn simulate_second_order(
    expr: &Expr,
    angle: f64,
    frame: Frame,
    shooter_real: (f64, f64),
    surface: &Surface,
    soldiers: &[SoldierRef],
    shooter_player: usize,
    shooter_soldier: usize,
    shooter_team: i32,
) -> ShotPath {
    simulate_second_order_fn(
        &|x, y, dy| expr.eval(x, y, dy),
        angle,
        frame,
        shooter_real,
        surface,
        soldiers,
        shooter_player,
        shooter_soldier,
        shooter_team,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn straight_line_hits() {
        let frame = Frame::new(TEAM1);
        let shooter = (100.0, 225.0);
        let target = SoldierRef {
            player: 1,
            soldier: 0,
            team: TEAM2,
            x: 500.0,
            y: 225.0,
        };
        let surface = Surface::default();
        let f = |_x: f64| 0.0;
        let p = simulate_normal_fn(&f, frame, shooter, &surface, &[target], 0, 0, TEAM1);
        assert!(p.hits.iter().any(|h| h.player == 1 && h.soldier == 0));
        assert!(p.end_real.0 > 700.0);
    }

    #[test]
    fn mirror_frame_hits() {
        let frame = Frame::new(TEAM2);
        let shooter = (670.0, 225.0);
        let target = SoldierRef {
            player: 1,
            soldier: 0,
            team: TEAM1,
            x: 270.0,
            y: 225.0,
        };
        let surface = Surface::default();
        let f = |_x: f64| 0.0;
        let p = simulate_normal_fn(&f, frame, shooter, &surface, &[target], 0, 0, TEAM2);
        assert!(p.hits.iter().any(|h| h.player == 1));
    }

    #[test]
    fn wall_stops_shot() {
        let frame = Frame::new(TEAM1);
        let shooter = (100.0, 225.0);
        let surface = Surface {
            black: vec![Circle::new(400.0, 225.0, 40.0)],
            white: vec![],
        };
        let f = |_x: f64| 0.0;
        let p = simulate_normal_fn(&f, frame, shooter, &surface, &[], 0, 0, TEAM1);
        assert_eq!(p.reason, EndReason::Terrain);
        assert!(p.end_real.0 < 400.0);
    }
}
