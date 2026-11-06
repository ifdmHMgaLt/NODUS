use nodus::expr::Expr;
use nodus::game::World;
use nodus::protocol::{FST_ODE, NORMAL_FUNC, SND_ODE};
use nodus::rng::Rng;
use nodus::sim::{
    simulate_first_order, simulate_normal_fn, simulate_second_order, Frame, SoldierRef, Surface,
    TEAM1, TEAM2,
};
use nodus::synth::{
    build_curve, fit_rbf, formula_for, make_missions, plan_shot, Target, MISSION_CAP,
};

fn demo_world(team: i32) -> World {
    let mut w = World::default();
    w.add_player(0, "bot".into(), team, 1, true);
    w.add_player(1, "enemy".into(), 3 - team, 2, false);
    w.started = true;
    let (sx, ex1, ex2) = if team == TEAM1 {
        (100.0, 520.0, 640.0)
    } else {
        (670.0, 250.0, 130.0)
    };
    w.players[0].soldiers[0] = nodus::game::SoldierState {
        x: sx as i32,
        y: 225,
        alive: true,
        angle: 0.0,
    };
    w.players[1].soldiers[0] = nodus::game::SoldierState {
        x: ex1 as i32,
        y: 180,
        alive: true,
        angle: 0.0,
    };
    w.players[1].soldiers[1] = nodus::game::SoldierState {
        x: ex2 as i32,
        y: 250,
        alive: true,
        angle: 0.0,
    };
    w
}

#[test]
fn planner_kills_enemies_in_every_mode() {
    for mode in [NORMAL_FUNC, FST_ODE, SND_ODE] {
        for team in [TEAM1, TEAM2] {
            // Second-order shots inherit the client's launch angle, which
            // is zero until the human touches the angle control.
            let angles: &[f64] = if mode == SND_ODE { &[0.0, 0.7] } else { &[0.0] };
            for &launch in angles {
                let mut w = demo_world(team);
                w.mode = mode;
                w.players[0].soldiers[0].angle = launch;
                let mut rng = Rng::new(7);
                let plan = plan_shot(&w, &mut rng, &[]).expect("plan");
                if mode == SND_ODE {
                    assert!((plan.angle - launch).abs() < 1e-12, "angle not fixed");
                }
                let report = w.apply_fire(0, &plan.formula, plan.angle).unwrap();
                assert_eq!(
                    report.ally_hits, 0,
                    "mode {mode} team {team} angle {launch}: {}",
                    plan.formula
                );
                assert!(
                    report.enemy_hits >= 1,
                    "mode {mode} team {team} angle {launch}: {} kills, formula {}",
                    report.enemy_hits,
                    plan.formula
                );
            }
        }
    }
}

#[test]
fn all_modes_hit_the_same_point() {
    let pts = vec![(0.0, 0.0), (6.0, 3.0), (12.0, -2.0)];
    let frame = Frame::new(TEAM1);
    let shooter = (100.0, 225.0);
    let (cx, cy) = frame.to_game(shooter.0, shooter.1);
    let (rbf, angle, _mx, _my) = build_curve(&pts, cx, cy, 2.5, None).unwrap();
    let surface = Surface::default();
    let (tx, ty) = frame.to_real(12.0, -2.0);
    let target = SoldierRef {
        player: 1,
        soldier: 0,
        team: TEAM2,
        x: tx,
        y: ty,
    };
    let soldiers = [target];

    let f = |x: f64| rbf.eval(x);
    let normal = simulate_normal_fn(&f, frame, shooter, &surface, &soldiers, 0, 0, TEAM1);
    assert!(!normal.hits.is_empty(), "normal missed");

    let f1 = Expr::parse(&formula_for(FST_ODE, &rbf)).expect("first order formula");
    let first = simulate_first_order(&f1, frame, shooter, &surface, &soldiers, 0, 0, TEAM1);
    assert!(!first.hits.is_empty(), "first order missed");

    let f2 = Expr::parse(&formula_for(SND_ODE, &rbf)).expect("second order formula");
    let second =
        simulate_second_order(&f2, angle, frame, shooter, &surface, &soldiers, 0, 0, TEAM1);
    assert!(
        !second.hits.is_empty(),
        "second order missed, angle {angle}"
    );
}

#[test]
fn fixed_angle_constrains_the_start() {
    let pts = vec![(-10.0, 2.0), (-2.0, 4.0), (5.0, -1.0), (12.0, 3.0)];
    let frame = Frame::new(TEAM1);
    let (cx, cy) = frame.to_game(100.0, 225.0);
    for angle in [0.0, 0.5, -0.4, 1.0] {
        let (rbf, a, mx, my) = build_curve(&pts, cx, cy, 2.5, Some(angle)).unwrap();
        assert!((a - angle).abs() < 1e-12);
        assert!((rbf.eval(mx) - my).abs() < 1e-6, "muzzle not on curve");
        assert!(
            (rbf.derivative(mx) - angle.tan()).abs() < 1e-5,
            "start slope not imposed: {} vs {}",
            rbf.derivative(mx),
            angle.tan()
        );
    }
}

#[test]
fn derivative_formulas_match_exactly() {
    let pts = vec![(-7.0, 1.0), (0.0, 3.5), (4.0, -2.0), (9.0, 6.0)];
    let r = fit_rbf(&pts, 2.5).unwrap();
    let first = Expr::parse(&formula_for(FST_ODE, &r)).unwrap();
    let second = Expr::parse(&formula_for(SND_ODE, &r)).unwrap();
    for x in [-7.0, -2.0, 0.5, 4.0, 9.0, 14.0] {
        assert!(
            (first.eval_x(x) - r.derivative(x)).abs() < 1e-6,
            "F' at {x}: {} vs {}",
            first.eval_x(x),
            r.derivative(x)
        );
        assert!(
            (second.eval_x(x) - r.second(x)).abs() < 1e-5,
            "F'' at {x}: {} vs {}",
            second.eval_x(x),
            r.second(x)
        );
    }
}

#[test]
fn interpolation_exact() {
    let pts = vec![(-7.0, 1.0), (0.0, 3.5), (4.0, -2.0), (9.0, 6.0)];
    let r = fit_rbf(&pts, 3.0).unwrap();
    for (x, y) in pts {
        assert!((r.eval(x) - y).abs() < 1e-7, "{x} {}", r.eval(x));
    }
}

#[test]
fn derivative_matches_numeric() {
    let pts = vec![(-7.0, 1.0), (0.0, 3.5), (4.0, -2.0), (9.0, 6.0)];
    let r = fit_rbf(&pts, 2.5).unwrap();
    let x = 2.3;
    let h = 1e-6;
    let d = (r.eval(x + h) - r.eval(x - h)) / (2.0 * h);
    assert!((d - r.derivative(x)).abs() < 1e-4);
    let d2 = (r.derivative(x + h) - r.derivative(x - h)) / (2.0 * h);
    assert!((d2 - r.second(x)).abs() < 1e-3);
}

#[test]
fn big_rooms_keep_long_missions_alive() {
    let targets: Vec<Target> = (0..18)
        .map(|i| Target {
            player: 1 + i / 2,
            soldier: i % 2,
            u: -18.0 + i as f64 * 2.0,
            v: ((i * 7) % 20) as f64 - 10.0,
        })
        .collect();
    let missions = make_missions(&targets, &mut Rng::new(1));
    assert!(missions.iter().any(|m| m.len() == 18), "no full mission");
    assert!(
        missions.iter().any(|m| m.len() == 17),
        "no drop-one mission"
    );
    assert!(
        missions.iter().any(|m| (9..18).contains(&m.len())),
        "no scattered large mission"
    );
    assert!(missions.len() <= MISSION_CAP, "mission list not capped");
}

#[test]
fn formula_roundtrip_normal() {
    let pts = vec![(0.0, 0.0), (5.0, 4.0), (12.0, -1.0)];
    let r = fit_rbf(&pts, 2.4).unwrap();
    let s = formula_for(NORMAL_FUNC, &r);
    let e = Expr::parse(&s).unwrap();
    for x in [0.0, 2.0, 5.0, 9.0, 12.0] {
        assert!((e.eval_x(x) - r.eval(x)).abs() < 1e-4, "{s}");
    }
}
