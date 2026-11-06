//! Shot synthesis: fit a smooth curve through enemy soldiers and render it as
//! a formula the game will integrate.

mod breach;
mod geometry;
mod planner;
mod rbf;
mod render;
mod worker;

use crate::sim::ShotPath;

pub use planner::{plan_shot, plan_shot_wide};
pub use render::formula_for;
pub use worker::{plan_shot_streaming, state_key, LiveSearch};

// Internals used by the integration tests in `tests/`; not part of the API.
#[doc(hidden)]
pub use planner::{build_curve, make_missions, MISSION_CAP, PLAN_BUDGET};
#[doc(hidden)]
pub use rbf::{fit_rbf, Rbf};

/// An enemy soldier in the shooter's game-plane coordinates.
#[derive(Clone, Debug)]
pub struct Target {
    pub player: usize,
    pub soldier: usize,
    pub u: f64,
    pub v: f64,
}

#[derive(Clone, Debug)]
pub struct ShotPlan {
    pub formula: String,
    pub angle: f64,
    pub path: ShotPath,
    pub kills: usize,
    pub ally_hits: usize,
    pub score: f64,
    pub control_points: usize,
    pub mode: i32,
    /// A shot that hits rock on purpose; only used when nothing else can reach.
    pub dig: bool,
}
