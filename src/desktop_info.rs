use crate::proxy::SummaryMatch;
use crate::workbench::ContextMenu;
use crate::workspace::Workspace;
use rune::{El, Waker, col, el, icon, row, text};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
use windows_sys::Win32::{
    Foundation::{FILETIME, SYSTEMTIME},
    NetworkManagement::{IpHelper::*, Ndis::IfOperStatusUp},
    System::{Performance::*, SystemInformation::*, Time::*},
};

const INTERVAL: Duration = Duration::from_secs(1);
type Branches = HashMap<PathBuf, String>;
type Interfaces = HashMap<u64, (u64, u64)>;

pub fn local_clock(ms: i64) -> Option<String> {
    let ticks = u64::try_from(ms.checked_add(11_644_473_600_000)?)
        .ok()?
        .checked_mul(10_000)?;
    let ft = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        if FileTimeToSystemTime(&ft, &mut utc) == 0
            || SystemTimeToTzSpecificLocalTime(null(), &utc, &mut local) == 0
        {
            return None;
        }
    }
    Some(format!("{:02}:{:02}", local.wHour, local.wMinute))
}

struct Cpu {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}
impl Cpu {
    fn open() -> Option<Self> {
        let mut cpu = Self {
            query: null_mut(),
            counter: null_mut(),
        };
        unsafe {
            if PdhOpenQueryW(null(), 0, &mut cpu.query) != 0 {
                return None;
            }
            let path = windows_sys::core::w!(r"\Processor Information(_Total)\% Processor Time");
            if PdhAddEnglishCounterW(cpu.query, path, 0, &mut cpu.counter) != 0 {
                return None;
            }
        }
        Some(cpu)
    }
    fn percent(&self) -> Option<u32> {
        let mut value = PDH_FMT_COUNTERVALUE::default();
        unsafe {
            if PdhCollectQueryData(self.query) != 0
                || PdhGetFormattedCounterValue(self.counter, PDH_FMT_DOUBLE, null_mut(), &mut value)
                    != 0
                || !matches!(value.CStatus, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA)
            {
                return None;
            }
            let percent = value.Anonymous.doubleValue;
            percent
                .is_finite()
                .then(|| percent.clamp(0., 100.).round() as u32)
        }
    }
}
impl Drop for Cpu {
    fn drop(&mut self) {
        if !self.query.is_null() {
            unsafe {
                PdhCloseQuery(self.query);
            }
        }
    }
}

fn used_memory() -> Option<u64> {
    let mut memory = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    (unsafe { GlobalMemoryStatusEx(&mut memory) } != 0)
        .then(|| memory.ullTotalPhys.saturating_sub(memory.ullAvailPhys))
}

fn interfaces() -> Option<Interfaces> {
    let mut table = null_mut();
    unsafe {
        if GetIfTable2(&mut table) != 0 || table.is_null() {
            return None;
        }
        let rows =
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let up = || {
            rows.iter().filter(|r| {
                r.OperStatus == IfOperStatusUp
                    && !matches!(r.Type, IF_TYPE_SOFTWARE_LOOPBACK | IF_TYPE_TUNNEL)
            })
        };
        // Prefer hardware adapters to avoid counting both a virtual switch and its uplink.
        let physical = up().any(|r| r.InterfaceAndOperStatusFlags._bitfield & 1 != 0);
        let data = up()
            .filter(|r| !physical || r.InterfaceAndOperStatusFlags._bitfield & 1 != 0)
            .map(|r| (r.InterfaceLuid.Value, (r.InOctets, r.OutOctets)))
            .collect();
        FreeMibTable(table.cast());
        Some(data)
    }
}

fn network_rate(before: &Interfaces, now: &Interfaces, elapsed: Duration) -> Option<(u64, u64)> {
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0. {
        return None;
    }
    let mut down = 0u64;
    let mut up = 0u64;
    for (id, &(rx, tx)) in now {
        if let Some(&(prev_rx, prev_tx)) = before.get(id) {
            down = down.saturating_add(rx.saturating_sub(prev_rx));
            up = up.saturating_add(tx.saturating_sub(prev_tx));
        }
    }
    Some((
        (down as f64 / seconds).round() as u64,
        (up as f64 / seconds).round() as u64,
    ))
}

fn rate(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MiB/s", bytes as f64 / 1_048_576.)
    } else {
        format!("{:.0} KiB/s", bytes as f64 / 1024.)
    }
}

#[derive(Default, PartialEq)]
pub struct Metrics {
    cpu: Option<u32>,
    used: Option<u64>,
    net: Option<(u64, u64)>,
}

impl Metrics {
    pub fn summary(&self) -> Option<String> {
        if self.cpu.is_none() && self.used.is_none() {
            return None;
        }
        let cpu = self.cpu.map_or_else(|| "--".into(), |p| format!("{p}%"));
        let mem = self.used.map_or_else(
            || "--".into(),
            |b| format!("{:.1} GiB", b as f64 / 1_073_741_824.),
        );
        Some(format!("CPU {cpu} · 内存 {mem}"))
    }
}

pub fn bar(w: &Workspace) -> El {
    let metrics = &w.desktop.metrics;
    let cpu = metrics.cpu.map_or_else(|| "--".into(), |p| format!("{p}%"));
    let mem = metrics.used.map_or_else(
        || "--".into(),
        |b| format!("{:.1} GiB", b as f64 / 1_073_741_824.),
    );
    let (up, down) = metrics.net.map_or_else(
        || ("--".into(), "--".into()),
        |(rx, tx)| (rate(tx), rate(rx)),
    );
    let matched = w
        .current()
        .map(|t| w.layout.proxy.match_summary(&t.proxy_summary))
        .unwrap_or(SummaryMatch::System(None));
    let open = matches!(w.layout.menu, Some(ContextMenu::ProxySwitcher));
    let (dot_color, pill_fill, pill_border, pill_hover, value_color) = match &matched {
        SummaryMatch::Profile { .. } | SummaryMatch::Custom(_) => {
            (0x5eb8ff, 0x162942, 0x2f5a8c, 0x1c3556, 0xe3f1ff)
        }
        SummaryMatch::Direct => (0x5bd49a, 0x142923, 0x285a48, 0x1a352e, 0xd4f6e4),
        SummaryMatch::System(_) => (0x7f93ab, 0x182130, 0x2a374b, 0x202c3f, 0xc9d5e5),
    };
    let value_label = matched.pill_label();
    let help_text = format!(
        "当前终端代理：{}。点击一键切换预设（Agent 终端将平滑重开并续接原对话）。",
        matched.detail_label()
    );
    let switcher = row()
        .key("proxy-switcher")
        .role("button")
        .label(format!("代理：{value_label}"))
        .help(help_text)
        .focusable(true)
        .h(22.)
        .px(8.)
        .gap(5.)
        .items_center()
        .radius(6.)
        .fill(crate::sidebar::ink(if open {
            pill_hover
        } else {
            pill_fill
        }))
        .border(
            1.,
            crate::sidebar::ink(if open { 0x4b8ed6 } else { pill_border }),
        )
        .hover(crate::sidebar::ink(pill_hover))
        .on_key(Workspace::workbench_key)
        .on_press_tx(|w: &mut Workspace, tx| {
            tx.instant();
            if matches!(w.layout.menu, Some(ContextMenu::ProxySwitcher)) {
                w.layout.menu = None;
            } else {
                w.layout.menu = Some(ContextMenu::ProxySwitcher);
            }
        })
        .child(
            el().size(6., 6.)
                .radius(3.)
                .fill(crate::sidebar::ink(dot_color)),
        )
        .child(icon(&crate::sidebar::icons::GLOBE, 12.).color(crate::sidebar::ink(0x889eb8)))
        .child(
            text("代理")
                .font(crate::sidebar::ui_font(11.5, 450.))
                .color(crate::sidebar::ink(0x889eb8)),
        )
        .child(
            text(value_label)
                .font(crate::sidebar::ui_font(11.5, 580.))
                .truncate()
                .color(crate::sidebar::ink(value_color)),
        )
        .child(icon(&crate::sidebar::icons::DOWN, 10.).color(crate::sidebar::ink(0x8da3bd)));

    let metric_pair = |label: &'static str, value: String| {
        row()
            .items_center()
            .gap(4.)
            .child(
                text(label)
                    .font(crate::sidebar::ui_font(11., 460.))
                    .color(crate::sidebar::ink(0x7d90a8)),
            )
            .child(
                text(value)
                    .font(crate::sidebar::ui_font(11.5, 540.))
                    .color(crate::sidebar::ink(0xcbd6e6)),
            )
    };
    let sep = || el().w(1.).h(10.).fill(crate::sidebar::ink(0x273447));
    let stats = row()
        .items_center()
        .gap(10.)
        .min_w(0.)
        .child(metric_pair("CPU", cpu))
        .child(sep())
        .child(metric_pair("内存", mem))
        .child(sep())
        .child(metric_pair("↑", up))
        .child(metric_pair("↓", down));

    col()
        .key("system-metrics")
        .w_full()
        .h(30.)
        .fill(crate::sidebar::ink(0x111824))
        .child(
            row()
                .w_full()
                .grow(1.)
                .items_center()
                .px(10.)
                .gap(10.)
                .child(switcher)
                .child(el().grow(1.))
                .child(stats),
        )
        .child(el().w_full().h(1.).fill(crate::sidebar::ink(0x222d3e)))
}

fn worktree_head(project: &Path) -> Option<PathBuf> {
    for parent in project.ancestors() {
        let dot = parent.join(".git");
        let Ok(meta) = dot.metadata() else { continue };
        if meta.is_dir() {
            return Some(dot.join("HEAD"));
        }
        if !meta.is_file() || meta.len() > 32_768 {
            return None;
        }
        let file = std::fs::read_to_string(&dot).ok()?;
        let git_dir = file
            .strip_prefix("gitdir: ")?
            .trim_end_matches(['\r', '\n']);
        return Some(parent.join(git_dir).join("HEAD"));
    }
    None
}

fn branch_name(head: &Path) -> Option<String> {
    use std::io::Read;
    let mut head_text = String::new();
    std::fs::File::open(head)
        .ok()?
        .take(4097)
        .read_to_string(&mut head_text)
        .ok()?;
    let value = head_text.trim_end();
    if head_text.len() > 4096 || value.chars().any(char::is_control) {
        return None;
    }
    if let Some(name) = value
        .strip_prefix("ref: refs/heads/")
        .filter(|s| !s.is_empty())
    {
        Some(name.into())
    } else if matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        Some(value[..8].into())
    } else {
        None
    }
}

fn read_branches(projects: &[PathBuf]) -> Branches {
    let mut heads = HashMap::new();
    let mut branches = HashMap::new();
    for project in projects {
        if let Some(head) = worktree_head(project)
            && let Some(name) = heads
                .entry(head.clone())
                .or_insert_with(|| branch_name(&head))
        {
            branches.insert(project.clone(), name.clone());
        }
    }
    branches
}

#[derive(Default)]
struct Pending {
    metrics: Option<Metrics>,
    branches: Option<Branches>,
}

pub struct DesktopInfo {
    pub metrics: Metrics,
    pub branches: Branches,
    pending: Arc<Mutex<Pending>>,
    metrics_enabled: Arc<AtomicBool>,
    paths: Vec<PathBuf>,
    projects: Option<mpsc::Sender<Vec<PathBuf>>>,
    stop: Option<mpsc::Sender<()>>,
}
impl Default for DesktopInfo {
    fn default() -> Self {
        Self {
            metrics: Metrics::default(),
            branches: Branches::new(),
            pending: Arc::default(),
            metrics_enabled: Arc::new(AtomicBool::new(true)),
            paths: Vec::new(),
            projects: None,
            stop: None,
        }
    }
}
impl DesktopInfo {
    pub fn set_metrics_enabled(&self, enabled: bool) {
        self.metrics_enabled.store(enabled, Ordering::Release);
    }

    pub fn start(&mut self, wake: Waker) {
        let (stop, exit) = mpsc::channel();
        self.stop = Some(stop);
        let pending = self.pending.clone();
        let enabled = self.metrics_enabled.clone();
        let notify = wake.clone();
        std::thread::spawn(move || {
            let cpu = Cpu::open();
            let mut previous: Option<(Instant, Interfaces)> = None;
            loop {
                if enabled.load(Ordering::Acquire) {
                    let instant = Instant::now();
                    let current = interfaces();
                    let net =
                        current
                            .as_ref()
                            .zip(previous.as_ref())
                            .and_then(|(now, (at, old))| {
                                network_rate(old, now, instant.duration_since(*at))
                            });
                    previous = current.map(|v| (instant, v));
                    let metrics = Metrics {
                        cpu: cpu.as_ref().and_then(Cpu::percent),
                        used: used_memory(),
                        net,
                    };
                    pending.lock().unwrap().metrics = Some(metrics);
                    notify();
                } else {
                    previous = None;
                }
                if !matches!(
                    exit.recv_timeout(INTERVAL),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    break;
                }
            }
        });
        let (send, receive) = mpsc::channel::<Vec<PathBuf>>();
        self.projects = Some(send);
        let pending = self.pending.clone();
        std::thread::spawn(move || {
            let mut paths = Vec::new();
            let mut old = Branches::new();
            loop {
                match receive.recv_timeout(INTERVAL) {
                    Ok(next) => paths = receive.try_iter().last().unwrap_or(next),
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => (),
                }
                let branches = read_branches(&paths);
                if branches != old {
                    old.clone_from(&branches);
                    pending.lock().unwrap().branches = Some(branches);
                    wake();
                }
            }
        });
    }
    pub fn projects(&mut self, tabs: &[crate::workspace::Tab]) {
        let Some(send) = &self.projects else { return };
        let mut paths: Vec<_> = tabs
            .iter()
            .filter(|t| !t.closing && t.project.is_absolute())
            .map(|t| t.project.clone())
            .collect();
        paths.sort();
        paths.dedup();
        if paths != self.paths {
            self.paths.clone_from(&paths);
            let _ = send.send(paths);
        }
    }
    pub fn poll(&mut self) -> bool {
        let mut pending = self.pending.lock().unwrap();
        let mut changed = false;
        if let Some(m) = pending.metrics.take() {
            changed |= m != self.metrics;
            self.metrics = m;
        }
        if let Some(b) = pending.branches.take() {
            changed |= b != self.branches;
            self.branches = b;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_nested_worktree_detach_and_plain_directory_follow_head_changes() {
        let root = crate::tests::phase_four::folder("git-branches");
        let repo = root.join("main");
        let child = repo.join("src");
        let worktree = root.join("linked");
        let admin = root.join("storage/worktrees/linked");
        let plain = root.join("plain");
        for p in [&repo.join(".git"), &child, &admin, &worktree, &plain] {
            std::fs::create_dir_all(p).unwrap();
        }
        // Make the ordinary directory independent of the enclosing test repository.
        std::fs::write(plain.join(".git"), "unreadable marker").unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            worktree.join(".git"),
            "gitdir: ../storage/worktrees/linked\r\n",
        )
        .unwrap();
        std::fs::write(admin.join("HEAD"), "ref: refs/heads/feat/nested\n").unwrap();
        let projects = [repo.clone(), child.clone(), worktree.clone(), plain.clone()];
        let b = read_branches(&projects);
        assert_eq!(b[&repo], "main");
        assert_eq!(b[&child], "main");
        assert_eq!(b[&worktree], "feat/nested");
        assert!(!b.contains_key(&plain));
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/review\n").unwrap();
        std::fs::write(
            admin.join("HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        let b = read_branches(&projects);
        assert_eq!(b[&child], "review");
        assert_eq!(b[&worktree], "01234567");
    }

    #[test]
    fn interface_replacement_reset_idle_and_missing_samples_have_distinct_rates() {
        let one = Duration::from_secs(2);
        let before = Interfaces::from([(11, (500, 60)), (12, (9000, 300))]);
        let after = Interfaces::from([(11, (540, 70)), (13, (750_000, 480_000))]);
        assert_eq!(network_rate(&before, &after, one), Some((20, 5)));
        assert_eq!(network_rate(&after, &after, one), Some((0, 0)));
        assert_eq!(network_rate(&after, &before, one), Some((0, 0)));
        assert_eq!(network_rate(&before, &after, Duration::ZERO), None);
        assert_eq!(local_clock(i64::MAX), None);
    }

    #[test]
    fn native_machine_sampler_publishes_physical_memory_cpu_and_network_without_a_terminal() {
        let (send, recv) = mpsc::channel();
        let mut desk = DesktopInfo::default();
        desk.start(Arc::new(move || {
            let _ = send.send(());
        }));
        let until = Instant::now() + Duration::from_secs(12);
        loop {
            recv.recv_timeout(until.saturating_duration_since(Instant::now()))
                .expect("Windows sampler did not wake");
            desk.poll();
            if let (Some(cpu), Some(bytes), Some((down, up))) =
                (desk.metrics.cpu, desk.metrics.used, desk.metrics.net)
            {
                assert!(cpu <= 100 && bytes > 0);
                println!("native CPU {cpu}% used {bytes} B, network in {down} B/s out {up} B/s");
                break;
            }
        }
    }
}
