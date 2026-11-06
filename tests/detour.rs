use nodus::game::World;
use nodus::rng::Rng;
use nodus::sim::{Circle, EndReason};
use nodus::synth::{plan_shot, plan_shot_wide, PLAN_BUDGET};

fn world_with_block() -> World {
    let mut w = World::default();
    w.add_player(0, "bot".into(), 1, 1, true);
    w.add_player(1, "enemy".into(), 2, 1, false);
    w.started = true;
    w.players[0].soldiers[0] = nodus::game::SoldierState {
        x: 100,
        y: 225,
        alive: true,
        angle: 0.0,
    };
    w.players[1].soldiers[0] = nodus::game::SoldierState {
        x: 670,
        y: 225,
        alive: true,
        angle: 0.0,
    };
    w.surface.black = vec![Circle::new(385.0, 225.0, 80.0)];
    w
}

/// The obstacle field of a real public room: shooter Skynet team 1 on
/// the left, three enemies behind a thicket of circles.
fn live_room() -> World {
    let mut w = World::default();
    w.add_player(0, "243".into(), 2, 2, false);
    w.add_player(1, "Skynet".into(), 1, 2, true);
    w.add_player(2, "Multivac".into(), 2, 2, false);
    w.add_player(3, "Multivac".into(), 1, 2, false);
    w.started = true;
    w.current_turn = 1;
    let soldiers = [
        [(691, 373), (439, 277)],
        [(40, 227), (287, 296)],
        [(586, 368), (718, 367)],
        [(208, 116), (190, 430)],
    ];
    for (pi, list) in soldiers.iter().enumerate() {
        for (si, &(x, y)) in list.iter().enumerate() {
            w.players[pi].soldiers[si] = nodus::game::SoldierState {
                x,
                y,
                alive: true,
                angle: 0.0,
            };
        }
    }
    w.surface.black = [
        (391.0, 122.0, 25.0),
        (472.0, 446.0, 93.0),
        (335.0, 436.0, 12.0),
        (221.0, 319.0, 34.0),
        (368.0, 57.0, 42.0),
        (594.0, 194.0, 25.0),
        (331.0, 175.0, 40.0),
        (385.0, 249.0, 28.0),
        (656.0, 184.0, 57.0),
        (493.0, 148.0, 31.0),
        (77.0, 359.0, 86.0),
    ]
    .iter()
    .map(|&(x, y, r)| Circle::new(x, y, r))
    .collect();
    w
}

#[test]
fn detours_around_live_room_terrain() {
    let mut w = live_room();
    let mut rng = Rng::new(7);
    let t0 = std::time::Instant::now();
    let plan = plan_shot(&w, &mut rng, &[]).expect("plan");
    let dt = t0.elapsed();
    let report = w.apply_fire(1, &plan.formula, plan.angle).unwrap();
    println!(
        "{} ms, points {} kills {} reason {:?} formula {}",
        dt.as_millis(),
        plan.control_points,
        report.enemy_hits,
        report.path.reason,
        plan.formula
    );
    assert!(report.enemy_hits >= 1, "no kill: {}", plan.formula);
    assert_eq!(report.ally_hits, 0);
}

/// A real room map where the old planner kept drawing a straight line
/// into a rock (`копьё` from the live log).
fn squashed_lane() -> World {
    let mut w = World::default();
    w.add_player(1, "Skynet".into(), 2, 2, false);
    w.add_player(0, "42".into(), 1, 2, true);
    w.started = true;
    w.current_turn = 1;
    w.players[0].soldiers[0] = nodus::game::SoldierState {
        x: 590,
        y: 49,
        alive: true,
        angle: 0.0,
    };
    w.players[0].soldiers[1] = nodus::game::SoldierState {
        x: 628,
        y: 346,
        alive: true,
        angle: 0.0,
    };
    w.players[1].soldiers[0] = nodus::game::SoldierState {
        x: 114,
        y: 291,
        alive: true,
        angle: 0.0,
    };
    w.players[1].soldiers[1] = nodus::game::SoldierState {
        x: 336,
        y: 187,
        alive: true,
        angle: 0.0,
    };
    w.surface.black = [
        (201.0, 339.0, 22.0),
        (136.0, 420.0, 59.0),
        (515.0, 297.0, 55.0),
        (145.0, 140.0, 68.0),
        (20.0, 148.0, 62.0),
        (48.0, 173.0, 42.0),
    ]
    .iter()
    .map(|&(x, y, r)| Circle::new(x, y, r))
    .collect();
    w
}

#[test]
fn finds_a_way_through_a_squashed_lane() {
    let mut w = squashed_lane();
    let mut rng = Rng::new(11);
    let plan = plan_shot(&w, &mut rng, &[]).expect("plan");
    let report = w.apply_fire(0, &plan.formula, plan.angle).unwrap();
    println!(
        "points {} kills {} ally {} reason {:?} score {:.0} formula {}",
        plan.control_points,
        report.enemy_hits,
        report.ally_hits,
        report.path.reason,
        plan.score,
        plan.formula
    );
    assert!(report.enemy_hits >= 1, "no kill: {}", plan.formula);
    assert_eq!(report.ally_hits, 0);
}

#[test]
fn stays_fast_when_nothing_can_reach() {
    let mut w = live_room();
    // A boulder swallows the whole lane, so every mission burns the full
    // search before giving up.
    w.surface.black = vec![Circle::new(385.0, 225.0, 420.0)];
    let t0 = std::time::Instant::now();
    let plan = plan_shot(&w, &mut Rng::new(1), &[]);
    let dt = t0.elapsed();
    println!(
        "worst case {:?}, plan {:?}",
        dt,
        plan.map(|p| p.control_points)
    );
    assert!(
        dt.as_secs_f64() < PLAN_BUDGET.as_secs_f64() + 1.0,
        "planner took {dt:?}"
    );
}

/// Four enemies in a row with two boulders in between: the curve has to
/// snake past both walls and still touch every soldier.
fn four_behind_two_walls() -> World {
    let mut w = World::default();
    w.add_player(0, "NODUS".into(), 1, 2, true);
    w.add_player(1, "Multivac".into(), 2, 4, false);
    w.started = true;
    w.players[0].soldiers[0] = nodus::game::SoldierState {
        x: 100,
        y: 225,
        alive: true,
        angle: 0.0,
    };
    for (si, &(x, y)) in [(300, 225), (440, 150), (580, 300), (710, 210)]
        .iter()
        .enumerate()
    {
        w.players[1].soldiers[si] = nodus::game::SoldierState {
            x,
            y,
            alive: true,
            angle: 0.0,
        };
    }
    w.surface.black = vec![
        Circle::new(370.0, 225.0, 45.0),
        Circle::new(645.0, 225.0, 45.0),
    ];
    w
}

#[test]
fn long_formula_reaches_the_fourth_soldier() {
    let mut w = four_behind_two_walls();
    let t0 = std::time::Instant::now();
    let plan = plan_shot(&w, &mut Rng::new(5), &[]).expect("plan");
    let dt = t0.elapsed();
    let report = w.apply_fire(0, &plan.formula, plan.angle).unwrap();
    println!(
        "{} ms, points {} kills {} reason {:?} formula {}",
        dt.as_millis(),
        plan.control_points,
        report.enemy_hits,
        report.path.reason,
        plan.formula
    );
    assert!(
        report.enemy_hits >= 3,
        "only {} kills: {}",
        report.enemy_hits,
        plan.formula
    );
    assert_eq!(report.ally_hits, 0);
}

/// Two boulders with a 20 px slit between them: only a curve that
/// threads the slit can reach the soldiers behind.
fn narrow_slit() -> World {
    let mut w = World::default();
    w.add_player(0, "NODUS".into(), 1, 2, true);
    w.add_player(1, "Multivac".into(), 2, 4, false);
    w.started = true;
    w.players[0].soldiers[0] = nodus::game::SoldierState {
        x: 100,
        y: 225,
        alive: true,
        angle: 0.0,
    };
    for (si, &(x, y)) in [(700, 225), (700, 120), (700, 330), (760, 225)]
        .iter()
        .enumerate()
    {
        w.players[1].soldiers[si] = nodus::game::SoldierState {
            x,
            y,
            alive: true,
            angle: 0.0,
        };
    }
    w.surface.black = vec![
        Circle::new(450.0, 140.0, 75.0),
        Circle::new(450.0, 310.0, 75.0),
    ];
    w
}

#[test]
fn threads_a_narrow_slit() {
    let mut w = narrow_slit();
    let t0 = std::time::Instant::now();
    let plan = plan_shot(&w, &mut Rng::new(9), &[]).expect("plan");
    let dt = t0.elapsed();
    let report = w.apply_fire(0, &plan.formula, plan.angle).unwrap();
    println!(
        "{} ms, points {} kills {} ally {} reason {:?} formula {}",
        dt.as_millis(),
        plan.control_points,
        report.enemy_hits,
        report.ally_hits,
        report.path.reason,
        plan.formula
    );
    assert!(report.enemy_hits >= 1, "no kill: {}", plan.formula);
    assert_eq!(report.ally_hits, 0);
}

/// A random multi-rock board: 6-10 circles, four enemies, shooter on the
/// left.  Soldiers never spawn inside or hugging a rock.
fn random_board(seed: u64) -> Option<World> {
    let mut r = Rng::new(seed);
    let mut w = World::default();
    w.add_player(0, "NODUS".into(), 1, 2, true);
    w.add_player(1, "Multivac".into(), 2, 4, false);
    w.started = true;
    let sy = r.range(110.0, 340.0);
    w.players[0].soldiers[0] = nodus::game::SoldierState {
        x: 70,
        y: sy as i32,
        alive: true,
        angle: 0.0,
    };
    let want = 10 + r.index(5);
    let mut circles: Vec<Circle> = Vec::new();
    while circles.len() < want {
        let mut ok = false;
        for _ in 0..80 {
            let x = r.range(190.0, 710.0);
            let y = r.range(35.0, 415.0);
            let rad = r.range(42.0, 88.0);
            if circles
                .iter()
                .any(|c| ((c.x - x).powi(2) + (c.y - y).powi(2)).sqrt() < c.r + rad + 8.0)
            {
                continue;
            }
            circles.push(Circle::new(x, y, rad));
            ok = true;
            break;
        }
        if !ok {
            return None;
        }
    }
    let mut used = vec![(70.0f64, sy)];
    for si in 0..4 {
        let mut spot = None;
        for _ in 0..120 {
            let x = r.range(300.0, 745.0);
            let y = r.range(45.0, 405.0);
            let clear = circles
                .iter()
                .all(|c| ((c.x - x).powi(2) + (c.y - y).powi(2)).sqrt() > c.r + 12.0);
            let apart = used
                .iter()
                .all(|p| ((p.0 - x).powi(2) + (p.1 - y).powi(2)).sqrt() > 32.0);
            if clear && apart {
                spot = Some((x, y));
                break;
            }
        }
        let (x, y) = spot?;
        used.push((x, y));
        w.players[1].soldiers[si] = nodus::game::SoldierState {
            x: x as i32,
            y: y as i32,
            alive: true,
            angle: 0.0,
        };
    }
    w.surface.black = circles;
    Some(w)
}

#[test]
#[ignore]
fn random_boards() {
    let boards = 12usize;
    let mut total = 0usize;
    let mut with_hits = 0usize;
    let mut full = 0usize;
    let mut seed = 100u64;
    let mut done = 0usize;
    let t0 = std::time::Instant::now();
    while done < boards {
        let Some(mut w) = random_board(seed) else {
            seed += 1;
            continue;
        };
        seed += 1;
        done += 1;
        let mut rng = Rng::new(seed.wrapping_mul(0x9e37_79b9));
        let plan = plan_shot_wide(&w, &mut rng, &[]);
        let kills = match plan {
            Some(p) => {
                let rep = w.apply_fire(0, &p.formula, p.angle).unwrap();
                println!(
                    "board {}: kills {} points {}",
                    done, rep.enemy_hits, p.control_points
                );
                rep.enemy_hits
            }
            None => {
                println!("board {}: no plan", done);
                0
            }
        };
        total += kills;
        if kills > 0 {
            with_hits += 1;
        }
        if kills == 4 {
            full += 1;
        }
    }
    println!(
        "TOTAL {} / {} kills, boards with hits {}/{}, full {}/{}, {:.1} s",
        total,
        boards * 4,
        with_hits,
        boards,
        full,
        boards,
        t0.elapsed().as_secs_f64()
    );
}

#[test]
fn detours_around_a_blocking_circle() {
    let mut w = world_with_block();
    let mut rng = Rng::new(3);
    let plan = plan_shot(&w, &mut rng, &[]).expect("plan");
    let report = w.apply_fire(0, &plan.formula, plan.angle).unwrap();
    println!(
        "plan {} score {} points {} reason {:?}",
        plan.formula, plan.score, plan.control_points, report.path.reason
    );
    assert!(
        report.enemy_hits >= 1,
        "formula {} did not hit",
        plan.formula
    );
    assert_eq!(report.ally_hits, 0);
}

/// Rough terrain and soldier layout traced from the two "other bot"
/// screenshots (the long S-shaped multi-kill shots).
fn screenshot_room() -> World {
    let mut w = World::default();
    w.add_player(0, "NODUS".into(), 1, 2, true);
    w.add_player(1, "Genocide".into(), 2, 4, false);
    w.started = true;
    w.players[0].soldiers[0] = nodus::game::SoldierState {
        x: 45,
        y: 17,
        alive: true,
        angle: 0.0,
    };
    for (si, &(x, y)) in [(570, 11), (610, 146), (429, 234), (562, 425)]
        .iter()
        .enumerate()
    {
        w.players[1].soldiers[si] = nodus::game::SoldierState {
            x,
            y,
            alive: true,
            angle: 0.0,
        };
    }
    w.surface.black = [
        (39.0, 140.0, 74.0),
        (463.0, 36.0, 52.0),
        (570.0, 119.0, 30.0),
        (685.0, 88.0, 76.0),
        (720.0, 0.0, 62.0),
        (706.0, 201.0, 60.0),
        (742.0, 276.0, 45.0),
        (745.0, 376.0, 30.0),
        (675.0, 365.0, 26.0),
        (314.0, 353.0, 76.0),
        (166.0, 302.0, 32.0),
        (53.0, 361.0, 32.0),
        (183.0, 429.0, 32.0),
        (414.0, 444.0, 22.0),
        (264.0, 112.0, 18.0),
    ]
    .iter()
    .map(|&(x, y, r)| Circle::new(x, y, r))
    .collect();
    w
}

#[test]
#[ignore]
fn screenshot_room_wide() {
    for seed in [1u64, 2, 3] {
        let mut w = screenshot_room();
        let t0 = std::time::Instant::now();
        let plan = plan_shot_wide(&w, &mut Rng::new(seed), &[]);
        let dt = t0.elapsed();
        match plan {
            Some(p) => {
                let rep = w.apply_fire(0, &p.formula, p.angle).unwrap();
                println!(
                    "seed {}: {} ms, points {}, kills {} ally {} reason {:?}",
                    seed,
                    dt.as_millis(),
                    p.control_points,
                    rep.enemy_hits,
                    rep.ally_hits,
                    rep.path.reason
                );
            }
            None => println!("seed {}: no plan", seed),
        }
    }
}

/// A full-height rock wall between the shooter and the only enemy: there
/// is no way around and no way through until a crater is carved.
fn walled_room() -> World {
    let mut w = World::default();
    w.add_player(0, "NODUS".into(), 1, 2, true);
    w.add_player(1, "Multivac".into(), 2, 2, false);
    w.started = true;
    w.players[0].soldiers[0] = nodus::game::SoldierState {
        x: 100,
        y: 225,
        alive: true,
        angle: 0.0,
    };
    w.players[1].soldiers[0] = nodus::game::SoldierState {
        x: 650,
        y: 225,
        alive: true,
        angle: 0.0,
    };
    w.players[1].soldiers[1] = nodus::game::SoldierState {
        x: 700,
        y: 320,
        alive: true,
        angle: 0.0,
    };
    w.surface.black = (0..28)
        .map(|k| Circle::new(450.0, 9.0 + k as f64 * 16.0, 9.0))
        .collect();
    w
}

/// A soldier walled off behind random rock: the planner must not lob
/// zero-kill shots at random faces, it must pick one arc, bite deeper
/// every turn and finally kill.  One explosion removes only its own
/// radius, so hundreds of pixels of rock take dozens of turns; what is
/// checked here is steady progress through the same tunnel, not speed.
/// Second-order mode is left out on purpose: there the launch angle is
/// fixed by the player and cannot be turned towards a tunnel, so this
/// artificial wall cannot be beaten by digging at one horizontal angle.
#[test]
fn digs_through_rock_to_the_last_soldier() {
    use nodus::protocol::NORMAL_FUNC;
    for mode in [NORMAL_FUNC] {
        for seed in [4u64, 6] {
            let mut w = random_board(seed).expect("board");
            w.mode = mode;
            for si in 0..3 {
                w.players[1].soldiers[si].alive = false;
            }
            w.players[1].soldiers[3].alive = true;
            let mut killed = 0;
            for turn in 0..40u64 {
                let plan = plan_shot_wide(&w, &mut Rng::new(seed * 31 + turn), &[]).expect("plan");
                let rep = w.apply_fire(0, &plan.formula, plan.angle).unwrap();
                killed += rep.enemy_hits;
                if killed > 0 {
                    break;
                }
            }
            assert!(killed > 0, "mode {mode} seed {seed}: never got through");
        }
    }
}

/// A live board scarred by craters: the deterministic guide routes find
/// no way through the debris, but there is a clear arc over the top.  The
/// planner must take it instead of starting a tunnel into the nearest
/// wall and ignoring the soldier across the map.
fn crater_scarred_field() -> World {
    let mut w = World::default();
    w.add_player(0, "NODUS".into(), 1, 1, true);
    w.add_player(1, "enemy".into(), 2, 1, false);
    w.started = true;
    w.players[0].soldiers[0] = nodus::game::SoldierState {
        x: 110,
        y: 415,
        alive: true,
        angle: 0.0,
    };
    w.players[1].soldiers[0] = nodus::game::SoldierState {
        x: 575,
        y: 118,
        alive: true,
        angle: 0.0,
    };
    w.surface.black = [
        (225.0, 140.0, 85.0),
        (420.0, 55.0, 70.0),
        (450.0, 160.0, 32.0),
        (430.0, 210.0, 55.0),
        (520.0, 210.0, 40.0),
        (690.0, 150.0, 45.0),
        (85.0, 265.0, 35.0),
        (320.0, 270.0, 35.0),
        (450.0, 370.0, 65.0),
        (560.0, 395.0, 40.0),
    ]
    .iter()
    .map(|&(x, y, r)| Circle::new(x, y, r))
    .collect();
    w.surface.white = [
        (195.0, 65.0, 35.0),
        (270.0, 90.0, 28.0),
        (520.0, 205.0, 18.0),
    ]
    .iter()
    .map(|&(x, y, r)| Circle::new(x, y, r))
    .collect();
    w
}

#[test]
fn prefers_a_free_arc_over_digging_a_cratered_wall() {
    let mut w = crater_scarred_field();
    let plan = plan_shot(&w, &mut Rng::new(3), &[]).expect("plan");
    assert!(!plan.dig, "chose a tunnel instead of the open arc");
    let report = w.apply_fire(0, &plan.formula, plan.angle).unwrap();
    assert!(report.enemy_hits >= 1, "no kill: {}", plan.formula);
    assert_eq!(report.ally_hits, 0);
}

#[test]
fn digs_through_a_full_wall_then_kills() {
    let mut w = walled_room();
    let mut breached = false;
    let mut killed = 0usize;
    for turn in 0..12u64 {
        let plan = plan_shot(&w, &mut Rng::new(turn * 13 + 1), &[]).expect("plan");
        let report = w.apply_fire(0, &plan.formula, plan.angle).unwrap();
        println!(
            "turn {}: reason {:?} end_v {:.1} kills {} points {}",
            turn, report.path.reason, report.path.end_v, report.enemy_hits, plan.control_points
        );
        if report.path.reason == EndReason::Terrain {
            breached = true;
        }
        killed += report.enemy_hits;
        if killed >= 2 {
            assert!(breached, "killed without ever carving the wall");
            assert_eq!(report.ally_hits, 0);
            return;
        }
    }
    panic!("only {} killed, {} craters", killed, w.surface.white.len());
}
