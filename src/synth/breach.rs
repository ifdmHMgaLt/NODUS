//! Digging through a wall when no curve can get around it.

use std::time::Instant;

use crate::game::World;
use crate::sim::{Circle, EndReason, Surface, EXPLOSION_R, SCALE};

use super::planner::{make_candidate, PlanContext};
use super::render::formula_for;
use super::ShotPlan;

/// March along a line through the surface: is it clear, where does it first
/// hit rock, and how much rock follows before open space again.
fn ray_stats(s: &Surface, a: (f64, f64), b: (f64, f64)) -> (bool, f64, f64) {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let len = (dx * dx + dy * dy).sqrt();
    if !len.is_finite() || len < 1.0 {
        return (true, 0.0, 0.0);
    }
    let steps = len.ceil() as usize;
    let step = len / steps as f64;
    let mut first: Option<f64> = None;
    let mut run = 0.0;
    for k in 1..=steps {
        let t = k as f64 / steps as f64;
        if s.solid_real(a.0 + dx * t, a.1 + dy * t) {
            if first.is_none() {
                first = Some(t * len);
            }
            run += step;
        } else if first.is_some() {
            break;
        }
    }
    (first.is_none(), first.unwrap_or(len), run)
}

/// Everything is walled off: dig.  The shot keeps the same control point from
/// turn to turn, so each crater lands where the previous one ended and the
/// projectile tunnels along one arc instead of scattering holes.  An aim is
/// judged by replaying the shot with its crater added: if the curve travels
/// farther, the crater bit into the right face.  Once through, the normal
/// planner takes over.
pub(super) fn breach_plan(ctx: &PlanContext, world: &World) -> Option<(f64, ShotPlan)> {
    let s = &world.surface;
    let from_real = ctx.shooter_real;
    let mut enemies: Vec<(f64, f64)> = Vec::new();
    for p in &world.players {
        if p.team == ctx.team {
            continue;
        }
        for sd in &p.soldiers {
            if sd.alive {
                enemies.push((sd.x as f64, sd.y as f64));
            }
        }
    }
    if enemies.is_empty() {
        return None;
    }
    let goals: Vec<(f64, f64)> = enemies
        .iter()
        .filter_map(|&(ex, ey)| {
            let g = ctx.frame.to_game(ex, ey);
            if g.0 > ctx.cx + 0.6 && g.1.abs() < 14.5 {
                Some(g)
            } else {
                None
            }
        })
        .collect();
    if goals.is_empty() {
        return None;
    }
    let sg = (ctx.cx, ctx.cy);
    let mut best: Option<(f64, ShotPlan)> = None;
    for &goal in &goals {
        if Instant::now() >= ctx.deadline {
            break;
        }
        let du = goal.0 - sg.0;
        let dv = goal.1 - sg.1;
        let dist = (du * du + dv * dv).sqrt();
        if !dist.is_finite() || dist < 1.0 {
            continue;
        }
        let dir = (du / dist, dv / dist);
        let perp = (-dir.1, dir.0);
        // Aims for this soldier: the soldier itself, the spot where the
        // straight corridor to it meets rock, and every crater already carved
        // on that corridor.  All of them lie on one line, so a tunnel that
        // starts at the rock face keeps growing towards the soldier.
        let mut aims: Vec<((f64, f64), bool)> = Vec::new();
        let mut push = |p: (f64, f64), crater: bool| {
            if aims
                .iter()
                .any(|(q, _)| (q.0 - p.0).powi(2) + (q.1 - p.1).powi(2) < 0.25)
            {
                return;
            }
            aims.push((p, crater));
        };
        push(goal, false);
        let goal_real = ctx.frame.to_real(goal.0, goal.1);
        let (clear, first, _run) = ray_stats(s, from_real, goal_real);
        let span = dist * SCALE;
        if !clear && first < span - 1.0 {
            let t = (first / span).clamp(0.0, 1.0);
            push((sg.0 + du * t, sg.1 + dv * t), false);
        }
        let mut holes: Vec<(f64, f64)> = Vec::new();
        for w in &world.surface.white {
            let cg = ctx.frame.to_game(w.x, w.y);
            let r = (cg.0 - sg.0, cg.1 - sg.1);
            let proj = r.0 * dir.0 + r.1 * dir.1;
            let lat = (r.0 * perp.0 + r.1 * perp.1).abs();
            if proj > 0.5 && lat < 2.6 && cg.1.abs() < 14.5 {
                holes.push(cg);
            }
        }
        holes.sort_by(|a, b| {
            let pa = (a.0 - sg.0) * dir.0 + (a.1 - sg.1) * dir.1;
            let pb = (b.0 - sg.0) * dir.0 + (b.1 - sg.1) * dir.1;
            pb.partial_cmp(&pa).unwrap()
        });
        holes.truncate(1);
        for h in holes {
            push(h, true);
        }
        let dist_to = |u: f64, v: f64| ((u - goal.0).powi(2) + (v - goal.1).powi(2)).sqrt();
        for &((au, av), crater) in &aims {
            if Instant::now() >= ctx.deadline {
                break;
            }
            for &sigma in &[0.7, 1.1, 1.7, 2.6, 3.6] {
                let mut sets: Vec<Vec<(f64, f64)>> = vec![vec![(au, av)]];
                if crater {
                    // Pin the curve beyond the hole as well, so it comes back
                    // into the crater along the corridor instead of shaving
                    // the near lip and stopping on the face again.
                    for d in [1.1, 2.2] {
                        sets.push(vec![(au, av), (au + dir.0 * d, av + dir.1 * d)]);
                    }
                }
                for tp in sets {
                    let Some(c) = make_candidate(ctx, &tp, sigma) else {
                        continue;
                    };
                    if c.ally_hits > 0 || c.path.reason != EndReason::Terrain {
                        continue;
                    }
                    let (bx, by) = c.path.end_real;
                    if !bx.is_finite() || !by.is_finite() {
                        continue;
                    }
                    let mut post = world.clone();
                    post.surface.white.push(Circle::new(
                        (bx as i32) as f64,
                        (by as i32) as f64,
                        EXPLOSION_R,
                    ));
                    let refs = post.alive_soldiers();
                    let ctx2 = PlanContext {
                        world: &post,
                        frame: ctx.frame,
                        shooter_real: ctx.shooter_real,
                        cx: ctx.cx,
                        cy: ctx.cy,
                        shooter_player: ctx.shooter_player,
                        shooter_soldier: ctx.shooter_soldier,
                        team: ctx.team,
                        refs,
                        farthest: ctx.farthest,
                        fixed_angle: ctx.fixed_angle,
                        deadline: ctx.deadline,
                    };
                    let Some(next) = make_candidate(&ctx2, &tp, sigma) else {
                        continue;
                    };
                    let forward = (next.path.end_u - c.path.end_u) * dir.0
                        + (next.path.end_v - c.path.end_v) * dir.1;
                    if forward <= 0.02 {
                        continue;
                    }
                    let c_dist = dist_to(c.path.end_u, c.path.end_v);
                    let n_dist = dist_to(next.path.end_u, next.path.end_v);
                    let gain = c_dist - n_dist;
                    // A tunnel must actually bite deeper.  A crater that
                    // shaves a pixel off the wall would keep the bot drilling
                    // the same face forever; it is not progress, just noise.
                    if next.kills == 0 && gain < 0.3 {
                        continue;
                    }
                    let progress = if next.kills > 0 {
                        1_000_000.0
                    } else if next.path.reason != EndReason::Terrain {
                        300_000.0 + gain * 1000.0
                    } else {
                        gain * 1000.0
                    };
                    if progress <= 0.0 {
                        continue;
                    }
                    // Among tunnels prefer the one whose soldier is nearest:
                    // digging across the map for a far target is exactly the
                    // tunnel vision this planner is meant to avoid.
                    let score = progress + c.kills as f64 * 1_000_000.0
                        - c.path.length * 2.0
                        - n_dist * 50.0;
                    let plan = ShotPlan {
                        formula: formula_for(world.mode, &c.rbf),
                        angle: c.angle,
                        path: c.path.clone(),
                        kills: c.kills,
                        ally_hits: c.ally_hits,
                        score,
                        control_points: tp.len() + 1,
                        mode: world.mode,
                        dig: true,
                    };
                    if best.as_ref().map(|b| score > b.0).unwrap_or(true) {
                        best = Some((score, plan));
                    }
                }
            }
        }
    }
    best
}
