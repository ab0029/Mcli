use super::*;
use alacritty_terminal::grid::Dimensions;
use std::thread;

fn mark(rt: &rune::Runtime<Workspace>, key: &str) -> (f32, f32) {
    let [x, y, width, height] = rt.presented(key).unwrap().0;
    (x + width / 2., y + height / 2.)
}

fn percentile(values: &mut [f64], q: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((q * values.len() as f64).ceil() as usize).saturating_sub(1)]
}

#[test]
#[ignore = "五个真实 PowerShell 的 Release 交互采样"]
fn benchmark_sidebar_motion() {
    use serde_json::json;
    let folder = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/workbench-sidebar-reference/load")
        .join(std::process::id().to_string());
    let mut rt = app().headless_runtime((1672., 916.), rune::Env::new(true, 1.));
    rt.update(|w, _| w.first_open = false);
    let mut ids = Vec::new();
    for name in ["temp", "KeyCover", "notes", "review", "Codex"] {
        let dir = folder.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        ids.push(super::multiple::launch_profile_shell(&mut rt, &dir));
    }
    rt.state.session().unwrap().input(
        b"1..1600 | ForEach-Object { '0123456789abcdef' * 12 }; 'MCLI-GRID-READY'\r".to_vec(),
    );
    wait_for_workspace(&mut rt, Instant::now(), |w| {
        frame(w.session().unwrap())
            .lines()
            .any(|l| l.trim() == "MCLI-GRID-READY")
    });
    let dt = 1. / 60.;
    for _ in 0..20 {
        rt.frame(dt);
    }
    let mut drag_ms = Vec::new();
    let mut fold_ms = Vec::new();
    let mut distinct = Vec::new();
    let mut settle_ms = Vec::new();
    let mut overshoot_px = Vec::new();
    let mut grid_changes = 0;
    let mut max_pointer_error: f32 = 0.;
    let mut old_cols = rt.state.session().unwrap().term.lock().unwrap().columns();
    for gesture in 0..4 {
        let (x, y) = mark(&rt, "sidebar-divider");
        rt.pointer_down(x, y);
        let direction = if gesture % 2 == 0 { -1. } else { 1. };
        let start = Instant::now();
        for step in 1..=48 {
            let next = x + direction * 160. * step as f32 / 48.;
            let now = Instant::now();
            rt.pointer_move(next, y);
            rt.frame(dt);
            drag_ms.push(now.elapsed().as_secs_f64() * 1000.);
            max_pointer_error =
                max_pointer_error.max((mark(&rt, "sidebar-divider").0 - next).abs());
            let cols = rt.state.session().unwrap().term.lock().unwrap().columns();
            grid_changes += usize::from(cols != old_cols);
            old_cols = cols;
            let deadline = start + Duration::from_secs_f64(dt * f64::from(step));
            thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }
        rt.pointer_up(x + direction * 160., y);
        rt.frame(dt);
        let cols = rt.state.session().unwrap().term.lock().unwrap().columns();
        grid_changes += usize::from(cols != old_cols);
        old_cols = cols;
        assert_eq!(cols, usize::from(rt.state.size.cols));
    }
    for _ in 0..8 {
        let (x, y) = mark(&rt, &format!("project-{}", ids[0].0));
        let next = format!("project-{}", ids[1].0);
        let mut positions = vec![mark(&rt, &next).1.round() as i32];
        for i in 0..24 {
            let now = Instant::now();
            if i == 0 {
                rt.pointer_down(x, y);
                rt.pointer_up(x, y);
            }
            rt.frame(dt);
            fold_ms.push(now.elapsed().as_secs_f64() * 1000.);
            positions.push(mark(&rt, &next).1.round() as i32);
            thread::sleep(Duration::from_secs_f64(dt).saturating_sub(now.elapsed()));
        }
        let start = positions[0];
        let end = *positions.last().unwrap();
        assert!((end - start).abs() >= 40, "手势必须实际展开或折叠会话");
        let remaining = positions
            .iter()
            .rposition(|p| (*p - end).abs() > 1)
            .unwrap_or(0);
        settle_ms.push((remaining + 1) as f64 * dt * 1000.);
        overshoot_px.push(
            positions
                .iter()
                .map(|p| (p - start.max(end)).max(start.min(end) - p).max(0))
                .max()
                .unwrap(),
        );
        positions.sort();
        positions.dedup();
        distinct.push(positions.len());
    }
    let report = json!({
        "real_shells": ids.len(), "scrollback_input_lines": 1600, "sample_hz": 60,
        "drag_frames": drag_ms.len(), "drag_p95_ms": percentile(&mut drag_ms, 0.95),
        "drag_max_ms": percentile(&mut drag_ms, 1.), "native_grid_changes": grid_changes,
        "max_divider_pointer_error_px": max_pointer_error,
        "fold_frames": fold_ms.len(), "fold_p95_ms": percentile(&mut fold_ms, 0.95),
        "fold_steps_per_gesture": distinct, "fold_settle_p95_ms": percentile(&mut settle_ms, 0.95),
        "fold_overshoot_px": overshoot_px,
    });
    println!("{report}");
    if let Some(path) = std::env::var_os("MCLI_MOTION_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
