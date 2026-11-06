//! Coarse A* routing through the gaps between rocks.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub(super) struct GuideGrid {
    pub(super) du: f64,
    pub(super) u0: f64,
    pub(super) v0: f64,
    pub(super) nu: usize,
    pub(super) nv: usize,
    pub(super) blocked: Vec<bool>,
    pub(super) penalty: Vec<f32>,
}

impl GuideGrid {
    fn pos(&self, i: usize, j: usize) -> (f64, f64) {
        (self.u0 + i as f64 * self.du, self.v0 + j as f64 * self.du)
    }

    fn cell(&self, u: f64, v: f64) -> Option<(usize, usize)> {
        let i = ((u - self.u0) / self.du).round();
        let j = ((v - self.v0) / self.du).round();
        if i < 0.0 || j < 0.0 {
            return None;
        }
        let (i, j) = (i as usize, j as usize);
        if i >= self.nu || j >= self.nv {
            return None;
        }
        Some((i, j))
    }

    fn free(&self, i: usize, j: usize) -> bool {
        !self.blocked[j * self.nu + i]
    }
}

/// Closest free cell, searching a small neighbourhood so the route may start
/// or end right next to a rock.
fn nearest_free(grid: &GuideGrid, u: f64, v: f64) -> Option<(usize, usize)> {
    let (ci, cj) = grid.cell(u, v)?;
    if grid.free(ci, cj) {
        return Some((ci, cj));
    }
    for r in 1..=4usize {
        let mut best: Option<(usize, usize)> = None;
        let mut best_d = f64::INFINITY;
        for dj in -(r as i32)..=(r as i32) {
            for di in -(r as i32)..=(r as i32) {
                if di.abs() != r as i32 && dj.abs() != r as i32 {
                    continue;
                }
                let i = ci as i32 + di;
                let j = cj as i32 + dj;
                if i < 0 || j < 0 {
                    continue;
                }
                let (i, j) = (i as usize, j as usize);
                if i >= grid.nu || j >= grid.nv || !grid.free(i, j) {
                    continue;
                }
                let (u2, v2) = grid.pos(i, j);
                let d = (u2 - u).powi(2) + (v2 - v).powi(2);
                if d < best_d {
                    best_d = d;
                    best = Some((i, j));
                }
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

fn octile(ai: usize, aj: usize, bi: usize, bj: usize) -> f64 {
    let dx = (ai as f64 - bi as f64).abs();
    let dy = (aj as f64 - bj as f64).abs();
    dx.max(dy) + 0.414_213_562_4 * dx.min(dy)
}

/// 8-connected A* on the guide grid.  Diagonal moves are rejected when they
/// would cut the corner of a rock; the result is string-pulled into a polyline.
pub(super) fn astar(grid: &GuideGrid, from: (f64, f64), to: (f64, f64)) -> Option<Vec<(f64, f64)>> {
    let (si, sj) = nearest_free(grid, from.0, from.1)?;
    let (gi, gj) = nearest_free(grid, to.0, to.1)?;
    let n = grid.nu * grid.nv;
    let mut g = vec![f64::INFINITY; n];
    let mut parent = vec![usize::MAX; n];
    let mut closed = vec![false; n];
    let mut heap: BinaryHeap<(Reverse<u64>, usize)> = BinaryHeap::new();
    let start = sj * grid.nu + si;
    let goal = gj * grid.nu + gi;
    g[start] = 0.0;
    heap.push((Reverse(0), start));
    while let Some((_, cur)) = heap.pop() {
        if cur == goal {
            break;
        }
        if closed[cur] {
            continue;
        }
        closed[cur] = true;
        let ci = cur % grid.nu;
        let cj = cur / grid.nu;
        for dj in -1i32..=1 {
            for di in -1i32..=1 {
                if di == 0 && dj == 0 {
                    continue;
                }
                let ni = ci as i32 + di;
                let nj = cj as i32 + dj;
                if ni < 0 || nj < 0 {
                    continue;
                }
                let (ni, nj) = (ni as usize, nj as usize);
                if ni >= grid.nu || nj >= grid.nv {
                    continue;
                }
                if !grid.free(ni, nj) {
                    continue;
                }
                // No squeezing through the corner of two rocks.
                if di != 0 && dj != 0 && (!grid.free(ci, nj) || !grid.free(ni, cj)) {
                    continue;
                }
                let next = nj * grid.nu + ni;
                let step = if di != 0 && dj != 0 {
                    std::f64::consts::SQRT_2
                } else {
                    1.0
                };
                let ng = g[cur] + step + grid.penalty[next] as f64;
                if ng < g[next] {
                    g[next] = ng;
                    parent[next] = cur;
                    let f = ng + octile(ni, nj, gi, gj);
                    heap.push((Reverse((f * 1000.0) as u64), next));
                }
            }
        }
    }
    if parent[goal] == usize::MAX && goal != start {
        return None;
    }
    let mut cells = vec![goal];
    let mut cur = goal;
    while cur != start {
        cur = parent[cur];
        if cur == usize::MAX {
            return None;
        }
        cells.push(cur);
    }
    cells.reverse();
    let mut pts: Vec<(f64, f64)> = cells
        .iter()
        .map(|&k| grid.pos(k % grid.nu, k / grid.nu))
        .collect();
    // String pulling: drop every point that is not needed to clear a rock.
    let clear = |a: (f64, f64), b: (f64, f64)| {
        let d = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        let steps = ((d / (grid.du * 0.5)).ceil() as usize).max(1);
        (0..=steps).all(|k| {
            let t = k as f64 / steps as f64;
            let u = a.0 + (b.0 - a.0) * t;
            let v = a.1 + (b.1 - a.1) * t;
            match grid.cell(u, v) {
                Some((i, j)) => grid.free(i, j),
                None => false,
            }
        })
    };
    let mut out = vec![pts[0]];
    let mut i = 0;
    while i + 1 < pts.len() {
        let mut j = pts.len() - 1;
        while j > i + 1 && !clear(pts[i], pts[j]) {
            j -= 1;
        }
        out.push(pts[j]);
        i = j;
    }
    pts = out;
    Some(pts)
}

/// Symmetric mean distance between two polylines, used to drop near-duplicate
/// routes produced by different cost grids.
pub(super) fn route_distance(a: &[(f64, f64)], b: &[(f64, f64)]) -> f64 {
    let one = |a: &[(f64, f64)], b: &[(f64, f64)]| {
        let mut sum = 0.0;
        for p in a {
            let mut best = f64::INFINITY;
            for q in b {
                let d = ((p.0 - q.0).powi(2) + (p.1 - q.1).powi(2)).sqrt();
                best = best.min(d);
            }
            sum += best;
        }
        sum / a.len().max(1) as f64
    };
    one(a, b).max(one(b, a))
}
