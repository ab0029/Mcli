use crate::clawbot::qr::{self, QrMatrix};
use crate::clawbot::router::{self, MoreQueue, ParsedCommand};
use crate::clawbot::service::{Control, Event, Service};
use crate::clawbot::{AuditLog, ConnectionState, LastBoundTarget};
use crate::shell::Foreground;
use crate::terminal::SessionId;
use crate::workspace::{Tab, Workspace};
use alacritty_terminal::term::TermMode;
use rune::Tx;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(45);
const SHELL_SETTLE_DELAY: Duration = Duration::from_secs(1);

struct PendingLaunch {
    tab_id: SessionId,
    agent: Option<String>,
    launched_cli: bool,
    deferred_prompt: Option<String>,
    context_token: String,
    created_at: Instant,
}

struct PendingReply {
    context_token: Option<String>,
    saw_running: bool,
    notified_waiting: bool,
    awaiting_transcript_gen: Option<u64>,
    fallback_deadline: Option<Instant>,
}

struct PendingShell {
    tab_id: SessionId,
    context_token: String,
    ready_at: Instant,
}

struct PendingLast {
    tab_id: SessionId,
    count: usize,
    generation: u64,
    context_token: String,
    deadline: Instant,
}

#[derive(Default)]
pub struct ClawbotPanel {
    service: Option<Service>,
    data_dir: Option<PathBuf>,
    pub state: ConnectionState,
    pub qr_matrix: Option<QrMatrix>,
    qr_url_cached: String,
    pub bound_tab: Option<SessionId>,
    pub paused: bool,
    pub audit: AuditLog,
    pub notice: Option<String>,
    more: MoreQueue,
    pending_launch: Option<PendingLaunch>,
    pending_replies: HashMap<SessionId, PendingReply>,
    pending_shell: Option<PendingShell>,
    pending_last: Option<PendingLast>,
    pending_startup_notice: Option<Instant>,
    startup_notice_sent: bool,
    last_tab_states: HashMap<SessionId, String>,
    reply_gen: u64,
}

impl Default for ConnectionState {
    fn default() -> Self {
        Self::Disabled
    }
}

impl ClawbotPanel {
    pub fn stop(&mut self) {
        self.service.take();
        self.pending_launch = None;
        self.pending_shell = None;
        self.pending_last = None;
        self.pending_replies.clear();
    }
}

fn format_tab_brief(index: usize, tab: &Tab, is_bound: bool) -> String {
    let dir_name = tab
        .directory
        .file_name()
        .unwrap_or(tab.directory.as_os_str())
        .to_string_lossy();
    let mark = if is_bound { "★ " } else { "" };
    let label = tab.label();
    let state = tab.state_text();
    let preview = tab.preview.trim();
    if preview.is_empty() {
        format!("{mark}[{index}] {dir_name} · {label} ({state})")
    } else {
        let short: String = preview.chars().take(42).collect();
        format!("{mark}[{index}] {dir_name} · {label} ({state}) -- {short}")
    }
}

fn parse_agent_name(raw: &str) -> Option<Foreground> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "codex" => Some(Foreground::Codex),
        "agy" => Some(Foreground::Agy),
        "kimi" => Some(Foreground::Kimi),
        "grok" => Some(Foreground::Grok),
        _ => None,
    }
}

fn format_prompt_bytes(text: &str, mode: TermMode) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 16);
    let clean = text.replace("\r\n", "\n");
    if mode.contains(TermMode::BRACKETED_PASTE) && clean.contains('\n') {
        out.extend_from_slice(b"\x1b[200~");
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
    } else {
        out.extend_from_slice(clean.replace('\n', " ").as_bytes());
    }
    out.push(b'\r');
    out
}

impl Workspace {
    pub fn init_clawbot(&mut self, data_dir: PathBuf) {
        self.clawbot.data_dir = Some(data_dir);
        if self.layout.clawbot.enabled {
            self.ensure_clawbot_service();
        } else {
            self.clawbot.state = ConnectionState::Disabled;
        }
    }

    pub fn ensure_clawbot_service(&mut self) {
        if self.clawbot.service.is_some() {
            return;
        }
        let Some(dir) = self.clawbot.data_dir.clone() else {
            return;
        };
        let service = Service::start(dir, self.layout.clawbot.clone(), self.wake.clone());
        self.clawbot.service = Some(service);
    }

    pub fn sync_clawbot_prefs(&mut self) {
        if !self.layout.clawbot.enabled {
            self.clawbot.stop();
            self.clawbot.state = ConnectionState::Disabled;
            self.clawbot.qr_matrix = None;
            self.clawbot.qr_url_cached.clear();
            self.clawbot.paused = false;
            self.sync_tray_tooltip();
            return;
        }
        self.ensure_clawbot_service();
        if let Some(svc) = &self.clawbot.service {
            svc.send_control(Control::ApplyPrefs(self.layout.clawbot.clone()));
        }
    }

    pub fn clawbot_start_qr(&mut self, tx: &mut Tx) {
        tx.instant();
        if !self.layout.clawbot.enabled {
            self.layout.clawbot.enabled = true;
            self.save_clawbot_prefs();
        }
        self.ensure_clawbot_service();
        if let Some(svc) = &self.clawbot.service {
            self.clawbot.notice = Some("正在向微信请求授权二维码…".into());
            svc.send_control(Control::StartQrLogin);
        }
    }

    pub fn clawbot_cancel_qr(&mut self, tx: &mut Tx) {
        tx.instant();
        self.clawbot.qr_matrix = None;
        self.clawbot.qr_url_cached.clear();
        self.clawbot.notice = None;
        if let Some(svc) = &self.clawbot.service {
            svc.send_control(Control::CancelQrLogin);
        }
    }

    pub fn clawbot_unbind(&mut self, tx: &mut Tx) {
        tx.instant();
        self.clawbot.qr_matrix = None;
        self.clawbot.qr_url_cached.clear();
        self.clawbot.paused = false;
        self.clawbot.bound_tab = None;
        self.clawbot.pending_replies.clear();
        self.clawbot.pending_launch = None;
        self.clawbot.pending_last = None;
        self.clawbot
            .audit
            .push("解绑", "微信账号", "已清除本机加密凭据");
        self.clawbot.notice = Some("已解除微信绑定并清除本机凭据".into());
        if let Some(svc) = &self.clawbot.service {
            svc.send_control(Control::Unbind);
        }
        self.sync_tray_tooltip();
    }

    pub fn clawbot_toggle_pause(&mut self, tx: &mut Tx) {
        tx.instant();
        let next = !self.clawbot.paused;
        self.clawbot.paused = next;
        self.clawbot.audit.push(
            if next { "暂停" } else { "恢复" },
            "远程通道",
            if next {
                "已暂停响应手机指令"
            } else {
                "已恢复响应手机指令"
            },
        );
        if let Some(svc) = &self.clawbot.service {
            svc.send_control(Control::SetPaused(next));
        }
        self.sync_tray_tooltip();
    }

    pub fn clawbot_claim_ownership(&mut self, tx: &mut Tx) {
        tx.instant();
        self.ensure_clawbot_service();
        if let Some(svc) = &self.clawbot.service {
            svc.send_control(Control::ClaimOwnership);
        }
    }

    fn sync_tray_tooltip(&self) {
        let name = crate::single_instance::app_display_name();
        let tip = match &self.clawbot.state {
            ConnectionState::Connected { paused: false, .. } => {
                format!("{name} · 多 Agent 终端工作台（微信已连接）")
            }
            ConnectionState::Connected { paused: true, .. } => {
                format!("{name} · 多 Agent 终端工作台（微信已暂停）")
            }
            ConnectionState::Reconnecting { .. } => {
                format!("{name} · 多 Agent 终端工作台（微信重连中）")
            }
            _ => format!("{name} · 多 Agent 终端工作台"),
        };
        self.set_tray_tooltip(&tip);
    }

    fn clawbot_reply(&mut self, text: &str, context_token: Option<String>) {
        let chunks = self.clawbot.more.prepare(text);
        if let Some(svc) = &self.clawbot.service {
            for chunk in chunks {
                svc.send_text(chunk, context_token.clone());
            }
        }
    }

    fn clawbot_reply_raw(&self, text: String, context_token: Option<String>) {
        if let Some(svc) = &self.clawbot.service {
            svc.send_text(text, context_token);
        }
    }

    fn live_tabs(&self) -> Vec<(usize, &Tab)> {
        self.tabs
            .iter()
            .filter(|t| !t.closing)
            .enumerate()
            .map(|(i, t)| (i + 1, t))
            .collect()
    }

    fn bound_tab_ref(&self) -> Option<&Tab> {
        let id = self.clawbot.bound_tab?;
        self.tabs.iter().find(|t| t.id == id && !t.closing)
    }

    fn bound_tab_ended_notice(&self) -> Option<String> {
        let tab = self.bound_tab_ref()?;
        tab.ended.as_ref()?;
        let dir = tab
            .directory
            .file_name()
            .unwrap_or(tab.directory.as_os_str())
            .to_string_lossy();
        Some(format!(
            "⚠️ 当前绑定的终端（{dir} · {}）已退出。可发送 /reopen 在原目录重开，或发送 /list 与 /use 切换其他终端。",
            tab.label()
        ))
    }

    fn remember_bound_tab(&mut self, id: SessionId) {
        self.clawbot.bound_tab = Some(id);
        let Some(tab) = self.tabs.iter().find(|t| t.id == id && !t.closing) else {
            return;
        };
        let key = tab.bound.as_ref().or(tab.expected.as_ref());
        let next = LastBoundTarget {
            directory: tab.directory.clone(),
            agent: key
                .map(|k| k.agent.clone())
                .or_else(|| tab.foreground.map(|f| f.name().to_owned())),
            conversation_id: key.map(|k| k.id.clone()),
        };
        if self.layout.clawbot.last_bound.as_ref() != Some(&next) {
            self.layout.clawbot.last_bound = Some(next);
            self.save_clawbot_prefs();
        }
    }

    fn sync_bound_target_metadata(&mut self) {
        let Some(id) = self.clawbot.bound_tab else {
            return;
        };
        let Some(tab) = self
            .tabs
            .iter()
            .find(|t| t.id == id && !t.closing && t.ended.is_none())
        else {
            return;
        };
        let key = tab.bound.as_ref().or(tab.expected.as_ref());
        let next = LastBoundTarget {
            directory: tab.directory.clone(),
            agent: key
                .map(|k| k.agent.clone())
                .or_else(|| tab.foreground.map(|f| f.name().to_owned())),
            conversation_id: key.map(|k| k.id.clone()),
        };
        if self.layout.clawbot.last_bound.as_ref() != Some(&next) {
            self.layout.clawbot.last_bound = Some(next);
            self.save_clawbot_prefs();
        }
    }

    fn try_auto_rebind_last_target(&mut self) -> bool {
        if self.bound_tab_ref().is_some() {
            return false;
        }
        let Some(target) = self.layout.clawbot.last_bound.clone() else {
            return false;
        };
        // 1. Exact match by (agent, conversation_id) on bound or expected key.
        if let (Some(agent), Some(conv_id)) = (&target.agent, &target.conversation_id) {
            if let Some(tab) = self.tabs.iter().find(|t| {
                !t.closing
                    && t.ended.is_none()
                    && t.bound
                        .as_ref()
                        .or(t.expected.as_ref())
                        .is_some_and(|k| k.agent == *agent && k.id == *conv_id)
            }) {
                let id = tab.id;
                let label = tab.label();
                self.clawbot.bound_tab = Some(id);
                self.clawbot
                    .audit
                    .push("自动重绑", label, "已按原官方对话 ID 重新绑定");
                return true;
            }
        }
        // 2. Match by normalized project directory (+ agent if specified).
        let want_dir = crate::workspace::project_key(&target.directory);
        let want_fg = target.agent.as_deref().and_then(parse_agent_name);
        let candidates: Vec<SessionId> = self
            .tabs
            .iter()
            .filter(|t| {
                !t.closing
                    && t.ended.is_none()
                    && crate::workspace::project_key(&t.directory) == want_dir
                    && match want_fg {
                        Some(fg) => {
                            t.foreground == Some(fg)
                                || t.expected
                                    .as_ref()
                                    .is_some_and(|k| parse_agent_name(&k.agent) == Some(fg))
                        }
                        None => t.foreground.is_none(),
                    }
            })
            .map(|t| t.id)
            .collect();
        if let [only] = candidates.as_slice() {
            let id = *only;
            let label = self
                .tabs
                .iter()
                .find(|t| t.id == id)
                .map(Tab::label)
                .unwrap_or_default();
            self.clawbot.bound_tab = Some(id);
            self.clawbot
                .audit
                .push("自动重绑", label, "已按原项目目录重新绑定");
            return true;
        }
        false
    }

    fn prune_closed_clawbot_tabs(&mut self) -> bool {
        let mut changed = false;
        let live_ids: std::collections::HashSet<SessionId> = self
            .tabs
            .iter()
            .filter(|t| !t.closing)
            .map(|t| t.id)
            .collect();
        self.clawbot
            .last_tab_states
            .retain(|id, _| live_ids.contains(id));

        let removed_pending: Vec<(SessionId, Option<String>)> = self
            .clawbot
            .pending_replies
            .keys()
            .copied()
            .filter(|id| !live_ids.contains(id))
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|id| {
                self.clawbot
                    .pending_replies
                    .remove(&id)
                    .map(|p| (id, p.context_token))
            })
            .collect();

        for (id, ctx) in removed_pending {
            changed = true;
            if self.clawbot.bound_tab == Some(id) {
                self.clawbot.bound_tab = None;
            }
            self.clawbot
                .audit
                .push("终端关闭", format!("tab-{}", id.0), "已终止未完成回传");
            self.clawbot_reply(
                "⚠️ 正在等待回复的终端已被关闭，任务已中断。可发送 /reopen 重开或 /list 切换其他终端。",
                ctx,
            );
        }
        if let Some(bound_id) = self.clawbot.bound_tab
            && !live_ids.contains(&bound_id)
        {
            self.clawbot.bound_tab = None;
            changed = true;
        }
        if let Some(pending) = &self.clawbot.pending_last
            && !live_ids.contains(&pending.tab_id)
        {
            self.clawbot.pending_last = None;
            changed = true;
        }
        changed
    }

    fn bound_description(&self) -> String {
        if let Some(tab) = self.bound_tab_ref() {
            let idx = self
                .live_tabs()
                .into_iter()
                .find(|(_, t)| t.id == tab.id)
                .map(|(i, _)| i)
                .unwrap_or(0);
            format_tab_brief(idx, tab, false)
        } else if let (Some(dir), Some(agent)) = (
            self.layout.clawbot.default_project.as_ref(),
            self.layout.clawbot.normalized_agent(),
        ) {
            format!("未手动绑定（默认自动使用 {} · {agent}）", dir.display())
        } else {
            "未绑定（请先发送 /list 并用 /use <编号> 选择终端）".into()
        }
    }

    pub fn poll_clawbot(&mut self, _clock: f64, tx: &mut Tx) -> bool {
        let events = self
            .clawbot
            .service
            .as_ref()
            .map(|s| s.receive.try_iter().take(64).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut changed = false;

        for event in events {
            changed = true;
            match event {
                Event::State(next) => {
                    if let ConnectionState::WaitingQr { ref qr_url, .. } = next {
                        if self.clawbot.qr_url_cached != *qr_url {
                            self.clawbot.qr_url_cached = qr_url.clone();
                            self.clawbot.qr_matrix = qr::encode(qr_url).ok();
                        }
                        self.clawbot.notice = None;
                    } else {
                        self.clawbot.qr_matrix = None;
                        self.clawbot.qr_url_cached.clear();
                    }
                    if let ConnectionState::Connected { paused, .. } = next {
                        self.clawbot.paused = paused;
                        self.clawbot.notice = None;
                    }
                    self.clawbot.state = next;
                    self.sync_tray_tooltip();
                }
                Event::RejectedSender { from_user_id } => {
                    self.clawbot.audit.push(
                        "拦截",
                        crate::clawbot::mask_user_id(&from_user_id),
                        "非绑定微信账号，已拒绝",
                    );
                }
                Event::RestoredOnline { has_active_window } => {
                    if has_active_window
                        && !self.clawbot.startup_notice_sent
                        && !self.clawbot.paused
                    {
                        let due = Instant::now() + Duration::from_millis(800);
                        self.clawbot.pending_startup_notice = Some(due);
                        tx.wake_after(Duration::from_millis(800));
                    }
                }
                Event::OfflineSkipped {
                    count,
                    context_token,
                } => {
                    self.clawbot.audit.push(
                        "离线保护",
                        "微信",
                        format!("跳过重启前积压的 {count} 条旧消息"),
                    );
                    self.clawbot_reply_raw(
                        format!(
                            "⚠️ Mcli 刚重新启动，已跳过离线期间积压的 {count} 条旧消息（为防误执行）。如需继续，请重新发送指令。"
                        ),
                        context_token,
                    );
                }
                Event::KeepaliveSent { hours_left } => {
                    self.clawbot.audit.push(
                        "保活提醒",
                        "微信",
                        format!("令牌到期前约 {hours_left} 小时已发送提醒"),
                    );
                }
                Event::DeliverySkipped(reason) => {
                    self.clawbot.notice = Some(reason.clone());
                    self.clawbot.audit.push("跳过推送", "微信", reason);
                }
                Event::DeliveryError(err) => {
                    self.clawbot.notice = Some(format!("微信发送失败：{err}"));
                    self.clawbot.audit.push("发送失败", "微信", err);
                }
                Event::Inbound {
                    context_token,
                    text,
                    ..
                } => {
                    self.handle_clawbot_inbound(text, context_token, tx);
                }
            }
        }

        // Prune removed tabs, auto-rebind restored target, and keep target metadata fresh.
        changed |= self.prune_closed_clawbot_tabs();
        changed |= self.try_auto_rebind_last_target();
        self.sync_bound_target_metadata();
        // Send one concise startup notice after tabs have had a moment to restore.
        if let Some(due) = self.clawbot.pending_startup_notice {
            let now = Instant::now();
            if now >= due {
                self.clawbot.pending_startup_notice = None;
                if !self.clawbot.startup_notice_sent
                    && self.clawbot.state.is_online()
                    && !self.clawbot.paused
                {
                    self.clawbot.startup_notice_sent = true;
                    let count = self.live_tabs().len();
                    let bound = self.bound_description();
                    let name = crate::single_instance::app_display_name();
                    self.clawbot
                        .audit
                        .push("上线简报", "微信", format!("已恢复 {count} 个终端"));
                    self.clawbot_reply_raw(
                        format!(
                            "🟢 {name} 已重新启动并恢复连接（当前 {count} 个终端）。\n当前远程目标：{bound}\n注：关窗前未完成的任务不会自动续跑，可发送 /list 查看或直接继续对话。"
                        ),
                        None,
                    );
                    changed = true;
                }
            } else {
                tx.wake_after(due - now);
            }
        }
        // Advance pending CLI auto-launch or deferred prompt injection.
        changed |= self.poll_clawbot_launch(tx);
        // Advance pending `/sh` settle capture.
        changed |= self.poll_clawbot_shell(tx);
        // Advance pending `/last` transcript fetch timeout fallback.
        changed |= self.poll_clawbot_last(tx);
        // Observe tab state transitions for completion / waiting / error replies.
        changed |= self.poll_clawbot_tab_transitions(tx);
        changed
    }

    fn handle_clawbot_inbound(&mut self, text: String, ctx: String, tx: &mut Tx) {
        let cmd = router::parse_command(&text);
        match cmd {
            ParsedCommand::Help => {
                let help = router::help_text(
                    &self.bound_description(),
                    self.layout.clawbot.allow_raw_shell,
                );
                self.clawbot.audit.push("/help", "工作台", "返回指令说明");
                self.clawbot_reply(&help, Some(ctx));
            }
            ParsedCommand::Ping => {
                self.clawbot
                    .audit
                    .push("/ping", "微信", "会话令牌已续期 24 小时");
                self.clawbot_reply_raw(
                    "✅ Mcli 在线，微信会话令牌已续期 24 小时。".to_owned(),
                    Some(ctx),
                );
            }
            ParsedCommand::List => {
                let live = self.live_tabs();
                let reply = if live.is_empty() {
                    "当前 Mcli 窗口暂无活跃终端。可发送 /new <目录> [agent] 新建。".into()
                } else {
                    let mut lines = vec![format!(
                        "当前共 {} 个终端（★ 为当前远程目标）：",
                        live.len()
                    )];
                    for (idx, tab) in live {
                        let is_bound = self.clawbot.bound_tab == Some(tab.id);
                        lines.push(format_tab_brief(idx, tab, is_bound));
                    }
                    lines.push("发送 /use <编号> 切换远程目标。".into());
                    lines.join("\n")
                };
                self.clawbot.audit.push("/list", "工作台", "返回终端列表");
                self.clawbot_reply(&reply, Some(ctx));
            }
            ParsedCommand::Use(arg) => {
                let Some(query) = arg else {
                    let msg = format!("当前远程目标：{}", self.bound_description());
                    self.clawbot.audit.push("/use", "查询", "返回当前目标");
                    self.clawbot_reply(&msg, Some(ctx));
                    return;
                };
                let live = self.live_tabs();
                if let Ok(num) = query.parse::<usize>() {
                    if let Some((_, tab)) = live.iter().find(|(i, _)| *i == num) {
                        let id = tab.id;
                        let desc = format_tab_brief(num, tab, false);
                        let is_agent = tab.foreground.is_some()
                            || tab.bound.is_some()
                            || tab.expected.is_some();
                        self.remember_bound_tab(id);
                        self.clawbot.audit.push("/use", desc.clone(), "已切换目标");
                        let msg = if is_agent {
                            format!(
                                "已切换远程目标为：{desc}\n发送 /r 查看最后一条完整回复（/r 3 查看最近 3 轮）。"
                            )
                        } else {
                            format!("已切换远程目标为：{desc}")
                        };
                        self.clawbot_reply(&msg, Some(ctx));
                    } else {
                        self.clawbot_reply(
                            &format!("未找到编号 {num}，请先发送 /list 查看有效编号。"),
                            Some(ctx),
                        );
                    }
                    return;
                }
                let q = query.to_lowercase();
                let matches: Vec<(usize, SessionId, String, bool)> = live
                    .into_iter()
                    .filter(|(_, tab)| {
                        tab.directory.to_string_lossy().to_lowercase().contains(&q)
                            || tab.label().to_lowercase().contains(&q)
                    })
                    .map(|(i, tab)| {
                        let is_agent = tab.foreground.is_some()
                            || tab.bound.is_some()
                            || tab.expected.is_some();
                        (i, tab.id, format_tab_brief(i, tab, false), is_agent)
                    })
                    .collect();
                match matches.as_slice() {
                    [] => {
                        self.clawbot_reply(
                            &format!("未匹配到包含「{query}」的终端，请发送 /list 查看。"),
                            Some(ctx),
                        );
                    }
                    [(_, id, desc, is_agent)] => {
                        self.remember_bound_tab(*id);
                        self.clawbot.audit.push("/use", desc.clone(), "已切换目标");
                        let msg = if *is_agent {
                            format!(
                                "已切换远程目标为：{desc}\n发送 /r 查看最后一条完整回复（/r 3 查看最近 3 轮）。"
                            )
                        } else {
                            format!("已切换远程目标为：{desc}")
                        };
                        self.clawbot_reply(&msg, Some(ctx));
                    }
                    many => {
                        let mut lines =
                            vec![format!("匹配到 {} 个终端，请用编号精确选择：", many.len())];
                        for (_, _, desc, _) in many {
                            lines.push(desc.clone());
                        }
                        self.clawbot_reply(&lines.join("\n"), Some(ctx));
                    }
                }
            }
            ParsedCommand::Status => {
                let reply = if let Some(tab) = self.bound_tab_ref() {
                    let proxy_desc = self
                        .layout
                        .proxy
                        .match_summary(&tab.proxy_summary)
                        .detail_label();
                    let mut lines = vec![
                        format!("目标目录：{}", tab.directory.display()),
                        format!("前台工具：{}", tab.label()),
                        format!("当前状态：{}", tab.state_text()),
                        format!("当前代理：{proxy_desc}"),
                    ];
                    if !tab.preview.trim().is_empty() {
                        lines.push(format!("最近摘要：{}", tab.preview.trim()));
                    }
                    if let Some(err) = &tab.error {
                        lines.push(format!("错误提示：{err}"));
                    }
                    if self.layout.show_metrics_bar
                        && let Some(summary) = self.desktop.metrics.summary()
                    {
                        lines.push(format!("整机状态：{summary}"));
                    }
                    if tab.foreground.is_some() || tab.bound.is_some() || tab.expected.is_some() {
                        lines.push(
                            "快捷查看：发送 /r 读取最后一条完整回复（/r 3 读取最近 3 轮）。".into(),
                        );
                    }
                    lines.join("\n")
                } else {
                    format!("尚未绑定终端。{}", self.bound_description())
                };
                self.clawbot.audit.push("/status", "当前目标", "返回状态");
                self.clawbot_reply(&reply, Some(ctx));
            }
            ParsedCommand::Last { count } => {
                self.handle_clawbot_last(count, ctx, tx);
            }
            ParsedCommand::Proxy { target, restart } => {
                self.handle_clawbot_proxy(target, restart, ctx, tx);
            }
            ParsedCommand::New {
                target,
                agent,
                proxy,
            } => {
                self.handle_clawbot_new(target, agent, proxy, None, ctx, tx);
            }
            ParsedCommand::Reopen => {
                self.handle_clawbot_reopen(ctx, tx);
            }
            ParsedCommand::Screen { lines } => {
                let reply = match self.bound_tab_ref().and_then(|t| t.session.as_ref()) {
                    Some(session) => {
                        let snap = session.screen_snapshot(lines);
                        if snap.is_empty() {
                            "当前终端屏幕为空。".into()
                        } else {
                            let label = self.bound_tab_ref().map(Tab::label).unwrap_or_default();
                            format!("[{label} 末屏快照]\n{snap}")
                        }
                    }
                    None => "当前未绑定活跃终端，请先用 /list 与 /use 选择。".into(),
                };
                self.clawbot.audit.push(
                    "/screen",
                    self.bound_description(),
                    format!("抓取 {lines} 行"),
                );
                self.clawbot_reply(&reply, Some(ctx));
            }
            ParsedCommand::Stop => {
                let target_id = self.clawbot.bound_tab;
                if let Some(tab) = self
                    .tabs
                    .iter()
                    .find(|t| Some(t.id) == target_id && !t.closing)
                    && let Some(session) = &tab.session
                    && tab.ended.is_none()
                {
                    let label = tab.label();
                    session.input(b"\x03".to_vec());
                    self.clawbot
                        .audit
                        .push("/stop", label.clone(), "已发送 Ctrl+C");
                    self.clawbot_reply(&format!("已向 {label} 发送 Ctrl+C 中断信号。"), Some(ctx));
                } else {
                    self.clawbot_reply("当前未绑定运行中的终端，无法中断。", Some(ctx));
                }
            }
            ParsedCommand::More => {
                if let Some(next) = self.clawbot.more.next_chunk() {
                    self.clawbot.audit.push("/more", "分页回复", "发送下一段");
                    self.clawbot_reply_raw(next, Some(ctx));
                } else {
                    self.clawbot_reply_raw("没有更多待查看的分段内容。".to_owned(), Some(ctx));
                }
            }
            ParsedCommand::Approve | ParsedCommand::Deny => {
                let approve = matches!(cmd, ParsedCommand::Approve);
                let Some(tab) = self.bound_tab_ref() else {
                    self.clawbot_reply("当前未绑定终端，请先发送 /use 选择。", Some(ctx));
                    return;
                };
                let state = tab.state_text();
                if state != "等待用户" {
                    self.clawbot_reply(
                        &format!(
                            "当前终端状态为「{state}」，并非「等待用户」，已拒绝执行 {} 以防误操作。",
                            if approve { "/approve" } else { "/deny" }
                        ),
                        Some(ctx),
                    );
                    return;
                }
                let Some(session) = &tab.session else {
                    self.clawbot_reply("终端会话不可写。", Some(ctx));
                    return;
                };
                let bytes: &[u8] = if approve { b"\r" } else { b"\x1b" };
                let label = tab.label();
                let id = tab.id;
                session.input(bytes.to_vec());
                if let Some(pending) = self.clawbot.pending_replies.get_mut(&id) {
                    pending.context_token = Some(ctx.clone());
                    pending.notified_waiting = false;
                }
                self.clawbot.audit.push(
                    if approve { "/approve" } else { "/deny" },
                    label.clone(),
                    if approve {
                        "已确认（Enter）"
                    } else {
                        "已拒绝（Esc）"
                    },
                );
                self.clawbot_reply(
                    &format!(
                        "已向 {label} 发送{}信号。",
                        if approve {
                            "确认（Enter）"
                        } else {
                            "取消/拒绝（Esc）"
                        }
                    ),
                    Some(ctx),
                );
            }
            ParsedCommand::Key(key) => {
                let Some(tab) = self.bound_tab_ref() else {
                    self.clawbot_reply("当前未绑定终端，请先发送 /use 选择。", Some(ctx));
                    return;
                };
                if let Some(ended_msg) = self.bound_tab_ended_notice() {
                    self.clawbot_reply(&ended_msg, Some(ctx));
                    return;
                }
                let Some(session) = &tab.session else {
                    self.clawbot_reply("终端会话不可写。", Some(ctx));
                    return;
                };
                let label = tab.label();
                session.input(key.bytes().to_vec());
                self.clawbot.audit.push("/key", label.clone(), key.label());
                self.clawbot_reply(
                    &format!("已向 {label} 发送按键：{}。", key.label()),
                    Some(ctx),
                );
            }
            ParsedCommand::Shell(command) => {
                if !self.layout.clawbot.allow_raw_shell {
                    self.clawbot
                        .audit
                        .push("/sh", "安全策略", "未开启裸 Shell 执行，已拒绝");
                    self.clawbot_reply(
                        "安全策略已拦截：当前未开启「允许远程执行 PowerShell 命令」。如需使用，请在桌面端「设置 -> 微信 ClawBot」中显式开启。",
                        Some(ctx),
                    );
                    return;
                }
                if command.trim().is_empty() {
                    self.clawbot_reply("用法：/sh <PowerShell 命令>", Some(ctx));
                    return;
                }
                if let Some(ended_msg) = self.bound_tab_ended_notice() {
                    self.clawbot_reply(&ended_msg, Some(ctx));
                    return;
                }
                let Some(tab) = self.bound_tab_ref() else {
                    self.clawbot_reply(
                        "当前未绑定终端，请先发送 /use 选择普通 PowerShell 终端。",
                        Some(ctx),
                    );
                    return;
                };
                if tab.foreground.is_some() {
                    self.clawbot_reply(
                        &format!(
                            "当前绑定终端正在运行 {}，/sh 仅允许在普通 PowerShell 终端执行。",
                            tab.label()
                        ),
                        Some(ctx),
                    );
                    return;
                }
                let Some(session) = &tab.session else {
                    self.clawbot_reply("终端尚未就绪。", Some(ctx));
                    return;
                };
                let id = tab.id;
                let dir_str = tab.directory.display().to_string();
                let mut bytes = command.as_bytes().to_vec();
                bytes.push(b'\r');
                session.input(bytes);
                self.clawbot.pending_shell = Some(PendingShell {
                    tab_id: id,
                    context_token: ctx.clone(),
                    ready_at: Instant::now() + SHELL_SETTLE_DELAY,
                });
                tx.wake_after(SHELL_SETTLE_DELAY);
                self.clawbot.audit.push(
                    "/sh",
                    dir_str,
                    command.chars().take(48).collect::<String>(),
                );
            }
            ParsedCommand::UnknownSlash(detail) => {
                self.clawbot_reply(
                    &format!("无法识别指令：{detail}\n发送 /help 查看全部可用指令。"),
                    Some(ctx),
                );
            }
            ParsedCommand::Prompt(prompt) => {
                self.handle_clawbot_prompt(prompt, ctx, tx);
            }
        }
    }

    fn resolve_new_dir(&self, target: Option<&str>) -> Result<PathBuf, String> {
        let Some(raw) = target.map(str::trim).filter(|s| !s.is_empty()) else {
            if let Some(dir) = self
                .layout
                .clawbot
                .default_project
                .as_ref()
                .filter(|p| p.is_dir())
            {
                return Ok(dir.clone());
            }
            if let Some(tab) = self.bound_tab_ref().or_else(|| self.current())
                && tab.directory.is_dir()
            {
                return Ok(tab.directory.clone());
            }
            return crate::shell::starting_directory()
                .map_err(|e| format!("无法确定起始目录：{e}"));
        };
        let direct = PathBuf::from(raw);
        if direct.is_absolute() && direct.is_dir() {
            return Ok(direct);
        }
        let q = raw.to_lowercase();
        let mut matched: Vec<PathBuf> = self
            .tabs
            .iter()
            .filter(|t| !t.closing && t.directory.is_dir())
            .map(|t| t.directory.clone())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().to_lowercase().contains(&q))
                    || p.to_string_lossy().to_lowercase().contains(&q)
            })
            .collect();
        matched.sort();
        matched.dedup();
        match matched.as_slice() {
            [only] => Ok(only.clone()),
            [] => Err(format!("未找到目录或项目「{raw}」，请提供完整绝对路径。")),
            _ => Err(format!("「{raw}」匹配到多个项目，请提供更精确路径。")),
        }
    }

    fn format_proxy_catalog(&self) -> String {
        if self.layout.proxy.profiles.is_empty() {
            return "尚未配置代理预设，请在桌面端「设置 -> 代理预设」中添加。".into();
        }
        let mut lines = vec!["可用代理预设：".to_owned()];
        for (i, p) in self.layout.proxy.profiles.iter().enumerate() {
            let def_tag = if self
                .layout
                .proxy
                .default_profile
                .as_deref()
                .is_some_and(|d| d.eq_ignore_ascii_case(&p.name))
            {
                " [默认]"
            } else {
                ""
            };
            lines.push(format!(
                "[{}] {} -> {}{}",
                i + 1,
                p.name,
                crate::proxy::sanitize_proxy_summary(&p.url),
                def_tag
            ));
        }
        lines.join("\n")
    }

    fn handle_clawbot_proxy(
        &mut self,
        target: Option<String>,
        restart: bool,
        ctx: String,
        tx: &mut Tx,
    ) {
        let Some(query) = target else {
            let cur = if let Some(tab) = self.bound_tab_ref() {
                format!(
                    "当前目标（{}）代理：{}",
                    tab.label(),
                    self.layout
                        .proxy
                        .match_summary(&tab.proxy_summary)
                        .detail_label()
                )
            } else {
                "当前未绑定终端。".into()
            };
            let catalog = self.format_proxy_catalog();
            let msg = format!(
                "{cur}\n\n{catalog}\n\n用法：/proxy <预设名|编号> | /proxy off (直连) | /proxy sys (系统默认)\n若当前为 Agent 终端，切换时将自动按原官方对话 ID 平滑续接。"
            );
            self.clawbot.audit.push("/proxy", "查询", "返回代理列表");
            self.clawbot_reply(&msg, Some(ctx));
            return;
        };

        let Some(resolved) = self.layout.proxy.resolve_target(&query) else {
            let catalog = self.format_proxy_catalog();
            self.clawbot_reply(
                &format!(
                    "未找到代理预设「{query}」。\n{catalog}\n支持：/proxy <预设名|编号>、/proxy off、/proxy sys"
                ),
                Some(ctx),
            );
            return;
        };

        if let Some(ended_msg) = self.bound_tab_ended_notice() {
            self.clawbot_reply(&ended_msg, Some(ctx));
            return;
        }
        let Some(tab) = self.bound_tab_ref() else {
            self.clawbot_reply(
                "当前未绑定终端，请先发送 /list 并用 /use <编号> 选择，或使用 /new <目录> <agent> proxy=<预设名>。",
                Some(ctx),
            );
            return;
        };
        let id = tab.id;
        let label = tab.label();
        let dir_name = tab
            .directory
            .file_name()
            .unwrap_or(tab.directory.as_os_str())
            .to_string_lossy()
            .into_owned();
        let target_desc = resolved.display_label();

        if tab.foreground.is_none() {
            if let Some(session) = &tab.session {
                let cmd = format!("mp {}\r", resolved.shell_arg());
                session.input(cmd.into_bytes());
            }
            if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                t.proxy_summary = resolved.initial_summary();
            }
            self.clawbot.audit.push(
                "/proxy",
                format!("{dir_name} · PowerShell"),
                target_desc.clone(),
            );
            self.clawbot_reply(
                &format!("已将 {dir_name} · PowerShell 代理切换为：{target_desc}"),
                Some(ctx),
            );
            return;
        }

        if tab.state_text() == "运行中" && !restart {
            self.clawbot
                .audit
                .push("/proxy", label.clone(), "运行中拒绝切换");
            self.clawbot_reply(
                &format!(
                    "{label} 当前正在运行中，为防打断已暂停切换。请等待完成，或发送 /proxy {query} restart 强制重开并切换。"
                ),
                Some(ctx),
            );
            return;
        }

        match self.reopen_tab_with_proxy(id, &resolved, tx) {
            Ok((new_id, has_resume)) => {
                self.remember_bound_tab(new_id);
                let action = if has_resume {
                    format!(
                        "已将代理切换为：{target_desc}，并在 {dir_name} 平滑重开终端、按原官方对话 ID 续接 {label}。"
                    )
                } else {
                    format!(
                        "已将代理切换为：{target_desc}，并在 {dir_name} 以新代理重新启动 {label}。"
                    )
                };
                self.clawbot
                    .audit
                    .push("/proxy", format!("{dir_name} · {label}"), target_desc);
                self.clawbot_reply(&action, Some(ctx));
            }
            Err(err) => {
                self.clawbot_reply(&format!("切换代理失败：{err}"), Some(ctx));
            }
        }
    }

    fn handle_clawbot_new(
        &mut self,
        target: Option<String>,
        agent: Option<String>,
        proxy: Option<String>,
        deferred_prompt: Option<String>,
        ctx: String,
        tx: &mut Tx,
    ) {
        let dir = match self.resolve_new_dir(target.as_deref()) {
            Ok(d) => d,
            Err(e) => {
                self.clawbot_reply(&e, Some(ctx));
                return;
            }
        };
        let launch_agent = agent.filter(|a| a != "shell");
        if let Some(ref a) = launch_agent
            && parse_agent_name(a).is_none()
        {
            self.clawbot_reply(
                &format!("不支持的 Agent「{a}」，仅支持 codex、agy、kimi、grok 或 shell。"),
                Some(ctx),
            );
            return;
        }
        let proxy_target = if let Some(ref p) = proxy {
            match self.layout.proxy.resolve_target(p) {
                Some(t) => Some(t),
                None => {
                    let catalog = self.format_proxy_catalog();
                    self.clawbot_reply(&format!("未找到代理预设「{p}」。\n{catalog}"), Some(ctx));
                    return;
                }
            }
        } else {
            None
        };
        let new_id = if let Some(ref pt) = proxy_target {
            let seal = match crate::proxy::seal_target(pt) {
                Ok(s) => s,
                Err(e) => {
                    self.clawbot_reply(&format!("加密代理配置失败：{e}"), Some(ctx));
                    return;
                }
            };
            self.open_shell_with_proxy(
                Some(dir.clone()),
                None,
                Some(seal),
                Some(pt.initial_summary()),
                None,
                tx,
            )
        } else {
            self.new_shell(Some(dir.clone()), tx);
            let Some(id) = self.tabs.last().map(|t| t.id) else {
                self.clawbot_reply("新建终端失败。", Some(ctx));
                return;
            };
            id
        };
        self.remember_bound_tab(new_id);
        let dir_name = dir
            .file_name()
            .unwrap_or(dir.as_os_str())
            .to_string_lossy()
            .into_owned();
        let proxy_note = proxy_target
            .as_ref()
            .map(|pt| format!("（代理：{}）", pt.short_label()))
            .unwrap_or_default();
        if let Some(ref cli) = launch_agent {
            self.clawbot.pending_launch = Some(PendingLaunch {
                tab_id: new_id,
                agent: Some(cli.clone()),
                launched_cli: false,
                deferred_prompt,
                context_token: ctx.clone(),
                created_at: Instant::now(),
            });
            tx.wake_after(Duration::from_millis(200));
            self.clawbot
                .audit
                .push("/new", format!("{dir_name} · {cli}"), "新建并启动 Agent");
            self.clawbot_reply(
                &format!("已在 {dir_name} 新建终端并启动 {cli}{proxy_note}，就绪后自动继续…"),
                Some(ctx),
            );
        } else {
            self.clawbot
                .audit
                .push("/new", format!("{dir_name} · PowerShell"), "已新建并绑定");
            self.clawbot_reply(
                &format!("已在 {dir_name} 新建 PowerShell{proxy_note} 并设为当前远程目标。"),
                Some(ctx),
            );
        }
    }

    fn handle_clawbot_reopen(&mut self, ctx: String, tx: &mut Tx) {
        let plan = if let Some(tab) = self.bound_tab_ref() {
            let resume = tab.bound.as_ref().or(tab.expected.as_ref()).cloned();
            let agent = resume
                .as_ref()
                .map(|k| k.agent.clone())
                .or_else(|| tab.foreground.map(|f| f.name().to_owned()));
            Some((tab.directory.clone(), resume, agent))
        } else if let Some(last) = self.layout.clawbot.last_bound.clone() {
            let resume = last
                .agent
                .as_deref()
                .zip(last.conversation_id.as_deref())
                .and_then(|(a, id)| crate::transcript::Key::new(a, id));
            Some((last.directory, resume, last.agent))
        } else {
            None
        };
        let Some((dir, resume, agent)) = plan else {
            self.clawbot_reply(
                "没有可重开的历史绑定终端，请使用 /new <目录> [codex|agy|kimi|grok|shell] 新建。",
                Some(ctx),
            );
            return;
        };
        if !dir.is_dir() {
            self.clawbot_reply(
                &format!(
                    "原目录已不存在：{}，请使用 /new 指定新路径。",
                    dir.display()
                ),
                Some(ctx),
            );
            return;
        }
        let dir_name = dir
            .file_name()
            .unwrap_or(dir.as_os_str())
            .to_string_lossy()
            .into_owned();
        if let Some(key) = resume {
            let label = key.label();
            self.open_shell(Some(dir), Some(key), tx);
            if let Some(new_id) = self.tabs.last().map(|t| t.id) {
                self.remember_bound_tab(new_id);
            }
            self.clawbot.audit.push(
                "/reopen",
                format!("{dir_name} · {label}"),
                "按原官方对话 ID 续接",
            );
            self.clawbot_reply(
                &format!(
                    "已在 {dir_name} 重开终端并按原对话 ID 续接 {label}，已设为当前远程目标。"
                ),
                Some(ctx),
            );
        } else {
            self.handle_clawbot_new(
                Some(dir.to_string_lossy().into_owned()),
                agent,
                None,
                None,
                ctx,
                tx,
            );
        }
    }

    fn handle_clawbot_prompt(&mut self, prompt: String, ctx: String, tx: &mut Tx) {
        if prompt.is_empty() {
            return;
        }
        if let Some(ended_msg) = self.bound_tab_ended_notice() {
            self.clawbot_reply(&ended_msg, Some(ctx));
            return;
        }
        // Strategy A (recommended default): prefer explicitly bound Agent tab;
        // otherwise route to configured default project + default agent (auto-launch if absent).
        let target_id = if let Some(tab) = self.bound_tab_ref() {
            if tab.foreground.is_some() {
                Some(tab.id)
            } else {
                self.clawbot_reply(
                    "当前绑定的是普通 PowerShell 终端，普通文本不会直接当作 Shell 命令执行。请发送 /use 选择 Agent 终端，或用 /new <目录> <codex|agy|kimi|grok> 启动 Agent。",
                    Some(ctx),
                );
                return;
            }
        } else if let (Some(def_dir), Some(def_agent)) = (
            self.layout.clawbot.default_project.clone(),
            self.layout.clawbot.normalized_agent(),
        ) {
            let fg = parse_agent_name(def_agent);
            let existing = self
                .tabs
                .iter()
                .find(|t| {
                    !t.closing
                        && t.ended.is_none()
                        && t.foreground == fg
                        && crate::workspace::project_key(&t.directory)
                            == crate::workspace::project_key(&def_dir)
                })
                .map(|t| t.id);
            if let Some(id) = existing {
                self.remember_bound_tab(id);
                Some(id)
            } else {
                self.handle_clawbot_new(
                    Some(def_dir.to_string_lossy().into_owned()),
                    Some(def_agent.to_owned()),
                    None,
                    Some(prompt),
                    ctx,
                    tx,
                );
                return;
            }
        } else {
            None
        };

        let Some(id) = target_id else {
            self.clawbot_reply(
                "尚未绑定远程目标终端，也未在桌面端设置「默认项目 + 默认 Agent」。请先发送 /list 并用 /use <编号> 绑定一个 Agent 终端。",
                Some(ctx),
            );
            return;
        };
        self.inject_agent_prompt(id, prompt, ctx);
    }

    fn inject_agent_prompt(&mut self, id: SessionId, prompt: String, ctx: String) {
        let Some(tab) = self.tabs.iter().find(|t| t.id == id && !t.closing) else {
            self.clawbot_reply("目标终端已关闭，请重新用 /use 选择。", Some(ctx));
            return;
        };
        let state = tab.state_text();
        if state == "运行中" {
            self.clawbot.audit.push("拒注", tab.label(), "终端运行中");
            self.clawbot_reply(
                &format!(
                    "{} 当前正在运行中，为防内容串台已拒绝投递。请等待完成或发送 /stop 中断。",
                    tab.label()
                ),
                Some(ctx),
            );
            return;
        }
        if state == "等待用户" {
            self.clawbot
                .audit
                .push("拒注", tab.label(), "终端等待用户确认");
            self.clawbot_reply(
                &format!("{} 当前正在等待用户确认，已暂停投递新消息。请先发送 /screen 查看，或用 /approve、/deny 响应。", tab.label()),
                Some(ctx),
            );
            return;
        }
        let Some(session) = &tab.session else {
            self.clawbot_reply("目标终端尚未就绪，请稍后再试。", Some(ctx));
            return;
        };
        let mode = session.mode();
        let label = tab.label();
        let dir_name = tab
            .directory
            .file_name()
            .unwrap_or(tab.directory.as_os_str())
            .to_string_lossy()
            .into_owned();
        let bytes = format_prompt_bytes(&prompt, mode);
        session.input(bytes);
        self.clawbot.pending_replies.insert(
            id,
            PendingReply {
                context_token: Some(ctx.clone()),
                saw_running: false,
                notified_waiting: false,
                awaiting_transcript_gen: None,
                fallback_deadline: None,
            },
        );
        if let Some(svc) = &self.clawbot.service {
            svc.send_typing(true, Some(ctx.clone()));
        }
        self.clawbot.audit.push(
            "投递消息",
            format!("{dir_name} · {label}"),
            prompt.chars().take(40).collect::<String>(),
        );
        self.clawbot_reply_raw(
            format!("已投递至 {dir_name} · {label}，等待回复中…"),
            Some(ctx),
        );
    }

    fn poll_clawbot_launch(&mut self, tx: &mut Tx) -> bool {
        let Some(pending) = self.clawbot.pending_launch.as_mut() else {
            return false;
        };
        if pending.created_at.elapsed() >= LAUNCH_TIMEOUT {
            let ctx = pending.context_token.clone();
            let cli = pending.agent.clone().unwrap_or_default();
            self.clawbot.pending_launch = None;
            self.clawbot_reply(
                &format!("等待 {cli} 启动超时（45 秒），请发送 /screen 检查终端提示。"),
                Some(ctx),
            );
            return true;
        }
        let tab_id = pending.tab_id;
        let Some(tab) = self.tabs.iter().find(|t| t.id == tab_id && !t.closing) else {
            self.clawbot.pending_launch = None;
            return true;
        };
        if tab.ended.is_some() || tab.error.is_some() {
            let ctx = pending.context_token.clone();
            self.clawbot.pending_launch = None;
            self.clawbot_reply("新建终端启动异常，请发送 /screen 查看详情。", Some(ctx));
            return true;
        }
        if !pending.launched_cli {
            if let Some(session) = &tab.session
                && let Some(cli) = pending.agent.as_deref()
            {
                session.input(format!("{cli}\r").into_bytes());
                pending.launched_cli = true;
                tx.wake_after(Duration::from_millis(350));
                return true;
            }
            tx.wake_after(Duration::from_millis(200));
            return false;
        }
        let want_fg = pending.agent.as_deref().and_then(parse_agent_name);
        let ready = tab.foreground == want_fg
            && matches!(tab.state_text().as_str(), "就绪" | "在线")
            && pending.created_at.elapsed() >= Duration::from_secs(2);
        if ready {
            let finished = self.clawbot.pending_launch.take().unwrap();
            if let Some(prompt) = finished.deferred_prompt {
                self.inject_agent_prompt(finished.tab_id, prompt, finished.context_token);
            } else {
                let cli = finished.agent.unwrap_or_else(|| "Agent".into());
                self.clawbot_reply(
                    &format!("{cli} 已就绪，可以直接发送消息了。"),
                    Some(finished.context_token),
                );
            }
            return true;
        }
        tx.wake_after(Duration::from_millis(300));
        false
    }

    fn poll_clawbot_shell(&mut self, tx: &mut Tx) -> bool {
        let Some(pending) = &self.clawbot.pending_shell else {
            return false;
        };
        let now = Instant::now();
        if now < pending.ready_at {
            tx.wake_after(pending.ready_at - now);
            return false;
        }
        let done = self.clawbot.pending_shell.take().unwrap();
        let snap = self
            .tabs
            .iter()
            .find(|t| t.id == done.tab_id)
            .and_then(|t| t.session.as_ref())
            .map(|s| s.screen_snapshot(30))
            .unwrap_or_default();
        let msg = if snap.is_empty() {
            "命令已发送，终端暂无新增输出。".into()
        } else {
            format!("[PowerShell 输出]\n{snap}")
        };
        self.clawbot_reply(&msg, Some(done.context_token));
        true
    }

    fn poll_clawbot_tab_transitions(&mut self, tx: &mut Tx) -> bool {
        if !self.clawbot.state.is_online() {
            return false;
        }
        let mut changed = false;
        let mut to_notify_waiting: Vec<(SessionId, String, String, Option<String>)> = Vec::new();
        let mut to_notify_error: Vec<(SessionId, String, String, Option<String>)> = Vec::new();
        let mut to_fetch_reply: Vec<(SessionId, Option<crate::transcript::Key>, Option<String>)> =
            Vec::new();
        let mut fallback_now: Vec<SessionId> = Vec::new();

        let notify_desktop = self.layout.clawbot.notify_desktop_tasks;
        for tab in &self.tabs {
            if tab.closing {
                continue;
            }
            let state = tab.state_text();
            let prev = self
                .clawbot
                .last_tab_states
                .insert(tab.id, state.clone())
                .unwrap_or_default();

            let pending = self.clawbot.pending_replies.get_mut(&tab.id);
            let tracked = pending.is_some() || (notify_desktop && tab.foreground.is_some());
            if !tracked {
                continue;
            }

            if state == "运行中"
                && let Some(p) = pending
            {
                p.saw_running = true;
                p.notified_waiting = false;
            } else if state == "等待用户" && prev != "等待用户" {
                let ctx = self
                    .clawbot
                    .pending_replies
                    .get_mut(&tab.id)
                    .map(|p| {
                        p.notified_waiting = true;
                        p.context_token.clone()
                    })
                    .unwrap_or(None);
                let snap = tab
                    .session
                    .as_ref()
                    .map(|s| s.screen_snapshot(12))
                    .unwrap_or_default();
                to_notify_waiting.push((tab.id, tab.label(), snap, ctx));
            } else if matches!(state.as_str(), "出错" | "异常退出") && prev != state {
                let ctx = self
                    .clawbot
                    .pending_replies
                    .remove(&tab.id)
                    .and_then(|p| p.context_token);
                let snap = tab
                    .session
                    .as_ref()
                    .map(|s| s.screen_snapshot(15))
                    .unwrap_or_default();
                to_notify_error.push((tab.id, tab.label(), snap, ctx));
            } else if state == "就绪" && prev == "运行中" {
                let ctx = self
                    .clawbot
                    .pending_replies
                    .get(&tab.id)
                    .and_then(|p| p.context_token.clone());
                to_fetch_reply.push((tab.id, tab.bound.clone(), ctx));
            }
        }

        // Check if any pending transcript fetch timed out (fallback to screen snapshot).
        let now = Instant::now();
        for (&id, pending) in &self.clawbot.pending_replies {
            if let Some(deadline) = pending.fallback_deadline {
                if now >= deadline {
                    fallback_now.push(id);
                } else {
                    tx.wake_after(deadline - now);
                }
            }
        }

        for (_, label, snap, ctx) in to_notify_waiting {
            changed = true;
            let body = if snap.is_empty() {
                format!(
                    "⏳ {label} 正在等待用户确认，请发送 /screen 查看，或用 /approve、/deny 响应。"
                )
            } else {
                format!(
                    "⏳ {label} 正在等待用户确认：\n{snap}\n\n可发送 /approve 确认、/deny 拒绝，或 /screen 查看完整屏显。"
                )
            };
            self.clawbot.audit.push("等待确认", label, "已推送微信提醒");
            self.clawbot_reply(&body, ctx);
        }

        for (_, label, snap, ctx) in to_notify_error {
            changed = true;
            let body = if snap.is_empty() {
                format!("❌ {label} 执行出错或异常退出。")
            } else {
                format!("❌ {label} 执行出错：\n{snap}")
            };
            self.clawbot.audit.push("出错提醒", label, "已推送微信");
            self.clawbot_reply(&body, ctx);
        }

        for (id, bound_key, ctx) in to_fetch_reply {
            changed = true;
            if let (Some(key), Some(history)) = (bound_key, self.history.service.as_ref()) {
                self.clawbot.reply_gen = self.clawbot.reply_gen.wrapping_add(1);
                let generation = self.clawbot.reply_gen;
                let entry = self
                    .clawbot
                    .pending_replies
                    .entry(id)
                    .or_insert(PendingReply {
                        context_token: ctx,
                        saw_running: true,
                        notified_waiting: false,
                        awaiting_transcript_gen: None,
                        fallback_deadline: None,
                    });
                entry.awaiting_transcript_gen = Some(generation);
                entry.fallback_deadline = Some(Instant::now() + Duration::from_secs(2));
                let _ = history
                    .send
                    .send(crate::history::Command::LatestReply(key, id, generation));
                tx.wake_after(Duration::from_secs(2));
            } else {
                self.deliver_screen_completion(id, ctx);
            }
        }

        for id in fallback_now {
            changed = true;
            let ctx = self
                .clawbot
                .pending_replies
                .remove(&id)
                .and_then(|p| p.context_token);
            self.deliver_screen_completion(id, ctx);
        }

        changed
    }

    pub fn on_clawbot_latest_reply(
        &mut self,
        tab_id: SessionId,
        generation: u64,
        result: Result<Option<String>, String>,
    ) {
        let matches = self
            .clawbot
            .pending_replies
            .get(&tab_id)
            .is_some_and(|p| p.awaiting_transcript_gen == Some(generation));
        if !matches {
            return;
        }
        let ctx = self
            .clawbot
            .pending_replies
            .remove(&tab_id)
            .and_then(|p| p.context_token);
        if let Some(svc) = &self.clawbot.service {
            svc.send_typing(false, ctx.clone());
        }
        let header = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .map(|t| {
                let dir = t
                    .directory
                    .file_name()
                    .unwrap_or(t.directory.as_os_str())
                    .to_string_lossy();
                format!("✅ {dir} · {} 已完成：", t.label())
            })
            .unwrap_or_else(|| "✅ Agent 已完成：".into());

        match result {
            Ok(Some(text)) if !text.trim().is_empty() => {
                self.clawbot.audit.push(
                    "结构化回传",
                    header.clone(),
                    format!("{} 字符", text.chars().count()),
                );
                self.clawbot_reply(&format!("{header}\n\n{text}"), ctx);
            }
            _ => self.deliver_screen_completion(tab_id, ctx),
        }
    }

    fn deliver_screen_completion(&mut self, tab_id: SessionId, ctx: Option<String>) {
        self.clawbot.pending_replies.remove(&tab_id);
        if let Some(svc) = &self.clawbot.service {
            svc.send_typing(false, ctx.clone());
        }
        let Some(tab) = self.tabs.iter().find(|t| t.id == tab_id) else {
            return;
        };
        let label = tab.label();
        let dir = tab
            .directory
            .file_name()
            .unwrap_or(tab.directory.as_os_str())
            .to_string_lossy()
            .into_owned();
        let snap = tab
            .session
            .as_ref()
            .map(|s| s.screen_snapshot(30))
            .unwrap_or_default();
        let body = if !tab.preview.trim().is_empty() && snap.is_empty() {
            format!("✅ {dir} · {label} 已完成：\n{}", tab.preview.trim())
        } else if !snap.is_empty() {
            format!("✅ {dir} · {label} 已完成（末屏摘要）：\n{snap}")
        } else {
            format!("✅ {dir} · {label} 已完成，可发送 /screen 查看详情。")
        };
        self.clawbot
            .audit
            .push("完成回传", format!("{dir} · {label}"), "末屏快照");
        self.clawbot_reply(&body, ctx);
    }

    fn handle_clawbot_last(&mut self, count: usize, ctx: String, tx: &mut Tx) {
        let Some(tab) = self.bound_tab_ref() else {
            self.clawbot_reply(
                "当前未绑定终端，请先发送 /list 并用 /use <编号> 选择目标终端。",
                Some(ctx),
            );
            return;
        };
        let tab_id = tab.id;
        let label = tab.label();
        let dir_name = tab
            .directory
            .file_name()
            .unwrap_or(tab.directory.as_os_str())
            .to_string_lossy()
            .into_owned();
        let key = tab.bound.clone().or_else(|| tab.expected.clone());
        if let (Some(key), Some(history)) = (key, self.history.service.as_ref()) {
            self.clawbot.reply_gen = self.clawbot.reply_gen.wrapping_add(1);
            let generation = self.clawbot.reply_gen;
            let deadline = Instant::now() + Duration::from_secs(2);
            self.clawbot.pending_last = Some(PendingLast {
                tab_id,
                count,
                generation,
                context_token: ctx,
                deadline,
            });
            let _ = history.send.send(crate::history::Command::RecentTurns(
                key,
                tab_id,
                count.saturating_add(1),
                generation,
            ));
            tx.wake_after(Duration::from_secs(2));
            self.clawbot.audit.push(
                "/last",
                format!("{dir_name} · {label}"),
                format!("读取最近 {count} 轮"),
            );
        } else {
            self.clawbot.audit.push(
                "/last",
                format!("{dir_name} · {label}"),
                "未关联官方对话，回退末屏",
            );
            self.deliver_last_screen_fallback(tab_id, ctx, "当前终端未关联官方 Agent 对话");
        }
    }

    fn poll_clawbot_last(&mut self, tx: &mut Tx) -> bool {
        let Some(deadline) = self.clawbot.pending_last.as_ref().map(|p| p.deadline) else {
            return false;
        };
        let now = Instant::now();
        if now >= deadline {
            if let Some(pending) = self.clawbot.pending_last.take() {
                self.deliver_last_screen_fallback(
                    pending.tab_id,
                    pending.context_token,
                    "读取官方对话超时",
                );
            }
            true
        } else {
            tx.wake_after(deadline - now);
            false
        }
    }

    pub fn on_clawbot_recent_turns(
        &mut self,
        tab_id: SessionId,
        generation: u64,
        result: Result<Vec<crate::transcript::Turn>, String>,
    ) {
        let matches = self
            .clawbot
            .pending_last
            .as_ref()
            .is_some_and(|p| p.tab_id == tab_id && p.generation == generation);
        if !matches {
            return;
        }
        let Some(pending) = self.clawbot.pending_last.take() else {
            return;
        };
        let (dir_name, label, is_running) = self
            .tabs
            .iter()
            .find(|t| t.id == tab_id)
            .map(|t| {
                let dir = t
                    .directory
                    .file_name()
                    .unwrap_or(t.directory.as_os_str())
                    .to_string_lossy()
                    .into_owned();
                (dir, t.label(), t.state_text() == "运行中")
            })
            .unwrap_or_else(|| ("工作区".into(), "Agent".into(), false));

        let turns = match result {
            Ok(turns) if !turns.is_empty() => turns,
            Ok(_) => {
                self.deliver_last_screen_fallback(
                    tab_id,
                    pending.context_token,
                    "暂未在官方记录中找到已完成回复",
                );
                return;
            }
            Err(_) => {
                self.deliver_last_screen_fallback(
                    tab_id,
                    pending.context_token,
                    "读取官方对话记录失败",
                );
                return;
            }
        };

        if pending.count <= 1 {
            if let Some(reply) = turns
                .last()
                .and_then(|t| t.reply.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let header = format!("💬 {dir_name} · {label} 最新回复：");
                self.clawbot.audit.push(
                    "/last",
                    format!("{dir_name} · {label}"),
                    format!("结构化回传 {} 字符", reply.chars().count()),
                );
                self.clawbot_reply(&format!("{header}\n\n{reply}"), Some(pending.context_token));
                return;
            }
            if turns.len() >= 2
                && let Some(prev) = turns[turns.len() - 2]
                    .reply
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            {
                let status_note = if is_running {
                    "当前正在生成新回复"
                } else {
                    "最新一轮尚未写出完整回复"
                };
                let latest_q = crate::transcript::snippet(&turns.last().unwrap().prompt);
                let header = if latest_q.is_empty() {
                    format!("⏳ {dir_name} · {label} {status_note}，以下为上一轮已完成回复：")
                } else {
                    format!(
                        "⏳ {dir_name} · {label} {status_note}（最新提问：{latest_q}），以下为上一轮已完成回复："
                    )
                };
                self.clawbot.audit.push(
                    "/last",
                    format!("{dir_name} · {label}"),
                    format!("生成中回退上一轮 {} 字符", prev.chars().count()),
                );
                self.clawbot_reply(&format!("{header}\n\n{prev}"), Some(pending.context_token));
                return;
            }
            self.deliver_last_screen_fallback(
                tab_id,
                pending.context_token,
                if is_running {
                    "Agent 正在生成首条回复，尚未写入官方记录"
                } else {
                    "官方对话尚无已完成回复"
                },
            );
            return;
        }

        let start = turns.len().saturating_sub(pending.count);
        let slice = &turns[start..];
        let total = slice.len();
        let mut sections = vec![format!("💬 {dir_name} · {label} 最近 {total} 轮对话：")];
        for (idx, turn) in slice.iter().enumerate() {
            let prompt_text = truncate_chars(turn.prompt.trim(), 400);
            let prompt_display = if prompt_text.is_empty() {
                "（无提问记录）".to_owned()
            } else {
                prompt_text
            };
            let reply_display = match turn
                .reply
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(r) => r.to_owned(),
                None if idx + 1 == total && is_running => "（正在生成中…）".to_owned(),
                None => "（暂无完整回复）".to_owned(),
            };
            sections.push(format!(
                "【第 {}/{total} 轮】\n问：{prompt_display}\n答：{reply_display}",
                idx + 1
            ));
        }
        let body = sections.join("\n\n");
        self.clawbot.audit.push(
            "/last",
            format!("{dir_name} · {label}"),
            format!("返回最近 {total} 轮（{} 字符）", body.chars().count()),
        );
        self.clawbot_reply(&body, Some(pending.context_token));
    }

    fn deliver_last_screen_fallback(&mut self, tab_id: SessionId, ctx: String, reason: &str) {
        let Some(tab) = self.tabs.iter().find(|t| t.id == tab_id && !t.closing) else {
            self.clawbot_reply(&format!("ℹ️ {reason}，目标终端已关闭。"), Some(ctx));
            return;
        };
        let label = tab.label();
        let dir_name = tab
            .directory
            .file_name()
            .unwrap_or(tab.directory.as_os_str())
            .to_string_lossy()
            .into_owned();
        let snap = tab
            .session
            .as_ref()
            .map(|s| s.screen_snapshot(35))
            .unwrap_or_default();
        let body = if !snap.is_empty() {
            format!("ℹ️ {reason}，已回退为 {dir_name} · {label} 末屏快照：\n{snap}")
        } else if !tab.preview.trim().is_empty() {
            format!(
                "ℹ️ {reason}，{dir_name} · {label} 当前摘要：\n{}",
                tab.preview.trim()
            )
        } else {
            format!("ℹ️ {reason}，且当前终端屏幕为空。")
        };
        self.clawbot_reply(&body, Some(ctx));
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}
