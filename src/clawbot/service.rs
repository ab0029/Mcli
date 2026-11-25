use crate::clawbot::protocol::{self, ApiError, CancelToken, QrStatus};
use crate::clawbot::store::{AuthRecord, AuthStore, OwnerLock, now_epoch_ms};
use crate::clawbot::{ClawbotPrefs, ConnectionState, mask_user_id};
use rune::Waker;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const DEFAULT_POLL_TIMEOUT_MS: i32 = 38_000;
const QR_POLL_TIMEOUT_MS: i32 = 30_000;
const MAX_BACKOFF_SECS: u64 = 30;
const SEEN_MSG_CAP: usize = 128;
const OFFLINE_STALE_GRACE_MS: u64 = 60_000;

pub enum Control {
    ApplyPrefs(ClawbotPrefs),
    StartQrLogin,
    CancelQrLogin,
    Unbind,
    SetPaused(bool),
    ClaimOwnership,
    Quit,
}

pub enum Outbound {
    Text {
        text: String,
        context_token: Option<String>,
    },
    Typing {
        active: bool,
        context_token: Option<String>,
    },
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    State(ConnectionState),
    Inbound {
        from_user_id: String,
        context_token: String,
        text: String,
    },
    RejectedSender {
        from_user_id: String,
    },
    RestoredOnline {
        has_active_window: bool,
    },
    OfflineSkipped {
        count: usize,
        context_token: Option<String>,
    },
    KeepaliveSent {
        hours_left: u8,
    },
    DeliverySkipped(String),
    DeliveryError(String),
}

struct SharedAuth {
    auth: Option<AuthRecord>,
    base_url_override: String,
    typing_ticket: Option<String>,
}

pub struct Service {
    control: mpsc::Sender<Control>,
    outbound: mpsc::Sender<Outbound>,
    pub receive: mpsc::Receiver<Event>,
    poll_cancel: Arc<CancelToken>,
    out_cancel: Arc<CancelToken>,
    poll_thread: Option<JoinHandle<()>>,
    out_thread: Option<JoinHandle<()>>,
}

impl Service {
    pub fn start(data_dir: PathBuf, initial_prefs: ClawbotPrefs, wake: Waker) -> Self {
        let (ctrl_tx, ctrl_rx) = mpsc::channel();
        let (out_tx, out_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let poll_cancel = CancelToken::new();
        let out_cancel = CancelToken::new();
        let shared = Arc::new(Mutex::new(SharedAuth {
            auth: None,
            base_url_override: initial_prefs.normalized_base_url().to_owned(),
            typing_ticket: None,
        }));

        let poll_ev = ev_tx.clone();
        let poll_wake = wake.clone();
        let poll_shared = shared.clone();
        let poll_tok = poll_cancel.clone();
        let poll_out = out_tx.clone();
        let poll_thread = thread::Builder::new()
            .name("mcli-clawbot-poll".into())
            .spawn(move || {
                run_poll_loop(
                    data_dir,
                    initial_prefs,
                    ctrl_rx,
                    poll_out,
                    poll_ev,
                    poll_wake,
                    poll_shared,
                    poll_tok,
                );
            })
            .expect("spawn clawbot poll thread");

        let out_ev = ev_tx;
        let out_wake = wake;
        let out_shared = shared;
        let out_tok = out_cancel.clone();
        let out_thread = thread::Builder::new()
            .name("mcli-clawbot-send".into())
            .spawn(move || {
                run_outbound_loop(out_rx, out_ev, out_wake, out_shared, out_tok);
            })
            .expect("spawn clawbot outbound thread");

        Self {
            control: ctrl_tx,
            outbound: out_tx,
            receive: ev_rx,
            poll_cancel,
            out_cancel,
            poll_thread: Some(poll_thread),
            out_thread: Some(out_thread),
        }
    }

    pub fn send_control(&self, cmd: Control) {
        // Cancel any blocking long-poll immediately so control commands respond without waiting 35s.
        self.poll_cancel.cancel();
        let _ = self.control.send(cmd);
    }

    pub fn send_text(&self, text: String, context_token: Option<String>) {
        let _ = self.outbound.send(Outbound::Text {
            text,
            context_token,
        });
    }

    pub fn send_typing(&self, active: bool, context_token: Option<String>) {
        let _ = self.outbound.send(Outbound::Typing {
            active,
            context_token,
        });
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.poll_cancel.cancel();
        self.out_cancel.cancel();
        let _ = self.control.send(Control::Quit);
        let _ = self.outbound.send(Outbound::Quit);
        if let Some(t) = self.poll_thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.out_thread.take() {
            let _ = t.join();
        }
    }
}

fn emit(tx: &mpsc::Sender<Event>, wake: &Waker, event: Event) {
    if tx.send(event).is_ok() {
        wake();
    }
}

fn run_outbound_loop(
    rx: mpsc::Receiver<Outbound>,
    ev: mpsc::Sender<Event>,
    wake: Waker,
    shared: Arc<Mutex<SharedAuth>>,
    cancel: Arc<CancelToken>,
) {
    while let Ok(item) = rx.recv() {
        match item {
            Outbound::Quit => break,
            Outbound::Text {
                text,
                context_token,
            } => {
                let snapshot = {
                    let guard = shared.lock().unwrap();
                    guard.auth.clone().map(|a| {
                        let base = a.effective_base_url(&guard.base_url_override).to_owned();
                        (a, base)
                    })
                };
                let Some((auth, base_url)) = snapshot else {
                    emit(
                        &ev,
                        &wake,
                        Event::DeliverySkipped("微信尚未绑定，跳过发送".into()),
                    );
                    continue;
                };
                let now = now_epoch_ms();
                let ctx = context_token
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| auth.active_context_token(now).map(str::to_owned));
                let Some(ctx) = ctx else {
                    emit(
                        &ev,
                        &wake,
                        Event::DeliverySkipped(
                            "微信 24 小时活跃窗口已过期或缺少会话令牌，请从手机端发消息激活".into(),
                        ),
                    );
                    continue;
                };
                match protocol::send_message(
                    &base_url,
                    &auth.bot_token,
                    &auth.ilink_user_id,
                    &ctx,
                    &text,
                    Some(&cancel),
                ) {
                    Ok(()) => {}
                    Err(ApiError::Cancelled) => break,
                    Err(ApiError::SessionExpired(msg)) => {
                        emit(
                            &ev,
                            &wake,
                            Event::State(ConnectionState::SessionExpired { reason: msg }),
                        );
                    }
                    Err(e) => {
                        emit(&ev, &wake, Event::DeliveryError(e.to_string()));
                    }
                }
            }
            Outbound::Typing {
                active,
                context_token,
            } => {
                let snapshot = {
                    let guard = shared.lock().unwrap();
                    guard.auth.clone().map(|a| {
                        let base = a.effective_base_url(&guard.base_url_override).to_owned();
                        (a, base, guard.typing_ticket.clone())
                    })
                };
                let Some((auth, base_url, cached_ticket)) = snapshot else {
                    continue;
                };
                let now = now_epoch_ms();
                let Some(ctx) = context_token
                    .filter(|s| !s.trim().is_empty())
                    .or_else(|| auth.active_context_token(now).map(str::to_owned))
                else {
                    continue;
                };
                let ticket = match cached_ticket {
                    Some(t) => Some(t),
                    None => match protocol::get_typing_ticket(
                        &base_url,
                        &auth.bot_token,
                        &auth.ilink_user_id,
                        &ctx,
                        Some(&cancel),
                    ) {
                        Ok(t) => {
                            if let Some(ref val) = t {
                                shared.lock().unwrap().typing_ticket = Some(val.clone());
                            }
                            t
                        }
                        Err(_) => None,
                    },
                };
                if let Some(ticket) = ticket {
                    let _ = protocol::send_typing(
                        &base_url,
                        &auth.bot_token,
                        &auth.ilink_user_id,
                        &ticket,
                        active,
                        Some(&cancel),
                    );
                }
            }
        }
    }
}

enum Mode {
    Idle,
    QrLogin { qrcode: String, qr_url: String },
    Polling,
    SessionExpired,
}

fn run_poll_loop(
    data_dir: PathBuf,
    mut prefs: ClawbotPrefs,
    ctrl_rx: mpsc::Receiver<Control>,
    out_tx: mpsc::Sender<Outbound>,
    ev: mpsc::Sender<Event>,
    wake: Waker,
    shared: Arc<Mutex<SharedAuth>>,
    cancel: Arc<CancelToken>,
) {
    let store = AuthStore::in_dir(&data_dir);
    let started_at_ms = now_epoch_ms();
    let mut restored_online_emitted = false;
    let mut owner_lock: Option<OwnerLock> = None;
    let mut auth: Option<AuthRecord> = None;
    let mut paused = false;
    let mut mode = Mode::Idle;
    let mut backoff_attempt: u32 = 0;
    let mut poll_timeout_ms = DEFAULT_POLL_TIMEOUT_MS;
    let mut seen_ids: VecDeque<String> = VecDeque::with_capacity(SEEN_MSG_CAP);

    let sync_shared =
        |shared: &Arc<Mutex<SharedAuth>>, auth: &Option<AuthRecord>, prefs: &ClawbotPrefs| {
            let mut guard = shared.lock().unwrap();
            guard.auth = auth.clone();
            guard.base_url_override = prefs.normalized_base_url().to_owned();
        };

    let activate = |prefs: &ClawbotPrefs,
                    owner_lock: &mut Option<OwnerLock>,
                    auth: &mut Option<AuthRecord>,
                    mode: &mut Mode,
                    paused: bool,
                    backoff: &mut u32,
                    restored_online_emitted: &mut bool| {
        *backoff = 0;
        if !prefs.enabled {
            *owner_lock = None;
            *mode = Mode::Idle;
            sync_shared(&shared, &None, prefs);
            emit(&ev, &wake, Event::State(ConnectionState::Disabled));
            return;
        }
        if owner_lock.is_none() {
            match OwnerLock::try_acquire(&data_dir) {
                Ok(Some(lock)) => *owner_lock = Some(lock),
                Ok(None) => {
                    *mode = Mode::Idle;
                    sync_shared(&shared, &None, prefs);
                    emit(
                        &ev,
                        &wake,
                        Event::State(ConnectionState::TakenByOtherWindow),
                    );
                    return;
                }
                Err(e) => {
                    *mode = Mode::Idle;
                    emit(
                        &ev,
                        &wake,
                        Event::State(ConnectionState::Error(format!("无法获取单实例锁：{e}"))),
                    );
                    return;
                }
            }
        }
        if auth.is_none() {
            match store.load() {
                Ok(loaded) => *auth = loaded,
                Err(e) => {
                    *auth = None;
                    *mode = Mode::Idle;
                    sync_shared(&shared, &None, prefs);
                    emit(
                        &ev,
                        &wake,
                        Event::State(ConnectionState::SessionExpired {
                            reason: format!("{e}，请重新扫码绑定"),
                        }),
                    );
                    return;
                }
            }
        }
        if let Some(rec) = auth.as_ref() {
            sync_shared(&shared, auth, prefs);
            *mode = Mode::Polling;
            emit(
                &ev,
                &wake,
                Event::State(ConnectionState::Connected {
                    user_hint: mask_user_id(&rec.ilink_user_id),
                    paused,
                }),
            );
            if !*restored_online_emitted {
                *restored_online_emitted = true;
                let has_active_window = rec.active_context_token(now_epoch_ms()).is_some();
                emit(&ev, &wake, Event::RestoredOnline { has_active_window });
            }
        } else {
            sync_shared(&shared, &None, prefs);
            *mode = Mode::Idle;
            emit(&ev, &wake, Event::State(ConnectionState::Unbound));
        }
    };

    activate(
        &prefs,
        &mut owner_lock,
        &mut auth,
        &mut mode,
        paused,
        &mut backoff_attempt,
        &mut restored_online_emitted,
    );

    loop {
        // Drain all pending control messages first.
        let mut quit = false;
        let first_ctrl = match mode {
            Mode::Idle | Mode::SessionExpired => match ctrl_rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            },
            Mode::QrLogin { .. } | Mode::Polling => match ctrl_rx.try_recv() {
                Ok(c) => Some(c),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => break,
            },
        };

        for cmd in first_ctrl.into_iter().chain(ctrl_rx.try_iter()) {
            match cmd {
                Control::Quit => {
                    quit = true;
                    break;
                }
                Control::ApplyPrefs(next) => {
                    let url_changed = next.normalized_base_url() != prefs.normalized_base_url();
                    prefs = next;
                    if url_changed {
                        shared.lock().unwrap().typing_ticket = None;
                    }
                    activate(
                        &prefs,
                        &mut owner_lock,
                        &mut auth,
                        &mut mode,
                        paused,
                        &mut backoff_attempt,
                        &mut restored_online_emitted,
                    );
                }
                Control::ClaimOwnership => {
                    activate(
                        &prefs,
                        &mut owner_lock,
                        &mut auth,
                        &mut mode,
                        paused,
                        &mut backoff_attempt,
                        &mut restored_online_emitted,
                    );
                }
                Control::SetPaused(p) => {
                    paused = p;
                    if let Some(rec) = auth.as_ref()
                        && matches!(mode, Mode::Polling)
                    {
                        emit(
                            &ev,
                            &wake,
                            Event::State(ConnectionState::Connected {
                                user_hint: mask_user_id(&rec.ilink_user_id),
                                paused,
                            }),
                        );
                    }
                }
                Control::CancelQrLogin => {
                    if matches!(mode, Mode::QrLogin { .. }) {
                        activate(
                            &prefs,
                            &mut owner_lock,
                            &mut auth,
                            &mut mode,
                            paused,
                            &mut backoff_attempt,
                            &mut restored_online_emitted,
                        );
                    }
                }
                Control::Unbind => {
                    let _ = store.clear();
                    auth = None;
                    paused = false;
                    shared.lock().unwrap().typing_ticket = None;
                    sync_shared(&shared, &None, &prefs);
                    mode = Mode::Idle;
                    emit(
                        &ev,
                        &wake,
                        Event::State(if prefs.enabled {
                            ConnectionState::Unbound
                        } else {
                            ConnectionState::Disabled
                        }),
                    );
                }
                Control::StartQrLogin => {
                    if !prefs.enabled {
                        prefs.enabled = true;
                    }
                    if owner_lock.is_none() {
                        match OwnerLock::try_acquire(&data_dir) {
                            Ok(Some(lock)) => owner_lock = Some(lock),
                            Ok(None) => {
                                mode = Mode::Idle;
                                emit(
                                    &ev,
                                    &wake,
                                    Event::State(ConnectionState::TakenByOtherWindow),
                                );
                                continue;
                            }
                            Err(e) => {
                                mode = Mode::Idle;
                                emit(
                                    &ev,
                                    &wake,
                                    Event::State(ConnectionState::Error(format!(
                                        "无法获取单实例锁：{e}"
                                    ))),
                                );
                                continue;
                            }
                        }
                    }
                    cancel.reset();
                    match protocol::get_bot_qrcode(prefs.normalized_base_url(), Some(&cancel)) {
                        Ok(info) => {
                            emit(
                                &ev,
                                &wake,
                                Event::State(ConnectionState::WaitingQr {
                                    qr_url: info.qrcode_img_content.clone(),
                                    scanned: false,
                                }),
                            );
                            mode = Mode::QrLogin {
                                qrcode: info.qrcode,
                                qr_url: info.qrcode_img_content,
                            };
                        }
                        Err(ApiError::Cancelled) => {}
                        Err(e) => {
                            mode = Mode::Idle;
                            emit(
                                &ev,
                                &wake,
                                Event::State(ConnectionState::Error(e.to_string())),
                            );
                        }
                    }
                }
            }
        }
        if quit {
            break;
        }

        cancel.reset();
        match &mode {
            Mode::Idle | Mode::SessionExpired => continue,
            Mode::QrLogin { qrcode, qr_url } => {
                let qr = qrcode.clone();
                let url_display = qr_url.clone();
                match protocol::get_qrcode_status(
                    prefs.normalized_base_url(),
                    &qr,
                    QR_POLL_TIMEOUT_MS,
                    Some(&cancel),
                ) {
                    Ok(QrStatus::Wait) => {}
                    Ok(QrStatus::Scanned) => {
                        emit(
                            &ev,
                            &wake,
                            Event::State(ConnectionState::WaitingQr {
                                qr_url: url_display,
                                scanned: true,
                            }),
                        );
                    }
                    Ok(QrStatus::Expired) => {
                        mode = Mode::Idle;
                        emit(
                            &ev,
                            &wake,
                            Event::State(ConnectionState::Error(
                                "二维码已过期，请点击「重新生成二维码」".into(),
                            )),
                        );
                    }
                    Ok(QrStatus::Confirmed {
                        bot_token,
                        ilink_bot_id,
                        ilink_user_id,
                        base_url,
                    }) => {
                        let rec = AuthRecord {
                            bot_token,
                            ilink_bot_id,
                            ilink_user_id,
                            base_url,
                            get_updates_buf: String::new(),
                            last_context_token: None,
                            last_inbound_ms: None,
                            keepalive_sent_mask: 0,
                        };
                        if let Err(e) = store.save(&rec) {
                            emit(
                                &ev,
                                &wake,
                                Event::State(ConnectionState::Error(format!(
                                    "无法加密保存微信凭据：{e}"
                                ))),
                            );
                            mode = Mode::Idle;
                            continue;
                        }
                        let hint = mask_user_id(&rec.ilink_user_id);
                        auth = Some(rec);
                        paused = false;
                        backoff_attempt = 0;
                        shared.lock().unwrap().typing_ticket = None;
                        sync_shared(&shared, &auth, &prefs);
                        mode = Mode::Polling;
                        emit(
                            &ev,
                            &wake,
                            Event::State(ConnectionState::Connected {
                                user_hint: hint,
                                paused: false,
                            }),
                        );
                    }
                    Err(ApiError::Cancelled) => {}
                    Err(e) => {
                        mode = Mode::Idle;
                        emit(
                            &ev,
                            &wake,
                            Event::State(ConnectionState::Error(e.to_string())),
                        );
                    }
                }
            }
            Mode::Polling => {
                let Some(rec) = auth.as_mut() else {
                    mode = Mode::Idle;
                    continue;
                };
                if prefs.keepalive_remind && !paused {
                    let now = now_epoch_ms();
                    if let Some(hours_left) = rec.take_due_keepalive_hours(now) {
                        let ctx = rec.last_context_token.clone();
                        let _ = store.save(rec);
                        let msg = format!(
                            "⏰ Mcli 会话保活提醒：当前微信会话令牌预计约 {hours_left} 小时后过期。请随手回复 /ping 即可自动续期 24 小时（无需重新扫码，也不会打扰正在运行的 Agent）。"
                        );
                        let _ = out_tx.send(Outbound::Text {
                            text: msg,
                            context_token: ctx,
                        });
                        emit(&ev, &wake, Event::KeepaliveSent { hours_left });
                    }
                }
                let base_url = rec
                    .effective_base_url(prefs.normalized_base_url())
                    .to_owned();
                let token = rec.bot_token.clone();
                let cursor = rec.get_updates_buf.clone();
                let expected_user = rec.ilink_user_id.clone();

                match protocol::get_updates(
                    &base_url,
                    &token,
                    &cursor,
                    poll_timeout_ms,
                    Some(&cancel),
                ) {
                    Ok(page) => {
                        if backoff_attempt > 0 {
                            backoff_attempt = 0;
                            emit(
                                &ev,
                                &wake,
                                Event::State(ConnectionState::Connected {
                                    user_hint: mask_user_id(&expected_user),
                                    paused,
                                }),
                            );
                        }
                        if let Some(ms) = page.longpolling_timeout_ms {
                            poll_timeout_ms = (ms as i32).clamp(10_000, 65_000) + 5_000;
                        }
                        let mut dirty = false;
                        if page.get_updates_buf != rec.get_updates_buf {
                            rec.get_updates_buf = page.get_updates_buf;
                            dirty = true;
                        }
                        let mut pending_inbound = Vec::new();
                        let mut stale_count: usize = 0;
                        let mut stale_ctx: Option<String> = None;
                        for msg in page.messages {
                            if !msg.message_id.is_empty() {
                                if seen_ids.iter().any(|s| s == &msg.message_id) {
                                    continue;
                                }
                                seen_ids.push_back(msg.message_id.clone());
                                while seen_ids.len() > SEEN_MSG_CAP {
                                    seen_ids.pop_front();
                                }
                            }
                            // Strict single-user whitelist: only respond to the bound ilink_user_id.
                            if msg.from_user_id != expected_user {
                                emit(
                                    &ev,
                                    &wake,
                                    Event::RejectedSender {
                                        from_user_id: msg.from_user_id,
                                    },
                                );
                                continue;
                            }
                            if !msg.context_token.is_empty() {
                                rec.last_context_token = Some(msg.context_token.clone());
                                rec.last_inbound_ms = Some(now_epoch_ms());
                                rec.keepalive_sent_mask = 0;
                                dirty = true;
                            }
                            if paused {
                                continue;
                            }
                            // Drop messages created before this Mcli process started (beyond clock-skew grace)
                            // so restarting after a crash/close never replays stale offline commands.
                            if msg.create_time_ms.is_some_and(|ts| {
                                ts.saturating_add(OFFLINE_STALE_GRACE_MS) < started_at_ms
                            }) {
                                stale_count += 1;
                                if !msg.context_token.is_empty() {
                                    stale_ctx = Some(msg.context_token);
                                }
                                continue;
                            }
                            pending_inbound.push(Event::Inbound {
                                from_user_id: msg.from_user_id,
                                context_token: msg.context_token,
                                text: msg.text,
                            });
                        }
                        if dirty {
                            let _ = store.save(rec);
                            sync_shared(&shared, &auth, &prefs);
                        }
                        if stale_count > 0 {
                            emit(
                                &ev,
                                &wake,
                                Event::OfflineSkipped {
                                    count: stale_count,
                                    context_token: stale_ctx,
                                },
                            );
                        }
                        for inbound in pending_inbound {
                            emit(&ev, &wake, inbound);
                        }
                    }
                    Err(ApiError::Cancelled) => {}
                    Err(ApiError::SessionExpired(reason)) => {
                        mode = Mode::SessionExpired;
                        emit(
                            &ev,
                            &wake,
                            Event::State(ConnectionState::SessionExpired { reason }),
                        );
                    }
                    Err(e) => {
                        backoff_attempt = backoff_attempt.saturating_add(1);
                        let delay_secs = (1u64 << backoff_attempt.min(5)).min(MAX_BACKOFF_SECS);
                        emit(
                            &ev,
                            &wake,
                            Event::State(ConnectionState::Reconnecting {
                                user_hint: mask_user_id(&expected_user),
                                attempt: backoff_attempt,
                                reason: e.to_string(),
                            }),
                        );
                        // Wait in interruptible steps via ctrl_rx timeout so Quit/Cancel stays instant.
                        if let Ok(cmd) = ctrl_rx.recv_timeout(Duration::from_secs(delay_secs)) {
                            match cmd {
                                Control::Quit => break,
                                Control::ApplyPrefs(next) => {
                                    prefs = next;
                                    activate(
                                        &prefs,
                                        &mut owner_lock,
                                        &mut auth,
                                        &mut mode,
                                        paused,
                                        &mut backoff_attempt,
                                        &mut restored_online_emitted,
                                    );
                                }
                                Control::Unbind => {
                                    let _ = store.clear();
                                    auth = None;
                                    sync_shared(&shared, &None, &prefs);
                                    mode = Mode::Idle;
                                    emit(&ev, &wake, Event::State(ConnectionState::Unbound));
                                }
                                Control::SetPaused(p) => paused = p,
                                Control::StartQrLogin
                                | Control::CancelQrLogin
                                | Control::ClaimOwnership => {}
                            }
                        }
                    }
                }
            }
        }
    }
}
