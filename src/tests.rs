use super::*;
mod multiple;
pub(crate) mod phase_four;
mod restore_retry;
mod session_proxy;
mod sidebar_motion;
mod workbench;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};
use rune::Mods;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// Direct launches isolate the CLI protocol in the opt-in vendor tests.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Agent {
    Codex,
    Agy,
}

impl Agent {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Agy => "agy",
        }
    }
    fn program(self) -> io::Result<PathBuf> {
        let exe = match self {
            Self::Codex => "codex.exe",
            Self::Agy => "agy.exe",
        };
        let paths = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs: Vec<_> = std::env::split_paths(&paths)
            .filter(|p| p.is_absolute())
            .collect();
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join(match self {
                Self::Codex => "Programs/OpenAI/Codex/bin",
                Self::Agy => "agy/bin",
            }));
        }
        dirs.into_iter()
            .map(|p| p.join(exe))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("找不到 {exe}，请检查安装位置或 PATH"),
                )
            })
    }
}

#[test]
fn clipboard_routes_images_and_quoted_file_lists_without_submitting() {
    use rune::clipboard::{ClipboardContent, ClipboardImage};

    let mode = TermMode::BRACKETED_PASTE;
    let text = ClipboardContent::new("甲\r\n\r\n  乙");
    assert_eq!(
        input::clipboard(&text, mode),
        input::paste("甲\n\n  乙", mode)
    );
    assert!(input::clipboard(&ClipboardContent::default(), mode).is_empty());

    let header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01";
    let image = text.with_image(ClipboardImage::from_png(&header[..]).unwrap());
    assert_eq!(input::clipboard(&image, mode), [0x16]);
    let files = image.with_files([
        PathBuf::from(r"L:\含 空格\甲.txt"),
        PathBuf::from(r"\\server\share\乙.md"),
        PathBuf::from("bad\rpath"),
    ]);
    let quoted = r#""L:\含 空格\甲.txt" "\\server\share\乙.md""#;
    assert_eq!(input::clipboard(&files, mode), input::paste(quoted, mode));
    assert_eq!(
        input::clipboard(&files, TermMode::empty()),
        quoted.as_bytes()
    );
}

#[derive(Clone, Default)]
struct Replies(Arc<Mutex<Vec<String>>>);
impl EventListener for Replies {
    fn send_event(&self, e: Event) {
        if let Event::PtyWrite(s) = e {
            self.0.lock().unwrap().push(s);
        }
    }
}
fn line<T>(term: &Term<T>, y: i32) -> String {
    (0..term.columns())
        .filter_map(|x| {
            let c = &term.grid()[Line(y)][Column(x)];
            (!c.flags
                .contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR_SPACER))
            .then_some(c.c)
        })
        .collect::<String>()
        .trim_end()
        .to_owned()
}
fn frame(session: &Session) -> String {
    let t = session.term.lock().unwrap();
    (0..t.screen_lines() as i32)
        .map(|y| line(&t, y))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn split_vt_cursor_reply_wide_color_and_alternate_screen() {
    let replies = Replies::default();
    let size = Size::default();
    let mut term = Term::new(Config::default(), &size, replies.clone());
    let mut parser = Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new();
    for byte in "ABC\rZ\x1b[2;3H\x1b[6n\x1b[31m中A".as_bytes() {
        parser.advance(&mut term, &[*byte]);
    }
    assert_eq!(line(&term, 0), "ZBC");
    assert_eq!(line(&term, 1), "  中A");
    assert_eq!(
        term.grid()[Line(1)][Column(2)].fg,
        Color::Named(NamedColor::Red)
    );
    assert_eq!(*replies.0.lock().unwrap(), vec!["\x1b[2;3R"]);
    parser.advance(&mut term, b"\x1b[?1049h\x1b[HALT");
    assert!(term.mode().contains(TermMode::ALT_SCREEN));
    assert_eq!(line(&term, 0), "ALT");
    term.resize(Size { cols: 95, ..size });
    parser.advance(&mut term, b"\x1b[?1049l");
    assert_eq!(line(&term, 0), "ZBC");
    assert_eq!(line(&term, 1), "  中A");
}

#[test]
fn mode_dependent_input_and_multiline_paste() {
    let normal = TermMode::empty();
    let key = |key, mods, mode| input::key(&KeyEvent { key, mods }, mode);
    assert_eq!(key(Key::Left, Mods::default(), normal), b"\x1b[D");
    assert_eq!(
        key(Key::Left, Mods::default(), TermMode::APP_CURSOR),
        b"\x1bOD"
    );
    assert_eq!(
        key(
            Key::Command('c'),
            Mods {
                ctrl: true,
                ..Default::default()
            },
            normal
        ),
        [3]
    );
    assert_eq!(
        key(
            Key::Tab,
            Mods {
                shift: true,
                ..Default::default()
            },
            normal
        ),
        b"\x1b[Z"
    );
    assert_eq!(
        key(Key::Text("重构 A1".into()), Mods::default(), normal),
        "重构 A1".as_bytes()
    );
    let s = "中文\r\n\r\n  two";
    assert_eq!(input::paste(s, normal), "中文\r\r  two".as_bytes());
    assert_eq!(
        input::paste(s, TermMode::BRACKETED_PASTE),
        "\x1b[200~中文\n\n  two\x1b[201~".as_bytes()
    );
    assert_eq!(
        input::paste("\x1b[201~", TermMode::BRACKETED_PASTE),
        b"\x1b[200~[201~\x1b[201~"
    );
    assert_eq!(
        input::mouse(0, 4, 2, false, Mods::default(), TermMode::SGR_MOUSE),
        b"\x1b[<0;5;3M"
    );
    assert_eq!(
        input::mouse(0, 4, 2, true, Mods::default(), TermMode::SGR_MOUSE),
        b"\x1b[<0;5;3m"
    );
    assert!(viewport(0., 0., 9., 1.).is_none());
}

#[test]
fn viewport_rounds_cell_metrics_at_fractional_scale() {
    let s = viewport(720., 480., 9., 1.5).unwrap();
    assert_eq!((s.cols, s.lines, s.cell_h), (80, 25, 29));
}

#[test]
fn interactive_terminal_advertises_color_and_respects_no_color() {
    use std::ffi::OsString;
    for (automation, saved, explicit, expected) in [
        (true, false, "1", None),
        (true, true, "1", Some("1")),
        (false, false, "1", Some("1")),
        (true, false, "yes", Some("yes")),
    ] {
        let mut vars: Vec<(OsString, OsString)> = [
            ("Term", "dumb"),
            ("colorterm", "old"),
            ("NO_COLOR", explicit),
            ("CODEX_CI", if automation { "1" } else { "0" }),
            ("CODEX_NON_INTERACTIVE", "1"),
            ("MCLI_SESSION_ID", "old"),
            ("UNRELATED_SETTING", "kept"),
        ]
        .into_iter()
        .map(|(name, value)| (name.into(), value.into()))
        .collect();
        pty::terminal_environment(&mut vars, "test-window", || saved);
        let value = |name: &str| {
            vars.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .and_then(|(_, v)| v.to_str())
        };
        assert_eq!(value("TERM"), Some("xterm-256color"));
        assert_eq!(value("COLORTERM"), Some("truecolor"));
        assert_eq!(value("TERM_PROGRAM"), Some("Mcli"));
        assert_eq!(value("NO_COLOR"), expected);
        assert_eq!(value("MCLI_SESSION_ID"), Some("test-window"));
        assert_eq!(value("AGY_CLI_OSC7501"), Some("on"));
        assert_eq!(value("PI_PROGRAM_STATUS"), Some("1"));
        assert_eq!(value("UNRELATED_SETTING"), Some("kept"));
        assert_eq!(
            vars.iter()
                .filter(|(k, _)| k.eq_ignore_ascii_case("TERM"))
                .count(),
            1
        );
    }
    let mut explicit_off: Vec<(OsString, OsString)> = vec![
        ("AGY_CLI_OSC7501".into(), "off".into()),
        ("PI_PROGRAM_STATUS".into(), "0".into()),
    ];
    pty::terminal_environment(&mut explicit_off, "test-window", || false);
    assert_eq!(
        explicit_off
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("AGY_CLI_OSC7501"))
            .and_then(|(_, v)| v.to_str()),
        Some("off")
    );
    assert_eq!(
        explicit_off
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("PI_PROGRAM_STATUS"))
            .and_then(|(_, v)| v.to_str()),
        Some("0")
    );
}

#[test]
fn windows_conpty_delivers_terminal_line_attributes() {
    use alacritty_terminal::term::cell::Flags;
    let (s, _) = fixture(
        "render-styles",
        r#"
$e = [char]27
[Console]::Write("$e[4:2mD$e[0m$e[4:3mW$e[0m$e[4:4mP$e[0m$e[9mS$e[0m")
[Console]::Write("$e[4;58:2::42:193:245mU$e[0m")
"#,
        &[],
    );
    wait_for(&s, 12, || s.end.lock().unwrap().is_some());
    let term = s.term.lock().unwrap();
    let row = &term.grid()[Line(0)];
    assert_eq!(row[Column(0)].c, 'D');
    for (column, expected) in [
        Flags::DOUBLE_UNDERLINE,
        Flags::UNDERCURL,
        Flags::DOTTED_UNDERLINE,
        Flags::STRIKEOUT,
        Flags::UNDERLINE,
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            row[Column(column)].flags.contains(expected),
            "column {column}: {:?}, expected {expected:?}",
            row[Column(column)].flags
        );
    }
    assert_eq!(
        row[Column(4)].underline_color(),
        Some(Color::Spec(alacritty_terminal::vte::ansi::Rgb {
            r: 42,
            g: 193,
            b: 245
        }))
    );
    let mut screen = Term::new(Config::default(), &Size::default(), Replies::default());
    let mut parser = Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new();
    let mut sgr = terminal::ConptySgr::default();
    for byte in b"\x1b[38;5;21mX\x1b[1m\x1b[21mB\x1b[24mC" {
        sgr.feed(&[*byte], |part| parser.advance(&mut screen, part));
    }
    let cells = &screen.grid()[Line(0)];
    assert_eq!(cells[Column(0)].fg, Color::Indexed(21));
    assert!(
        cells[Column(1)]
            .flags
            .contains(Flags::BOLD | Flags::DOUBLE_UNDERLINE)
    );
    assert!(!cells[Column(2)].flags.contains(Flags::DOUBLE_UNDERLINE));
}

#[test]
fn intensity_uses_bright_ansi_foregrounds_without_recoloring_rgb_or_indexed_text() {
    use alacritty_terminal::term::{cell::Flags, color::Colors};
    use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
    use terminal::palette;
    let mut colors = Colors::default();
    let get = terminal_view::foreground;
    assert_eq!(
        get(Color::Named(NamedColor::Blue), Flags::empty(), &colors),
        palette(4)
    );
    assert_eq!(
        get(Color::Named(NamedColor::Blue), Flags::BOLD, &colors),
        palette(12)
    );
    assert_eq!(get(Color::Indexed(4), Flags::BOLD, &colors), palette(4));
    let rgb = Rgb {
        r: 20,
        g: 50,
        b: 160,
    };
    assert_eq!(get(Color::Spec(rgb), Flags::BOLD, &colors), rgb);
    assert_eq!(
        get(Color::Named(NamedColor::Foreground), Flags::BOLD, &colors),
        palette(15)
    );
    colors[NamedColor::BrightBlue] = Some(rgb);
    assert_eq!(
        get(Color::Named(NamedColor::Blue), Flags::BOLD, &colors),
        rgb
    );
    colors[NamedColor::Foreground] = Some(rgb);
    assert_eq!(
        get(Color::Named(NamedColor::Foreground), Flags::BOLD, &colors),
        rgb
    );
}

pub(crate) fn scratch(name: &str) -> PathBuf {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/phase-one-native-terminal")
        .join(name);
    std::fs::create_dir_all(&cwd).unwrap();
    cwd
}
fn pwsh() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|d| d.join("pwsh.exe"))
        .find(|p| p.is_file())
        .expect("本机测试需要 PowerShell 7")
}
fn wait_for(session: &Session, timeout: u64, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(timeout);
    while !condition() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(30));
    }
    let end = session.end.lock().unwrap().clone();
    assert!(
        condition(),
        "终端超时，状态 {:?}，屏幕 {}",
        end,
        frame(session)
    );
}
fn fixture(name: &str, script: &str, args: &[&str]) -> (Session, PathBuf) {
    fixture_in(scratch(name), script, args)
}
fn fixture_in(cwd: PathBuf, script: &str, args: &[&str]) -> (Session, PathBuf) {
    let file = cwd.join("probe.ps1");
    std::fs::write(&file, script).unwrap();
    let mut argv = vec![
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-File".into(),
        file.display().to_string(),
    ];
    argv.extend(args.iter().map(|s| s.to_string()));
    let s = Session::spawn(&pwsh(), &argv, &cwd, Size::default(), Arc::new(|| {})).unwrap();
    (s, cwd)
}

#[test]
fn real_conpty_preserves_directory_arguments_input_resize_and_exit_tail() {
    let (s, cwd) = fixture(
        "含 空格的项目",
        r#"
param($Marker)
[Console]::WriteLine("DIR=" + (Split-Path -Leaf $pwd))
[Console]::WriteLine("ARG=" + $Marker)
[Console]::WriteLine("HOST=" + $env:MCLI_SESSION_ID)
[Console]::WriteLine("PROBE_READY")
$line=[Console]::ReadLine()
[Console]::WriteLine("INPUT=" + $line)
[Console]::WriteLine("SIZE=" + [Console]::WindowWidth + "x" + [Console]::WindowHeight)
[Console]::WriteLine("END_MARKER")
exit 37
"#,
        &["含 空格 \"q\" \\"],
    );
    wait_for(&s, 20, || frame(&s).contains("PROBE_READY"));
    let first = frame(&s);
    assert!(
        first.contains("DIR=含 空格的项目")
            && first.contains("ARG=含 空格 \"q\" \\")
            && first.contains(&s.id.marker()),
        "{first}"
    );
    s.resize(Size {
        cols: 92,
        lines: 28,
        ..Size::default()
    })
    .unwrap();
    s.input("重构 A1\r".as_bytes());
    wait_for(&s, 15, || s.end.lock().unwrap().is_some());
    assert_eq!(*s.end.lock().unwrap(), Some(End::Code(37)));
    let last = frame(&s);
    assert!(
        last.contains("INPUT=重构 A1")
            && last.contains("SIZE=92x28")
            && last.contains("END_MARKER"),
        "{last}"
    );
    assert!(
        Session::spawn(
            &cwd.join("missing.exe"),
            &[],
            &cwd,
            Size::default(),
            Arc::new(|| {})
        )
        .is_err()
    );
    assert!(
        Session::spawn(
            &pwsh(),
            &[],
            &cwd.join("missing"),
            Size::default(),
            Arc::new(|| {})
        )
        .is_err()
    );
}

#[test]
fn close_during_output_and_reopen_ten_times() {
    // GetProcessHandleCount includes every parallel test in this executable.
    const ISOLATED: &str = "MCLI_QA_COUNT_OWN_HANDLES";
    if std::env::var_os(ISOLATED).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::close_during_output_and_reopen_ten_times",
                "--nocapture",
            ])
            .env(ISOLATED, "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessHandleCount, OpenProcess, PROCESS_SYNCHRONIZE,
        WaitForSingleObject,
    };
    let clock = Instant::now();
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    let (anchor, dir) = fixture(
        "close-loop-anchor",
        "[Console]::WriteLine('ANCHOR');$null=[Console]::ReadLine()",
        &[],
    );
    let anchor_id = anchor.id;
    rt.update(|w, tx| attach_session(w, anchor, &dir, tx));
    wait_for_workspace(&mut rt, clock, |w| w.focused && w.session().is_some());
    let mut counts = Vec::new();
    for n in 0..10 {
        let (s, directory) = fixture(
            "close-loop",
            r#"while ($true) { [Console]::WriteLine("ACTIVE=$PID"); Start-Sleep -Milliseconds 10 }"#,
            &[],
        );
        let pid = || {
            frame(&s).lines().find_map(|line| {
                line.strip_prefix("ACTIVE=")
                    .and_then(|v| v.parse::<u32>().ok())
            })
        };
        wait_for(&s, 15, || pid().is_some());
        for columns in [100, 45, 115, 80] {
            s.resize(Size {
                cols: columns,
                ..Size::default()
            })
            .unwrap();
        }
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid().unwrap()) };
        assert!(!raw.is_null());
        let child = unsafe { OwnedHandle::from_raw_handle(raw) };
        let id = s.id;
        let end = s.end.clone();
        rt.update(|w, tx| attach_session(w, s, &directory, tx));
        wait_for_workspace(&mut rt, clock, |w| {
            w.session().is_some() && w.active == Some(id)
        });
        if n % 2 == 0 {
            press(
                &mut rt,
                &format!("session-{}", anchor_id.0),
                clock.elapsed().as_secs_f64(),
            );
        }
        press(
            &mut rt,
            &format!("close-{}", id.0),
            clock.elapsed().as_secs_f64(),
        );
        wait_for_workspace(&mut rt, clock, |w| {
            w.tabs.len() == 1 && w.active == Some(anchor_id) && w.focused
        });
        assert_eq!(*end.lock().unwrap(), Some(End::Closed), "cycle {n}");
        assert_eq!(
            unsafe { WaitForSingleObject(child.as_raw_handle(), 5000) },
            0,
            "child still alive in cycle {n}"
        );
        drop(child);
        let mut count = 0;
        assert_ne!(
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) },
            0
        );
        counts.push(count);
    }
    println!("10 次回收后的宿主句柄数 {counts:?}");
    assert!(counts[9] <= counts[1] + 4, "{counts:?}");
}

#[test]
fn explicit_close_reclaims_a_shell_that_stays_alive() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Threading::GetExitCodeProcess;

    let args = ["-NoProfile", "-Command", "Start-Sleep -Seconds 60"].map(str::to_owned);
    let child = pty::spawn(
        &pwsh(),
        &args,
        &scratch("shell-close"),
        80,
        24,
        "close-test",
        None,
    )
    .unwrap();
    // Keep the console open so this real child cannot receive CTRL_CLOSE_EVENT.
    pty::finish_requested_close(&child.process, 100).unwrap();
    let mut exit = 0;
    assert_ne!(
        unsafe { GetExitCodeProcess(child.process.as_raw_handle(), &mut exit) },
        0
    );
    assert_eq!(exit, 1);
    pty::finish_requested_close(&child.process, 100).unwrap();
}

fn press(rt: &mut rune::Runtime<Workspace>, key: &str, time: f64) {
    let e = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .unwrap();
    let [x, y, w, h] = e.rect;
    rt.pointer_down(x + w * 0.5, y + h * 0.5);
    rt.pointer_up(x + w * 0.5, y + h * 0.5);
    rt.frame(time);
}

#[track_caller]
fn wait_for_workspace(
    rt: &mut rune::Runtime<Workspace>,
    clock: Instant,
    ready: impl Fn(&Workspace) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        rt.frame(clock.elapsed().as_secs_f64());
        if ready(&rt.state) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "终端状态超时：{:?}\n{}",
            rt.state.current().and_then(|t| t.error.as_ref()),
            rt.state
                .session()
                .map(|s| frame(s)
                    .lines()
                    .filter(|l| !l.contains('@') && !l.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n"))
                .unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn attach_session(w: &mut Workspace, session: Session, cwd: &Path, tx: &mut Tx) {
    let id = session.id;
    let (send, recv) = mpsc::channel();
    send.send(Ok(session)).unwrap();
    w.add_pending(id, cwd.to_owned(), recv, tx);
}

#[test]
fn window_opens_a_shell_and_can_retry_or_reopen_it() {
    let clock = Instant::now();
    let mut rt = app().headless_runtime((940., 650.), rune::Env::new(true, 1.));
    wait_for_workspace(&mut rt, clock, |w| w.session().is_some() && w.focused);
    let first = rt.state.active.unwrap();
    press(
        &mut rt,
        &format!("close-{}", first.0),
        clock.elapsed().as_secs_f64(),
    );
    wait_for_workspace(&mut rt, clock, |w| w.tabs.is_empty());
    assert!(rt.presented("new-shell").is_some());
    let (send, recv) = mpsc::channel();
    send.send(Err("启动失败样本".into())).unwrap();
    let failed = SessionId::new();
    rt.update(|w, tx| w.add_pending(failed, scratch("retry"), recv, tx));
    wait_for_workspace(&mut rt, clock, |w| {
        w.current().is_some_and(|t| t.error.is_some())
    });
    press(
        &mut rt,
        &format!("retry-{}", failed.0),
        clock.elapsed().as_secs_f64(),
    );
    wait_for_workspace(&mut rt, clock, |w| w.session().is_some() && w.focused);
    assert_ne!(rt.state.active, Some(first));
    assert!(rt.state.current().unwrap().error.is_none());
}

#[test]
fn directory_reports_handle_split_windows_paths_and_reject_incomplete_reports() {
    let mut reports = shell::ShellReports::default();
    let input = concat!(
        "text\x1b[32m\x1b]0;title\x07",
        "\x1b]9;9;\"L:\\项目 A;B\"\x07",
        "\x1b]9;9;relative\x07",
        "\x1b]9;9;C:\\unfinished\x1b[31m",
        "\x1b]9;9;C:\\cancelled\x18",
        "\x1b]9;9;D:\\\x1b\\",
        "\x1b]9;9;\\\\host\\share\\中文\x07",
    );
    let mut paths = Vec::new();
    for byte in input.as_bytes().chunks(1) {
        reports.feed(byte, "sample", |report| {
            if let shell::Report::Directory(path) = report {
                paths.push(path)
            }
        });
    }
    assert_eq!(
        paths,
        [r"L:\项目 A;B", r"D:\", r"\\host\share\中文"].map(PathBuf::from)
    );
    let raw = concat!(
        "\x1b]633;P;McliAgent=other=agy\x07",
        "\x1b]633;P;McliAgent=sample=bad\x07",
        "\x1b]633;P;McliAgent=sample=codex\x1b\\",
        "\x1b]0;agy\x07",
        "\x1b]633;P;McliAgent=sample=shell\x07"
    );
    let mut foreground = vec![];
    for byte in raw.as_bytes().chunks(1) {
        reports.feed(byte, "sample", |report| {
            if let shell::Report::Foreground(agent) = report {
                foreground.push(agent)
            }
        });
    }
    assert_eq!(foreground, vec![Some(shell::Foreground::Codex), None]);
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

fn interactive_shell(
    program: &Path,
    cwd: &Path,
    before_integration: &str,
) -> rune::Runtime<Workspace> {
    let mut args = shell::arguments();
    let command = args.last_mut().unwrap();
    *command = format!(
        "{}\n{}\n{}\n{}",
        before_integration,
        command,
        "if (Get-Module -ListAvailable PSReadLine) { Set-PSReadLineOption -HistorySaveStyle SaveNothing }",
        "[Console]::WriteLine(('MCLI-SHELL=' + $PID))"
    );
    let s = Session::spawn(program, &args, cwd, Size::default(), Arc::new(|| {})).unwrap();
    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.));
    rt.update(|w, tx| attach_session(w, s, cwd, tx));
    let clock = Instant::now();
    wait_for_workspace(&mut rt, clock, |w| {
        w.focused
            && w.current().is_some_and(|t| t.directory == *cwd)
            && w.session()
                .is_some_and(|s| frame(s).contains("MCLI-SHELL="))
    });
    rt
}

fn shell_line(rt: &mut rune::Runtime<Workspace>, command: &str) {
    for key in [Key::Text(command.into()), Key::Enter] {
        rt.key(KeyEvent {
            key,
            mods: Mods::default(),
        });
    }
}

#[test]
fn shell_follows_real_locations_and_runs_cmd_launchers_in_the_same_session() {
    let parent = scratch("shell-first");
    let child = parent.join("中文 A;B's");
    std::fs::create_dir_all(&child).unwrap();
    let bin = parent.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in ["codex", "agy"] {
        std::fs::write(
            bin.join(format!("{tool}.cmd")),
            format!("@echo off\r\necho CMD-{tool}=%CD%\r\nexit /b 19\r\n"),
        )
        .unwrap();
    }
    // Compose with a profile's own location handler, prompt and command lookup.
    let profile = format!(
        "$env:PATH = {} + ';' + $env:PATH\n$global:mcliMoves = 0\n$ExecutionContext.SessionState.InvokeCommand.LocationChangedAction = {{ $global:mcliMoves++ }}\nfunction global:prompt {{ 'McliTest> ' }}",
        shell_quote(&bin)
    );
    let clock = Instant::now();
    let mut rt = interactive_shell(&pwsh(), &parent, &profile);
    let marker = rt.state.session().unwrap().id.marker();
    let screen = frame(rt.state.session().unwrap());
    let pid = screen
        .lines()
        .find_map(|s| s.trim().strip_prefix("MCLI-SHELL="))
        .unwrap();
    shell_line(
        &mut rt,
        &format!(
            "Set-Alias enterhere Push-Location; enterhere -LiteralPath {}",
            shell_quote(&child)
        ),
    );
    wait_for_workspace(&mut rt, clock, |w| {
        w.current().is_some_and(|t| t.directory == child)
    });
    for tool in ["codex", "agy"] {
        shell_line(&mut rt, tool);
        let prefix = format!("CMD-{tool}=");
        wait_for_workspace(&mut rt, clock, |w| {
            frame(w.session().unwrap())
                .lines()
                .filter_map(|line| line.strip_prefix(&prefix))
                .any(|path| Path::new(path) == child)
        });
    }
    shell_line(
        &mut rt,
        "cd -LiteralPath '.\\missing' -ErrorAction SilentlyContinue; Write-Output ('SAME=' + $PID + ',' + $global:mcliMoves)",
    );
    let expected = format!("SAME={pid},1");
    wait_for_workspace(&mut rt, clock, |w| {
        frame(w.session().unwrap()).contains(&expected)
    });
    assert_eq!(&rt.state.current().unwrap().directory, &child);
    shell_line(&mut rt, "Pop-Location");
    wait_for_workspace(&mut rt, clock, |w| {
        w.current().is_some_and(|t| t.directory == parent)
    });
    assert_eq!(rt.state.session().unwrap().id.marker(), marker);
    assert!(frame(rt.state.session().unwrap()).contains("McliTest>"));
    shell_line(&mut rt, "exit 23");
    wait_for_workspace(&mut rt, clock, |w| {
        w.current().is_some_and(|t| t.ended == Some(End::Code(23)))
    });
}

#[test]
fn windows_powershell_reports_directories_and_keeps_the_profile_prompt() {
    let exe = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let parent = scratch("shell-legacy");
    let child = parent.join("中文 目录");
    std::fs::create_dir_all(&child).unwrap();
    let mut rt = interactive_shell(
        &exe,
        &parent,
        "function global:prompt { 'LegacyProfile> ' }",
    );
    shell_line(&mut rt, &format!("cd -LiteralPath {}", shell_quote(&child)));
    wait_for_workspace(&mut rt, Instant::now(), |w| {
        w.current().is_some_and(|t| t.directory == child)
            && frame(w.session().unwrap()).contains("LegacyProfile>")
    });
}

#[test]
#[ignore = "需要本机已安装的两个官方 CLI 和既有信任的目录，只打开并退出官方界面"]
fn official_agents_open_from_powershell_and_return_to_the_same_shell() {
    let clock = Instant::now();
    let mut rt = interactive_shell(&pwsh(), &scratch("shell-first"), "");
    let screen = frame(rt.state.session().unwrap());
    let pid = screen
        .lines()
        .find_map(|s| s.trim().strip_prefix("MCLI-SHELL="))
        .unwrap();
    for agent in [Agent::Codex, Agent::Agy] {
        let cwd = official_test_project(agent);
        shell_line(
            &mut rt,
            &format!(
                "cd -LiteralPath {}; $mcliStartDir = $PWD.ProviderPath",
                shell_quote(&cwd)
            ),
        );
        wait_for_workspace(&mut rt, clock, |w| {
            w.current().is_some_and(|t| t.directory == *&cwd)
        });
        let name = agent.name().to_ascii_lowercase();
        shell_line(
            &mut rt,
            &format!(
                "{name}; Write-Output ('MCLI-RETURN-{name}=' + $PID + ',' + ($PWD.ProviderPath -eq $mcliStartDir))"
            ),
        );
        let deadline = Instant::now() + Duration::from_secs(45);
        let mut skipped = false;
        loop {
            rt.frame(clock.elapsed().as_secs_f64());
            let t = frame(rt.state.session().unwrap()).to_lowercase();
            assert!(
                !t.contains("do you trust") && !t.contains("trust this directory"),
                "测试目录需沿用既有信任"
            );
            if !skipped && t.contains("update available") && t.contains("esc skip") {
                rt.key(KeyEvent {
                    key: Key::Escape,
                    mods: Default::default(),
                });
                skipped = true;
            } else if !t.contains("esc skip")
                && match agent {
                    Agent::Codex => {
                        t.contains("codex")
                            && (t.contains("for shortcuts") || t.contains("ask codex"))
                    }
                    Agent::Agy => {
                        t.contains("antigravity")
                            && (t.contains("for shortcuts")
                                || t.contains("type a message")
                                || t.contains("mode: idle")
                                || t.contains('❯'))
                    }
                }
            {
                break;
            }
            assert!(Instant::now() < deadline, "{name} 官方输入栏未就绪");
            std::thread::sleep(Duration::from_millis(60));
        }
        rt.key(KeyEvent {
            key: Key::Text("/exit".into()),
            mods: Default::default(),
        });
        rt.frame(clock.elapsed().as_secs_f64());
        std::thread::sleep(Duration::from_millis(350));
        rt.key(KeyEvent {
            key: Key::Enter,
            mods: Default::default(),
        });
        let expected = format!("MCLI-RETURN-{name}={pid},True");
        wait_for_workspace(&mut rt, clock, |w| {
            frame(w.session().unwrap()).contains(&expected)
        });
        assert_eq!(&rt.state.current().unwrap().directory, &cwd);
        assert!(rt.state.current().unwrap().ended.is_none());
        println!("{name}: official TUI opened and exited to PowerShell PID {pid}");
    }
}

#[test]
fn drag_copy_and_multiline_paste_through_rune_and_conpty() {
    let (s, cwd) = fixture(
        "clipboard",
        r#"
[Console]::WriteLine("中文"); [Console]::WriteLine(); [Console]::WriteLine("  two"); [Console]::WriteLine("READY")
$a=[Console]::ReadLine(); $b=[Console]::ReadLine(); $c=[Console]::ReadLine()
[Console]::WriteLine("GOT=<$a><$b><$c>")
"#,
        &[],
    );
    wait_for(&s, 10, || frame(&s).contains("READY"));
    let mut rt = app().headless_runtime((960., 620.), rune::Env::new(true, 1.));
    let clipboard = rune::clipboard::memory();
    rt.use_clipboard(&clipboard);
    rt.update(|w, tx| attach_session(w, s, &cwd, tx));
    rt.frame(0.);
    rt.frame(0.1);
    let e = rt
        .elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some("terminal"))
        .unwrap();
    let (x, y) = (e.rect[0] + 0.2, e.rect[1] + HEIGHT / 2.);
    let last = x + rt.state.cell * 4.9;
    rt.pointer_down(x, y);
    rt.frame(0.2);
    rt.pointer_move(last, y + HEIGHT * 2.);
    rt.frame(0.3);
    rt.pointer_up(last, y + HEIGHT * 2.);
    rt.frame(0.4);
    let selected = rt.state.session().unwrap().selected().unwrap();
    assert_eq!(selected, "中文\n\n  two");
    let ctrl = |c| KeyEvent {
        key: Key::Command(c),
        mods: Mods {
            ctrl: true,
            cmd: true,
            ..Default::default()
        },
    };
    rt.key(ctrl('c'));
    rt.frame(0.5);
    assert_eq!(
        clipboard.text().as_deref().map(|s| s.as_str()),
        Some(selected.as_str())
    );
    rt.update(|w, _| w.window_active = false);
    rt.key(ctrl('v'));
    rt.frame(0.55);
    rt.frame(0.56);
    rt.update(|w, _| w.window_active = true);
    rt.key(ctrl('v'));
    rt.frame(0.6);
    rt.frame(0.7);
    let s = rt.state.session().unwrap();
    std::thread::sleep(Duration::from_millis(120));
    assert!(
        s.end.lock().unwrap().is_none(),
        "粘贴最后一行时意外附加了提交键"
    );
    rt.key(KeyEvent {
        key: Key::Enter,
        mods: Mods::default(),
    });
    rt.frame(0.8);
    let s = rt.state.session().unwrap();
    wait_for(s, 10, || s.end.lock().unwrap().is_some());
    assert!(frame(s).contains("GOT=<中文><><  two>"), "{}", frame(s));
}

const RAW_CONSOLE_PEER: &str = r#"
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class Wire {
 [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int n);
 [DllImport("kernel32.dll")] static extern bool GetConsoleMode(IntPtr h,out uint m);
 [DllImport("kernel32.dll")] static extern bool SetConsoleMode(IntPtr h,uint m);
 [DllImport("kernel32.dll")] static extern bool SetConsoleCP(uint cp);
 [DllImport("kernel32.dll")] static extern bool ReadFile(IntPtr h,[Out]byte[] b,uint n,out uint got,IntPtr o);
 public static bool Enable(){var i=GetStdHandle(-10);var o=GetStdHandle(-11);uint m;return GetConsoleMode(o,out m) && SetConsoleMode(o,m|4) && SetConsoleCP(65001) && SetConsoleMode(i,0x290);}
 public static string Read(){var bytes=new System.Collections.Generic.List<byte>();var b=new byte[256];uint n;
  while(bytes.Count<4096 && ReadFile(GetStdHandle(-10),b,256,out n,IntPtr.Zero) && n>0){
   for(int k=0;k<n;k++)bytes.Add(b[k]);if(System.Text.Encoding.ASCII.GetString(bytes.ToArray()).Contains("WIREEND"))break;}
  return Convert.ToHexString(bytes.ToArray());}
}
'@
if(![Wire]::Enable()){exit 12}
"#;

#[test]
fn vt_peer_receives_paste_mouse_and_cursor_reply_in_its_input_stream() {
    let (s, _) = fixture(
        "vt-peer",
        &[
            RAW_CONSOLE_PEER,
            r#"
$e=[char]27
[Console]::Write("$e[?2004h$e[?1000h$e[?1006hWIRE_READY$e[5;8H$e[6n")
$hex=[Wire]::Read()
[Console]::Write("$e[?1000l$e[?1006l$e[?2004l$e[HWIRE_HEX=$hex")
"#,
        ]
        .concat(),
        &[],
    );
    wait_for(&s, 20, || frame(&s).contains("WIRE_READY"));
    let mode = s.mode();
    assert!(
        mode.contains(
            TermMode::BRACKETED_PASTE | TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE
        ),
        "{mode:?}"
    );
    s.input(input::paste("中\n\n  a", mode));
    s.input(input::mouse(0, 4, 2, false, Mods::default(), mode));
    s.input(input::mouse(0, 4, 2, true, Mods::default(), mode));
    s.input(b"WIREEND");
    wait_for(&s, 8, || s.end.lock().unwrap().is_some());
    let wire = frame(&s).replace('\n', "");
    for sequence in [
        "\x1b[5;8R",
        "\x1b[200~中\n\n  a\x1b[201~",
        "\x1b[<0;5;3M",
        "\x1b[<0;5;3m",
        "WIREEND",
    ] {
        let hex: String = sequence
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        assert!(wire.contains(&hex), "缺少 {hex}，{wire}");
    }
}

#[test]
fn rune_terminal_focus_and_ime_commit() {
    let scale = 1.;
    let mut rt = app().headless_runtime((940., 650.), rune::Env::new(true, scale));
    let (s, cwd) = fixture(
        "rune-ui",
        r#"[Console]::WriteLine("READY"); [Console]::Write("中文A"); $text=[Console]::ReadLine(); [Console]::WriteLine("COUNTED=" + $text); exit 0"#,
        &[],
    );
    wait_for(&s, 15, || frame(&s).contains("中文A"));
    rt.update(|w, tx| attach_session(w, s, &cwd, tx));
    rt.frame(0.5);
    rt.update(|_, tx| tx.focus("terminal", false));
    rt.frame(0.6);
    rt.frame(0.62);
    let (grid, _) = rt.presented("terminal").unwrap();
    let expected = [grid[0] + rt.state.cell * 5., grid[1] + HEIGHT];
    let anchor = rt.ime_area.expect("聚焦时应向系统输入法提供光标区域");
    for axis in 0..2 {
        assert!(
            (anchor[axis] - expected[axis]).abs() * scale <= 1.,
            "{scale} 倍缩放下，中文后的候选锚点 {anchor:?} 偏离光标 {expected:?}"
        );
    }
    rt.ime(Ime::Preedit("chong".into(), Some((5, 5)), vec![]));
    rt.frame(0.65);
    assert_eq!(rt.state.preedit, "chong");
    assert!(!frame(rt.state.session().unwrap()).contains("chong"));
    rt.ime(Ime::Preedit(String::new(), None, vec![]));
    rt.frame(0.66);
    assert!(rt.state.preedit.is_empty());

    rt.ime(Ime::Preedit("dai".into(), None, vec![]));
    rt.frame(0.68);
    rt.window_focus(false);
    rt.frame(0.70);
    rt.frame(0.71);
    assert!(rt.state.preedit.is_empty() && rt.ime_area.is_none());
    // Windows can finish a composition after notifying the old window it lost focus.
    rt.ime(Ime::Commit("失焦".into()));
    rt.key(KeyEvent {
        key: Key::Text("AFTER_BLUR".into()),
        mods: Mods::default(),
    });
    rt.frame(0.72);
    rt.window_focus(true);
    rt.frame(0.73);

    rt.ime(Ime::Preedit("lin".into(), None, vec![]));
    rt.frame(0.74);
    rt.window_focus(false);
    rt.ime(Ime::Commit("迟到".into()));
    rt.window_focus(true);
    rt.frame(0.75);

    rt.ime(Ime::Preedit("chong".into(), None, vec![]));
    rt.frame(0.82);
    rt.ime(Ime::Commit("重构".into()));
    rt.frame(0.84);
    rt.key(KeyEvent {
        key: Key::Text(" A1".into()),
        mods: Mods::default(),
    });
    rt.frame(0.86);
    let close_key = format!("close-{}", rt.state.active.unwrap().0);
    rt.update(|_, tx| tx.focus(close_key, false));
    rt.frame(0.9);
    rt.frame(0.91);
    assert!(rt.ime_area.is_none());
    rt.key(KeyEvent {
        key: Key::Text("WRONG_FOCUS".into()),
        mods: Mods::default(),
    });
    rt.frame(0.95);
    rt.update(|_, tx| tx.focus("terminal", false));
    rt.frame(1.);
    rt.key(KeyEvent {
        key: Key::Enter,
        mods: Mods::default(),
    });
    rt.frame(1.1);
    let s = rt.state.session().unwrap();
    wait_for(s, 10, || s.end.lock().unwrap().is_some());
    let out = frame(s);
    assert_eq!(
        out.lines()
            .filter(|line| line.starts_with("COUNTED="))
            .collect::<Vec<_>>(),
        ["COUNTED=重构 A1"],
        "{out}"
    );
}

// Opt-in checks use the user's installed official executable. Keep screen evidence inside the
// ignored test directory. Only submit the marker after observing a ready prompt.
fn official_test_project(agent: Agent) -> PathBuf {
    let path = match agent {
        Agent::Agy => std::env::var_os("MCLI_AGY_TEST_PROJECT").map(PathBuf::from),
        Agent::Codex => std::env::var_os("MCLI_CODEX_TEST_PROJECT").map(PathBuf::from),
    }
    .unwrap_or_else(|| scratch("含 空格的项目"));
    assert!(
        path.is_absolute() && path.is_dir(),
        "测试项目需要是现有目录的绝对路径"
    );
    path
}

fn official_screen(agent: Agent) {
    let cwd = official_test_project(agent);
    let s = Session::spawn(
        &agent.program().unwrap(),
        &[],
        &cwd,
        Size {
            cols: 105,
            lines: 33,
            ..Size::default()
        },
        Arc::new(|| {}),
    )
    .unwrap();
    std::thread::sleep(Duration::from_secs(6));
    let dir = scratch("official");
    let name = agent.name().to_ascii_lowercase();
    let mut first = frame(&s);
    std::fs::write(dir.join(format!("{name}-first.txt")), &first).unwrap();
    assert!(!first.trim().is_empty(), "官方首屏为空");
    if agent == Agent::Codex && first.contains("Update available") && first.contains("esc skip") {
        s.input(b"\x1b");
        std::thread::sleep(Duration::from_secs(3));
        first = frame(&s);
        std::fs::write(dir.join(format!("{name}-after-skip.txt")), &first).unwrap();
    }
    let lower = first.to_lowercase();
    let allowed = match agent {
        Agent::Codex => {
            lower.contains("codex")
                && (lower.contains("for shortcuts") || lower.contains("ask codex"))
                && !lower.contains("update now")
                && !lower.contains("trust this")
        }
        Agent::Agy => {
            lower.contains("antigravity")
                && !lower.contains("do you trust")
                && (first.contains('❯')
                    || (lower.contains("for shortcuts")
                        && first.lines().any(|line| line.trim() == ">"))
                    || lower.contains("type a message")
                    || lower.contains("type @"))
        }
    };
    assert!(allowed, "{name}: 已保存首屏，当前没有识别到可提交的输入栏");
    let expected = format!("MCLI-{}-OK", name.to_uppercase());
    s.input(format!("Reply with the single token formed by joining MCLI, {} and OK using hyphens. Do not use any tools.",name.to_uppercase()).into_bytes());
    std::thread::sleep(Duration::from_millis(650));
    let typed = frame(&s).to_lowercase();
    assert!(
        typed.contains("do not use any tools") || typed.contains("using hyphens"),
        "普通输入区未回显测试文字"
    );
    s.input(b"\r");
    let until = Instant::now() + Duration::from_secs(55);
    while Instant::now() < until
        && !frame(&s).contains(&expected)
        && s.end.lock().unwrap().is_none()
    {
        std::thread::sleep(Duration::from_millis(350));
    }
    let out = frame(&s);
    std::fs::write(dir.join(format!("{name}-reply.txt")), &out).unwrap();
    assert!(out.contains(&expected), "{name}: 没有观察到独立回复标记");
}
#[test]
#[ignore = "需要本机已登录的官方 Codex 和网络，将发送一条无工具测试提问"]
fn official_codex_screen_and_reply() {
    official_screen(Agent::Codex)
}
#[test]
#[ignore = "需要本机已登录的官方 agy 和网络，将发送一条无工具测试提问"]
fn official_agy_screen_and_reply() {
    official_screen(Agent::Agy)
}

#[test]
#[ignore = "需要已登录的 agy 和已信任的目录，验证官方菜单和运行中断"]
fn official_agy_help_and_interrupt() {
    let s = Session::spawn(
        &Agent::Agy.program().unwrap(),
        &[],
        &official_test_project(Agent::Agy),
        Size {
            cols: 110,
            lines: 38,
            ..Size::default()
        },
        Arc::new(|| {}),
    )
    .unwrap();
    let dir = scratch("official");
    let observe = |stage: &str, check: &dyn Fn(&str) -> bool| {
        let until = Instant::now() + Duration::from_secs(25);
        loop {
            let screen = frame(&s);
            if check(&screen) || Instant::now() >= until {
                let safe = screen
                    .lines()
                    .filter(|l| !l.contains('@'))
                    .collect::<Vec<_>>()
                    .join("\n");
                std::fs::write(dir.join(format!("agy-navigation-{stage}.txt")), safe).unwrap();
                assert!(check(&screen), "未观察到 agy {stage}");
                break;
            }
            std::thread::sleep(Duration::from_millis(90));
        }
    };
    let ready = |t: &str| {
        t.contains("for shortcuts")
            && !t.contains("Do you trust")
            && t.lines().any(|line| line.trim_start().starts_with('>'))
    };
    let help = |t: &str| {
        t.contains("general")
            && t.contains("commands")
            && t.contains("shortcuts")
            && t.contains("tab to cycle")
    };
    observe("ready", &ready);
    s.input(b"/help");
    std::thread::sleep(Duration::from_millis(500));
    s.input(b"\r");
    observe("help", &help);
    s.input(b"\t");
    observe("commands", &|t| help(t) && !t.contains("Quick Reference"));
    for key in [b"\x1b[B".as_slice(), b"\x1b[A", b"\x1b"] {
        s.input(key);
        std::thread::sleep(Duration::from_millis(180));
    }
    observe("return", &|t| ready(t) && !help(t));
    s.input(b"Without tools, write out every number from one to two hundred in words.");
    std::thread::sleep(Duration::from_millis(550));
    s.input(b"\r");
    observe("running", &|t| t.contains("esc to cancel"));
    s.input(b"\x03");
    observe("interrupted", &|t| {
        ready(t)
            && !t.contains("esc to cancel")
            && ["interrupt", "cancel"]
                .iter()
                .any(|word| t.to_lowercase().contains(word))
    });
}

#[test]
#[ignore = "需要已登录的 agy 和已信任的测试目录，用单次进程的请求审批模式验证无副作用的 PowerShell 计算"]
fn official_agy_read_only_approval() {
    let dir = scratch("official");
    let args = [
        "--settings",
        r#"{"toolPermission":"request-review","permissions":{"ask":["command(*)"]}}"#,
    ]
    .map(String::from);
    let session = Session::spawn(
        &Agent::Agy.program().unwrap(),
        &args,
        &official_test_project(Agent::Agy),
        Size {
            cols: 126,
            lines: 40,
            ..Size::default()
        },
        Arc::new(|| {}),
    )
    .unwrap();
    let observe = |stage: &str, predicate: &dyn Fn(&str) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(55);
        loop {
            let screen = frame(&session);
            if predicate(&screen)
                || Instant::now() >= deadline
                || session.end.lock().unwrap().is_some()
            {
                let sanitized: Vec<_> = screen.lines().filter(|line| !line.contains('@')).collect();
                std::fs::write(
                    dir.join(format!("agy-approval-{stage}.txt")),
                    sanitized.join("\n"),
                )
                .unwrap();
                assert!(predicate(&screen), "agy 在 {stage} 未出现预期界面");
                return screen;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    };
    observe("ready", &|t| {
        (t.contains("for shortcuts") || t.contains("Mode: idle"))
            && t.lines().any(|l| l.trim_start().starts_with('>'))
            && !t.contains("Do you trust")
    });
    let request = |stage: &str| {
        let prompt = if stage == "deny-menu" {
            "Use the terminal tool exactly once to run this exact PowerShell command: Write-Output (73 * 89). Do not calculate it yourself or run other commands. If denied, stop."
        } else {
            "New request. I would like a fresh, one-time permission prompt for exactly Write-Output (73 * 89). Please call the terminal tool again now. Run no other command and do not compute the answer yourself."
        };
        session.input(prompt.as_bytes());
        std::thread::sleep(Duration::from_millis(700));
        session.input(b"\r");
        observe(stage, &|t| {
            let lower = t.to_ascii_lowercase();
            t.contains("Write-Output (73 * 89)")
                && (lower.contains("allow once")
                    || lower.contains("always allow")
                    || lower.contains("yes, allow")
                    || lower.contains("reject")
                    || lower.contains("1. yes"))
        })
    };
    let _ = request("deny-menu");
    println!(
        "agy 原生审批页面的 OSC 7501 状态 {:?}",
        session.listener.shell.lock().unwrap().status
    );
    for _ in 0..3 {
        session.input(b"\x1b[B");
        std::thread::sleep(Duration::from_millis(100));
    }
    observe("deny-selected", &|t| {
        t.lines()
            .any(|l| l.trim_start().starts_with("> 4. No, cancel"))
    });
    session.input(b"\r");
    let denied = observe("denied", &|t| {
        let lower = t.to_ascii_lowercase();
        lower.contains("user declined the tool call")
            && (lower.contains("for shortcuts") || lower.contains("mode: idle"))
            && !lower.contains("always allow")
            && !lower.contains("allow once")
            && !lower.contains("1. yes")
    });
    assert!(!denied.contains("6497"));
    let menu = request("allow-menu");
    let exact_command = menu.lines().any(|l| {
        l.trim()
            .trim_matches(|ch| matches!(ch, '│' | '┃' | '$' | '>'))
            .trim()
            .eq_ignore_ascii_case("Write-Output (73 * 89)")
    });
    let one_time_default = menu.lines().any(|l| {
        let label = l.trim().to_ascii_lowercase();
        (label.starts_with('>') || label.starts_with('❯') || label.starts_with('›'))
            && (label.contains("allow once") || label.contains("yes"))
            && !label.contains("always")
    });
    assert!(
        exact_command && one_time_default,
        "审批卡没有同时标明预期的命令和单次批准选项"
    );
    session.input(b"\r");
    observe("allowed", &|t| t.contains("6497"));
}

#[test]
#[ignore = "需要已登录的 Codex，在同一临时目录启动两个官方会话并核对 /new 前后的编号"]
fn official_codex_same_directory_conversations() {
    fn expect_screen(s: &Session, description: &str, matches: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(75);
        loop {
            let text = frame(s);
            if matches(&text) {
                return text;
            }
            assert!(
                s.end.lock().unwrap().is_none() && Instant::now() < deadline,
                "{description}. 可见项 {:?}",
                [
                    "Ask Codex",
                    "for shortcuts",
                    "esc skip",
                    "Update available",
                    "esc to interrupt",
                    "Use current Git worktree",
                    "without spaces or tools",
                    "Session:",
                    "cedar",
                    "larch",
                    "olive",
                    "maple"
                ]
                .into_iter()
                .filter(|label| text.contains(label))
                .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    fn ready(s: &Session) {
        // The Windows client can replace its first prompt with an update dialog during startup.
        std::thread::sleep(Duration::from_secs(6));
        let text = expect_screen(s, "未进入官方启动界面", |t| {
            t.contains("esc skip") || t.contains("for shortcuts") || t.contains("Ask Codex")
        });
        if text.contains("esc skip") && text.contains("Update available") {
            s.input(b"\x1b");
            std::thread::sleep(Duration::from_secs(2));
        }
        expect_screen(s, "普通输入栏未就绪", |t| {
            !t.contains("esc skip")
                && !t.contains("Update now")
                && !t.contains("trust this")
                && (t.contains("for shortcuts") || t.contains("Ask Codex"))
        });
    }
    fn submit(s: &Session, text: &str) {
        s.input(text.as_bytes());
        let tail = &text[text.len().saturating_sub(13)..];
        expect_screen(s, "终端输入区未出现合成提问或命令", |t| {
            t.contains(tail)
        });
        std::thread::sleep(Duration::from_millis(700));
        s.input(b"\r");
    }
    fn mark(s: &Session, words: &str, answer: &str) {
        submit(
            s,
            &format!("Reply with just the concatenation of {words} without spaces or tools."),
        );
        expect_screen(s, answer, |t| {
            t.contains(answer) && !t.contains("esc to interrupt")
        });
    }
    fn session_id(text: &str) -> Option<&str> {
        text.lines()
            .rev()
            .filter_map(|l| l.split_once("Session:")?.1.split_whitespace().next())
            .find(|id| id.len() == 36 && id.bytes().all(|b| b == b'-' || b.is_ascii_hexdigit()))
    }
    fn status(s: &Session, old: Option<&str>) -> String {
        submit(s, "/status");
        let text = expect_screen(s, "未取得预期的 /status 对话编号", |t| {
            session_id(t).is_some_and(|id| Some(id) != old)
        });
        session_id(&text).unwrap().to_owned()
    }

    let cwd = scratch("含 空格的项目");
    let program = Agent::Codex.program().unwrap();
    let spawn = || {
        Session::spawn(
            &program,
            &[],
            &cwd,
            Size {
                cols: 115,
                lines: 36,
                ..Size::default()
            },
            Arc::new(|| {}),
        )
        .unwrap()
    };
    let a = spawn();
    ready(&a);
    mark(&a, "cedar and red", "cedarred");
    let first_a = status(&a, None);
    let b = spawn();
    assert_ne!(a.id.marker(), b.id.marker());
    ready(&b);
    mark(&b, "larch and gold", "larchgold");
    let first_b = status(&b, None);
    assert_ne!(first_a, first_b);
    submit(&a, "/new");
    let menu = expect_screen(&a, "未显示官方 /new 工作目录菜单", |t| {
        t.contains("Where should the new conversation run?")
    });
    assert!(
        menu.contains("› 1. Use current Git worktree"),
        "所选项不是当前目录"
    );
    a.input(b"\r");
    expect_screen(&a, "工作目录菜单未关闭", |t| {
        !t.contains("Where should the new conversation run?")
    });
    ready(&a);
    mark(&a, "olive and green", "olivegreen");
    let next_a = status(&a, Some(&first_a));
    mark(&b, "maple and rose", "maplerose");
    let next_b = status(&b, None);
    assert_ne!(next_a, first_b);
    assert_eq!(first_b, next_b);
    let evidence = format!(
        "A {} {first_a} {next_a}\nB {} {first_b} {next_b}\n",
        a.id.marker(),
        b.id.marker()
    );
    std::fs::write(
        scratch("official").join("codex-conversation-ids.txt"),
        evidence,
    )
    .unwrap();
}

#[test]
#[ignore = "需要已登录的 agy，在同一受信任的测试目录内验证两份真实终端、中文提交和 /new"]
fn official_agy_same_directory_and_chinese() {
    use std::time::SystemTime;
    let cwd = official_test_project(Agent::Agy);
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let dir = scratch(&format!("official/agy-pair-{stamp}"));
    let spawn = |slot: &str| {
        let args = [
            "--log-file".to_owned(),
            dir.join(format!("{slot}.log")).display().to_string(),
        ];
        Session::spawn(
            &Agent::Agy.program().unwrap(),
            &args,
            &cwd,
            Size {
                cols: 118,
                lines: 36,
                ..Size::default()
            },
            Arc::new(|| {}),
        )
        .unwrap()
    };
    let observe = |s: &Session, stage: &str, check: &dyn Fn(&str) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(65);
        loop {
            let text = frame(s);
            if check(&text) || s.end.lock().unwrap().is_some() || Instant::now() >= deadline {
                let text = text
                    .lines()
                    .filter(|line| !line.contains('@'))
                    .collect::<Vec<_>>()
                    .join("\n");
                std::fs::write(dir.join(format!("{stage}.txt")), &text).unwrap();
                assert!(check(&text), "未通过 agy {stage}");
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    };
    let ready = |t: &str| {
        t.contains("for shortcuts")
            && !t.contains("Do you trust")
            && t.lines().any(|l| l.trim_start().starts_with('>'))
    };
    let submit = |s: &Session, text: &str| {
        s.input(text.as_bytes());
        std::thread::sleep(Duration::from_millis(650));
        s.input(b"\r");
    };
    let mark = |s: &Session, tag: &str, words: &str, answer: &str| {
        submit(
            s,
            &format!(
                "Sample {tag}. Reply only with the concatenation of {words} without spaces. Use no tools."
            ),
        );
        // A service error can still leave a valid, persisted user turn. This case
        // tests attribution; the independent reply and approval tests require success.
        observe(s, tag, &|t| {
            t.contains(tag)
                && t.contains("for shortcuts")
                && (t.to_lowercase().contains(answer) || t.contains("FAILED_PRECONDITION"))
        });
    };
    let a = spawn("A");
    observe(&a, "A-ready", &ready);
    mark(&a, "MCLI-R4-AGY-A0", "amber and dust", "amberdust");
    let b = spawn("B");
    assert_ne!(a.id.marker(), b.id.marker());
    std::fs::write(
        dir.join("launch-markers.txt"),
        format!("A {}\nB {}\n", a.id.marker(), b.id.marker()),
    )
    .unwrap();
    observe(&b, "B-ready", &ready);
    mark(&b, "MCLI-R4-AGY-B0", "ice and lemon", "icelemon");
    submit(&a, "/new");
    observe(&a, "A-new", &|t| ready(t) && !t.contains("MCLI-R4-AGY-A0"));

    let mut rt = app().headless_runtime((1040., 640.), rune::Env::new(true, 1.5));
    rt.update(|w, tx| attach_session(w, a, &cwd, tx));
    rt.frame(0.);
    rt.frame(0.1);
    rt.update(|_, tx| tx.focus("terminal", false));
    rt.frame(0.2);
    rt.frame(0.3);
    let key = |key| KeyEvent {
        key,
        mods: Mods::default(),
    };
    rt.key(key(Key::Text(
        "Sample MCLI-R4-AGY-A1. Without tools, repeat the last two words exactly: ".into(),
    )));
    rt.ime(Ime::Preedit("chonggou".into(), None, vec![]));
    rt.frame(0.4);
    rt.ime(Ime::Preedit(String::new(), None, vec![]));
    rt.frame(0.5);
    rt.ime(Ime::Preedit("chonggou".into(), None, vec![]));
    rt.ime(Ime::Commit("重构".into()));
    rt.key(key(Key::Text(" A1".into())));
    rt.frame(0.6);
    let source = rt.state.session().unwrap();
    observe(source, "A-typed", &|t| {
        t.matches("重构 A1").count() == 1 && !t.contains("chonggou")
    });
    rt.key(key(Key::Enter));
    rt.frame(0.7);
    let a = rt.state.session().unwrap();
    observe(a, "A-submitted", &|t| {
        t.contains("重构 A1")
            && t.contains("for shortcuts")
            && (t.matches("重构 A1").count() >= 2 || t.contains("FAILED_PRECONDITION"))
    });
    mark(&b, "MCLI-R4-AGY-B1", "rose and water", "rosewater");
    assert!(!frame(&b).contains("amberdust"));
    submit(a, "/exit");
    wait_for(a, 15, || a.end.lock().unwrap().is_some());
    assert_eq!(*a.end.lock().unwrap(), Some(End::Code(0)));

    // Only inspect transcripts for IDs emitted by these two dedicated CLI processes.
    // Count the actual user messages, including duplicates, independently of screen text.
    let brain = PathBuf::from(std::env::var_os("USERPROFILE").unwrap())
        .join(".gemini/antigravity-cli/brain");
    let mut report = Vec::new();
    for (slot, expected) in [
        ("A", vec![vec!["A0"], vec!["A1"]]),
        ("B", vec![vec!["B0", "B1"]]),
    ] {
        let log = std::fs::read_to_string(dir.join(format!("{slot}.log"))).unwrap();
        let mut conversations = Vec::new();
        for id in log.lines().filter_map(|l| {
            l.split_once("Created conversation ")
                .map(|(_, suffix)| suffix.split_whitespace().next().unwrap_or(""))
        }) {
            if id.len() == 36
                && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
                && !conversations.contains(&id)
            {
                conversations.push(id);
            }
        }
        assert_eq!(
            conversations.len(),
            expected.len(),
            "官方日志中的对话数量与 /new 操作不符"
        );
        for (id, tags) in conversations.into_iter().zip(expected) {
            let path = brain
                .join(id)
                .join(".system_generated/logs/transcript.jsonl");
            let transcript = std::fs::read_to_string(path).unwrap();
            let mut actual = Vec::new();
            for line in transcript.lines() {
                let row: serde_json::Value = serde_json::from_str(line).unwrap();
                if row["source"] != "USER_EXPLICIT" || row["type"] != "USER_INPUT" {
                    continue;
                }
                let content = row["content"].as_str().unwrap();
                for tag in ["A0", "A1", "B0", "B1"] {
                    if content.contains(&format!("MCLI-R4-AGY-{tag}")) {
                        if tag == "A1" {
                            assert_eq!(content.matches("重构 A1").count(), 1);
                            assert!(!content.contains("chonggou"));
                        }
                        actual.push(tag);
                    }
                }
            }
            assert_eq!(actual, tags, "官方用户消息与该终端的提交不一致");
            report.push(format!("{slot} {id} {}", actual.join(",")));
        }
    }
    std::fs::write(dir.join("ownership.txt"), report.join("\n")).unwrap();
}

#[test]
#[ignore = "需要已登录的 Codex，临时使用人工审批和只读沙箱验证本项目的测试文件"]
fn official_codex_read_only_approval() {
    let cwd = scratch("含 空格的项目");
    std::fs::write(
        cwd.parent().unwrap().join("approval-marker.txt"),
        "MCLI-READ-SAMPLE",
    )
    .unwrap();
    let args = [
        "-c",
        "approvals_reviewer=\"user\"",
        "-a",
        "on-request",
        "-s",
        "read-only",
    ]
    .map(String::from);
    let s = Session::spawn(
        &Agent::Codex.program().unwrap(),
        &args,
        &cwd,
        Size {
            cols: 120,
            lines: 38,
            ..Size::default()
        },
        Arc::new(|| {}),
    )
    .unwrap();
    let evidence = scratch("official");
    let snapshot = |label: &str| {
        let text = frame(&s);
        std::fs::write(evidence.join(format!("approval-{label}.txt")), &text).unwrap();
        text
    };
    std::thread::sleep(Duration::from_secs(4));
    if frame(&s).contains("esc skip") {
        s.input(b"\x1b");
        std::thread::sleep(Duration::from_secs(3));
    }
    assert!(frame(&s).contains("Ask Codex"), "普通输入栏未就绪");
    let prompt = "Please call the shell with sandbox_permissions require_escalated to request approval before running exactly this one harmless command: Get-Content ../approval-marker.txt . Run nothing else. If denied, stop.";
    let is_gate = |text: &str| {
        let t = text.to_lowercase();
        t.contains("yes, proceed") && t.contains("approval-marker.txt")
    };
    let request = |label: &str| {
        s.input(prompt.as_bytes());
        std::thread::sleep(Duration::from_millis(700));
        s.input(b"\r");
        let deadline = Instant::now() + Duration::from_secs(50);
        while Instant::now() < deadline && !is_gate(&frame(&s)) {
            std::thread::sleep(Duration::from_millis(350));
        }
        let screen = snapshot(label);
        assert!(is_gate(&screen), "未观察到该文件的人工审批");
        println!(
            "Codex 原生审批页面的 OSC 7501 状态 {:?}",
            s.listener.shell.lock().unwrap().status
        );
        // The action shown in the approval prompt must be exactly the fixture read.
        let exact = screen.lines().any(|ln| {
            let command = ln
                .trim()
                .trim_start_matches('$')
                .trim()
                .to_ascii_lowercase()
                .replace('\\', "/");
            matches!(
                command.as_str(),
                "get-content ../approval-marker.txt"
                    | "get-content '../approval-marker.txt'"
                    | "get-content -literalpath '../approval-marker.txt'"
            )
        });
        assert!(exact, "审批面板未显示预期的单条只读命令");
    };
    request("deny-menu");
    s.input(b"\x1b");
    std::thread::sleep(Duration::from_secs(4));
    let denied = snapshot("denied");
    assert!(!denied.contains("MCLI-READ-SAMPLE") && !is_gate(&denied));
    let until = Instant::now() + Duration::from_secs(25);
    while Instant::now() < until && frame(&s).contains("esc to interrupt") {
        std::thread::sleep(Duration::from_millis(350));
    }
    request("allow-menu");
    s.input(b"y");
    std::thread::sleep(Duration::from_secs(1));
    if is_gate(&frame(&s)) {
        s.input(b"\r");
    }
    let until = Instant::now() + Duration::from_secs(35);
    while Instant::now() < until && !frame(&s).contains("MCLI-READ-SAMPLE") {
        std::thread::sleep(Duration::from_millis(350));
    }
    assert!(snapshot("allowed").contains("MCLI-READ-SAMPLE"));
}
mod conversation_index;
mod conversation_workflow;
mod conversations;
mod history_ui;
