mod db;
mod ingest;
mod meta;
mod models;
mod sam;
mod sample;

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;

use db::Db;
use meta::MetaClient;
use models::*;

#[derive(Clone)]
pub struct AppState {
    db: Db,
    meta: MetaClient,
    data_dir: PathBuf,
}

fn err(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({"error": msg}))).into_response()
}

async fn health(State(s): State<AppState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "service": "backstep",
        "meta_api_configured": s.meta.has_key(),
        "meta_base_url": s.meta.base,
    }))
}

async fn list_recordings(State(s): State<AppState>) -> Json<Value> {
    Json(json!({"recordings": db::list_recordings(&s.db)}))
}

async fn get_recording(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    match db::get_recording(&s.db, &id) {
        Some(rec) => Json(json!({
            "recording": rec,
            "frames": db::list_frames(&s.db, &id),
            "parts": db::list_parts(&s.db, &id),
            "steps": db::list_steps(&s.db, &id),
            "objects": db::list_objects(&s.db, &id),
            "trace": db::list_trace(&s.db, &id),
        }))
        .into_response(),
        None => err(StatusCode::NOT_FOUND, "recording not found"),
    }
}

async fn get_frame(State(s): State<AppState>, Path((id, seq)): Path<(String, i64)>) -> Response {
    let path = match db::frame_path(&s.db, &id, seq) {
        Some(p) => p,
        None => return err(StatusCode::NOT_FOUND, "frame not found"),
    };
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(_) => return err(StatusCode::NOT_FOUND, "frame file missing"),
    };
    let ctype = if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".png") {
        "image/png"
    } else {
        "image/jpeg"
    };
    Response::builder()
        .header(header::CONTENT_TYPE, ctype)
        .header(header::CACHE_CONTROL, "public, max-age=3600")
        .body(Body::from(bytes))
        .unwrap()
        .into_response()
}

async fn upload(State(s): State<AppState>, mut mp: Multipart) -> Response {
    let mut title: Option<String> = None;
    let mut file_bytes: Option<Vec<u8>> = None;
    let mut filename = "recording.mp4".to_string();
    while let Ok(Some(field)) = mp.next_field().await {
        match field.name() {
            Some("title") => {
                title = field.text().await.ok().filter(|t| !t.trim().is_empty());
            }
            Some("file") => {
                if let Some(f) = field.file_name() {
                    filename = f.to_string();
                }
                match field.bytes().await {
                    Ok(b) => file_bytes = Some(b.to_vec()),
                    Err(e) => {
                        return err(
                            StatusCode::BAD_REQUEST,
                            &format!("upload read failed: {}", e),
                        )
                    }
                }
            }
            _ => {}
        }
    }
    let bytes = match file_bytes {
        Some(b) if !b.is_empty() => b,
        _ => return err(StatusCode::BAD_REQUEST, "no file field in upload"),
    };
    if bytes.len() > 200 * 1024 * 1024 {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "file exceeds 200 MB limit");
    }
    let lower = filename.to_lowercase();
    if !(lower.ends_with(".mp4") || lower.ends_with(".mov")) {
        return err(
            StatusCode::BAD_REQUEST,
            "only .mp4 or .mov recordings are supported",
        );
    }
    let id = uuid::Uuid::new_v4().to_string();
    let dir = s.data_dir.join("recordings").join(&id);
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "could not create recording dir",
        );
    }
    let ext = if lower.ends_with(".mov") {
        "mov"
    } else {
        "mp4"
    };
    let video_path = dir.join(format!("original.{}", ext));
    if tokio::fs::write(&video_path, &bytes).await.is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "could not store upload");
    }
    {
        let c = match s.db.lock() {
            Ok(c) => c,
            Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "db lock"),
        };
        let _ = c.execute(
            "INSERT INTO recordings(id,title,kind,status,created_at,video_path) VALUES(?1,?2,'upload','created',?3,?4)",
            params![id, title.clone().unwrap_or_else(|| "Untitled teardown".to_string()), chrono::Utc::now().to_rfc3339(), video_path.to_string_lossy().to_string()],
        );
    }
    db::trace(
        &s.db,
        &id,
        "upload",
        &format!("Received {} ({} bytes)", filename, bytes.len()),
        None,
    );
    let state = s.clone();
    let rid = id.clone();
    tokio::spawn(async move { ingest::run_ingest(state, rid, video_path).await });
    match db::get_recording(&s.db, &id) {
        Some(rec) => (StatusCode::CREATED, Json(json!({"recording": rec}))).into_response(),
        None => err(StatusCode::INTERNAL_SERVER_ERROR, "recording vanished"),
    }
}

async fn rerun(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    if db::get_recording(&s.db, &id).is_none() {
        return err(StatusCode::NOT_FOUND, "recording not found");
    }
    let state = s.clone();
    tokio::spawn(async move { ingest::rerun(state, id).await });
    Json(json!({"ok": true, "message": "analysis re-started"})).into_response()
}

async fn delete_recording(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    if id == sample::SAMPLE_ID {
        return err(
            StatusCode::BAD_REQUEST,
            "the sample memory cannot be deleted",
        );
    }
    {
        let c = match s.db.lock() {
            Ok(c) => c,
            Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "db lock"),
        };
        let _ = c.execute("DELETE FROM frames WHERE recording_id=?1", params![id]);
        let _ = c.execute("DELETE FROM parts WHERE recording_id=?1", params![id]);
        let _ = c.execute("DELETE FROM steps WHERE recording_id=?1", params![id]);
        let _ = c.execute("DELETE FROM sam_objects WHERE recording_id=?1", params![id]);
        let _ = c.execute(
            "DELETE FROM trace_events WHERE recording_id=?1",
            params![id],
        );
        let _ = c.execute("DELETE FROM recordings WHERE id=?1", params![id]);
    }
    let _ = tokio::fs::remove_dir_all(s.data_dir.join("recordings").join(&id)).await;
    Json(json!({"ok": true})).into_response()
}

async fn manual(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    if db::get_recording(&s.db, &id).is_none() {
        return err(StatusCode::NOT_FOUND, "recording not found");
    }
    let mut steps = db::list_steps(&s.db, &id);
    steps.sort_by_key(|st| std::cmp::Reverse(st.order_index));
    let manual_steps: Vec<Value> = steps
        .iter()
        .enumerate()
        .map(|(i, st)| {
            json!({
                "manual_order": i + 1,
                "step_id": st.id,
                "instruction": st.reverse_action.clone().unwrap_or_else(|| format!("Reverse this teardown step: {}", st.action)),
                "original_action": st.action,
                "part_names": st.part_names,
                "state_target": st.state_before,
                "evidence_frame_seq": st.evidence_frame_seq,
                "citation": {
                    "type": "step", "id": st.id,
                    "frame_seq": st.evidence_frame_seq,
                    "ts_sec": frame_ts(&s.db, &id, st.evidence_frame_seq),
                    "label": format!("Teardown step {} · {}", st.order_index + 1, st.action)
                }
            })
        })
        .collect();
    Json(json!({"recording_id": id, "manual": manual_steps})).into_response()
}

const STOPWORDS: &[&str] = &[
    "the", "what", "when", "where", "who", "how", "why", "did", "does", "was", "were", "are", "is",
    "put", "my", "me", "it", "this", "that", "with", "and", "for", "from", "into", "happened",
    "done", "back", "together", "was", "were", "been", "have", "has", "had", "you", "your",
];

fn tokens(q: &str) -> Vec<String> {
    q.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2 && !STOPWORDS.contains(t))
        .map(|t| t.to_string())
        .collect()
}

fn score(text: &str, toks: &[String]) -> i64 {
    let lower = text.to_lowercase();
    toks.iter().filter(|t| lower.contains(t.as_str())).count() as i64
}

fn frame_ts(db: &Db, recording_id: &str, seq: Option<i64>) -> Option<f64> {
    seq.and_then(|sq| {
        db::list_frames(db, recording_id)
            .into_iter()
            .find(|f| f.seq == sq)
            .map(|f| f.ts_sec)
    })
}

async fn ask(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<AskRequest>,
) -> Response {
    if db::get_recording(&s.db, &id).is_none() {
        return err(StatusCode::NOT_FOUND, "recording not found");
    }
    let q = body.question.trim().to_string();
    if q.is_empty() {
        return err(StatusCode::BAD_REQUEST, "question is empty");
    }
    let parts = db::list_parts(&s.db, &id);
    let steps = db::list_steps(&s.db, &id);
    let objects = db::list_objects(&s.db, &id);
    let toks = tokens(&q);
    let lower_q = q.to_lowercase();

    // --- Part Finder: "where did I put / where is the X" ---
    let best_part = parts
        .iter()
        .map(|p| {
            (
                score(
                    &format!(
                        "{} {} {}",
                        p.name,
                        p.sam_prompt,
                        p.color.clone().unwrap_or_default()
                    ),
                    &toks,
                ),
                p,
            )
        })
        .max_by_key(|(sc, _)| *sc);

    if lower_q.contains("where") || lower_q.contains("last seen") || lower_q.contains("put") {
        if let Some((sc, part)) = best_part {
            if sc > 0 {
                let obj = objects
                    .iter()
                    .filter(|o| o.part_id.as_deref() == Some(part.id.as_str()))
                    .find(|o| Some(o.frame_seq) == part.evidence_frame_seq)
                    .or_else(|| {
                        objects
                            .iter()
                            .filter(|o| o.part_id.as_deref() == Some(part.id.as_str()))
                            .max_by_key(|o| o.frame_seq)
                    });
                let frame_seq = obj.map(|o| o.frame_seq).or(part.evidence_frame_seq);
                let ts = obj
                    .and_then(|_| frame_ts(&s.db, &id, frame_seq))
                    .or(part.last_seen_sec);
                let mut citations = vec![Citation {
                    kind: "object".to_string(),
                    id: obj.map(|o| o.id.clone()).unwrap_or_else(|| part.id.clone()),
                    frame_seq,
                    ts_sec: ts,
                    label: format!("{} — evidence frame", part.name),
                }];
                let mut sightings: Vec<&SamObject> = objects
                    .iter()
                    .filter(|o| o.part_id.as_deref() == Some(part.id.as_str()))
                    .collect();
                sightings.sort_by_key(|o| o.frame_seq);
                let sighting_count = sightings
                    .iter()
                    .map(|o| o.frame_seq)
                    .collect::<HashSet<i64>>()
                    .len();
                let mut cited_frames: HashSet<i64> = frame_seq.into_iter().collect();
                for o in sightings
                    .iter()
                    .filter(|o| Some(o.frame_seq) != frame_seq && cited_frames.insert(o.frame_seq))
                    .take(5)
                {
                    citations.push(Citation {
                        kind: "object".to_string(),
                        id: o.id.clone(),
                        frame_seq: Some(o.frame_seq),
                        ts_sec: frame_ts(&s.db, &id, Some(o.frame_seq)),
                        label: format!("{} — sighting", part.name),
                    });
                }
                let has_box = obj.is_some();
                let sighting_note = if sighting_count > 1 {
                    format!(
                        " It appears in {} tracked frames — every sighting is cited below.",
                        sighting_count
                    )
                } else {
                    String::new()
                };
                return Json(AskResponse {
                    answer: format!(
                        "The {} was last clearly seen at {} — frame {} in this recording.{}{}",
                        part.name,
                        ts.map(|t| format!("{:.0}s", t))
                            .unwrap_or_else(|| "an unknown time".into()),
                        frame_seq
                            .map(|f| f.to_string())
                            .unwrap_or_else(|| "?".into()),
                        if has_box {
                            " Its tracked region is highlighted on that frame."
                        } else {
                            ""
                        },
                        sighting_note
                    ),
                    citations,
                    verified: true,
                    insufficient_evidence: false,
                    source: "index".to_string(),
                })
                .into_response();
            }
        }
    }

    // --- Manual summary: "how does this go back together" ---
    if lower_q.contains("back together")
        || lower_q.contains("reassemble")
        || lower_q.contains("reassembly")
        || lower_q.contains("put back")
    {
        if steps.is_empty() {
            return Json(AskResponse {
                answer: "This recording has no extracted steps yet, so there is no reassembly order to replay.".into(),
                citations: vec![], verified: false, insufficient_evidence: true, source: "index".into(),
            })
            .into_response();
        }
        let mut rev: Vec<&Step> = steps.iter().collect();
        rev.sort_by_key(|st| std::cmp::Reverse(st.order_index));
        let lines: Vec<String> = rev
            .iter()
            .enumerate()
            .map(|(i, st)| {
                format!(
                    "{}. {}",
                    i + 1,
                    st.reverse_action
                        .clone()
                        .unwrap_or_else(|| format!("Reverse: {}", st.action))
                )
            })
            .collect();
        let citations: Vec<Citation> = rev
            .iter()
            .map(|st| Citation {
                kind: "step".into(),
                id: st.id.clone(),
                frame_seq: st.evidence_frame_seq,
                ts_sec: frame_ts(&s.db, &id, st.evidence_frame_seq),
                label: format!("Step {} · {}", st.order_index + 1, st.action),
            })
            .collect();
        return Json(AskResponse {
            answer: format!(
                "Reassembly is the teardown in reverse:\n{}",
                lines.join("\n")
            ),
            citations,
            verified: true,
            insufficient_evidence: false,
            source: "index".into(),
        })
        .into_response();
    }

    // If the model cannot answer but the deterministic matcher can, prefer
    // the grounded answer: model refusals fall through instead of returning.
    let mut model_refusal: Option<String> = None;
    // --- Model synthesis over the index, with deterministic citation validation ---
    if s.meta.has_key() {
        let index = json!({
            "parts": parts.iter().map(|p| json!({"id": p.id, "name": p.name, "last_seen_sec": p.last_seen_sec, "evidence_frame_seq": p.evidence_frame_seq})).collect::<Vec<_>>(),
            "steps": steps.iter().map(|st| json!({"id": st.id, "order_index": st.order_index, "action": st.action, "reverse_action": st.reverse_action, "part_names": st.part_names, "evidence_frame_seq": st.evidence_frame_seq})).collect::<Vec<_>>(),
            "objects": objects.iter().map(|o| json!({"id": o.id, "concept": o.concept, "frame_seq": o.frame_seq})).collect::<Vec<_>>()
        });
        let schema = json!({
            "type": "object", "additionalProperties": false,
            "properties": {
                "answer": {"type": "string"},
                "citation_ids": {"type": "array", "items": {"type": "string"}},
                "insufficient_evidence": {"type": "boolean"}
            },
            "required": ["answer", "citation_ids", "insufficient_evidence"]
        });
        let valid_ids: HashSet<String> = parts
            .iter()
            .map(|p| p.id.clone())
            .chain(steps.iter().map(|st| st.id.clone()))
            .chain(objects.iter().map(|o| o.id.clone()))
            .collect();
        let system = "You answer questions about one hands-on recording using ONLY the provided memory index. Every factual claim must be backed by citation_ids taken verbatim from the index ids. If the index does not contain the answer, set insufficient_evidence to true and say what is missing. Never invent ids, times, or parts.";
        let user = format!("Memory index:\n{}\n\nQuestion: {}", index, q);
        // If the model cannot answer but the deterministic matcher can, prefer
        // the grounded answer: model refusals fall through instead of returning.
        for model in ["muse-wonder-1.0", "muse-spark-1.3"] {
            if let Ok(v) = s
                .meta
                .chat_json(model, system, &user, "grounded_answer", schema.clone())
                .await
            {
                let insufficient = v
                    .get("insufficient_evidence")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                let answer_text = v
                    .get("answer")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let raw_ids: Vec<String> = v
                    .get("citation_ids")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                // Deterministic validation: only ids that exist in the index survive.
                let mut citations: Vec<Citation> = Vec::new();
                for cid in raw_ids.iter().filter(|cid| valid_ids.contains(*cid)) {
                    if let Some(p) = parts.iter().find(|p| &p.id == cid) {
                        citations.push(Citation {
                            kind: "part".into(),
                            id: p.id.clone(),
                            frame_seq: p.evidence_frame_seq,
                            ts_sec: frame_ts(&s.db, &id, p.evidence_frame_seq),
                            label: p.name.clone(),
                        });
                    } else if let Some(st) = steps.iter().find(|st| &st.id == cid) {
                        citations.push(Citation {
                            kind: "step".into(),
                            id: st.id.clone(),
                            frame_seq: st.evidence_frame_seq,
                            ts_sec: frame_ts(&s.db, &id, st.evidence_frame_seq),
                            label: format!("Step {} · {}", st.order_index + 1, st.action),
                        });
                    } else if let Some(o) = objects.iter().find(|o| &o.id == cid) {
                        citations.push(Citation {
                            kind: "object".into(),
                            id: o.id.clone(),
                            frame_seq: Some(o.frame_seq),
                            ts_sec: frame_ts(&s.db, &id, Some(o.frame_seq)),
                            label: o.concept.clone(),
                        });
                    }
                }
                if insufficient || citations.is_empty() {
                    if insufficient && !answer_text.is_empty() && model_refusal.is_none() {
                        model_refusal = Some(format!("{} (source: {})", answer_text, model));
                    }
                    continue;
                }
                return Json(AskResponse {
                    answer: answer_text,
                    citations,
                    verified: true,
                    insufficient_evidence: false,
                    source: model.to_string(),
                })
                .into_response();
            }
        }
        // fall through to deterministic retrieval if both models fail
    }

    // --- Deterministic retrieval fallback (no model required) ---
    let best_step = steps
        .iter()
        .map(|st| {
            (
                score(&format!("{} {}", st.action, st.part_names.join(" ")), &toks),
                st,
            )
        })
        .max_by_key(|(sc, _)| *sc);
    if let Some((sc, st)) = best_step {
        if sc > 0 {
            return Json(AskResponse {
                answer: format!(
                    "From this recording: {} ({}–{}s).",
                    st.action, st.start_sec as i64, st.end_sec as i64
                ),
                citations: vec![Citation {
                    kind: "step".into(),
                    id: st.id.clone(),
                    frame_seq: st.evidence_frame_seq,
                    ts_sec: frame_ts(&s.db, &id, st.evidence_frame_seq),
                    label: format!("Step {} · {}", st.order_index + 1, st.action),
                }],
                verified: true,
                insufficient_evidence: false,
                source: "index".into(),
            })
            .into_response();
        }
    }
    Json(AskResponse {
        answer: model_refusal.unwrap_or_else(|| "I don't have verified evidence for that in this recording. Ask about a named part (\"where is the …?\") or the reassembly order (\"how does this go back together?\").".into()),
        citations: vec![], verified: false, insufficient_evidence: true, source: "index".into(),
    })
    .into_response()
}

/// Check-my-work: compare a user's photo against a reassembly step's
/// target state (Muse Spark image understanding). Advisory verdict only.
async fn check_step(
    State(s): State<AppState>,
    Path(id): Path<String>,
    mut mp: Multipart,
) -> Response {
    if db::get_recording(&s.db, &id).is_none() {
        return err(StatusCode::NOT_FOUND, "recording not found");
    }
    let mut step_id: Option<String> = None;
    let mut file_bytes: Option<Vec<u8>> = None;
    while let Ok(Some(field)) = mp.next_field().await {
        match field.name() {
            Some("step_id") => step_id = field.text().await.ok(),
            Some("file") => {
                if let Ok(b) = field.bytes().await {
                    file_bytes = Some(b.to_vec());
                }
            }
            _ => {}
        }
    }
    let step_id = match step_id {
        Some(v) if !v.trim().is_empty() => v,
        _ => return err(StatusCode::BAD_REQUEST, "step_id is required"),
    };
    let bytes = match file_bytes {
        Some(b) if !b.is_empty() => b,
        _ => return err(StatusCode::BAD_REQUEST, "a photo file is required"),
    };
    if bytes.len() > 20 * 1024 * 1024 {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "photo exceeds 20 MB");
    }
    let step = db::list_steps(&s.db, &id)
        .into_iter()
        .find(|st| st.id == step_id);
    let step = match step {
        Some(st) => st,
        None => return err(StatusCode::NOT_FOUND, "step not found in this recording"),
    };
    if !s.meta.has_key() {
        return err(
            StatusCode::SERVICE_UNAVAILABLE,
            "MODEL_API_KEY is not set — check-my-work needs the Meta API",
        );
    }
    let instruction = step
        .reverse_action
        .clone()
        .unwrap_or_else(|| format!("Reverse: {}", step.action));
    let target = step
        .state_before
        .clone()
        .or(step.state_after.clone())
        .unwrap_or_else(|| instruction.clone());
    match s
        .meta
        .check_step_photo(&bytes, &instruction, &target, &step.action)
        .await
    {
        Ok(v) => {
            let verdict = v
                .get("verdict")
                .and_then(|x| x.as_str())
                .unwrap_or("unsure")
                .to_string();
            db::trace(
                &s.db,
                &id,
                "guide",
                &format!(
                    "Check-my-work on step {}: {}",
                    step.order_index + 1,
                    verdict
                ),
                None,
            );
            Json(json!({
                "step_id": step.id,
                "verdict": verdict,
                "explanation": v.get("explanation").and_then(|x| x.as_str()).unwrap_or(""),
                "confidence": v.get("confidence").and_then(|x| x.as_f64()).unwrap_or(0.0),
            }))
            .into_response()
        }
        Err(e) => err(StatusCode::BAD_GATEWAY, &format!("check failed: {}", e)),
    }
}

/// Mark an object with a hand-drawn box on one frame ("click to track").
///
/// The box is given in the frame's own pixels. SAM 3.1 segments the marked
/// object and every match is stored as a SamObject linked to the part, which
/// makes it a first-class citation for the Finder and the overlays.
async fn track_object(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<TrackRequest>,
) -> Response {
    let rec = match db::get_recording(&s.db, &id) {
        Some(r) => r,
        None => return err(StatusCode::NOT_FOUND, "recording not found"),
    };
    let part = db::list_parts(&s.db, &id)
        .into_iter()
        .find(|p| p.id == body.part_id);
    let part = match part {
        Some(p) => p,
        None => return err(StatusCode::NOT_FOUND, "part not found in this recording"),
    };
    let frame = db::list_frames(&s.db, &id)
        .into_iter()
        .find(|f| f.seq == body.frame_seq);
    let frame = match frame {
        Some(f) => f,
        None => return err(StatusCode::NOT_FOUND, "frame not found"),
    };
    let (w, h) = if rec.frame_w > 0 && rec.frame_h > 0 {
        (rec.frame_w, rec.frame_h)
    } else {
        (frame.width, frame.height)
    };
    if body.x1 < 0
        || body.y1 < 0
        || body.x2 <= body.x1
        || body.y2 <= body.y1
        || body.x2 > w
        || body.y2 > h
    {
        return err(
            StatusCode::BAD_REQUEST,
            &format!(
                "box [{},{},{},{}] is outside the {}x{} frame",
                body.x1, body.y1, body.x2, body.y2, w, h
            ),
        );
    }
    let area = (body.x2 - body.x1) * (body.y2 - body.y1);
    if area < 25 {
        return err(
            StatusCode::BAD_REQUEST,
            "box is too small — drag a larger region",
        );
    }
    if area * 10 > w * h * 9 {
        return err(
            StatusCode::BAD_REQUEST,
            "box covers nearly the whole frame — mark the part, not the scene",
        );
    }
    // frame.url is the API route; the file path lives in the db.
    let bytes = match db::frame_path(&s.db, &id, body.frame_seq) {
        Some(p) => match tokio::fs::read(&p).await {
            Ok(b) => b,
            Err(_) => return err(StatusCode::NOT_FOUND, "frame file missing"),
        },
        None => return err(StatusCode::NOT_FOUND, "frame file missing"),
    };
    if !s.meta.has_key() {
        return err(
            StatusCode::SERVICE_UNAVAILABLE,
            "MODEL_API_KEY is not set — live tracking needs the Meta API",
        );
    }
    // Single-image requests always prompt on frame 0 (SAM rejects <Nf> for
    // images); the recording frame is tracked in our own frame_seq column.
    let prompt = format!(
        "<0f>1<|box;x1={};y1={};x2={};y2={};w={};h={}|>",
        body.x1, body.y1, body.x2, body.y2, w, h
    );
    db::trace(
        &s.db,
        &id,
        "sam-3.1",
        &format!(
            "Tracking \"{}\" from a hand-drawn box on frame {}",
            part.name, frame.seq
        ),
        None,
    );
    let raw = match s.meta.sam_segment_image(&prompt, &bytes).await {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_GATEWAY, &format!("SAM 3.1 failed: {}", e)),
    };
    let records = crate::sam::parse_sam_text(&raw);
    if records.is_empty() {
        db::trace(
            &s.db,
            &id,
            "sam-3.1",
            "Hand-drawn box produced no mask",
            None,
        );
        return Json(json!({"objects": [], "message": "SAM returned no mask for that box — try a tighter box on the part"}))
            .into_response();
    }
    let mut out = Vec::new();
    {
        let c = match s.db.lock() {
            Ok(c) => c,
            Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "db lock"),
        };
        for rec in &records {
            let oid = uuid::Uuid::new_v4().to_string();
            let _ = c.execute(
                "INSERT INTO sam_objects(id,recording_id,part_id,concept,frame_seq,sam_frame,object_id,x1,y1,x2,y2,frame_w,frame_h,confidence,mask_data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    oid, id, part.id, format!("{} (tracked)", part.name), frame.seq,
                    rec.sam_frame, rec.object_id, rec.x1, rec.y1, rec.x2, rec.y2,
                    rec.frame_w, rec.frame_h, rec.confidence, rec.mask_data
                ],
            );
            out.push(json!({
                "id": oid, "concept": format!("{} (tracked)", part.name),
                "frame_seq": frame.seq, "object_id": rec.object_id,
                "x1": rec.x1, "y1": rec.y1, "x2": rec.x2, "y2": rec.y2,
                "frame_w": rec.frame_w, "frame_h": rec.frame_h,
                "confidence": rec.confidence,
            }));
        }
    }
    db::trace(
        &s.db,
        &id,
        "sam-3.1",
        &format!(
            "Hand-drawn box \"{}\" → {} object(s) on frame {}",
            part.name,
            out.len(),
            frame.seq
        ),
        None,
    );
    Json(json!({"objects": out})).into_response()
}

async fn rename_part(
    State(s): State<AppState>,
    Path(part_id): Path<String>,
    Json(body): Json<RenamePart>,
) -> Response {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return err(StatusCode::BAD_REQUEST, "name is empty");
    }
    let rec_id: Option<String> = {
        let c = match s.db.lock() {
            Ok(c) => c,
            Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "db lock"),
        };
        let r = c
            .query_row(
                "SELECT recording_id FROM parts WHERE id=?1",
                params![part_id],
                |row| row.get(0),
            )
            .ok();
        let _ = c.execute(
            "UPDATE parts SET name=?1, confirmed=1 WHERE id=?2",
            params![name, part_id],
        );
        r
    };
    if let Some(rid) = rec_id {
        db::trace(
            &s.db,
            &rid,
            "correction",
            &format!("Part renamed to \"{}\" and confirmed by the user", name),
            Some(&part_id),
        );
        Json(json!({"ok": true})).into_response()
    } else {
        err(StatusCode::NOT_FOUND, "part not found")
    }
}

#[tokio::main]
async fn main() {
    let data_dir =
        PathBuf::from(std::env::var("BACKSTEP_DATA_DIR").unwrap_or_else(|_| "../data".to_string()));
    let _ = std::fs::create_dir_all(&data_dir);
    let db = db::open(&data_dir.join("backstep.db")).expect("open db");
    sample::seed_sample(&db, &data_dir);

    let meta = MetaClient::new(
        std::env::var("META_BASE_URL").unwrap_or_else(|_| "https://api.meta.ai/v1".to_string()),
        std::env::var("MODEL_API_KEY").ok(),
    );
    if !meta.has_key() {
        eprintln!("[backstep] MODEL_API_KEY not set — live Meta analysis disabled; sample memory available");
    }
    let state = AppState { db, meta, data_dir };
    let app = Router::new()
        .route("/health", get(health))
        .route("/api/recordings", get(list_recordings).post(upload))
        .route(
            "/api/recordings/{id}",
            get(get_recording).delete(delete_recording),
        )
        .route("/api/recordings/{id}/frames/{seq}", get(get_frame))
        .route("/api/recordings/{id}/manual", get(manual))
        .route("/api/recordings/{id}/ask", post(ask))
        .route("/api/recordings/{id}/rerun", post(rerun))
        .route("/api/parts/{part_id}", post(rename_part))
        .route("/api/recordings/{id}/track", post(track_object))
        .route("/api/recordings/{id}/check", post(check_step))
        .layer(tower_http::cors::CorsLayer::permissive())
        .layer(DefaultBodyLimit::disable())
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8787);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("bind");
    // 0.0.0.0: Fly's proxy reaches the machine on its external interface.
    println!("[backstep] listening on port {}", port);
    axum::serve(listener, app).await.expect("serve");
}
