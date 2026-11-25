use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::{OsStr, c_void};
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
    WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_ADDREQ_FLAG_REPLACE,
    WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts,
};

const ERROR_WINHTTP_TIMEOUT: u32 = 12002;
const CHANNEL_VERSION: &str = "1.0.0";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
static CLIENT_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    Timeout,
    Cancelled,
    SessionExpired(String),
    HttpStatus(u32, String),
    Transport(String),
    Protocol(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "请求超时"),
            Self::Cancelled => write!(f, "请求已取消"),
            Self::SessionExpired(msg) => {
                if msg.is_empty() {
                    write!(f, "微信会话已过期（-14），请重新扫码")
                } else {
                    write!(f, "微信会话已过期：{msg}")
                }
            }
            Self::HttpStatus(code, body) => write!(f, "HTTP {code}: {body}"),
            Self::Transport(e) => write!(f, "网络错误：{e}"),
            Self::Protocol(e) => write!(f, "协议错误：{e}"),
        }
    }
}

#[derive(Default)]
pub struct CancelToken {
    cancelled: AtomicBool,
    in_flight: Mutex<usize>,
}

impl CancelToken {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn reset(&self) {
        self.cancelled.store(false, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        let handle = {
            let mut guard = self.in_flight.lock().unwrap();
            let h = *guard;
            *guard = 0;
            h
        };
        if handle != 0 {
            unsafe {
                WinHttpCloseHandle(handle as *mut c_void);
            }
        }
    }

    fn set_in_flight(&self, handle: *mut c_void) -> bool {
        let mut guard = self.in_flight.lock().unwrap();
        if self.cancelled.load(Ordering::Acquire) {
            return false;
        }
        *guard = handle as usize;
        true
    }

    /// Clears and returns `true` if caller still owns the handle (and must close it).
    fn clear_in_flight(&self, handle: *mut c_void) -> bool {
        let mut guard = self.in_flight.lock().unwrap();
        if *guard == handle as usize {
            *guard = 0;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QrCodeInfo {
    pub qrcode: String,
    pub qrcode_img_content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QrStatus {
    Wait,
    Scanned,
    Expired,
    Confirmed {
        bot_token: String,
        ilink_bot_id: String,
        ilink_user_id: String,
        base_url: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboundMessage {
    pub from_user_id: String,
    pub context_token: String,
    pub text: String,
    pub message_id: String,
    pub create_time_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdatesPage {
    pub get_updates_buf: String,
    pub longpolling_timeout_ms: Option<u32>,
    pub messages: Vec<InboundMessage>,
}

struct ParsedUrl {
    https: bool,
    host: String,
    port: u16,
    path_and_query: String,
    is_loopback: bool,
}

fn parse_url(raw: &str) -> Result<ParsedUrl, ApiError> {
    let trimmed = raw.trim();
    let (https, rest) = if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(ApiError::Protocol(format!("不支持的地址协议：{trimmed}")));
    };
    let (authority, path_and_query) = match rest.find('/') {
        Some(idx) => (&rest[..idx], rest[idx..].to_owned()),
        None => (rest, "/".to_owned()),
    };
    if authority.is_empty() {
        return Err(ApiError::Protocol("地址缺少主机名".into()));
    }
    let (host, port) = if let Some((h, p)) = authority.rsplit_once(':')
        && !h.contains(']')
    {
        let port = p
            .parse::<u16>()
            .map_err(|_| ApiError::Protocol(format!("端口无效：{p}")))?;
        (h.to_owned(), port)
    } else {
        (authority.to_owned(), if https { 443 } else { 80 })
    };
    let is_loopback =
        host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "[::1]";
    Ok(ParsedUrl {
        https,
        host,
        port,
        path_and_query,
        is_loopback,
    })
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn percent_encode_query(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[(byte >> 4) as usize]));
            out.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    out
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn random_wechat_uin() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let seq = CLIENT_SEQ.fetch_add(1, Ordering::Relaxed);
    let mixed = nanos
        .wrapping_mul(6364136223846793005)
        .wrapping_add(seq ^ 0x9e3779b9);
    let uin = ((mixed >> 16) as u32).max(1);
    base64_encode(uin.to_string().as_bytes())
}

fn next_client_id() -> String {
    let ms = crate::clawbot::store::now_epoch_ms();
    let seq = CLIENT_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("mcli-{}-{ms}-{seq}", std::process::id())
}

struct WinHttpGuard(*mut c_void);
impl Drop for WinHttpGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                WinHttpCloseHandle(self.0);
            }
        }
    }
}

pub fn http_request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    receive_timeout_ms: i32,
    cancel: Option<&CancelToken>,
) -> Result<(u32, Vec<u8>), ApiError> {
    if cancel.is_some_and(CancelToken::is_cancelled) {
        return Err(ApiError::Cancelled);
    }
    let parsed = parse_url(url)?;
    let agent = wide("Mcli-ClawBot/1.0");
    let access = if parsed.is_loopback {
        WINHTTP_ACCESS_TYPE_NO_PROXY
    } else {
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY
    };
    let mut session = unsafe { WinHttpOpen(agent.as_ptr(), access, null(), null(), 0) };
    if session.is_null() && !parsed.is_loopback {
        session = unsafe {
            WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
                null(),
                null(),
                0,
            )
        };
    }
    if session.is_null() {
        return Err(ApiError::Transport(format!(
            "WinHttpOpen 失败（{}）",
            unsafe { GetLastError() }
        )));
    }
    let _session_guard = WinHttpGuard(session);

    let host_w = wide(&parsed.host);
    let conn = unsafe { WinHttpConnect(session, host_w.as_ptr(), parsed.port, 0) };
    if conn.is_null() {
        return Err(ApiError::Transport(format!(
            "WinHttpConnect 失败（{}）",
            unsafe { GetLastError() }
        )));
    }
    let _conn_guard = WinHttpGuard(conn);

    let method_w = wide(method);
    let path_w = wide(&parsed.path_and_query);
    let flags = if parsed.https { WINHTTP_FLAG_SECURE } else { 0 };
    let req = unsafe {
        WinHttpOpenRequest(
            conn,
            method_w.as_ptr(),
            path_w.as_ptr(),
            null(),
            null(),
            null(),
            flags,
        )
    };
    if req.is_null() {
        return Err(ApiError::Transport(format!(
            "WinHttpOpenRequest 失败（{}）",
            unsafe { GetLastError() }
        )));
    }

    if let Some(tok) = cancel
        && !tok.set_in_flight(req)
    {
        unsafe {
            WinHttpCloseHandle(req);
        }
        return Err(ApiError::Cancelled);
    }

    let result = (|| -> Result<(u32, Vec<u8>), ApiError> {
        unsafe {
            WinHttpSetTimeouts(req, 10_000, 15_000, 15_000, receive_timeout_ms);
        }
        for (name, value) in headers {
            let line = wide(&format!("{name}: {value}\r\n"));
            let ok = unsafe {
                WinHttpAddRequestHeaders(
                    req,
                    line.as_ptr(),
                    u32::MAX,
                    WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE,
                )
            };
            if ok == 0 {
                return Err(ApiError::Transport(format!(
                    "添加请求头失败（{}）",
                    unsafe { GetLastError() }
                )));
            }
        }
        let (ptr, len) = match body {
            Some(bytes) if !bytes.is_empty() => {
                (bytes.as_ptr() as *const c_void, bytes.len() as u32)
            }
            _ => (null(), 0),
        };
        let sent = unsafe { WinHttpSendRequest(req, null(), 0, ptr, len, len, 0) };
        if sent == 0 {
            return Err(classify_winhttp_error(cancel));
        }
        let received = unsafe { WinHttpReceiveResponse(req, null_mut()) };
        if received == 0 {
            return Err(classify_winhttp_error(cancel));
        }
        let mut status: u32 = 0;
        let mut status_len = std::mem::size_of::<u32>() as u32;
        let ok = unsafe {
            WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&mut status as *mut u32).cast::<c_void>(),
                &mut status_len,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(classify_winhttp_error(cancel));
        }
        let mut out = Vec::new();
        loop {
            let mut avail: u32 = 0;
            if unsafe { WinHttpQueryDataAvailable(req, &mut avail) } == 0 {
                return Err(classify_winhttp_error(cancel));
            }
            if avail == 0 {
                break;
            }
            if out.len().saturating_add(avail as usize) > MAX_RESPONSE_BYTES {
                return Err(ApiError::Protocol("响应体积超过 4 MiB 上限".into()));
            }
            let start = out.len();
            out.resize(start + avail as usize, 0);
            let mut read: u32 = 0;
            if unsafe {
                WinHttpReadData(
                    req,
                    out[start..].as_mut_ptr().cast::<c_void>(),
                    avail,
                    &mut read,
                )
            } == 0
            {
                return Err(classify_winhttp_error(cancel));
            }
            out.truncate(start + read as usize);
            if read == 0 {
                break;
            }
        }
        Ok((status, out))
    })();

    let should_close = cancel.map_or(true, |tok| tok.clear_in_flight(req));
    if should_close {
        unsafe {
            WinHttpCloseHandle(req);
        }
    }
    result
}

fn classify_winhttp_error(cancel: Option<&CancelToken>) -> ApiError {
    if cancel.is_some_and(CancelToken::is_cancelled) {
        return ApiError::Cancelled;
    }
    let code = unsafe { GetLastError() };
    if code == ERROR_WINHTTP_TIMEOUT {
        ApiError::Timeout
    } else {
        ApiError::Transport(format!("WinHTTP 错误码 {code}"))
    }
}

fn check_api_json(status: u32, body: &[u8]) -> Result<Value, ApiError> {
    if !(200..300).contains(&status) {
        let preview = String::from_utf8_lossy(body);
        return Err(ApiError::HttpStatus(
            status,
            preview.chars().take(200).collect(),
        ));
    }
    let val: Value = serde_json::from_slice(body)
        .map_err(|e| ApiError::Protocol(format!("JSON 解析失败：{e}")))?;
    let errcode = val
        .get("errcode")
        .and_then(Value::as_i64)
        .or_else(|| val.get("ret").and_then(Value::as_i64))
        .unwrap_or(0);
    if errcode == -14 {
        let msg = val
            .get("errmsg")
            .and_then(Value::as_str)
            .unwrap_or("session timeout")
            .to_owned();
        return Err(ApiError::SessionExpired(msg));
    }
    if errcode != 0 {
        let msg = val
            .get("errmsg")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        return Err(ApiError::Protocol(format!(
            "接口返回错误码 {errcode}{}",
            if msg.is_empty() {
                String::new()
            } else {
                format!("（{msg}）")
            }
        )));
    }
    Ok(val)
}

#[derive(Serialize)]
struct BaseInfo<'a> {
    channel_version: &'a str,
}

pub fn get_bot_qrcode(
    base_url: &str,
    cancel: Option<&CancelToken>,
) -> Result<QrCodeInfo, ApiError> {
    let url = format!(
        "{}/ilink/bot/get_bot_qrcode?bot_type=3",
        base_url.trim_end_matches('/')
    );
    let (status, body) = http_request("GET", &url, &[], None, 15_000, cancel)?;
    let val = check_api_json(status, &body)?;
    let qrcode = val["qrcode"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::Protocol("缺少 qrcode 字段".into()))?
        .to_owned();
    let qrcode_img_content = val["qrcode_img_content"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::Protocol("缺少 qrcode_img_content 字段".into()))?
        .to_owned();
    Ok(QrCodeInfo {
        qrcode,
        qrcode_img_content,
    })
}

pub fn get_qrcode_status(
    base_url: &str,
    qrcode: &str,
    timeout_ms: i32,
    cancel: Option<&CancelToken>,
) -> Result<QrStatus, ApiError> {
    let url = format!(
        "{}/ilink/bot/get_qrcode_status?qrcode={}",
        base_url.trim_end_matches('/'),
        percent_encode_query(qrcode)
    );
    let headers = [("iLink-App-ClientVersion", "1")];
    let (status, body) = match http_request("GET", &url, &headers, None, timeout_ms, cancel) {
        Ok(res) => res,
        Err(ApiError::Timeout) => return Ok(QrStatus::Wait),
        Err(e) => return Err(e),
    };
    let val = check_api_json(status, &body)?;
    match val["status"].as_str().unwrap_or("wait") {
        "scaned" | "scanned" => Ok(QrStatus::Scanned),
        "expired" => Ok(QrStatus::Expired),
        "confirmed" => {
            let bot_token = val["bot_token"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| ApiError::Protocol("扫码确认但缺少 bot_token".into()))?
                .to_owned();
            let ilink_bot_id = val["ilink_bot_id"].as_str().unwrap_or("").to_owned();
            let ilink_user_id = val["ilink_user_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| ApiError::Protocol("扫码确认但缺少 ilink_user_id".into()))?
                .to_owned();
            let returned_base = val["baseurl"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(base_url)
                .trim_end_matches('/')
                .to_owned();
            Ok(QrStatus::Confirmed {
                bot_token,
                ilink_bot_id,
                ilink_user_id,
                base_url: returned_base,
            })
        }
        _ => Ok(QrStatus::Wait),
    }
}

pub fn get_updates(
    base_url: &str,
    bot_token: &str,
    get_updates_buf: &str,
    timeout_ms: i32,
    cancel: Option<&CancelToken>,
) -> Result<UpdatesPage, ApiError> {
    #[derive(Serialize)]
    struct Req<'a> {
        get_updates_buf: &'a str,
        base_info: BaseInfo<'a>,
    }
    let url = format!("{}/ilink/bot/getupdates", base_url.trim_end_matches('/'));
    let payload = serde_json::to_vec(&Req {
        get_updates_buf,
        base_info: BaseInfo {
            channel_version: CHANNEL_VERSION,
        },
    })
    .map_err(|e| ApiError::Protocol(e.to_string()))?;
    let auth = format!("Bearer {bot_token}");
    let uin = random_wechat_uin();
    let headers = [
        ("Content-Type", "application/json"),
        ("AuthorizationType", "ilink_bot_token"),
        ("Authorization", auth.as_str()),
        ("X-WECHAT-UIN", uin.as_str()),
    ];
    let (status, body) =
        match http_request("POST", &url, &headers, Some(&payload), timeout_ms, cancel) {
            Ok(res) => res,
            Err(ApiError::Timeout) => {
                return Ok(UpdatesPage {
                    get_updates_buf: get_updates_buf.to_owned(),
                    longpolling_timeout_ms: None,
                    messages: Vec::new(),
                });
            }
            Err(e) => return Err(e),
        };
    let val = check_api_json(status, &body)?;
    let next_buf = val["get_updates_buf"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| get_updates_buf.to_owned());
    let longpolling_timeout_ms = val["longpolling_timeout_ms"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok());

    let mut messages = Vec::new();
    if let Some(arr) = val["msgs"].as_array() {
        for raw in arr {
            // Skip bot's own outbound echoes (message_type == 2).
            if raw["message_type"].as_i64() == Some(2) {
                continue;
            }
            let from_user_id = raw["from_user_id"].as_str().unwrap_or("").trim().to_owned();
            let context_token = raw["context_token"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_owned();
            if from_user_id.is_empty() {
                continue;
            }
            let mut text_parts = Vec::new();
            if let Some(items) = raw["item_list"].as_array() {
                for item in items {
                    if item["type"].as_i64() == Some(1)
                        && let Some(t) = item["text_item"]["text"].as_str()
                        && !t.is_empty()
                    {
                        text_parts.push(t);
                    }
                }
            }
            let text = text_parts.join("\n");
            if text.trim().is_empty() {
                continue;
            }
            let message_id = raw["message_id"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| raw["msg_id"].as_str().map(str::to_owned))
                .or_else(|| raw["message_id"].as_i64().map(|n| n.to_string()))
                .unwrap_or_default();
            let create_time_ms = raw["create_time_ms"]
                .as_u64()
                .or_else(|| {
                    raw["create_time"].as_u64().map(|t| {
                        if t > 0 && t < 10_000_000_000 {
                            t * 1000
                        } else {
                            t
                        }
                    })
                })
                .filter(|&t| t > 0);
            messages.push(InboundMessage {
                from_user_id,
                context_token,
                text,
                message_id,
                create_time_ms,
            });
        }
    }
    Ok(UpdatesPage {
        get_updates_buf: next_buf,
        longpolling_timeout_ms,
        messages,
    })
}

pub fn send_message(
    base_url: &str,
    bot_token: &str,
    to_user_id: &str,
    context_token: &str,
    text: &str,
    cancel: Option<&CancelToken>,
) -> Result<(), ApiError> {
    #[derive(Serialize)]
    struct TextItem<'a> {
        text: &'a str,
    }
    #[derive(Serialize)]
    struct Item<'a> {
        r#type: u32,
        text_item: TextItem<'a>,
    }
    #[derive(Serialize)]
    struct Msg<'a> {
        from_user_id: &'a str,
        to_user_id: &'a str,
        client_id: String,
        message_type: u32,
        message_state: u32,
        context_token: &'a str,
        item_list: [Item<'a>; 1],
    }
    #[derive(Serialize)]
    struct Req<'a> {
        msg: Msg<'a>,
        base_info: BaseInfo<'a>,
    }
    let url = format!("{}/ilink/bot/sendmessage", base_url.trim_end_matches('/'));
    let payload = serde_json::to_vec(&Req {
        msg: Msg {
            from_user_id: "",
            to_user_id,
            client_id: next_client_id(),
            message_type: 2,
            message_state: 2,
            context_token,
            item_list: [Item {
                r#type: 1,
                text_item: TextItem { text },
            }],
        },
        base_info: BaseInfo {
            channel_version: CHANNEL_VERSION,
        },
    })
    .map_err(|e| ApiError::Protocol(e.to_string()))?;
    let auth = format!("Bearer {bot_token}");
    let uin = random_wechat_uin();
    let headers = [
        ("Content-Type", "application/json"),
        ("AuthorizationType", "ilink_bot_token"),
        ("Authorization", auth.as_str()),
        ("X-WECHAT-UIN", uin.as_str()),
    ];
    let (status, body) = http_request("POST", &url, &headers, Some(&payload), 15_000, cancel)?;
    check_api_json(status, &body)?;
    Ok(())
}

pub fn get_typing_ticket(
    base_url: &str,
    bot_token: &str,
    ilink_user_id: &str,
    context_token: &str,
    cancel: Option<&CancelToken>,
) -> Result<Option<String>, ApiError> {
    #[derive(Serialize)]
    struct Req<'a> {
        ilink_user_id: &'a str,
        context_token: &'a str,
        base_info: BaseInfo<'a>,
    }
    let url = format!("{}/ilink/bot/getconfig", base_url.trim_end_matches('/'));
    let payload = serde_json::to_vec(&Req {
        ilink_user_id,
        context_token,
        base_info: BaseInfo {
            channel_version: CHANNEL_VERSION,
        },
    })
    .map_err(|e| ApiError::Protocol(e.to_string()))?;
    let auth = format!("Bearer {bot_token}");
    let uin = random_wechat_uin();
    let headers = [
        ("Content-Type", "application/json"),
        ("AuthorizationType", "ilink_bot_token"),
        ("Authorization", auth.as_str()),
        ("X-WECHAT-UIN", uin.as_str()),
    ];
    let (status, body) = http_request("POST", &url, &headers, Some(&payload), 8_000, cancel)?;
    let val = check_api_json(status, &body)?;
    Ok(val["typing_ticket"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned))
}

pub fn send_typing(
    base_url: &str,
    bot_token: &str,
    ilink_user_id: &str,
    typing_ticket: &str,
    active: bool,
    cancel: Option<&CancelToken>,
) -> Result<(), ApiError> {
    #[derive(Serialize)]
    struct Req<'a> {
        ilink_user_id: &'a str,
        typing_ticket: &'a str,
        status: u32,
        base_info: BaseInfo<'a>,
    }
    let url = format!("{}/ilink/bot/sendtyping", base_url.trim_end_matches('/'));
    let payload = serde_json::to_vec(&Req {
        ilink_user_id,
        typing_ticket,
        status: if active { 1 } else { 2 },
        base_info: BaseInfo {
            channel_version: CHANNEL_VERSION,
        },
    })
    .map_err(|e| ApiError::Protocol(e.to_string()))?;
    let auth = format!("Bearer {bot_token}");
    let uin = random_wechat_uin();
    let headers = [
        ("Content-Type", "application/json"),
        ("AuthorizationType", "ilink_bot_token"),
        ("Authorization", auth.as_str()),
        ("X-WECHAT-UIN", uin.as_str()),
    ];
    let (status, body) = http_request("POST", &url, &headers, Some(&payload), 8_000, cancel)?;
    check_api_json(status, &body)?;
    Ok(())
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct _Unused {}
