//! 日志批量分发：降低高频日志场景下的 Tauri IPC 调用次数。

use std::collections::HashMap;
use std::sync::{
    mpsc::{self, Receiver, RecvTimeoutError, Sender},
    OnceLock,
};
use std::thread;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter};

use crate::types::{LogBatchEvent, LogEvent};

const LOG_BATCH_INTERVAL: Duration = Duration::from_millis(100);
const LOG_BATCH_SIZE: usize = 100;

struct PendingLog {
    service_id: String,
    event: LogEvent,
}

static LOG_SENDER: OnceLock<Sender<PendingLog>> = OnceLock::new();

/// 把单行日志加入批量分发队列，由后台线程定时合并后发送给前端。
pub fn emit_log(app: &AppHandle, service_id: &str, event: LogEvent) {
    let sender = LOG_SENDER.get_or_init(|| start_dispatcher(app.clone()));
    let pending = PendingLog {
        service_id: service_id.to_string(),
        event,
    };
    if sender.send(pending).is_err() {
        log::error!("日志批量分发线程已退出");
    }
}

fn start_dispatcher(app: AppHandle) -> Sender<PendingLog> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || run_dispatcher(app, receiver));
    sender
}

fn run_dispatcher(app: AppHandle, receiver: Receiver<PendingLog>) {
    let mut batches: HashMap<String, Vec<LogEvent>> = HashMap::new();
    let mut flush_at = Instant::now() + LOG_BATCH_INTERVAL;
    loop {
        let timeout = flush_at.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(timeout) {
            Ok(pending) => push_pending(&app, &mut batches, pending),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                flush_all(&app, &mut batches);
                return;
            }
        }
        if Instant::now() >= flush_at {
            flush_all(&app, &mut batches);
            flush_at = Instant::now() + LOG_BATCH_INTERVAL;
        }
    }
}

fn push_pending(
    app: &AppHandle,
    batches: &mut HashMap<String, Vec<LogEvent>>,
    pending: PendingLog,
) {
    let service_id = pending.service_id;
    let batch_full = {
        let batch = batches.entry(service_id.clone()).or_default();
        batch.push(pending.event);
        batch.len() >= LOG_BATCH_SIZE
    };
    if batch_full {
        flush_service(app, batches, &service_id);
    }
}

fn flush_all(app: &AppHandle, batches: &mut HashMap<String, Vec<LogEvent>>) {
    for (service_id, lines) in batches.drain() {
        emit_batch(app, &service_id, lines);
    }
}

fn flush_service(app: &AppHandle, batches: &mut HashMap<String, Vec<LogEvent>>, service_id: &str) {
    if let Some(lines) = batches.remove(service_id) {
        emit_batch(app, service_id, lines);
    }
}

fn emit_batch(app: &AppHandle, service_id: &str, lines: Vec<LogEvent>) {
    let event_name = format!("log:{service_id}");
    if let Err(error) = app.emit(&event_name, LogBatchEvent { lines }) {
        log::error!("发送服务 {service_id} 日志失败: {error}");
    }
}
