use axum::extract::Query;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};

const LOG_FILE: &str = "/tmp/tpn-worker.log";
const DEFAULT_LINES: usize = 100;
const MAX_LINES: usize = 1000;

#[derive(Deserialize)]
pub struct LogQuery {
    pub lines: Option<usize>,
}

/// GET /api/logs?lines=N — tail-style log reader
pub async fn get_logs(Query(q): Query<LogQuery>) -> impl IntoResponse {
    let n = q.lines.unwrap_or(DEFAULT_LINES).min(MAX_LINES);

    match std::fs::File::open(LOG_FILE) {
        Ok(file) => {
            let reader = BufReader::new(file);
            let mut ring: VecDeque<String> = VecDeque::with_capacity(n + 1);

            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if ring.len() == n {
                            ring.pop_front();
                        }
                        ring.push_back(l);
                    }
                    Err(_) => break,
                }
            }

            let lines: Vec<String> = ring.into_iter().collect();
            Json(serde_json::json!({
                "success": true,
                "lines": lines,
                "count": lines.len(),
                "file": LOG_FILE,
            }))
        }
        Err(e) => Json(serde_json::json!({
            "success": false,
            "lines": [],
            "count": 0,
            "error": format!("Cannot read log file: {}", e),
        })),
    }
}
