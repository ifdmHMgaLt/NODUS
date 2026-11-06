//! Background search: keeps proposing new geometry until the turn starts.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::game::World;
use crate::rng::Rng;

use super::breach::breach_plan;
use super::geometry::GuideGrid;
use super::planner::{
    as_plan, context_for, finish, grow_targets, guide_points, make_candidate, make_missions,
    median_gap, mission_points, refine, search, shifted_points, valid_points, Candidate,
    PlanContext, DEEP_SEARCH,
};
use super::ShotPlan;
use super::Target;

/// Fingerprint of everything that influences a plan.  The background worker
/// only keeps its result while the board still matches.
pub fn state_key(world: &World) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    world.mode.hash(&mut h);
    world.started.hash(&mut h);
    world.finished.hash(&mut h);
    world.current_turn.hash(&mut h);
    for p in &world.players {
        p.team.hash(&mut h);
        p.local.hash(&mut h);
        p.current.hash(&mut h);
        for s in &p.soldiers {
            s.x.hash(&mut h);
            s.y.hash(&mut h);
            s.alive.hash(&mut h);
            s.angle.to_bits().hash(&mut h);
        }
    }
    world.surface.black.len().hash(&mut h);
    world.surface.white.len().hash(&mut h);
    h.finish()
}

/// One point prediction: a mission, a spline width and the curve it produced.
#[derive(Clone)]
struct Hyp {
    mission: usize,
    sigma: f64,
    targets: Vec<(f64, f64)>,
    guided: bool,
    cand: Candidate,
}

/// Keep a scored candidate in the running tables.
fn record(
    world: &World,
    avoid: &[String],
    all_targets: usize,
    cand: &Candidate,
    best: &mut Option<(f64, ShotPlan)>,
    all: &mut Vec<(f64, ShotPlan)>,
    solved: &mut bool,
) {
    if *solved || cand.rbf.max_weight() >= 1e6 {
        return;
    }
    let (score, plan) = as_plan(world, cand, avoid);
    if cand.kills >= all_targets && cand.ally_hits == 0 {
        *solved = true;
    }
    all.push((score, plan.clone()));
    if best.as_ref().map(|b| score > b.0).unwrap_or(true) {
        *best = Some((score, plan));
    }
}

/// Wide search for the background worker.  Spend predictions on different
/// geometry, not on re-running the detour search at another spline width: a
/// few A* routes per mission, sampled at a few widths, three approach offsets
/// and two perpendicular wobbles.  Only the best shapes are refined and
/// searched further.
pub(super) fn plan_wide(
    ctx: &PlanContext,
    world: &World,
    missions: &[Vec<Target>],
    all_targets: usize,
    rng: &mut Rng,
    avoid: &[String],
) -> Option<ShotPlan> {
    let grids = ctx.guide_grids(5);
    let mut order: Vec<usize> = (0..missions.len()).collect();
    order.sort_by(|&a, &b| missions[b].len().cmp(&missions[a].len()));
    order.truncate(16);
    let mut best: Option<(f64, ShotPlan)> = None;
    let mut all: Vec<(f64, ShotPlan)> = Vec::new();
    let mut hyps: Vec<Hyp> = Vec::new();
    let mut solved = false;
    for &mi in &order {
        if solved || Instant::now() >= ctx.deadline {
            break;
        }
        let m = &missions[mi];
        let gap = median_gap(m).clamp(0.7, 4.5);
        let base = (gap * 0.85).clamp(0.7, 8.0);
        let exact = mission_points(m);
        if let Some(c) = search(ctx, exact.clone(), base, 5, 3, 10) {
            record(
                world,
                avoid,
                all_targets,
                &c,
                &mut best,
                &mut all,
                &mut solved,
            );
            hyps.push(Hyp {
                mission: mi,
                sigma: base,
                targets: exact.clone(),
                guided: false,
                cand: c,
            });
        }
        for route in ctx.guide_routes(m, &grids, 4) {
            // Small missions also get narrower and much wider bells: a pair
            // of soldiers close together wants a tight hump, a lone straggler
            // far behind a rock wants a long shallow one.
            let sigmas: Vec<f64> = if m.len() <= 3 {
                vec![base * 0.6, base, base * 1.5, base * 2.0, base * 3.5]
            } else {
                vec![base, base * 1.5]
            };
            for &sigma in &sigmas {
                let sigma = sigma.clamp(0.5, 5.0);
                for &dodge in &[0.0, 0.24, -0.24] {
                    if solved {
                        break;
                    }
                    let tp = if dodge == 0.0 {
                        exact.clone()
                    } else {
                        shifted_points(m, dodge)
                    };
                    let gp = guide_points(ctx, &route, &tp, 0.0);
                    if gp.len() < 2 {
                        continue;
                    }
                    if let Some(c) = make_candidate(ctx, &gp, sigma) {
                        record(
                            world,
                            avoid,
                            all_targets,
                            &c,
                            &mut best,
                            &mut all,
                            &mut solved,
                        );
                        hyps.push(Hyp {
                            mission: mi,
                            sigma,
                            targets: tp,
                            guided: true,
                            cand: c,
                        });
                    }
                }
            }
            for &wobble in &[0.35, -0.35] {
                if solved {
                    break;
                }
                let gp = guide_points(ctx, &route, &exact, wobble);
                if gp.len() < 2 {
                    continue;
                }
                if let Some(c) = make_candidate(ctx, &gp, base) {
                    record(
                        world,
                        avoid,
                        all_targets,
                        &c,
                        &mut best,
                        &mut all,
                        &mut solved,
                    );
                    hyps.push(Hyp {
                        mission: mi,
                        sigma: base,
                        targets: exact.clone(),
                        guided: true,
                        cand: c,
                    });
                }
            }
        }
    }
    // Refinement turns "almost through the slit" into "through the slit"; the
    // long search adds detours the guide did not think of.
    hyps.sort_by(|a, b| b.cand.score.partial_cmp(&a.cand.score).unwrap());
    // Deepen the best shape of every mission first — otherwise one
    // high-scoring mission hogs the budget; the rest goes to overall leaders.
    let mut picks: Vec<Hyp> = Vec::new();
    let mut covered = vec![false; missions.len()];
    for h in &hyps {
        if !covered[h.mission] {
            covered[h.mission] = true;
            picks.push(h.clone());
        }
    }
    for h in hyps {
        if picks.len() >= 32 {
            break;
        }
        if !picks
            .iter()
            .any(|p| p.mission == h.mission && p.cand.pts == h.cand.pts)
        {
            picks.push(h);
        }
    }
    picks.sort_by(|a, b| b.cand.score.partial_cmp(&a.cand.score).unwrap());
    let mut long_done: Vec<(usize, u64)> = Vec::new();
    let mut spent = 0usize;
    for (rank, hyp) in picks.into_iter().enumerate() {
        if solved || spent >= 32 || Instant::now() >= ctx.deadline {
            break;
        }
        if hyp.cand.kills == 0 && hyp.cand.selected_hits == 0 {
            continue;
        }
        spent += 1;
        let m = &missions[hyp.mission];
        let mut cur = hyp.cand;
        if hyp.guided {
            let passes = if rank < 8 { 2 } else { 1 };
            cur = refine(ctx, &cur, hyp.sigma, &hyp.targets, passes);
            record(
                world,
                avoid,
                all_targets,
                &cur,
                &mut best,
                &mut all,
                &mut solved,
            );
        }
        if solved || cur.kills >= m.len() {
            continue;
        }
        let key = (hyp.mission, hyp.sigma.to_bits());
        if long_done.contains(&key) {
            continue;
        }
        long_done.push(key);
        if let Some(c) = search(
            ctx,
            mission_points(m),
            hyp.sigma,
            DEEP_SEARCH.0,
            DEEP_SEARCH.1,
            DEEP_SEARCH.2,
        ) {
            record(
                world,
                avoid,
                all_targets,
                &c,
                &mut best,
                &mut all,
                &mut solved,
            );
        }
    }
    finish(ctx, world, rng, best, all)
}

/// Live results of the background search: workers publish their best plan as
/// soon as they find it, so the main thread can fire the best curve so far
/// instead of waiting for a fixed budget.
pub struct LiveSearch {
    best: Mutex<Option<(f64, ShotPlan)>>,
    pub solved: AtomicBool,
    pub evaluated: AtomicU64,
}

impl LiveSearch {
    pub fn new() -> Self {
        Self {
            best: Mutex::new(None),
            solved: AtomicBool::new(false),
            evaluated: AtomicU64::new(0),
        }
    }

    pub fn best(&self) -> Option<ShotPlan> {
        self.best
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|b| b.1.clone()))
    }

    pub fn count(&self) -> u64 {
        self.evaluated.load(Ordering::Relaxed)
    }

    fn offer(&self, score: f64, plan: ShotPlan) {
        let Ok(mut g) = self.best.lock() else {
            return;
        };
        if g.as_ref().map(|b| score > b.0).unwrap_or(true) {
            *g = Some((score, plan));
        }
    }
}

impl Default for LiveSearch {
    fn default() -> Self {
        Self::new()
    }
}

/// A fresh cost field from the same obstacles: rows get a random weight, so
/// A* threads the same map through a different gap every time.
fn jittered_grid(base: &GuideGrid, rng: &mut Rng) -> GuideGrid {
    let gain = rng.range(0.3, 2.4) as f32;
    let phase = rng.range(0.0, std::f64::consts::TAU);
    let freq = rng.range(0.04, 0.3);
    let noise = rng.range(0.02, 0.3) as f32;
    let mut p = base.penalty.clone();
    for j in 0..base.nv {
        let wave = (phase + j as f64 * freq).sin() as f32;
        for i in 0..base.nu {
            let k = j * base.nu + i;
            p[k] = p[k] * gain + wave * 0.09 + noise * rng.next_f64() as f32;
        }
    }
    GuideGrid {
        du: base.du,
        u0: base.u0,
        v0: base.v0,
        nu: base.nu,
        nv: base.nv,
        blocked: base.blocked.clone(),
        penalty: p,
    }
}

/// A curve through random sky points: the "completely different place"
/// family.  Most of these hit rock immediately, but they cover shapes no
/// detour search would ever propose.
pub(super) fn random_points(
    ctx: &PlanContext,
    targets: &[(f64, f64)],
    rng: &mut Rng,
) -> Option<Vec<(f64, f64)>> {
    let last = targets.last()?.0;
    let span = last - ctx.cx;
    if span < 1.0 {
        return None;
    }
    for _ in 0..3 {
        let k = 1 + rng.index(4);
        let mut free: Vec<(f64, f64)> = Vec::new();
        for _ in 0..k * 6 {
            let u = ctx.cx + 0.9 + rng.next_f64() * (span - 0.9);
            let v = rng.range(-12.8, 12.8);
            if free.iter().all(|p: &(f64, f64)| (p.0 - u).abs() > 0.35) {
                free.push((u, v));
                if free.len() == k {
                    break;
                }
            }
        }
        if free.is_empty() {
            continue;
        }
        free.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        free.extend_from_slice(targets);
        if valid_points(ctx, &free, targets) {
            return Some(free);
        }
    }
    None
}

/// Mission + A* grid + whether the grid was jittered.
type RouteKey = (usize, usize, bool);
type RouteCache = Vec<(RouteKey, Vec<(f64, f64)>)>;
/// Best local candidate with the spline width and control points it used.
type LocalBest = (Candidate, f64, Vec<(f64, f64)>);

fn cached_route<'g>(
    ctx: &PlanContext,
    mission: &[Target],
    key: RouteKey,
    grid: &GuideGrid,
    cache: &'g mut RouteCache,
) -> Option<&'g [(f64, f64)]> {
    let pos = cache.iter().position(|(k, _)| *k == key);
    if let Some(pos) = pos {
        return Some(&cache[pos].1);
    }
    let r = ctx.guide_route_on(grid, mission)?;
    if cache.len() > 96 {
        cache.remove(0);
    }
    cache.push((key, r));
    Some(&cache.last().unwrap().1)
}

/// One search thread: keeps proposing new geometry — random A* cost fields,
/// sky waypoints, offsets, widths and wobbles — and scores every curve with
/// the full projectile simulation.  Promising finds get nudged and re-run
/// through the longer beam search.
#[allow(clippy::too_many_arguments)]
fn search_worker(
    ctx: &PlanContext,
    world: &World,
    missions: &[Vec<Target>],
    all_targets: usize,
    base_grids: &[GuideGrid],
    avoid: &[String],
    seed: u64,
    id: u64,
    stop: &AtomicBool,
    live: &LiveSearch,
) {
    if missions.is_empty() {
        return;
    }
    let mut rng =
        Rng::new(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ id.wrapping_mul(0xd1b5_4a32_d192_ed03));
    let mut grids: Vec<GuideGrid> = Vec::new();
    if let Some(g) = base_grids.first() {
        for _ in 0..12 {
            grids.push(jittered_grid(g, &mut rng));
        }
    }
    let mut routes: RouteCache = Vec::new();
    let mut local: Option<LocalBest> = None;
    let mut best_score = f64::NEG_INFINITY;
    let started = Instant::now();
    let mut it = 0u64;
    let max_mission = missions.iter().map(|m| m.len()).max().unwrap_or(1);
    let mut every: Vec<Target> = Vec::new();
    for m in missions {
        for t in m {
            if !every
                .iter()
                .any(|u| u.player == t.player && u.soldier == t.soldier)
            {
                every.push(t.clone());
            }
        }
    }
    while !stop.load(Ordering::Relaxed) && !live.solved.load(Ordering::Relaxed) {
        it += 1;
        if it.is_multiple_of(256) && started.elapsed().as_secs() > 120 {
            break;
        }
        // "Big" includes the drop-one variants, not just the full list: one
        // unreachable soldier must not force the whole curve onto a rock.
        let mi = if rng.index(3) == 0 {
            let big: Vec<usize> = (0..missions.len())
                .filter(|&i| missions[i].len() + 1 >= max_mission)
                .collect();
            big[rng.index(big.len().max(1))]
        } else {
            rng.index(missions.len())
        };
        let m = &missions[mi];
        let gap = median_gap(m).clamp(0.7, 6.0);
        let base = (gap * 0.85).clamp(0.6, 8.0);
        let sigma = (base * rng.range(0.35, 3.8)).clamp(0.4, 6.0);
        let exact = mission_points(m);
        let tp = if rng.chance(0.5) {
            exact.clone()
        } else {
            shifted_points(m, rng.range(-0.45, 0.45))
        };
        let role = rng.index(100);
        let cand = if role < 55 && !grids.is_empty() {
            let jitter = rng.chance(0.7);
            let gi = rng.index(if jitter {
                grids.len()
            } else {
                base_grids.len().max(1)
            });
            let (grid, key) = if jitter {
                (&grids[gi], (mi, gi, true))
            } else {
                (&base_grids[gi], (mi, gi, false))
            };
            match cached_route(ctx, m, key, grid, &mut routes) {
                Some(r) => {
                    let wobble = if rng.chance(0.45) {
                        rng.range(-0.7, 0.7)
                    } else {
                        0.0
                    };
                    let gp = guide_points(ctx, r, &tp, wobble);
                    if gp.len() >= 2 {
                        make_candidate(ctx, &gp, sigma)
                    } else {
                        None
                    }
                }
                None => None,
            }
        } else if role < 78 {
            random_points(ctx, &tp, &mut rng).and_then(|p| make_candidate(ctx, &p, sigma))
        } else if role < 92 {
            local.as_ref().map(|(c, s, t)| refine(ctx, c, *s, t, 1))
        } else {
            make_candidate(ctx, &tp, sigma)
        };
        if let Some(c) = cand {
            live.evaluated.fetch_add(1, Ordering::Relaxed);
            if c.kills >= all_targets && c.ally_hits == 0 {
                live.solved.store(true, Ordering::Relaxed);
            }
            let (score, plan) = as_plan(world, &c, avoid);
            if score > best_score {
                best_score = score;
                live.offer(score, plan);
            }
            if local
                .as_ref()
                .map(|(l, _, _)| c.score > l.score)
                .unwrap_or(true)
            {
                local = Some((c, sigma, tp));
            }
        }
        if it.is_multiple_of(48) {
            if let Some((c, s, t)) = local.clone() {
                let passes = if it.is_multiple_of(192) { 2 } else { 1 };
                let r = refine(ctx, &c, s, &t, passes);
                live.evaluated.fetch_add(1, Ordering::Relaxed);
                let (score, plan) = as_plan(world, &r, avoid);
                if score > best_score {
                    best_score = score;
                    live.offer(score, plan);
                }
                if r.score > c.score {
                    local = Some((r, s, t));
                }
            }
        }
        if it.is_multiple_of(96) {
            // One more soldier per pass: this is what lets a curve collect a
            // whole row of stragglers instead of stopping at the first three.
            if let Some((c, s, t)) = local.clone() {
                if let Some(g) = grow_targets(ctx, &c, s, &every) {
                    live.evaluated.fetch_add(1, Ordering::Relaxed);
                    if g.kills >= all_targets && g.ally_hits == 0 {
                        live.solved.store(true, Ordering::Relaxed);
                    }
                    let (score, plan) = as_plan(world, &g, avoid);
                    if score > best_score {
                        best_score = score;
                        live.offer(score, plan);
                    }
                    if g.score > c.score {
                        local = Some((g, s, t));
                    }
                }
            }
        }
        if it.is_multiple_of(160) {
            if let Some(c) = search(
                ctx,
                exact.clone(),
                sigma,
                DEEP_SEARCH.0,
                DEEP_SEARCH.1,
                DEEP_SEARCH.2,
            ) {
                live.evaluated.fetch_add(1, Ordering::Relaxed);
                let (score, plan) = as_plan(world, &c, avoid);
                if score > best_score {
                    best_score = score;
                    live.offer(score, plan);
                }
            }
        }
        // Tunnels are not random curves: they aim at rock on purpose.  One
        // worker keeps offering the best current dig; any kill outranks it.
        if id == 0 && it.is_multiple_of(512) {
            if let Some((score, plan)) = breach_plan(ctx, world) {
                live.evaluated.fetch_add(1, Ordering::Relaxed);
                live.offer(score, plan);
            }
        }
    }
}

/// Fill `live` from every core until `stop` is raised, all enemies are
/// covered or the two-minute guard fires.  The caller reads `live.best()`
/// whenever it needs to shoot; there is deliberately no fixed sample size.
pub fn plan_shot_streaming(
    world: &World,
    seed: u64,
    avoid: &[String],
    stop: &AtomicBool,
    live: &LiveSearch,
) {
    // Background work may use the whole opponent turn, not the 10 s budget.
    let deadline = Instant::now() + Duration::from_secs(120);
    let Some((ctx, targets)) = context_for(world, deadline) else {
        return;
    };
    let missions = make_missions(&targets, &mut Rng::new(seed));
    let grids = ctx.guide_grids(5);
    let all_targets = targets.len();
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 24);
    let ctx = &ctx;
    let missions = &missions;
    let grids = &grids;
    std::thread::scope(|scope| {
        for id in 1..threads as u64 {
            let wid = id;
            scope.spawn(move || {
                search_worker(
                    ctx,
                    world,
                    missions,
                    all_targets,
                    grids,
                    avoid,
                    seed,
                    wid,
                    stop,
                    live,
                );
            });
        }
        search_worker(
            ctx,
            world,
            missions,
            all_targets,
            grids,
            avoid,
            seed,
            0,
            stop,
            live,
        );
    });
}
