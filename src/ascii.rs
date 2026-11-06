// Tiny ASCII map of the battlefield, useful for logs and for checking that
// a planned curve really threads between the hills.

use crate::game::World;
use crate::sim::{PLANE_H, PLANE_LEN};
use crate::synth::ShotPlan;

pub fn render(world: &World, plan: Option<&ShotPlan>, width: usize, height: usize) -> String {
    let w = width.max(20);
    let h = height.max(8);
    let mut grid = vec![vec![' '; w]; h];

    for (row, cells) in grid.iter_mut().enumerate() {
        let y = (row as f64 + 0.5) / h as f64 * PLANE_H;
        for (col, cell) in cells.iter_mut().enumerate() {
            let x = (col as f64 + 0.5) / w as f64 * PLANE_LEN;
            if world.surface.solid_real(x, y) {
                *cell = '#';
            }
        }
    }

    if let Some(p) = plan {
        for (x, y) in &p.path.samples {
            let col = (*x / PLANE_LEN * w as f64).floor() as isize;
            let row = (*y / PLANE_H * h as f64).floor() as isize;
            if col >= 0 && row >= 0 && (col as usize) < w && (row as usize) < h {
                let cell = &mut grid[row as usize][col as usize];
                if *cell == ' ' || *cell == '#' {
                    *cell = '.';
                }
            }
        }
    }

    let local_team = world
        .players
        .iter()
        .find(|p| p.local)
        .map(|p| p.team)
        .unwrap_or(0);
    for p in &world.players {
        for s in &p.soldiers {
            if !s.alive {
                continue;
            }
            let col = (s.x as f64 / PLANE_LEN * w as f64).floor() as isize;
            let row = (s.y as f64 / PLANE_H * h as f64).floor() as isize;
            if col >= 0 && row >= 0 && (col as usize) < w && (row as usize) < h {
                grid[row as usize][col as usize] = if p.local {
                    '@'
                } else if local_team != 0 && p.team == local_team {
                    'o'
                } else {
                    'X'
                };
            }
        }
    }

    grid.into_iter()
        .map(|row| row.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}
