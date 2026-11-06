use crate::expr::Expr;
use crate::protocol::*;
use crate::sim::{
    simulate_first_order, simulate_normal_expr, simulate_second_order, Circle, EndReason, Frame,
    ShotPath, SoldierRef, Surface, EXPLOSION_R, TEAM1,
};

#[derive(Clone, Debug)]
pub struct SoldierState {
    pub x: i32,
    pub y: i32,
    pub alive: bool,
    pub angle: f64,
}

#[derive(Clone, Debug)]
pub struct PlayerState {
    pub id: i32,
    pub name: String,
    pub team: i32,
    pub num_soldiers: usize,
    pub soldiers: Vec<SoldierState>,
    pub current: usize,
    pub local: bool,
}

impl PlayerState {
    fn next_turn(&mut self) -> bool {
        if self.num_soldiers == 0 || self.soldiers.is_empty() {
            return false;
        }
        let n = self.soldiers.len();
        for _ in 0..n {
            self.current = (self.current + 1) % n;
            if self.soldiers[self.current].alive {
                return true;
            }
        }
        false
    }

    pub fn current_soldier(&self) -> Option<&SoldierState> {
        self.soldiers.get(self.current)
    }
}

#[derive(Clone, Debug)]
pub struct FireReport {
    pub formula: String,
    pub angle: f64,
    pub team: i32,
    pub path: ShotPath,
    pub enemy_hits: usize,
    pub ally_hits: usize,
}

#[derive(Clone, Debug)]
pub struct World {
    pub mode: i32,
    pub players: Vec<PlayerState>,
    pub current_turn: usize,
    pub circles: Vec<Circle>,
    pub surface: Surface,
    pub started: bool,
    pub finished: bool,
    pub local_ids: Vec<i32>,
    pub last_fire: Option<FireReport>,
}

impl Default for World {
    fn default() -> Self {
        Self {
            mode: NORMAL_FUNC,
            players: Vec::new(),
            current_turn: 0,
            circles: Vec::new(),
            surface: Surface::default(),
            started: false,
            finished: false,
            local_ids: Vec::new(),
            last_fire: None,
        }
    }
}

impl World {
    pub fn set_mode(&mut self, mode: i32) {
        self.mode = mode;
    }

    pub fn add_player(&mut self, id: i32, name: String, team: i32, num: usize, local: bool) {
        if self.players.iter().any(|p| p.id == id) {
            return;
        }
        if local && !self.local_ids.contains(&id) {
            self.local_ids.push(id);
        }
        self.players.push(PlayerState {
            id,
            name,
            team,
            num_soldiers: num,
            soldiers: (0..num)
                .map(|_| SoldierState {
                    x: 0,
                    y: 0,
                    alive: false,
                    angle: 0.0,
                })
                .collect(),
            current: 0,
            local,
        });
    }

    pub fn remove_player(&mut self, id: i32) {
        if self.started {
            if let Some(p) = self.players.iter_mut().find(|p| p.id == id) {
                for s in &mut p.soldiers {
                    s.alive = false;
                }
            }
        } else {
            self.players.retain(|p| p.id != id);
            if self.current_turn >= self.players.len() {
                self.current_turn = 0;
            }
        }
    }

    pub fn set_team(&mut self, id: i32, team: i32) {
        if let Some(p) = self.players.iter_mut().find(|p| p.id == id) {
            p.team = team;
        }
    }

    pub fn increment_soldiers(&mut self, id: i32) {
        let n = self
            .players
            .iter()
            .find(|p| p.id == id)
            .map(|p| (p.num_soldiers + 1).min(4))
            .unwrap_or(0);
        if n > 0 {
            self.set_num_soldiers(id, n);
        }
    }

    pub fn decrement_soldiers(&mut self, id: i32) {
        let n = self
            .players
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.num_soldiers.saturating_sub(1))
            .unwrap_or(0);
        self.set_num_soldiers(id, n);
    }

    pub fn set_num_soldiers(&mut self, id: i32, num: usize) {
        if let Some(p) = self.players.iter_mut().find(|p| p.id == id) {
            p.num_soldiers = num;
            while p.soldiers.len() < num.min(4) {
                p.soldiers.push(SoldierState {
                    x: 0,
                    y: 0,
                    alive: false,
                    angle: 0.0,
                });
            }
        }
    }

    pub fn reorder(&mut self, ids: &[i32]) {
        let mut next = Vec::with_capacity(self.players.len());
        for id in ids {
            if let Some(p) = self.players.iter().find(|p| p.id == *id) {
                next.push(p.clone());
            }
        }
        for p in &self.players {
            if !next.iter().any(|q| q.id == p.id) {
                next.push(p.clone());
            }
        }
        self.players = next;
    }

    pub fn start_game(
        &mut self,
        circles: Vec<Circle>,
        coords: &[i32],
        start_player_raw: i32,
    ) -> Result<(), String> {
        if self.players.is_empty() {
            return Err("start game without players".into());
        }
        let mut k = 0usize;
        for p in &mut self.players {
            let n = p.num_soldiers.min(4);
            p.soldiers.clear();
            for _ in 0..n {
                if k + 1 >= coords.len() {
                    return Err("start game: truncated soldier list".into());
                }
                p.soldiers.push(SoldierState {
                    x: coords[k],
                    y: coords[k + 1],
                    alive: true,
                    angle: 0.0,
                });
                k += 2;
            }
            p.current = 0;
        }
        self.circles = circles.clone();
        self.surface.black = circles;
        self.surface.white.clear();
        let n = self.players.len() as i32;
        let mut sp = start_player_raw.abs();
        if sp == i32::MIN {
            sp = 0;
        }
        self.current_turn = (sp % n) as usize;
        self.started = true;
        self.finished = false;
        Ok(())
    }

    pub fn current_player(&self) -> Option<&PlayerState> {
        self.players.get(self.current_turn)
    }

    pub fn current_is_local(&self) -> bool {
        self.current_player().map(|p| p.local).unwrap_or(false)
    }

    pub fn advance_turn(&mut self) {
        let n = self.players.len();
        if n == 0 {
            return;
        }
        for _ in 0..n {
            self.current_turn = (self.current_turn + 1) % n;
            if self.players[self.current_turn].next_turn() {
                break;
            }
        }
    }

    pub fn set_angle(&mut self, player_id: i32, soldier: usize, angle: f64) {
        if let Some(p) = self.players.iter_mut().find(|p| p.id == player_id) {
            if let Some(s) = p.soldiers.get_mut(soldier) {
                s.angle = angle;
            }
        }
    }

    pub fn check_finished(&self) -> bool {
        let mut t1 = false;
        let mut t2 = false;
        for p in &self.players {
            for s in &p.soldiers {
                if s.alive {
                    if p.team == TEAM1 {
                        t1 = true;
                    } else {
                        t2 = true;
                    }
                }
            }
        }
        !t1 || !t2
    }

    pub fn alive_soldiers(&self) -> Vec<SoldierRef> {
        let mut out = Vec::new();
        for (pi, p) in self.players.iter().enumerate() {
            for (si, s) in p.soldiers.iter().enumerate() {
                if s.alive {
                    out.push(SoldierRef {
                        player: pi,
                        soldier: si,
                        team: p.team,
                        x: s.x as f64,
                        y: s.y as f64,
                    });
                }
            }
        }
        out
    }

    /// Simulate a shot without touching casualties or terrain.
    pub fn simulate_fire(
        &self,
        sender_id: i32,
        formula: &str,
        snd_angle: f64,
    ) -> Result<ShotPath, String> {
        if !self.started {
            return Err("shot before start".into());
        }
        let cur = self.current_player().ok_or("no current player")?;
        if cur.id != sender_id {
            return Err("shot out of turn".into());
        }
        let shooter = cur.current_soldier().ok_or("shooter has no soldier")?;
        if !shooter.alive {
            return Err("shooter is dead".into());
        }
        let expr = Expr::parse(formula)?;
        let frame = Frame::new(cur.team);
        let refs = self.alive_soldiers();
        let angle = if self.mode == SND_ODE { snd_angle } else { 0.0 };
        let path = match self.mode {
            NORMAL_FUNC => simulate_normal_expr(
                &expr,
                frame,
                (shooter.x as f64, shooter.y as f64),
                &self.surface,
                &refs,
                self.current_turn,
                cur.current,
                cur.team,
            ),
            FST_ODE => simulate_first_order(
                &expr,
                frame,
                (shooter.x as f64, shooter.y as f64),
                &self.surface,
                &refs,
                self.current_turn,
                cur.current,
                cur.team,
            ),
            SND_ODE => simulate_second_order(
                &expr,
                angle,
                frame,
                (shooter.x as f64, shooter.y as f64),
                &self.surface,
                &refs,
                self.current_turn,
                cur.current,
                cur.team,
            ),
            _ => return Err("unknown game mode".into()),
        };
        Ok(path)
    }

    /// Simulate a fired function exactly the way a Graphwar client does,
    /// then update the local belief about terrain and casualties.
    pub fn apply_fire(
        &mut self,
        sender_id: i32,
        formula: &str,
        snd_angle: f64,
    ) -> Result<FireReport, String> {
        let path = self.simulate_fire(sender_id, formula, snd_angle)?;
        let cur = self.current_player().ok_or("no current player")?.clone();
        let angle = if self.mode == SND_ODE { snd_angle } else { 0.0 };

        let mut enemy_hits = 0;
        let mut ally_hits = 0;
        for hit in &path.hits {
            if let Some(p) = self.players.get_mut(hit.player) {
                if let Some(s) = p.soldiers.get_mut(hit.soldier) {
                    if s.alive {
                        s.alive = false;
                        if p.team == cur.team {
                            ally_hits += 1;
                        } else {
                            enemy_hits += 1;
                        }
                    }
                }
            }
        }

        let (ex, ey) = path.end_real;
        if ex.is_finite() && ey.is_finite() && ex >= 0.0 && ey >= 0.0 {
            // Java uses (int) casts here, not rounding.
            self.surface.white.push(Circle::new(
                (ex as i32) as f64,
                (ey as i32) as f64,
                EXPLOSION_R,
            ));
        }

        let report = FireReport {
            formula: formula.to_string(),
            angle: if self.mode == SND_ODE { angle } else { 0.0 },
            team: cur.team,
            path,
            enemy_hits,
            ally_hits,
        };
        self.last_fire = Some(report.clone());
        Ok(report)
    }

    pub fn path_reason_name(reason: EndReason) -> &'static str {
        match reason {
            EndReason::Terrain => "terrain",
            EndReason::Bounds => "bounds",
            EndReason::TooSteep => "too-steep",
            EndReason::Invalid => "nan",
            EndReason::MaxSteps => "max-steps",
        }
    }
}
