//! Mission generation and the beam search that fits a curve through it.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::game::World;
use crate::protocol::{FST_ODE, SND_ODE};
use crate::rng::Rng;
use crate::sim::{
    simulate_first_order_fn, simulate_normal_fn, simulate_second_order_fn, EndReason, Frame,
    ShotPath, SoldierRef, PLANE_H, PLANE_LEN, R_GAME, SCALE, SOLDIER_R,
};

use super::breach::breach_plan;
use super::geometry::{astar, route_distance, GuideGrid};
use super::rbf::{fit_rbf, fit_rbf_slope, Rbf};
use super::render::formula_for;
use super::worker::{plan_wide, random_points};
use super::{ShotPlan, Target};

pub(super) fn median_gap(targets: &[Target]) -> f64 {
    if targets.len() < 2 {
        return 3.0;
    }
    let mut xs: Vec<f64> = targets.iter().map(|t| t.u).collect();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut gaps = Vec::new();
    for w in xs.windows(2) {
        if w[1] - w[0] > 0.2 {
            gaps.push(w[1] - w[0]);
        }
    }
    if gaps.is_empty() {
        return 3.0;
    }
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    gaps[gaps.len() / 2]
}

/// Build a curve through the muzzle and target points.  The muzzle position
/// depends on the initial tangent, so run a few fixed-point iterations.
/// With `fixed_angle` (second-order mode) the muzzle and slope are dictated by
/// the client, so the curve is fitted with an imposed derivative instead.
pub fn build_curve(
    targets: &[(f64, f64)],
    cx: f64,
    cy: f64,
    sigma: f64,
    fixed_angle: Option<f64>,
) -> Option<(Rbf, f64, f64, f64)> {
    if let Some(theta) = fixed_angle {
        let a = theta.clamp(-1.45, 1.45);
        let mx = cx + R_GAME * a.cos();
        let my = cy + R_GAME * a.sin();
        let mut pts = Vec::with_capacity(targets.len() + 1);
        pts.push((mx, my));
        pts.extend_from_slice(targets);
        let rbf = fit_rbf_slope(&pts, sigma, Some((mx, a.tan())))?;
        return Some((rbf, a, mx, my));
    }
    let mut mx = cx + R_GAME;
    let mut my = cy;
    for _ in 0..8 {
        let mut pts = Vec::with_capacity(targets.len() + 1);
        pts.push((mx, my));
        pts.extend_from_slice(targets);
        let rbf = fit_rbf(&pts, sigma)?;
        let d = rbf.derivative(mx);
        let na = d.atan().clamp(-1.45, 1.45);
        let nmx = cx + R_GAME * na.cos();
        let nmy = cy + R_GAME * na.sin();
        let done = (nmx - mx).abs() < 1e-10 && (nmy - my).abs() < 1e-10;
        mx = nmx;
        my = nmy;
        if done {
            break;
        }
    }
    let mut pts = Vec::with_capacity(targets.len() + 1);
    pts.push((mx, my));
    pts.extend_from_slice(targets);
    let rbf = fit_rbf(&pts, sigma)?;
    let angle = rbf.derivative(mx).atan().clamp(-1.45, 1.45);
    Some((rbf, angle, mx, my))
}

#[derive(Clone)]
pub(super) struct Candidate {
    pub(super) rbf: Rbf,
    pub(super) angle: f64,
    pub(super) path: ShotPath,
    pub(super) score: f64,
    pub(super) kills: usize,
    pub(super) ally_hits: usize,
    pub(super) selected_hits: usize,
    pub(super) pts: Vec<(f64, f64)>,
    pub(super) target_pts: Vec<(f64, f64)>,
}

pub(super) struct PlanContext<'a> {
    pub(super) world: &'a World,
    pub(super) frame: Frame,
    pub(super) shooter_real: (f64, f64),
    pub(super) cx: f64,
    pub(super) cy: f64,
    pub(super) shooter_player: usize,
    pub(super) shooter_soldier: usize,
    pub(super) team: i32,
    pub(super) refs: Vec<SoldierRef>,
    pub(super) farthest: f64,
    pub(super) fixed_angle: Option<f64>,
    /// Hard stop for any further search; the game gives a minute per turn.
    pub(super) deadline: Instant,
}

impl<'a> PlanContext<'a> {
    /// Distance to the nearest remaining solid in game units, negative inside
    /// rock.  A point inside rock but also inside an old crater is open space:
    /// its walls are the crater rim.
    fn solid_clearance(&self, u: f64, v: f64) -> f64 {
        let (rx, ry) = self.frame.to_real(u, v);
        if rx < 4.0 || rx > PLANE_LEN - 4.0 || ry < 4.0 || ry > PLANE_H - 4.0 {
            return -10.0;
        }
        let s = &self.world.surface;
        let mut m = f64::INFINITY;
        for c in &s.black {
            let dist = ((rx - c.x).powi(2) + (ry - c.y).powi(2)).sqrt();
            let mut d = dist - c.r;
            if d < 0.0 {
                // Inside a rock, unless an earlier explosion carved this spot
                // out.  Then the nearest solid is the rim of the crater, and
                // the route is allowed through the hole.
                let mut hole = 0.0f64;
                for w in &s.white {
                    let dw = ((rx - w.x).powi(2) + (ry - w.y).powi(2)).sqrt();
                    if dw < w.r {
                        hole = hole.max(w.r - dw);
                    }
                }
                if hole > 0.0 {
                    d = hole.min(-d);
                }
            }
            m = m.min(d / SCALE);
        }
        if !m.is_finite() {
            20.0
        } else {
            m
        }
    }

    /// Is this point close enough to an explosion crater for its rim to
    /// matter?
    fn near_crater(&self, u: f64, v: f64) -> bool {
        let (rx, ry) = self.frame.to_real(u, v);
        self.world.surface.white.iter().any(|w| {
            let dx = rx - w.x;
            let dy = ry - w.y;
            dx * dx + dy * dy < (w.r + 6.0).powi(2)
        })
    }

    fn clearance(&self, u: f64, v: f64) -> f64 {
        let (rx, ry) = self.frame.to_real(u, v);
        let mut m = self.solid_clearance(u, v);
        for s in &self.refs {
            if s.team != self.team {
                continue;
            }
            if s.player == self.shooter_player && s.soldier == self.shooter_soldier {
                continue;
            }
            let d = ((rx - s.x).powi(2) + (ry - s.y).powi(2)).sqrt() - SOLDIER_R;
            m = m.min(d / SCALE);
        }
        m
    }

    fn evaluate(&self, rbf: &Rbf, angle: f64, target_pts: &[(f64, f64)]) -> Candidate {
        let path = match self.world.mode {
            FST_ODE => simulate_first_order_fn(
                &|x, _y, _d| rbf.derivative(x),
                self.frame,
                self.shooter_real,
                &self.world.surface,
                &self.refs,
                self.shooter_player,
                self.shooter_soldier,
                self.team,
            ),
            SND_ODE => simulate_second_order_fn(
                &|x, _y, _d| rbf.second(x),
                angle,
                self.frame,
                self.shooter_real,
                &self.world.surface,
                &self.refs,
                self.shooter_player,
                self.shooter_soldier,
                self.team,
            ),
            _ => simulate_normal_fn(
                &|x| rbf.eval(x),
                self.frame,
                self.shooter_real,
                &self.world.surface,
                &self.refs,
                self.shooter_player,
                self.shooter_soldier,
                self.team,
            ),
        };
        let mut kills = 0usize;
        let mut allies = 0usize;
        for h in &path.hits {
            if let Some(p) = self.world.players.get(h.player) {
                if p.team != self.team {
                    kills += 1;
                } else if !(h.player == self.shooter_player && h.soldier == self.shooter_soldier) {
                    allies += 1;
                }
            }
        }
        let selected_hits = target_pts
            .iter()
            .filter(|(u, v)| {
                path.hits.iter().any(|h| {
                    self.refs
                        .iter()
                        .find(|s| s.player == h.player && s.soldier == h.soldier)
                        .map(|s| {
                            let (hu, hv) = self.frame.to_game(s.x, s.y);
                            (hu - u).abs() < 0.6 && (hv - v).abs() < 0.6
                        })
                        .unwrap_or(false)
                })
            })
            .count();
        let mut score = kills as f64 * 1_000_000.0 - allies as f64 * 2_000_000_000.0
            + selected_hits as f64 * 30_000.0
            - target_pts.len() as f64 * 250.0
            - path.length * 1.5;
        // Same kill count: keep the curve that carries further, it is closer
        // to the soldiers behind the wall.
        score += path.end_u.min(self.farthest) * 60.0;
        // Graded near misses: shaving a soldier beats sailing through empty sky.
        let mut graze = 0.0;
        for &(tu, tv) in target_pts {
            let mut best = f64::INFINITY;
            for &(rx, ry) in &path.samples {
                let (su, sv) = self.frame.to_game(rx, ry);
                let d = ((su - tu).powi(2) + (sv - tv).powi(2)).sqrt();
                if d < best {
                    best = d;
                }
            }
            graze += best.min(4.0);
        }
        score -= graze * 1_500.0;
        // A crater rim threaded with a pixel to spare survives our integrator
        // but not always the game's pixel grid.  Soft preference only.
        let mut tight = 0.0f64;
        if path.samples.len() > 6 {
            for &(rx, ry) in &path.samples[4..path.samples.len() - 2] {
                let (su, sv) = self.frame.to_game(rx, ry);
                if !self.near_crater(su, sv) {
                    continue;
                }
                let gap = self.solid_clearance(su, sv);
                if gap < 0.15 {
                    tight = tight.max(0.15 - gap);
                }
            }
        }
        score -= tight * 30_000.0;
        if path.reason == EndReason::Terrain {
            // Stopping in rock must lose to a curve that carries on.
            score -= 3_000.0;
        }
        if path.reason == EndReason::Bounds && path.end_u < self.farthest - 0.2 {
            score -= 500.0;
        }
        Candidate {
            rbf: rbf.clone(),
            angle,
            path,
            score,
            kills,
            ally_hits: allies,
            selected_hits,
            pts: Vec::new(),
            target_pts: target_pts.to_vec(),
        }
    }

    /// Vertical position of the simulated curve at game-x `u`.
    fn curve_v_at(&self, cand: &Candidate, u: f64) -> f64 {
        let mut best_d = f64::INFINITY;
        let mut best_v = self.cy;
        for &(rx, ry) in &cand.path.samples {
            let (su, sv) = self.frame.to_game(rx, ry);
            let d = (su - u).abs();
            if d < best_d {
                best_d = d;
                best_v = sv;
            }
        }
        best_v
    }

    /// Waypoint sets that bend the curve around whatever stopped it: three
    /// points on a slightly larger circle per blocker, plus a vertical scan
    /// in front of the stop as a fallback for ally hits and plane edges.
    fn detour_sets(
        &self,
        cand: &Candidate,
        at_u: f64,
        target_pts: &[(f64, f64)],
    ) -> Vec<Vec<(f64, f64)>> {
        let ok = |u: f64, v: f64| {
            (-13.2..=13.2).contains(&v)
                && u >= self.cx + 0.9
                && u <= self.farthest + 1.0
                // Waypoints in the same column as a soldier would make the
                // spline nearly vertical there: the fit blows up and the
                // curve between the two centres dives off the plane.
                && !target_pts.iter().any(|p| (p.0 - u).abs() < 0.35)
        };
        let mut out: Vec<Vec<(f64, f64)>> = Vec::new();
        // Obstacles between the muzzle and the last target, ordered by how
        // close they are to the current curve.
        let mut near: Vec<(f64, f64, f64, f64)> = Vec::new(); // (dist, u, v, r)
        let mut obstacles: Vec<(f64, f64, f64)> = Vec::new();
        for c in &self.world.surface.black {
            let (uc, vc) = self.frame.to_game(c.x, c.y);
            obstacles.push((uc, vc, (c.r + SOLDIER_R) / SCALE));
        }
        for s in &self.refs {
            if s.team != self.team
                || (s.player == self.shooter_player && s.soldier == self.shooter_soldier)
            {
                continue;
            }
            let (uc, vc) = self.frame.to_game(s.x, s.y);
            obstacles.push((uc, vc, SOLDIER_R / SCALE + 0.8));
        }
        for (uc, vc, r) in obstacles {
            if uc + r < self.cx + 0.5 || uc - r > self.farthest + 1.0 {
                continue;
            }
            let cv = self.curve_v_at(cand, uc);
            let dist = (vc - cv).abs() - r;
            if dist < 5.0 {
                near.push((dist, uc, vc, r));
            }
        }
        near.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        near.truncate(2);
        for &(_, uc, vc, r) in &near {
            for &clear in &[0.9, 2.4] {
                let rr = r + clear;
                for &side in &[1.0, -1.0] {
                    let mut set = Vec::new();
                    // Top (or bottom) half of the clearance circle: 180°, 135°,
                    // 90°, 45°, 0°.
                    let angles = [
                        std::f64::consts::PI,
                        3.0 * std::f64::consts::FRAC_PI_4,
                        std::f64::consts::FRAC_PI_2,
                        std::f64::consts::FRAC_PI_4,
                        0.0,
                    ];
                    for a in angles {
                        let u = uc + rr * a.cos();
                        let v = vc + side * rr * a.sin();
                        if ok(u, v) {
                            set.push((u, v));
                        }
                    }
                    // Two valid points still bend the curve; the evaluation
                    // throws away the ones that do not.
                    if set.len() >= 2 {
                        out.push(set);
                    }
                    // Straight over the top: one point directly above (or
                    // below) the blocker is the cheapest way past a rock.
                    let v = vc + side * rr;
                    if ok(uc, v) {
                        out.push(vec![(uc, v)]);
                    }
                }
            }
        }
        let xg = at_u - 0.55;
        if xg >= self.cx + 0.9 {
            let cur = self.curve_v_at(cand, xg);
            let mut opts: Vec<(f64, f64)> = Vec::new();
            let mut v = -13.0;
            while v <= 13.0 {
                if self.clearance(xg, v) > 0.8 {
                    opts.push(((v - cur).abs(), v));
                }
                v += 0.5;
            }
            opts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            for (_, v) in opts.into_iter().take(3) {
                if ok(xg, v) {
                    out.push(vec![(xg, v)]);
                }
            }
        }
        out.truncate(12);
        out
    }

    fn issue_u(&self, c: &Candidate) -> Option<f64> {
        if c.ally_hits > 0 {
            return c.path.first_ally_u;
        }
        if c.selected_hits < c.target_pts.len() || c.path.end_u < self.farthest - 0.2 {
            if let Some(u) = c.path.first_block_u {
                if u < self.farthest - 0.1 {
                    return Some(u);
                }
            }
            if c.path.end_u < self.farthest - 0.2 {
                return Some(c.path.end_u.max(self.cx + 1.0));
            }
        }
        None
    }
}

fn target_key(t: &Target) -> (usize, usize) {
    (t.player, t.soldier)
}

/// Grow the mission into control points.  Two soldiers stacked on the same
/// vertical line cannot both be interpolated by y(x), so the duplicate is
/// dropped; a second soldier further away in v is kept, the resulting
/// near-vertical wiggle can catch both.
pub(super) fn mission_points(mission: &[Target]) -> Vec<(f64, f64)> {
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for t in mission {
        let dup = pts
            .last()
            .map(|p| (p.0 - t.u).abs() < 0.32 && (p.1 - t.v).abs() < 2.6)
            .unwrap_or(false);
        if !dup {
            pts.push((t.u, t.v));
        }
    }
    pts
}

/// Same points, approached from slightly above or below the soldier centre.
/// The offset stays inside the soldier's collision radius, so the shot still
/// kills, but the final stretch of the curve comes in at a different angle.
pub(super) fn shifted_points(mission: &[Target], dv: f64) -> Vec<(f64, f64)> {
    mission_points(mission)
        .into_iter()
        .map(|(u, v)| (u, v + dv))
        .collect()
}

impl<'a> PlanContext<'a> {
    /// A* cost grids: the plain one plus deliberately different preferences
    /// (hug the top, hug the bottom, avoid walls, cut corners tighter).  On
    /// boards with more than one way through, these give genuinely different
    /// routes, which is what makes cheap predictions worth running.
    pub(super) fn guide_grids(&self, want: usize) -> Vec<GuideGrid> {
        let du: f64 = 0.4;
        let u0 = self.cx + 0.3;
        let u1 = self.farthest + 1.0;
        let v0 = -13.3;
        let v1 = 13.3;
        let nu = (((u1 - u0) / du).ceil() as usize) + 1;
        let nv = (((v1 - v0) / du).ceil() as usize) + 1;
        if nu * nv > 30_000 || nu < 3 || nv < 3 {
            return Vec::new();
        }
        let mut blocked = vec![false; nu * nv];
        let mut base = vec![0.0f32; nu * nv];
        for j in 0..nv {
            for i in 0..nu {
                let u = u0 + i as f64 * du;
                let v = v0 + j as f64 * du;
                let cl = self.clearance(u, v);
                let k = j * nu + i;
                blocked[k] = cl < 0.12;
                if cl < 1.2 {
                    base[k] = ((1.2 - cl).max(0.0) * 1.5) as f32;
                }
            }
        }
        let mk = |penalty: Vec<f32>| GuideGrid {
            du,
            u0,
            v0,
            nu,
            nv,
            blocked: blocked.clone(),
            penalty,
        };
        let mut out = vec![mk(base.clone())];
        if want > 1 {
            // sign = +1 makes the lower half expensive, sign = -1 the upper.
            for sign in [1.0f64, -1.0] {
                let mut p = base.clone();
                for j in 0..nv {
                    let v = v0 + j as f64 * du;
                    if sign * v < 0.0 {
                        let extra = (-sign * v) as f32 * 0.06;
                        for i in 0..nu {
                            p[j * nu + i] += extra;
                        }
                    }
                }
                out.push(mk(p));
            }
            out.push(mk(base.iter().map(|p| p * 2.5).collect()));
            out.push(mk(base.iter().map(|p| p * 0.25).collect()));
        }
        out.truncate(want.min(out.len()));
        out
    }

    /// Route through the mission's soldiers on the given cost grid.
    pub(super) fn guide_route_on(
        &self,
        grid: &GuideGrid,
        mission: &[Target],
    ) -> Option<Vec<(f64, f64)>> {
        let targets = mission_points(mission);
        if targets.is_empty() || targets[0].0 < self.cx + 1.0 {
            return None;
        }
        let mut route = vec![(self.cx + 0.7, self.cy)];
        route.extend(targets);
        let mut full: Vec<(f64, f64)> = Vec::new();
        for w in route.windows(2) {
            let seg = astar(grid, w[0], w[1])?;
            if full.is_empty() {
                full = seg;
            } else {
                full.extend(seg.into_iter().skip(1));
            }
        }
        Some(full)
    }

    /// The plain route, used by the fast single-shot planner.
    fn guide_route(&self, mission: &[Target]) -> Option<Vec<(f64, f64)>> {
        let grids = self.guide_grids(1);
        self.guide_route_on(grids.first()?, mission)
    }

    /// Up to `want` distinct routes through the mission, cheapest first.
    pub(super) fn guide_routes(
        &self,
        mission: &[Target],
        grids: &[GuideGrid],
        want: usize,
    ) -> Vec<Vec<(f64, f64)>> {
        let mut out: Vec<Vec<(f64, f64)>> = Vec::new();
        for g in grids {
            let Some(r) = self.guide_route_on(g, mission) else {
                continue;
            };
            if r.len() < 2 {
                continue;
            }
            if out.iter().any(|q| route_distance(&r, q) < 0.6) {
                continue;
            }
            out.push(r);
            if out.len() >= want {
                break;
            }
        }
        out
    }
}

/// Turn a raw guide into spline control points: targets exactly, the route
/// between them sampled every so often, always with distinct columns.
/// `wobble` shifts the free points perpendicular to the route, alternating
/// sides — another family of curves without a new A* run.
pub(super) fn guide_points(
    ctx: &PlanContext,
    guide: &[(f64, f64)],
    targets: &[(f64, f64)],
    wobble: f64,
) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    let mut last_u = ctx.cx + 0.6;
    let mut k = 0usize;
    for &(tu, tv) in targets {
        for (idx, &(gu, gv)) in guide.iter().enumerate() {
            if gu <= last_u + 0.3 {
                continue;
            }
            if gu >= tu - 0.32 {
                break;
            }
            if gv.abs() > 14.4 {
                continue;
            }
            if out.last().map(|p| gu - p.0 < 0.05).unwrap_or(false) {
                continue;
            }
            let mut v = gv;
            if wobble != 0.0 {
                let prev = guide[idx.saturating_sub(1)];
                let next = guide[(idx + 1).min(guide.len() - 1)];
                let (du, dv) = (next.0 - prev.0, next.1 - prev.1);
                let len = (du * du + dv * dv).sqrt().max(1e-9);
                let side = if k.is_multiple_of(2) { 1.0 } else { -1.0 };
                v += wobble * side * (du / len);
                if v.abs() > 13.2 {
                    continue;
                }
            }
            out.push((gu, v));
            k += 1;
        }
        while out.last().map(|p| tu - p.0 < 0.28).unwrap_or(false) {
            out.pop();
        }
        out.push((tu, tv));
        last_u = tu;
    }
    out
}

pub(super) fn valid_points(ctx: &PlanContext, pts: &[(f64, f64)], targets: &[(f64, f64)]) -> bool {
    for (i, p) in pts.iter().enumerate() {
        if p.1 < -14.4 || p.1 > 14.4 || p.0 < ctx.cx + 0.9 || p.0 > ctx.farthest + 1.0 {
            return false;
        }
        if i > 0 && p.0 - pts[i - 1].0 < 0.26 {
            return false;
        }
        let is_target = targets
            .iter()
            .any(|t| (t.0 - p.0).abs() < 1e-6 && (t.1 - p.1).abs() < 1e-6);
        if !is_target
            && targets
                .iter()
                .any(|t| (t.0 - p.0).abs() < 0.26 && (t.1 - p.1).abs() > 0.01)
        {
            return false;
        }
    }
    true
}

/// Nudge every free waypoint around and keep the moves that raise the score.
/// This is what turns "almost squeezed through" into "squeezed through".
pub(super) fn refine(
    ctx: &PlanContext,
    seed: &Candidate,
    sigma: f64,
    targets: &[(f64, f64)],
    passes: usize,
) -> Candidate {
    let mut cur = seed.clone();
    let offsets: [(f64, f64); 8] = [
        (0.0, 0.35),
        (0.0, -0.35),
        (0.35, 0.0),
        (-0.35, 0.0),
        (0.35, 0.35),
        (-0.35, 0.35),
        (0.35, -0.35),
        (-0.35, -0.35),
    ];
    for _ in 0..passes {
        let mut moved = false;
        for i in 0..cur.pts.len() {
            let p = cur.pts[i];
            let is_target = targets
                .iter()
                .any(|t| (t.0 - p.0).abs() < 1e-6 && (t.1 - p.1).abs() < 1e-6);
            if is_target {
                continue;
            }
            for &(du, dv) in &offsets {
                let mut tp = cur.pts.clone();
                tp[i] = (p.0 + du, p.1 + dv);
                if !valid_points(ctx, &tp, targets) {
                    continue;
                }
                if let Some(c) = make_candidate(ctx, &tp, sigma) {
                    if c.score > cur.score + 1.0 {
                        cur = c;
                        moved = true;
                    }
                }
            }
        }
        if !moved {
            break;
        }
    }
    cur
}

/// Force an already flying curve through more soldiers, one at a time.  The
/// formula grows only while an extra bell buys a kill or a clearly better path.
pub(super) fn grow_targets(
    ctx: &PlanContext,
    seed: &Candidate,
    sigma: f64,
    targets: &[Target],
) -> Option<Candidate> {
    let all: Vec<(f64, f64)> = targets.iter().map(|t| (t.u, t.v)).collect();
    let mut cur = seed.clone();
    for _ in 0..2 {
        let mut best: Option<Candidate> = None;
        for t in targets {
            if cur
                .path
                .hits
                .iter()
                .any(|h| h.player == t.player && h.soldier == t.soldier)
            {
                continue;
            }
            if cur
                .pts
                .iter()
                .any(|p| (p.0 - t.u).abs() < 1e-6 && (p.1 - t.v).abs() < 1e-6)
            {
                continue;
            }
            let mut tp = cur.pts.clone();
            tp.push((t.u, t.v));
            tp.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            if !valid_points(ctx, &tp, &all) {
                continue;
            }
            let Some(c) = make_candidate(ctx, &tp, sigma) else {
                continue;
            };
            let better =
                c.kills > cur.kills || (c.kills == cur.kills && c.score > cur.score + 1_000.0);
            if better && best.as_ref().map(|b| c.score > b.score).unwrap_or(true) {
                best = Some(c);
            }
        }
        match best {
            Some(c) => cur = c,
            None => break,
        }
    }
    if cur.kills > seed.kills || cur.score > seed.score + 1_000.0 {
        Some(cur)
    } else {
        None
    }
}

/// Build and score one curve through `tp`.  Shared by the beam search, the
/// guide route and the refinement step so all of them judge the same way.
pub(super) fn make_candidate(
    ctx: &PlanContext,
    tp: &[(f64, f64)],
    sigma: f64,
) -> Option<Candidate> {
    let (rbf, angle, _mx, _my) = build_curve(tp, ctx.cx, ctx.cy, sigma, ctx.fixed_angle)?;
    let w = rbf.max_weight();
    if !w.is_finite() || w > 20_000.0 {
        return None;
    }
    let mut cand = ctx.evaluate(&rbf, angle, tp);
    cand.pts = tp.to_vec();
    Some(cand)
}

/// Long-formula search: (detour depth, beam width, point cap).  A cluttered
/// map needs the curve to keep snaking past rock; the deadline, not these
/// numbers, is what actually stops the search.
pub(super) const DEEP_SEARCH: (usize, usize, usize) = (32, 12, 64);

/// Graphwar allows a minute per turn; ten seconds leaves margin for the shot
/// to reach the client before the clock runs out.
pub const PLAN_BUDGET: Duration = Duration::from_secs(10);

/// Upper bound on candidate target subsets — the width of the foreground
/// sweep, not a formula limit.
pub const MISSION_CAP: usize = 192;

pub(super) fn search(
    ctx: &PlanContext,
    pts: Vec<(f64, f64)>,
    sigma: f64,
    max_depth: usize,
    beam: usize,
    max_pts: usize,
) -> Option<Candidate> {
    let dbg = std::env::var("NODUS_PLAN_DEBUG").is_ok();
    let eval = |tp: &Vec<(f64, f64)>, depth: usize| -> Option<Candidate> {
        let cand = make_candidate(ctx, tp, sigma)?;
        if dbg {
            eprintln!(
                "  eval depth {depth} pts {} -> score {:.0} kills {} reason {:?} end_u {:.2}",
                tp.len(),
                cand.score,
                cand.kills,
                cand.path.reason,
                cand.path.end_u
            );
        }
        Some(cand)
    };
    let mut best: Option<Candidate> = None;
    let mut frontier: Vec<(Candidate, Vec<(f64, f64)>)> = Vec::new();
    if let Some(seed) = eval(&pts, 0) {
        best = Some(seed.clone());
        frontier.push((seed, pts));
    }
    for depth in 0..max_depth {
        if Instant::now() >= ctx.deadline {
            break;
        }
        let mut next: Vec<(Candidate, Vec<(f64, f64)>)> = Vec::new();
        for (cand, tp) in frontier {
            let Some(at) = ctx.issue_u(&cand) else {
                continue;
            };
            let sets = ctx.detour_sets(&cand, at, &tp);
            if dbg {
                eprintln!(
                    "depth {depth} at {at:.2} pts {:?} -> {} detour set(s) {:?}",
                    tp,
                    sets.len(),
                    sets
                );
            }
            for set in sets {
                // Two centres in the same column wreck the fit (huge
                // cancelling weights), so keep every new point in its own
                // column.
                let clash = set
                    .iter()
                    .any(|q| tp.iter().any(|p| (p.0 - q.0).abs() < 0.3))
                    || set
                        .iter()
                        .enumerate()
                        .any(|(i, q)| set[..i].iter().any(|p| (p.0 - q.0).abs() < 0.3));
                if clash {
                    continue;
                }
                let mut nt = tp.clone();
                nt.extend_from_slice(&set);
                if nt.len() > max_pts {
                    continue;
                }
                let Some(child) = eval(&nt, depth + 1) else {
                    continue;
                };
                if best.as_ref().map(|b| child.score > b.score).unwrap_or(true) {
                    best = Some(child.clone());
                }
                next.push((child, nt));
            }
        }
        if next.is_empty() {
            break;
        }
        next.sort_by(|a, b| b.0.score.partial_cmp(&a.0.score).unwrap());
        frontier = Vec::new();
        for (child, t) in next {
            if !frontier.iter().any(|f| {
                f.1.len() == t.len() && f.1.iter().zip(&t).all(|(a, b)| (a.0 - b.0).abs() < 1e-9)
            }) {
                frontier.push((child, t));
            }
            if frontier.len() >= beam {
                break;
            }
        }
    }
    best
}

/// Short formulas first: most turns need a handful of waypoints.  If soldiers
/// are left alive, repeat with longer formulas, deeper detours and a wider beam.
fn plan_one(
    ctx: &PlanContext,
    mission: &[Target],
    sigma: f64,
    guide: Option<&[(f64, f64)]>,
    deep: bool,
) -> Option<Candidate> {
    let pts = mission_points(mission);
    if pts.is_empty() {
        return None;
    }
    let quick = search(ctx, pts.clone(), sigma, 8, 5, 16);
    let solved = quick
        .as_ref()
        .map(|c| c.kills >= mission.len() && c.ally_hits == 0)
        .unwrap_or(false);
    if solved {
        return quick;
    }
    if Instant::now() >= ctx.deadline {
        return quick;
    }
    // Missions that never touch anything rarely improve with a wider search;
    // spend the long-formula budget on near misses and stragglers.
    let worth = quick.as_ref().map(|c| c.kills > 0).unwrap_or(true) || mission.len() <= 2;
    if !worth {
        return quick;
    }
    let long = search(ctx, pts, sigma, DEEP_SEARCH.0, DEEP_SEARCH.1, DEEP_SEARCH.2);
    let mut best = match (quick, long) {
        (Some(a), Some(b)) => Some(if b.score > a.score { b } else { a }),
        (a, b) => b.or(a),
    };
    if Instant::now() >= ctx.deadline {
        return best;
    }
    if let Some(guide) = guide {
        let tg = mission_points(mission);
        let gp = guide_points(ctx, guide, &tg, 0.0);
        if gp.len() >= 2 {
            if let Some(c) = make_candidate(ctx, &gp, sigma) {
                let c = refine(ctx, &c, sigma, &tg, if deep { 2 } else { 1 });
                if best.as_ref().map(|b| c.score > b.score).unwrap_or(true) {
                    best = Some(c);
                }
            }
        }
    }
    best
}

fn mission_keys(mission: &[Target]) -> Vec<(usize, usize)> {
    mission.iter().map(target_key).collect()
}

pub fn make_missions(targets: &[Target], rng: &mut Rng) -> Vec<Vec<Target>> {
    let n = targets.len();
    let mut out: Vec<Vec<Target>> = Vec::new();
    let mut seen: HashSet<Vec<(usize, usize)>> = HashSet::new();
    if n == 0 {
        return out;
    }
    let mut push = |m: Vec<Target>| {
        if m.is_empty() {
            return;
        }
        let mut keys = mission_keys(&m);
        keys.sort();
        if seen.insert(keys) {
            out.push(m);
        }
    };
    push(targets.to_vec());
    // "Everybody except one": a soldier in an unreachable spot makes the full
    // list hopeless while the rest is clean.  These come before the short
    // windows, which would otherwise fill the cap with six-target missions.
    if n > 2 {
        for skip in 0..n {
            push(
                targets
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != skip)
                    .map(|(_, t)| t.clone())
                    .collect(),
            );
        }
    }
    for t in targets {
        push(vec![t.clone()]);
    }
    if n > 6 {
        // Scattered large subsets before the contiguous windows: the soldiers
        // worth one curve are rarely neighbours in x, and the windows alone
        // would fill the cap before these got in.
        for _ in 0..48 {
            let size = 5 + rng.index(n - 4);
            let mut idx: Vec<usize> = (0..n).collect();
            for i in (1..idx.len()).rev() {
                let j = rng.index(i + 1);
                idx.swap(i, j);
            }
            idx.truncate(size.min(n));
            idx.sort();
            push(idx.into_iter().map(|i| targets[i].clone()).collect());
        }
        // Drop-two variants: one bad soldier is common, a pair of them is
        // not rare either.
        for a in 0..n {
            let b = (a + 1 + rng.index(n - 1)) % n;
            if a == b {
                continue;
            }
            push(
                targets
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != a && *i != b)
                    .map(|(_, t)| t.clone())
                    .collect(),
            );
        }
    }
    for size in 2..=12.min(n) {
        for start in 0..=n - size {
            push(targets[start..start + size].to_vec());
        }
    }
    out.truncate(MISSION_CAP);
    out
}

/// `avoid` holds the formulas already fired this game; a repeated miss is
/// penalised so the search keeps trying new ideas.
pub fn plan_shot(world: &World, rng: &mut Rng, avoid: &[String]) -> Option<ShotPlan> {
    plan_shot_budget(world, rng, avoid, 0)
}

/// Same search, but every mission is also tried at several spline widths —
/// for the pre-turn worker burning the opponent's thinking time.
pub fn plan_shot_wide(world: &World, rng: &mut Rng, avoid: &[String]) -> Option<ShotPlan> {
    plan_shot_budget(world, rng, avoid, 3)
}

/// The shooter, its targets and the frame they live in, or `None` when it is
/// not this client's turn or there is nobody left to shoot at.
pub(super) fn context_for(
    world: &World,
    deadline: Instant,
) -> Option<(PlanContext<'_>, Vec<Target>)> {
    if !world.started {
        return None;
    }
    let cur = world.current_player()?;
    if !cur.local {
        return None;
    }
    let shooter = cur.current_soldier()?.clone();
    if !shooter.alive {
        return None;
    }
    let team = cur.team;
    let frame = Frame::new(team);
    let (cx, cy) = frame.to_game(shooter.x as f64, shooter.y as f64);
    let mut targets = Vec::new();
    for (pi, p) in world.players.iter().enumerate() {
        if p.team == team {
            continue;
        }
        for (si, s) in p.soldiers.iter().enumerate() {
            if !s.alive {
                continue;
            }
            let (u, v) = frame.to_game(s.x as f64, s.y as f64);
            if u.abs() < 49.0 && (u - cx).abs() > 1.0 && v.abs() < 15.0 {
                targets.push(Target {
                    player: pi,
                    soldier: si,
                    u,
                    v,
                });
            }
        }
    }
    if targets.is_empty() {
        return None;
    }
    targets.sort_by(|a, b| a.u.total_cmp(&b.u));
    let farthest = targets.last().map(|t| t.u).unwrap_or(cx);
    let ctx = PlanContext {
        world,
        frame,
        shooter_real: (shooter.x as f64, shooter.y as f64),
        cx,
        cy,
        shooter_player: world.current_turn,
        shooter_soldier: cur.current,
        team,
        refs: world.alive_soldiers(),
        farthest,
        fixed_angle: (world.mode == SND_ODE).then_some(shooter.angle),
        deadline,
    };
    Some((ctx, targets))
}

fn plan_shot_budget(
    world: &World,
    rng: &mut Rng,
    avoid: &[String],
    extra_sigmas: usize,
) -> Option<ShotPlan> {
    let deadline = Instant::now() + PLAN_BUDGET;
    let (ctx, targets) = context_for(world, deadline)?;
    let missions = make_missions(&targets, rng);
    if extra_sigmas > 0 {
        return plan_wide(&ctx, world, &missions, targets.len(), rng, avoid);
    }
    let mut best: Option<(f64, ShotPlan)> = None;
    let mut all: Vec<(f64, ShotPlan)> = Vec::new();
    let mut guides: Vec<Option<Vec<(f64, f64)>>> = vec![None; missions.len()];
    for (mi, m) in missions.iter().enumerate() {
        if Instant::now() >= deadline {
            break;
        }
        let gap = median_gap(m).clamp(0.7, 4.5);
        let sigma = (gap * 0.85).clamp(0.7, 4.5);
        guides[mi] = ctx.guide_route(m);
        if let Some(c) = plan_one(&ctx, m, sigma, guides[mi].as_deref(), false) {
            if c.rbf.max_weight() >= 1e6 {
                continue;
            }
            let (score, plan) = as_plan(world, &c, avoid);
            all.push((score, plan.clone()));
            if best.as_ref().map(|b| score > b.0).unwrap_or(true) {
                best = Some((score, plan));
            }
            if c.kills == targets.len() && c.ally_hits == 0 {
                break;
            }
        }
    }
    // A* routes can miss a perfectly clear arc over old craters.  Before
    // falling back to digging, throw cheap random free arcs at the problem.
    if best.as_ref().map(|b| b.1.kills == 0).unwrap_or(true) {
        let mut attempts = 0;
        let mut found = false;
        while attempts < 400 && !found && Instant::now() < deadline {
            attempts += 1;
            let k = 1 + rng.index(3.min(targets.len()));
            let mut idx: Vec<usize> = (0..targets.len()).collect();
            for i in (1..idx.len()).rev() {
                let j = rng.index(i + 1);
                idx.swap(i, j);
            }
            idx.truncate(k);
            let mut tp: Vec<(f64, f64)> =
                idx.iter().map(|&i| (targets[i].u, targets[i].v)).collect();
            tp.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let sigma = rng.range(0.5, 4.0);
            let Some(pts) = random_points(&ctx, &tp, rng) else {
                continue;
            };
            let Some(c) = make_candidate(&ctx, &pts, sigma) else {
                continue;
            };
            let (score, plan) = as_plan(world, &c, avoid);
            all.push((score, plan.clone()));
            if best.as_ref().map(|b| score > b.0).unwrap_or(true) {
                best = Some((score, plan));
            }
            if c.kills > 0 && c.ally_hits == 0 {
                found = true;
            }
        }
    }
    finish(&ctx, world, rng, best, all)
}

/// Score a candidate exactly as the planner's final table does.
pub(super) fn as_plan(world: &World, c: &Candidate, avoid: &[String]) -> (f64, ShotPlan) {
    let formula = formula_for(world.mode, &c.rbf);
    // Repeating a shot that already missed just carves the same groove.
    let repeat = c.kills == 0 && avoid.contains(&formula);
    let score = c.score - if repeat { 100_000.0 } else { 0.0 };
    let plan = ShotPlan {
        formula,
        angle: c.angle,
        path: c.path.clone(),
        kills: c.kills,
        ally_hits: c.ally_hits,
        score,
        control_points: c.target_pts.len() + 1,
        mode: world.mode,
        dig: false,
    };
    (score, plan)
}

/// Nothing lands: rotate between the best few *different* plans instead of
/// lobbing the same dud over and over.  The RNG advances between turns.
pub(super) fn finish(
    ctx: &PlanContext,
    world: &World,
    rng: &mut Rng,
    best: Option<(f64, ShotPlan)>,
    mut all: Vec<(f64, ShotPlan)>,
) -> Option<ShotPlan> {
    let zero = best.as_ref().map(|b| b.1.kills == 0).unwrap_or(true);
    if zero {
        if let Some((_, p)) = breach_plan(ctx, world) {
            return Some(p);
        }
    }
    if best.as_ref().map(|b| b.1.kills == 0).unwrap_or(false) {
        all.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        let mut picks: Vec<ShotPlan> = Vec::new();
        for (_, p) in all {
            if p.kills > 0 {
                break;
            }
            if !picks.iter().any(|q| q.formula == p.formula) {
                picks.push(p);
            }
            if picks.len() >= 3 {
                break;
            }
        }
        if !picks.is_empty() {
            let i = rng.index(picks.len());
            return Some(picks.swap_remove(i));
        }
    }
    best.map(|(_, p)| p)
}
