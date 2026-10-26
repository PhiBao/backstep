use rusqlite::params;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::db;
use crate::models::Extraction;
use crate::sam::parse_sam_text;
use crate::AppState;

pub fn sha256_file(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut h = Sha256::new();
            h.update(&bytes);
            hex(&h.finalize())
        }
        Err(_) => String::new(),
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

async fn cmd(args: &[&str]) -> Result<String, anyhow::Error> {
    let out = tokio::process::Command::new(args[0])
        .args(&args[1..])
        .output()
        .await?;
    if !out.status.success() {
        anyhow::bail!(
            "{} failed: {}",
            args[0],
            String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(600)
                .collect::<String>()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub async fn run_ingest(state: AppState, recording_id: String, video_path: PathBuf) {
    let db = state.db.clone();
    let rid = recording_id.clone();
    if let Err(e) = run_inner(state, &rid, &video_path).await {
        db::trace(&db, &rid, "error", "Ingest failed", Some(&e.to_string()));
        db::set_status(&db, &rid, "failed", Some(&e.to_string()));
    }
}

async fn run_inner(state: AppState, rid: &str, video_path: &Path) -> Result<(), anyhow::Error> {
    let db = state.db.clone();
    db::set_status(&db, rid, "extracting", None);
    db::trace(
        &db,
        rid,
        "ffmpeg",
        "Probing video and extracting frames at 1 fps (max 300)",
        None,
    );

    let duration: Option<f64> = cmd(&[
        "ffprobe",
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
        video_path.to_str().unwrap_or_default(),
    ])
    .await
    .ok()
    .and_then(|s| s.trim().parse().ok());

    let frames_dir = state.data_dir.join("recordings").join(rid).join("frames");
    tokio::fs::create_dir_all(&frames_dir).await?;
    let pattern = frames_dir.join("frame-%04d.jpg");
    cmd(&[
        "ffmpeg",
        "-y",
        "-i",
        video_path.to_str().unwrap_or_default(),
        "-vf",
        "fps=1,scale=960:-2",
        "-frames:v",
        "300",
        pattern.to_str().unwrap_or_default(),
    ])
    .await?;

    let mut files: Vec<PathBuf> = Vec::new();
    let mut rd = tokio::fs::read_dir(&frames_dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("jpg") {
            files.push(p);
        }
    }
    files.sort();
    if files.is_empty() {
        anyhow::bail!("ffmpeg produced no frames — is the file a supported video (mp4/mov)?");
    }
    {
        let c = db.lock().unwrap();
        let _ = c.execute("DELETE FROM frames WHERE recording_id=?1", params![rid]);
        for (seq, p) in files.iter().enumerate() {
            let _ = c.execute(
                "INSERT INTO frames(recording_id,seq,ts_sec,path,width,height) VALUES(?1,?2,?3,?4,960,540)",
                params![rid, seq as i64, seq as f64, p.to_string_lossy().to_string()],
            );
        }
        let _ = c.execute(
            "UPDATE recordings SET duration_sec=?1, frame_count=?2 WHERE id=?3",
            params![
                duration.unwrap_or(files.len() as f64),
                files.len() as i64,
                rid
            ],
        );
    }
    db::trace(
        &db,
        rid,
        "ffmpeg",
        &format!("Extracted {} frames", files.len()),
        None,
    );

    if !state.meta.has_key() {
        db::set_status(&db, rid, "awaiting_key", Some("MODEL_API_KEY is not set on the server. Frames were extracted; set the key and re-run analysis to build the memory."));
        db::trace(
            &db,
            rid,
            "meta",
            "MODEL_API_KEY missing — analysis paused (frames are ready)",
            None,
        );
        return Ok(());
    }

    // --- Muse Spark video understanding (cached by content hash) ---
    db::set_status(&db, rid, "analyzing", None);
    let hash = sha256_file(video_path);
    let cache_key = format!("extract:{}:v1", hash);
    let extraction: Extraction = if let Some(cached) = db::cache_get(&db, &cache_key) {
        db::trace(
            &db,
            rid,
            "cache",
            "Reusing cached Muse Spark extraction for this exact video",
            None,
        );
        serde_json::from_str(&cached)?
    } else {
        db::trace(
            &db,
            rid,
            "meta-files",
            "Uploading video to Meta Files API",
            None,
        );
        let file_id = state.meta.upload_file(video_path).await?;
        db::trace(
            &db,
            rid,
            "muse-spark",
            "Analysing video with Muse Spark 1.3 (structured output)",
            Some(&format!("file_id={}", file_id)),
        );
        let ex = state.meta.spark_extract_video(&file_id).await?;
        db::cache_put(&db, &cache_key, &serde_json::to_string(&ex)?);
        ex
    };
    db::trace(
        &db,
        rid,
        "muse-spark",
        &format!(
            "Extracted {} parts and {} steps",
            extraction.parts.len(),
            extraction.steps.len()
        ),
        extraction.title.as_deref(),
    );

    // --- Persist parts & steps ---
    let mut part_ids: Vec<(String, String, Option<i64>, PathBuf)> = Vec::new(); // (id, sam_prompt, evidence_seq, frame_path)
    {
        let c = db.lock().unwrap();
        let _ = c.execute("DELETE FROM parts WHERE recording_id=?1", params![rid]);
        let _ = c.execute("DELETE FROM steps WHERE recording_id=?1", params![rid]);
        let _ = c.execute(
            "DELETE FROM sam_objects WHERE recording_id=?1",
            params![rid],
        );
        if let Some(t) = &extraction.title {
            if !t.trim().is_empty() {
                let _ = c.execute(
                    "UPDATE recordings SET title=?1 WHERE id=?2 AND kind='upload'",
                    params![t, rid],
                );
            }
        }
    }
    for part in extraction.parts.iter().take(8) {
        let pid = uuid::Uuid::new_v4().to_string();
        let ev_sec = part.evidence_sec.or(part.first_seen_sec).unwrap_or(0.0);
        let ev_seq = db::frame_seq_at(&db, rid, ev_sec);
        {
            let c = db.lock().unwrap();
            let _ = c.execute(
                "INSERT INTO parts(id,recording_id,name,sam_prompt,color,first_seen_sec,last_seen_sec,evidence_frame_seq) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![pid, rid, part.name, part.sam_prompt, part.color, part.first_seen_sec, part.last_seen_sec, ev_seq],
            );
        }
        if let Some(seq) = ev_seq {
            if let Some(fp) = db::frame_path(&db, rid, seq) {
                part_ids.push((pid, part.sam_prompt.clone(), Some(seq), PathBuf::from(fp)));
            }
        }
    }
    for (i, step) in extraction.steps.iter().take(20).enumerate() {
        let ev_seq = step
            .evidence_sec
            .and_then(|s| db::frame_seq_at(&db, rid, s));
        let c = db.lock().unwrap();
        let _ = c.execute(
            "INSERT INTO steps(id,recording_id,order_index,start_sec,end_sec,action,part_names,state_before,state_after,evidence_frame_seq) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                uuid::Uuid::new_v4().to_string(), rid, i as i64, step.start_sec, step.end_sec,
                step.action, serde_json::to_string(&step.part_names)?, step.state_before, step.state_after, ev_seq
            ],
        );
    }

    // --- SAM 3.1 segmentation on evidence frames ---
    db::set_status(&db, rid, "segmenting", None);
    for (pid, prompt, ev_seq, frame_path) in part_ids {
        let seq = match ev_seq {
            Some(s) => s,
            None => continue,
        };
        let bytes = match tokio::fs::read(&frame_path).await {
            Ok(b) => b,
            Err(_) => continue,
        };
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let sam_key = format!("sam:{}:{}", hex(&hasher.finalize()), prompt);
        let raw = if let Some(cached) = db::cache_get(&db, &sam_key) {
            cached
        } else {
            db::trace(
                &db,
                rid,
                "sam-3.1",
                &format!("Segmenting \"{}\" on frame {}", prompt, seq),
                None,
            );
            match state.meta.sam_segment_image(&prompt, &bytes).await {
                Ok(r) => {
                    db::cache_put(&db, &sam_key, &r);
                    r
                }
                Err(e) => {
                    db::trace(
                        &db,
                        rid,
                        "sam-3.1",
                        &format!("SAM failed for \"{}\": {}", prompt, e),
                        None,
                    );
                    continue;
                }
            }
        };
        let records = parse_sam_text(&raw);
        if records.is_empty() {
            db::trace(
                &db,
                rid,
                "sam-3.1",
                &format!(
                    "No match for \"{}\" on frame {} (zero matches is a valid result)",
                    prompt, seq
                ),
                None,
            );
            continue;
        }
        let c = db.lock().unwrap();
        for rec in &records {
            let _ = c.execute(
                "INSERT INTO sam_objects(id,recording_id,part_id,concept,frame_seq,sam_frame,object_id,x1,y1,x2,y2,frame_w,frame_h,confidence,mask_data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    uuid::Uuid::new_v4().to_string(), rid, pid, prompt, seq, rec.sam_frame, rec.object_id,
                    rec.x1, rec.y1, rec.x2, rec.y2, rec.frame_w, rec.frame_h, rec.confidence, rec.mask_data
                ],
            );
        }
        db::trace(
            &db,
            rid,
            "sam-3.1",
            &format!(
                "\"{}\" → {} object(s) on frame {}",
                prompt,
                records.len(),
                seq
            ),
            None,
        );
    }

    // --- Reverse manual synthesis (Muse Wonder, Spark fallback) ---
    let steps_now = db::list_steps(&db, rid);
    if !steps_now.is_empty() {
        let schema = json!({
            "type": "object", "additionalProperties": false,
            "properties": {"reverse_steps": {"type": "array", "items": {
                "type": "object", "additionalProperties": false,
                "properties": {"order_index": {"type": "integer"}, "reverse_action": {"type": "string"}},
                "required": ["order_index", "reverse_action"]
            }}},
            "required": ["reverse_steps"]
        });
        let user = format!(
            "These teardown steps were performed in order:\n{}\n\nWrite the reassembly instruction for each step (the physical reverse), one per step, imperative, one sentence each. Keep the same order_index values.",
            steps_now.iter().map(|s| format!("{}. [{}] {}", s.order_index, s.part_names.join(", "), s.action)).collect::<Vec<_>>().join("\n")
        );
        let mut applied = false;
        for model in ["muse-wonder-1.0", "muse-spark-1.3"] {
            match state
                .meta
                .chat_json(
                    model,
                    "You write precise reassembly instructions. Return JSON only.",
                    &user,
                    "reverse_steps",
                    schema.clone(),
                )
                .await
            {
                Ok(v) => {
                    if let Some(arr) = v.get("reverse_steps").and_then(|x| x.as_array()) {
                        let c = db.lock().unwrap();
                        for item in arr {
                            if let (Some(idx), Some(txt)) = (
                                item.get("order_index").and_then(|x| x.as_i64()),
                                item.get("reverse_action").and_then(|x| x.as_str()),
                            ) {
                                let _ = c.execute("UPDATE steps SET reverse_action=?1 WHERE recording_id=?2 AND order_index=?3", params![txt, rid, idx]);
                            }
                        }
                        applied = true;
                        db::trace(
                            &db,
                            rid,
                            "synthesis",
                            &format!("Reverse manual written by {}", model),
                            None,
                        );
                        break;
                    }
                }
                Err(e) => {
                    db::trace(
                        &db,
                        rid,
                        "synthesis",
                        &format!("{} reverse synthesis failed: {}", model, e),
                        None,
                    );
                }
            }
        }
        if !applied {
            // Deterministic fallback, clearly a template — never presented as model output.
            let c = db.lock().unwrap();
            for s in &steps_now {
                let _ = c.execute(
                    "UPDATE steps SET reverse_action=?1 WHERE recording_id=?2 AND order_index=?3",
                    params![
                        format!("Reverse this teardown step: {}", s.action),
                        rid,
                        s.order_index
                    ],
                );
            }
            db::trace(
                &db,
                rid,
                "synthesis",
                "Reverse manual used the deterministic fallback template",
                None,
            );
        }
    }

    db::set_status(&db, rid, "ready", None);
    db::trace(
        &db,
        rid,
        "done",
        "Memory built — Manual and Finder are ready",
        None,
    );
    Ok(())
}

/// Re-run analysis for a recording whose video already exists (e.g. after key arrives).
pub async fn rerun(state: AppState, recording_id: String) {
    let path = {
        let c = match state.db.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        c.query_row(
            "SELECT video_path FROM recordings WHERE id=?1",
            params![recording_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    };
    if let Some(p) = path {
        run_ingest(state, recording_id, PathBuf::from(p)).await;
    }
}
