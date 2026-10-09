//! 同步引擎：按固定间隔用各账号的登录会话拉取逐条请求日志并聚合成按天行，
//! 同时刷新各账号的 Go 额度，并把状态通过 Tauri 事件推给前端。
//!
//! 唯一数据源是控制台会话（`/request-logs`）：Service API Key 恒 403，
//! 因此程序不再支持 Key 鉴权，未登录的账号直接跳过。
//! 数据按 `account_id` 隔离，逐条日志主键 `(account_id, id)` 保证重复同步幂等。

use crate::providers::opencode::Quota;
use crate::providers::{AnyProvider, Credentials};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::RwLock;
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Serialize, Default)]
pub struct SyncStatus {
    pub syncing: bool,
    pub last_sync_ms: i64,
    pub last_full_sync_ms: i64,
    pub last_error: Option<String>,
    pub local_rows: i64,
    pub total_cost_micro_cents: i64,
    /// 本次同步的数据源备注，如 "request-logs ×123"
    pub source_note: String,
    /// 本次同步处理的条数
    pub last_added: i64,
}

static STATUS: RwLock<Option<SyncStatus>> = RwLock::new(None);
/// 每个账号的额度缓存（key = account_id）。
static QUOTAS: RwLock<Option<HashMap<String, Quota>>> = RwLock::new(None);
static SYNC_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 每个账号额度的最近拉取时间（毫秒时间戳）。
static QUOTA_FETCHED_AT: RwLock<Option<HashMap<String, i64>>> = RwLock::new(None);

/// 每个账号连续命中等退避的失败次数，用于指数退避。
static COOLDOWN_STRIKES: RwLock<Option<HashMap<String, i32>>> = RwLock::new(None);

/// 每个账号命中限流后的冷却截止时间（毫秒时间戳）。
static COOLDOWN_UNTIL: RwLock<Option<HashMap<String, i64>>> = RwLock::new(None);

/// 首次退避时长（秒），之后按指数翻倍。
const COOLDOWN_BASE_SECS: i64 = 30;
/// 退避上限（秒）。
const COOLDOWN_MAX_SECS: i64 = 600;

/// 某个账号的冷却剩余秒数（0 = 不在冷却）。
pub fn cooldown_left_secs_for(account_id: &str) -> i64 {
    let until = COOLDOWN_UNTIL
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(account_id))
        .copied()
        .unwrap_or(0);
    let left = until - chrono::Utc::now().timestamp_millis();
    if left > 0 { left / 1000 + 1 } else { 0 }
}

/// 全局冷却：取所有账号中最长的剩余时间（用于状态展示）。
#[allow(dead_code)] // 预留给前端显示总体冷却倒计时
pub fn cooldown_left_secs() -> i64 {
    let guard = COOLDOWN_UNTIL.read().unwrap_or_else(|e| e.into_inner());
    let now = chrono::Utc::now().timestamp_millis();
    guard
        .as_ref()
        .map(|m| {
            m.values()
                .map(|&until| (until - now).max(0) / 1000 + if until > now { 1 } else { 0 })
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

/// 进入冷却（按账号独立，指数退避）。
fn start_cooldown(account_id: &str, err: &str) {
    let mut guard = COOLDOWN_UNTIL.write().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);

    let mut strikes_guard = COOLDOWN_STRIKES.write().unwrap_or_else(|e| e.into_inner());
    let strikes_map = strikes_guard.get_or_insert_with(HashMap::new);
    let strikes = strikes_map.entry(account_id.to_string()).or_insert(0);
    *strikes += 1;

    let secs = (COOLDOWN_BASE_SECS * (1i64 << (*strikes - 1).min(5))).min(COOLDOWN_MAX_SECS);
    let until = chrono::Utc::now().timestamp_millis() + secs * 1000;
    map.insert(account_id.to_string(), until);

    write_status(|st| {
        st.last_error = Some(format!("{err}；已暂停请求 {secs} 秒以降低频率"))
    });
}

/// 成功后重置退避计数器。
fn reset_backoff(account_id: &str) {
    if let Some(ref mut map) = *COOLDOWN_STRIKES.write().unwrap_or_else(|e| e.into_inner()) {
        map.insert(account_id.to_string(), 0);
    }
    if let Some(ref mut map) = *COOLDOWN_UNTIL.write().unwrap_or_else(|e| e.into_inner()) {
        map.remove(account_id);
    }
}

/// 额度缓存是否仍然新鲜（相对于给定的最大存活时间毫秒）。
pub fn quota_is_fresh(account_id: &str, max_age_ms: i64) -> bool {
    let fetched = QUOTA_FETCHED_AT
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(account_id))
        .copied()
        .unwrap_or(0);
    chrono::Utc::now().timestamp_millis() - fetched < max_age_ms
}

/// 该错误是否值得退避（限流 / 服务端错误 / 网络抖动）。
fn needs_backoff(err: &str) -> bool {
    err.contains("429")
        || err.contains("HTTP 5")
        || err.contains("网络请求失败")
        || err.contains("timed out")
}

fn read_status() -> SyncStatus {
    STATUS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

fn write_status(f: impl FnOnce(&mut SyncStatus)) {
    let mut guard = STATUS.write().unwrap_or_else(|e| e.into_inner());
    let mut st = guard.clone().unwrap_or_default();
    f(&mut st);
    *guard = Some(st);
}

pub fn status() -> SyncStatus {
    read_status()
}

/// 某个账号的额度缓存。
pub fn quota_for(account_id: &str) -> Option<Quota> {
    QUOTAS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(account_id))
        .cloned()
}

pub fn set_quota_for(account_id: &str, q: Option<Quota>) {
    let mut guard = QUOTAS.write().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    match q {
        Some(q) => {
            map.insert(account_id.to_string(), q);
        }
        None => {
            map.remove(account_id);
        }
    }
}

/// 某个账号的额度；`account_id` 为空时用主账号。
pub fn quota_of(account_id: Option<&str>) -> Option<Quota> {
    let id = match account_id {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => crate::accounts::primary_id(),
    };
    quota_for(&id)
}

fn emit(app: &AppHandle, event: &str) {
    let _ = app.emit(event, ());
}

/// 账号维度的 meta key，如 `last_requestlog_ms:org_xxx`。
fn meta_key(base: &str, account_id: &str) -> String {
    format!("{base}:{account_id}")
}

fn account_meta(base: &str, account_id: &str) -> Option<String> {
    crate::store::get_meta(&meta_key(base, account_id))
}

/// 该账号是否做过首同步。
fn initialized(account_id: &str) -> bool {
    account_meta("initialized", account_id).as_deref() == Some("1")
}

/// 刷新某个账号的额度并广播。
pub async fn refresh_quota(
    account: &crate::accounts::Account,
    app: Option<&AppHandle>,
) -> Result<Quota, String> {
    let left = cooldown_left_secs_for(&account.id);
    if left > 0 {
        return Err(format!("限流冷却中（{left} 秒后恢复）"));
    }
    let provider = AnyProvider::for_id(account.provider);
    let q = provider.quota(&account.credentials()).await.map_err(|e| {
        if needs_backoff(&e) {
            start_cooldown(&account.id, &e);
        }
        e
    })?;
    set_quota_for(&account.id, Some(q.clone()));
    reset_backoff(&account.id);
    {
        let mut guard = QUOTA_FETCHED_AT.write().unwrap_or_else(|e| e.into_inner());
        guard.get_or_insert_with(HashMap::new).insert(account.id.clone(), chrono::Utc::now().timestamp_millis());
    }
    if let Some(app) = app {
        emit(app, "quota-updated");
    }
    Ok(q)
}

/// 拉取逐条日志分页，返回 (写入行数, 最大 started_at_ms)。
async fn pull_request_log_pages(
    provider: &AnyProvider,
    creds: &Credentials,
    account_id: &str,
    since: i64,
    until: i64,
) -> Result<(usize, i64), String> {
    let mut cursor: Option<String> = None;
    let mut total = 0usize;
    let mut max_started = since;
    let mut pages = 0;
    loop {
        pages += 1;
        if pages > 600 {
            break;
        }
        let page = provider
            .request_logs(creds, since, until, cursor.as_deref(), 100)
            .await
            .map_err(|e| {
                if needs_backoff(&e) {
                    start_cooldown(account_id, &e);
                }
                e
            })?;
        if page.items.is_empty() {
            break;
        }
        for l in &page.items {
            if l.started_at_ms > max_started {
                max_started = l.started_at_ms;
            }
        }
        total += crate::store::insert_request_logs(&page.items, account_id);
        match page.next_cursor {
            Some(c) if !c.is_empty() => cursor = Some(c),
            _ => break,
        }
        // 分页之间留间隔：首次全量要拉几十页，避免对服务端形成突发压力。
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    Ok((total, max_started))
}

/// 用某个账号的登录会话拉取逐条日志并落库（cursor 分页），同时聚合到按天表；
/// 返回处理条数。这是唯一的数据通道（Service Key 访问 /request-logs 恒 403）。
pub async fn sync_request_logs(
    account: &crate::accounts::Account,
    full: bool,
    app: Option<&AppHandle>,
) -> Result<usize, String> {
    let left = cooldown_left_secs_for(&account.id);
    if left > 0 {
        write_status(|st| st.source_note = format!("限流冷却中（{left}s）"));
        return Ok(0);
    }
    let provider = AnyProvider::for_id(account.provider);
    let creds = account.credentials();
    let _guard = SYNC_LOCK.lock().await;
    write_status(|st| st.syncing = true);
    let now = chrono::Utc::now().timestamp_millis();
    let since = if full {
        now - 30 * 24 * 3600 * 1000
    } else {
        account_meta("last_requestlog_ms", &account.id)
            .and_then(|s| s.parse::<i64>().ok())
            .map(|v| (v - 60_000).max(0))
            .unwrap_or(now - 24 * 3600 * 1000)
    };

    // 400 常见于 since 超出保留期或协议细节；全量时逐步缩小窗口重试。
    let mut windows: Vec<i64> = vec![since];
    if full {
        windows.push(now - 7 * 24 * 3600 * 1000);
        windows.push(now - 24 * 3600 * 1000);
    }
    let mut total = 0usize;
    let mut max_started = since;
    let mut last_err: Option<String> = None;
    for w in windows {
        match pull_request_log_pages(&provider, &creds, &account.id, w, now).await {
            Ok((n, m)) => {
                total = n;
                max_started = m;
                last_err = None;
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    if let Some(e) = last_err {
        write_status(|st| {
            st.syncing = false;
            st.last_error = Some(format!("{}：{e}", account.display_name()));
        });
        if let Some(app) = app {
            emit(app, "sync-status");
        }
        return Err(e);
    }

    // 聚合成按天行，供日志表 / 汇总使用
    let daily = crate::store::request_logs_to_daily();
    crate::store::upsert_rows(&daily);
    crate::store::set_meta(&meta_key("last_requestlog_ms", &account.id), &(max_started + 1).to_string());
    crate::store::set_meta(&meta_key("last_sync_ms", &account.id), &now.to_string());
    if full {
        crate::store::set_meta(&meta_key("last_full_sync_ms", &account.id), &now.to_string());
        crate::store::set_meta(&meta_key("initialized", &account.id), "1");
        write_status(|st| st.last_full_sync_ms = now);
    }
    // 全局水位（界面展示用）取各账号里最新的
    let newest = crate::store::get_meta("last_sync_ms")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    if now > newest {
        crate::store::set_meta("last_sync_ms", &now.to_string());
    }
    crate::store::prune_request_logs(now - 40 * 24 * 3600 * 1000);

    let (count, cost) = crate::store::stats_summary();
    let (log_count, _, _) = crate::store::request_log_stats();
    write_status(|st| {
        st.syncing = false;
        st.last_sync_ms = now.max(st.last_sync_ms);
        st.last_error = None;
        st.local_rows = count.max(log_count);
        st.total_cost_micro_cents = cost;
        st.source_note = format!("request-logs ×{total}");
        st.last_added = total as i64;
    });
    if let Some(app) = app {
        emit(app, "sync-status");
    }
    Ok(total)
} 

/// 从本地 meta 恢复状态（启动时调用一次）。
pub fn restore_from_db() {
    let (rows, cost) = crate::store::stats_summary();
    let last = crate::store::get_meta("last_sync_ms")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let last_full = crate::store::get_meta("last_full_sync_ms")
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    *STATUS.write().unwrap_or_else(|e| e.into_inner()) = Some(SyncStatus {
        syncing: false,
        last_sync_ms: last,
        last_full_sync_ms: last_full,
        last_error: None,
        local_rows: rows,
        total_cost_micro_cents: cost,
        source_note: String::new(),
        last_added: 0,
    });
}

/// 后台循环：按配置的间隔逐个账号刷新额度 + 增量同步逐条日志。
/// 启动时每个从未全量同步过的账号先做一次全量。
pub fn spawn_loop(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        for acc in crate::accounts::list() {
            if !acc.logged_in() {
                continue;
            }
            let _ = sync_request_logs(&acc, !initialized(&acc.id), Some(&app)).await;
            let _ = refresh_quota(&acc, Some(&app)).await;
        }

        loop {
            let secs = crate::incremental_secs().max(10);
            tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
            for acc in crate::accounts::list() {
                if !acc.logged_in() {
                    continue;
                }
                // 该账号在冷却中则跳过
                if cooldown_left_secs_for(&acc.id) > 0 {
                    continue;
                }
                match refresh_quota(&acc, Some(&app)).await {
                    Ok(_) => {}
                    Err(e) => write_status(|st| {
                        st.last_error = Some(format!("{}：{e}", acc.display_name()))
                    }),
                }
                let _ = sync_request_logs(&acc, false, Some(&app)).await;
            }
            emit(&app, "sync-status");
        }
    });
}
