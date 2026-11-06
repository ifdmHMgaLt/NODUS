//! One proxied game connection: protocol state, turn handling, firing.

use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nodus::ascii;
use nodus::expr::Expr;
use nodus::game::World;
use nodus::protocol::*;
use nodus::rng::Rng;
use nodus::synth;

use crate::cli::Opts;

pub(crate) struct Shared {
    pub(crate) debug: bool,
    /// The human pressed fire before us; stop aiming.
    pub(crate) client_fired: AtomicBool,
    /// Graphwar closed the proxy connection.
    pub(crate) client_closed: AtomicBool,
    /// Player ids the client has acted as.  Public room servers mark every
    /// player as "local" for every client, so the outbound frames are the
    /// only trustworthy source of "who am I".
    own_ids: Mutex<Vec<i32>>,
    /// Names the client sent when joining, used until an id shows up.
    own_names: Mutex<Vec<String>>,
}

impl Shared {
    pub(crate) fn new(debug: bool) -> Self {
        Self {
            debug,
            client_fired: AtomicBool::new(false),
            client_closed: AtomicBool::new(false),
            own_ids: Mutex::new(Vec::new()),
            own_names: Mutex::new(Vec::new()),
        }
    }

    /// The human is the first player this client announced.  Everything the
    /// client adds afterwards (computer opponents in an offline room) also
    /// belongs to it but must not be played by the bot.
    fn human_name(&self) -> Option<String> {
        self.own_names.lock().ok().and_then(|g| g.first().cloned())
    }

    fn human_id(&self) -> Option<i32> {
        self.own_ids.lock().ok().and_then(|g| g.first().copied())
    }

    pub(crate) fn note_own_id(&self, id: i32) {
        if let Ok(mut g) = self.own_ids.lock() {
            if !g.contains(&id) {
                g.push(id);
            }
        }
    }

    pub(crate) fn note_own_name(&self, name: &str) {
        if name.is_empty() {
            return;
        }
        if let Ok(mut g) = self.own_names.lock() {
            if !g.iter().any(|n| n == name) {
                g.push(name.to_string());
            }
        }
    }
}

pub(crate) struct Session {
    label: String,
    world: World,
    rng: Rng,
    opts: Opts,
    up: Arc<Mutex<TcpStream>>,
    shared: Arc<Shared>,
    act_at: Option<Instant>,
    fired: bool,
    fire_seen: bool,
    input: Vec<u8>,
    /// Formulas already fired this game, newest last.
    fired_formulas: Vec<String>,
    /// Shot being planned in the background for the current board.
    prefetch: Option<Prefetch>,
}

/// A worker that plans the next shot while the opponents are thinking.  Its
/// result is only used while `key` still matches the live world.  The search
/// keeps running on every core until the turn actually starts, so the plan
/// is always the best one found so far, not a fixed-size sample.
struct Prefetch {
    key: u64,
    live: Arc<synth::LiveSearch>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

impl Session {
    pub(crate) fn new(
        label: String,
        opts: Opts,
        up: Arc<Mutex<TcpStream>>,
        shared: Arc<Shared>,
    ) -> Self {
        Self {
            label,
            world: World::default(),
            rng: Rng::new(0x9e37_79b9_7f4a_7c15),
            opts,
            up,
            shared,
            act_at: None,
            fired: false,
            fire_seen: false,
            input: Vec::new(),
            fired_formulas: Vec::new(),
            prefetch: None,
        }
    }

    pub(crate) fn label(&self) -> &str {
        &self.label
    }

    pub(crate) fn client_closed(&self) -> bool {
        self.shared.client_closed.load(Ordering::SeqCst)
    }

    fn note_fired(&mut self, formula: &str) {
        self.fired_formulas.push(formula.to_string());
        if self.fired_formulas.len() > 12 {
            self.fired_formulas.remove(0);
        }
    }

    fn inject(&self, line: &str) {
        let mut up = match self.up.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let _ = up.write_all(line.as_bytes());
        let _ = up.write_all(b"\n");
        let _ = up.flush();
    }

    pub(crate) fn feed(&mut self, chunk: &[u8]) {
        self.input.extend_from_slice(chunk);
        while let Some(pos) = self.input.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.input.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let text = String::from_utf8_lossy(&line).into_owned();
            if !text.is_empty() {
                self.handle(&text);
            }
        }
    }

    fn dump_world(&self) {
        println!(
            "[{}] world mode={} turn={}",
            self.label, self.world.mode, self.world.current_turn
        );
        for (i, p) in self.world.players.iter().enumerate() {
            let sols: Vec<String> = p
                .soldiers
                .iter()
                .map(|s| format!("({},{},alive={})", s.x, s.y, s.alive))
                .collect();
            println!(
                "[{}] p{i} id={} name={} team={} local={} soldiers={}",
                self.label,
                p.id,
                p.name,
                p.team,
                p.local,
                sols.join(" ")
            );
        }
        let mut circles = String::new();
        for c in &self.world.surface.black {
            circles.push_str(&format!("({:.1},{:.1},{:.1}) ", c.x, c.y, c.r));
        }
        println!("[{}] circles {circles}", self.label);
    }

    /// Whether `id`/`name` is a player this client controls.  The public
    /// servers mark every player as local, so prefer what the client itself
    /// said in its outbound frames and only fall back to the server flag
    /// while nothing is known yet.
    fn is_ours(&self, id: i32, name: &str, server_local: bool) -> bool {
        if let Some(human) = self.shared.human_name() {
            return human == name;
        }
        if let Some(human) = self.shared.human_id() {
            return human == id;
        }
        server_local
    }

    /// Re-mark the roster after the client reveals its identity mid-game.
    fn refresh_local(&mut self) {
        let name = self.shared.human_name();
        let id = self.shared.human_id();
        if name.is_none() && id.is_none() {
            return;
        }
        let mut mine = Vec::new();
        for p in self.world.players.iter_mut() {
            p.local = match (&name, id) {
                (Some(n), _) => *n == p.name,
                (None, Some(i)) => i == p.id,
                _ => p.local,
            };
            if p.local {
                mine.push(p.id);
            }
        }
        self.world.local_ids = mine;
    }

    /// Malformed frames are dropped silently: the server sends incomplete
    /// state while a room is joining, and the client ignores it too.
    fn handle(&mut self, line: &str) {
        if self.opts.debug {
            println!("[{}] << {line}", self.label);
        }
        let p = split(line);
        if p.is_empty() {
            return;
        }
        let ty: i32 = match p[0].parse() {
            Ok(v) => v,
            Err(_) => return,
        };
        match ty {
            NO_INFO | TIME_UP => {}
            CHAT_MSG => {
                if p.len() >= 3 {
                    println!("[чат] {}", url_decode(&p[2]));
                }
            }
            ADD_PLAYER => {
                if p.len() < 7 {
                    return;
                }
                let id = match p[1].parse() {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let name = url_decode(&p[2]);
                let team = p[3].parse().unwrap_or(1);
                let local = self.is_ours(id, &name, p[4] == "1");
                let num: usize = p[5].parse().unwrap_or(2);
                if self.world.players.iter().any(|q| q.id == id) {
                    self.world.set_team(id, team);
                    self.world.set_num_soldiers(id, num);
                    if let Some(pl) = self.world.players.iter_mut().find(|q| q.id == id) {
                        pl.local = local;
                    }
                } else {
                    self.world.add_player(id, name.clone(), team, num, local);
                }
                if local {
                    println!("ваш игрок: {name} (id={id}, команда {team}, солдат {num})");
                } else {
                    println!("в комнате: {name} (id={id}, команда {team}, солдат {num})");
                }
            }
            SET_TEAM => {
                if p.len() >= 3 {
                    if let (Ok(team), Ok(id)) = (p[1].parse(), p[2].parse()) {
                        self.world.set_team(id, team);
                    }
                }
            }
            REMOVE_PLAYER => {
                if p.len() >= 2 {
                    if let Ok(id) = p[1].parse() {
                        self.world.remove_player(id);
                    }
                }
            }
            ADD_SOLDIER => {
                if p.len() >= 2 {
                    if let Ok(id) = p[1].parse() {
                        self.world.increment_soldiers(id);
                    }
                }
            }
            REMOVE_SOLDIER => {
                if p.len() >= 2 {
                    if let Ok(id) = p[1].parse() {
                        self.world.decrement_soldiers(id);
                    }
                }
            }
            SET_MODE => {
                if p.len() >= 2 {
                    if let Ok(mode) = p[1].parse() {
                        self.world.set_mode(mode);
                        println!("режим: {}", mode_name(mode));
                    }
                }
            }
            REORDER => {
                let ids: Vec<i32> = p[1..].iter().filter_map(|s| s.parse().ok()).collect();
                self.world.reorder(&ids);
            }
            START_GAME => self.on_start_game(&p),
            NEXT_TURN => {
                self.world.advance_turn();
                self.begin_turn();
            }
            FIRE_FUNC => self.on_fire_func(&p),
            SET_ANGLE => {
                if p.len() >= 4 {
                    if let (Ok(id), Ok(idx), Ok(angle)) = (
                        p[1].parse::<i32>(),
                        p[2].parse::<usize>(),
                        p[3].parse::<f64>(),
                    ) {
                        self.world.set_angle(id, idx, angle);
                    }
                }
            }
            GAME_FINISHED => {
                println!("игра окончена");
                self.world.started = false;
                self.world.finished = true;
                self.act_at = None;
                self.fired = false;
                self.fire_seen = false;
                if let Some(p) = &self.prefetch {
                    p.stop.store(true, Ordering::SeqCst);
                }
                self.prefetch = None;
            }
            GAME_FULL => println!("комната заполнена"),
            DISCONNECT => println!("сервер разорвал соединение"),
            NEW_LEADER => println!("ваш клиент теперь лидер комнаты"),
            START_COUNTDOWN => println!("отсчёт до старта"),
            _ => {}
        }
    }

    fn on_start_game(&mut self, p: &[String]) {
        if p.len() < 3 {
            return;
        }
        let num_circles: usize = match p[1].parse() {
            Ok(v) => v,
            Err(_) => return,
        };
        let need = 2 + 3 * num_circles;
        if p.len() < need + 1 {
            return;
        }
        let mut circles = Vec::with_capacity(num_circles);
        let mut k = 2;
        for _ in 0..num_circles {
            let x = p[k].parse::<f64>().unwrap_or(0.0);
            let y = p[k + 1].parse::<f64>().unwrap_or(0.0);
            let r = p[k + 2].parse::<f64>().unwrap_or(0.0);
            circles.push(nodus::sim::Circle::new(x, y, r));
            k += 3;
        }
        let start_raw: i32 = p[p.len() - 1].parse().unwrap_or(0);
        let mut coords = Vec::new();
        for s in &p[k..p.len() - 1] {
            coords.push(s.parse::<i32>().unwrap_or(0));
        }
        match self.world.start_game(circles, &coords, start_raw) {
            Ok(()) => {
                println!(
                    "старт: {} игроков, {} солдат; первым ходит {}",
                    self.world.players.len(),
                    coords.len() / 2,
                    self.world
                        .current_player()
                        .map(|q| q.name.as_str())
                        .unwrap_or("?")
                );
                if self.opts.debug {
                    self.dump_world();
                }
                self.fired_formulas.clear();
                self.begin_turn();
            }
            Err(e) => eprintln!("START_GAME: {e}"),
        }
    }

    fn begin_turn(&mut self) {
        self.act_at = None;
        self.fired = false;
        self.fire_seen = false;
        self.shared.client_fired.store(false, Ordering::SeqCst);
        self.refresh_local();
        if self.world.started && self.world.current_is_local() {
            let name = self
                .world
                .current_player()
                .map(|p| p.name.clone())
                .unwrap_or_default();
            println!("ход: {name} — считаю");
            self.act_at = Some(Instant::now() + Duration::from_millis(self.opts.delay_ms));
        }
    }

    fn on_fire_func(&mut self, p: &[String]) {
        if p.len() < 3 || !self.world.started {
            return;
        }
        let sid: i32 = match p[1].parse() {
            Ok(v) => v,
            Err(_) => return,
        };
        let formula = url_decode(&p[2]);
        let Some(cur) = self.world.current_player().cloned() else {
            return;
        };
        if cur.id != sid || self.fire_seen {
            return;
        }
        self.fire_seen = true;
        self.fired = true;
        self.act_at = None;
        let angle = cur.current_soldier().map(|s| s.angle).unwrap_or(0.0);
        println!("выстрел: {} «{}»", cur.name, formula);
        match self.world.apply_fire(sid, &formula, angle) {
            Ok(r) => println!(
                "  попаданий: {} по врагам, {} по своим; обрыв: {}",
                r.enemy_hits,
                r.ally_hits,
                World::path_reason_name(r.path.reason)
            ),
            Err(e) => eprintln!("  не удалось пересчитать выстрел: {e}"),
        }
    }

    pub(crate) fn tick(&mut self) {
        if self.shared.client_fired.swap(false, Ordering::SeqCst) {
            self.act_at = None;
        }
        self.refresh_local();
        self.maybe_prefetch();
        let due = matches!(self.act_at, Some(t) if Instant::now() >= t);
        if due {
            self.act_at = None;
            if self.world.started
                && self.world.current_is_local()
                && !self.fired
                && !self.shared.client_fired.load(Ordering::SeqCst)
            {
                self.take_shot();
            }
        }
    }

    /// Crunch the next shot ahead of time.  Every board change starts a fresh
    /// multi-core search; the previous one is told to stop.  It is not a
    /// one-shot worker: it keeps testing new curves for as long as the
    /// opponents take to move.
    fn maybe_prefetch(&mut self) {
        if !self.world.started || self.world.finished {
            return;
        }
        let key = synth::state_key(&self.world);
        if let Some(p) = &self.prefetch {
            if p.key == key {
                return;
            }
            p.stop.store(true, Ordering::SeqCst);
        }
        let world = self.world.clone();
        let avoid = self.fired_formulas.clone();
        let live = Arc::new(synth::LiveSearch::new());
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let live_w = Arc::clone(&live);
        let stop_w = Arc::clone(&stop);
        let done_w = Arc::clone(&done);
        let seed = key ^ 0x9e37_79b9_7f4a_7c15;
        thread::spawn(move || {
            synth::plan_shot_streaming(&world, seed, &avoid, &stop_w, &live_w);
            done_w.store(true, Ordering::SeqCst);
        });
        self.prefetch = Some(Prefetch {
            key,
            live,
            stop,
            done,
        });
    }

    fn take_shot(&mut self) {
        let Some(cur) = self.world.current_player().cloned() else {
            return;
        };
        if !cur.local {
            return;
        }
        let client_angle = cur.current_soldier().map(|s| s.angle).unwrap_or(0.0);
        println!(
            "\nход: {} · {} · {}",
            cur.name,
            mode_name(self.world.mode),
            mode_formula(self.world.mode)
        );

        let (ready, variants) = self.wait_for_prefetch();
        // The background workers have had their window; the foreground planner
        // needs every core now, and their result can no longer be used.
        self.stop_prefetch();
        let planned = self.resolve_plan(ready, variants, cur.team);
        self.validate_and_send(cur.id, cur.team, client_angle, planned);
    }

    /// Wait for the background search of this exact board.  The game allows a
    /// minute per turn, so give it up to ten seconds to sharpen the plan, but
    /// stop once the best plan has not improved for a while.
    fn wait_for_prefetch(&self) -> (Option<synth::ShotPlan>, u64) {
        let key = synth::state_key(&self.world);
        let Some(p) = self.prefetch.as_ref().filter(|p| p.key == key) else {
            return (None, 0);
        };
        let deadline = Instant::now() + Duration::from_millis(10_000);
        let mut last_score = f64::NEG_INFINITY;
        let mut last_change = Instant::now();
        loop {
            let plan = p.live.best();
            // A tunnel is not an answer, it is a fallback.  Keep the search
            // running until something actually dies: stopping the clock on a
            // dig is how the bot ended up drilling one wall while the rest of
            // the map was still unexplored.
            let kill = plan.as_ref().filter(|q| q.kills > 0);
            let score = kill.map(|q| q.score).unwrap_or(f64::NEG_INFINITY);
            if score > last_score {
                last_score = score;
                last_change = Instant::now();
            }
            let now = Instant::now();
            if p.done.load(Ordering::SeqCst)
                || p.live.solved.load(Ordering::SeqCst)
                || now >= deadline
                || (kill.is_some() && now.duration_since(last_change) > Duration::from_millis(5000))
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        (p.live.best(), p.live.count())
    }

    fn stop_prefetch(&self) {
        if let Some(p) = &self.prefetch {
            p.stop.store(true, Ordering::SeqCst);
        }
    }

    /// Pick between the background plan and a fresh foreground run.  Returns
    /// `None` when nothing was found and the bot has to fire straight.
    fn resolve_plan(
        &mut self,
        ready: Option<synth::ShotPlan>,
        variants: u64,
        team: i32,
    ) -> Option<synth::ShotPlan> {
        let alive_enemies = self
            .world
            .players
            .iter()
            .filter(|p| p.team != team)
            .flat_map(|p| p.soldiers.iter())
            .filter(|s| s.alive)
            .count();
        let t0 = Instant::now();
        let planned = match ready {
            Some(plan) if plan.kills > 0 => {
                println!("  готовый план из фона: {variants} вариантов");
                // The background beam found some soldiers but not all.  The
                // full planner walks every mission in turn, so give it one
                // last word: a longer formula may still sweep the rest.
                if plan.kills < alive_enemies {
                    if let Some(better) =
                        synth::plan_shot(&self.world, &mut self.rng, &self.fired_formulas)
                            .filter(|a| a.kills > plan.kills)
                    {
                        println!("  доискал: убито {} из {}", better.kills, alive_enemies);
                        Some(better)
                    } else {
                        Some(plan)
                    }
                } else {
                    Some(plan)
                }
            }
            other => {
                // A background curve that hits nothing is never better than
                // the full planner: it also knows how to dig through rock.
                // A background tunnel is held back on purpose.  Drilling is
                // the very last resort and may only be chosen after every
                // mission on the whole map has been tried — otherwise the
                // worker that found a crater first makes the bot pound one
                // wall while other soldiers stand in the open elsewhere.
                if let Some(p) = &other {
                    println!(
                        "  из фона без попаданий{} ({} вариантов) — доискиваю",
                        if p.dig {
                            ", только подкоп"
                        } else {
                            ""
                        },
                        variants
                    );
                }
                synth::plan_shot(&self.world, &mut self.rng, &self.fired_formulas)
            }
        };
        println!("  расчёт: {:.2} c", t0.elapsed().as_secs_f64());
        planned
    }

    /// Re-check the rendered string, not just the fitted curve: every client
    /// integrates the string, and rounding can change the hits.
    fn validate_and_send(
        &mut self,
        shooter: i32,
        team: i32,
        client_angle: f64,
        planned: Option<synth::ShotPlan>,
    ) {
        let mut planned_kills = 0usize;
        let (formula, angle) = match planned {
            Some(plan) => {
                planned_kills = plan.kills;
                println!(
                    "  цели: убито {}, задето своих {}, контрольных точек {}, оценка {:.0}",
                    plan.kills, plan.ally_hits, plan.control_points, plan.score
                );
                if self.opts.map {
                    println!("{}", ascii::render(&self.world, Some(&plan), 104, 34));
                }
                (plan.formula, plan.angle)
            }
            None => {
                println!("  синтез не удался — стреляю прямой");
                (String::from("0"), 0.0)
            }
        };

        if self.world.mode == SND_ODE && (angle - client_angle).abs() > 1e-6 {
            eprintln!(
                "  ваш угол {:.3} рад вне рабочего диапазона; выровняйте его и повторите",
                client_angle
            );
            self.fired = true;
            return;
        }

        let formula = if formula_ok(&formula) && Expr::parse(&formula).is_ok() {
            formula
        } else {
            eprintln!("  формула не прошла проверку — стреляю прямой");
            String::from("0")
        };
        let angle = if angle.is_finite() { angle } else { 0.0 };

        match self.world.simulate_fire(shooter, &formula, angle) {
            Ok(path) => {
                let mut enemy = 0usize;
                let mut ally = 0usize;
                for h in &path.hits {
                    if let Some(pl) = self.world.players.get(h.player) {
                        if pl.team == team {
                            ally += 1;
                        } else {
                            enemy += 1;
                        }
                    }
                }
                if enemy != planned_kills {
                    println!(
                        "  проверка по строке: {} по врагам, {} по своим (план: {})",
                        enemy, ally, planned_kills
                    );
                }
            }
            Err(e) => eprintln!("  проверка формулы не удалась: {e}"),
        }

        println!("  {formula}\n");
        self.fired = true;
        self.note_fired(&formula);

        if !self.opts.auto {
            println!("  --manual: формула не отправлена");
            return;
        }
        // In second-order mode the client fires along its own soldier angle;
        // the curve was fitted to exactly that angle, so no SET_ANGLE is sent.
        let msg = format!("{FIRE_FUNC}&{}&{}", shooter, url_encode(&formula));
        self.inject(&msg);
        println!("  выстрел отправлен");
    }
}

/// Cheap structural check before the real parser runs.  Every letter is let
/// through here; `Expr::parse` is what rejects unknown names and functions.
fn formula_ok(s: &str) -> bool {
    if s.is_empty() || s.len() > 1_000_000 {
        return false;
    }
    let mut depth = 0i32;
    for b in s.bytes() {
        match b {
            b'0'..=b'9'
            | b'.'
            | b','
            | b'\''
            | b'+'
            | b'-'
            | b'*'
            | b'/'
            | b'^'
            | b' '
            | b'a'..=b'z'
            | b'A'..=b'Z' => {}
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    depth == 0
}

fn mode_name(mode: i32) -> &'static str {
    match mode {
        FST_ODE => "первый порядок",
        SND_ODE => "второй порядок",
        _ => "обычный",
    }
}

fn mode_formula(mode: i32) -> &'static str {
    match mode {
        FST_ODE => "y' = f(x, y)",
        SND_ODE => "y'' = f(x, y, y')",
        _ => "y = f(x)",
    }
}
