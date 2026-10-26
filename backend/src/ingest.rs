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

    // --- Normalize: 720p H.264 + faststart working copy. The Meta APIs bill
    // per frame/token and only accept MP4/MOV H.264/H.265/MPEG-4, so a 4K
    // phone original is transcoded once here. Everything downstream
    // (frames, Files upload, Spark, SAM) uses the normalized copy.
    let normalized = video_path
        .parent()
        .map(|d| d.join("normalized.mp4"))
        .unwrap_or_else(|| PathBuf::from("normalized.mp4"));
    let normalized_ok = cmd(&[
        "ffmpeg",
        "-y",
        "-i",
        video_path.to_str().unwrap_or_default(),
        "-vf",
        "scale=1280:-2",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "23",
        "-c:a",
        "aac",
        "-movflags",
        "+faststart",
        normalized.to_str().unwrap_or_default(),
    ])
    .await
    .is_ok()
        && normalized.exists();
    let work_path = if normalized_ok {
        &normalized
    } else {
        video_path
    };
    if normalized_ok {
        db::trace(
            &db,
            rid,
            "ffmpeg",
            "Normalized to 720p H.264 (faststart) for analysis",
            None,
        );
    } else {
        db::trace(
            &db,
            rid,
            "ffmpeg",
            "Normalization skipped — analysing the original file",
            None,
        );
    }

    let duration: Option<f64> = cmd(&[
        "ffprobe",
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
        work_path.to_str().unwrap_or_default(),
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
        work_path.to_str().unwrap_or_default(),
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
    // Cache on the ORIGINAL bytes: identical uploads share one extraction
    // even if normalization settings change later.
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
        eprintln!("[ingest {}] uploading to Meta Files API...", rid);
        let file_id = state.meta.upload_file(work_path).await?;
        eprintln!("[ingest {}] upload done: {}", rid, file_id);
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
        {
            let c = db.lock().unwrap();
            let _ = c.execute(
                "INSERT INTO steps(id,recording_id,order_index,start_sec,end_sec,action,part_names,state_before,state_after,evidence_frame_seq) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    uuid::Uuid::new_v4().to_string(), rid, i as i64, step.start_sec, step.end_sec,
                    step.action, serde_json::to_string(&step.part_names)?, step.state_before, step.state_after, ev_seq
                ],
            );
        }
    }

    // --- SAM 3.1 segmentation on evidence frames ---
    db::set_status(&db, rid, "segmenting", None);
    for (pid, prompt, ev_seq, frame_path) in &part_ids {
        let seq = match ev_seq {
            Some(s) => *s,
            None => continue,
        };
        segment_frame(&state, &db, rid, pid, prompt, seq, frame_path).await?;
    }

    // --- Retry zero-match parts with simplified prompts on nearby frames ---
    retry_zero_match_parts(&state, &db, rid).await;

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
                        {
                            let c = db.lock().unwrap();
                            for item in arr {
                                if let (Some(idx), Some(txt)) = (
                                    item.get("order_index").and_then(|x| x.as_i64()),
                                    item.get("reverse_action").and_then(|x| x.as_str()),
                                ) {
                                    let _ = c.execute("UPDATE steps SET reverse_action=?1 WHERE recording_id=?2 AND order_index=?3", params![txt, rid, idx]);
                                }
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
            {
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

/// Segment one prompt on one frame; insert matches linked to `part_id`.
/// Returns the number of objects inserted. Cached by (frame bytes, prompt).
async fn segment_frame(
    state: &AppState,
    db: &db::Db,
    rid: &str,
    part_id: &str,
    prompt: &str,
    seq: i64,
    frame_path: &Path,
) -> Result<usize, anyhow::Error> {
    let bytes = tokio::fs::read(frame_path)
        .await
        .map_err(|e| anyhow::anyhow!("frame {} unreadable: {}", seq, e))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let sam_key = format!("sam:{}:{}", hex(&hasher.finalize()), prompt);
    let raw = if let Some(cached) = db::cache_get(db, &sam_key) {
        cached
    } else {
        db::trace(
            db,
            rid,
            "sam-3.1",
            &format!("Segmenting \"{}\" on frame {}", prompt, seq),
            None,
        );
        match state.meta.sam_segment_image(prompt, &bytes).await {
            Ok(r) => {
                db::cache_put(db, &sam_key, &r);
                r
            }
            Err(e) => {
                db::trace(
                    db,
                    rid,
                    "sam-3.1",
                    &format!("SAM failed for \"{}\": {}", prompt, e),
                    None,
                );
                return Ok(0);
            }
        }
    };
    let records = parse_sam_text(&raw);
    if records.is_empty() {
        db::trace(
            db,
            rid,
            "sam-3.1",
            &format!(
                "No match for \"{}\" on frame {} (zero matches is a valid result)",
                prompt, seq
            ),
            None,
        );
        return Ok(0);
    }
    {
        let c = db.lock().unwrap();
        for rec in &records {
            let _ = c.execute(
                "INSERT INTO sam_objects(id,recording_id,part_id,concept,frame_seq,sam_frame,object_id,x1,y1,x2,y2,frame_w,frame_h,confidence,mask_data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    uuid::Uuid::new_v4().to_string(), rid, part_id, prompt, seq, rec.sam_frame, rec.object_id,
                    rec.x1, rec.y1, rec.x2, rec.y2, rec.frame_w, rec.frame_h, rec.confidence, rec.mask_data
                ],
            );
        }
    }
    db::trace(
        db,
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
    Ok(records.len())
}

/// SAM is weak on fine-grained phrasing ("small black screw"). For parts with
/// zero objects, ask the model for simpler variants ("screw", "black screw")
/// and try them on the evidence frame plus neighbours. Bounded: max 4 parts,
/// max 2 variants each, max 3 frames each, stop at first success per part.
async fn retry_zero_match_parts(state: &AppState, db: &db::Db, rid: &str) {
    let parts = db::list_parts(db, rid);
    let objects = db::list_objects(db, rid);
    let missing: Vec<_> = parts
        .iter()
        .filter(|p| {
            !objects
                .iter()
                .any(|o| o.part_id.as_deref() == Some(p.id.as_str()))
        })
        .take(4)
        .collect();
    if missing.is_empty() {
        return;
    }
    db::trace(
        db,
        rid,
        "sam-3.1",
        &format!(
            "Retrying {} unmatched part(s) with simplified prompts",
            missing.len()
        ),
        None,
    );
    let schema = serde_json::json!({
        "type": "object", "additionalProperties": false,
        "properties": {"variants": {"type": "array", "items": {
            "type": "object", "additionalProperties": false,
            "properties": {"name": {"type": "string"}, "prompts": {"type": "array", "items": {"type": "string"}}},
            "required": ["name", "prompts"]
        }}},
        "required": ["variants"]
    });
    let user = format!(
        "For each part below, propose up to 2 SIMPLER noun phrases for an image segmentation model. Rules: 1-2 words each, concrete visual nouns only, no verbs, drop materials/sizes unless essential (\"screw\" beats \"small black screw\", \"fan\" beats \"black computer cooling fan\").\n{}",
        missing
            .iter()
            .map(|p| format!("- {} (current prompt: \"{}\")", p.name, p.sam_prompt))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let mut variants: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for model in ["muse-wonder-1.0", "muse-spark-1.3"] {
        match state
            .meta
            .chat_json(
                model,
                "You write short segmentation prompts. Return JSON only.",
                &user,
                "simplify_prompts",
                schema.clone(),
            )
            .await
        {
            Ok(v) => {
                if let Some(arr) = v.get("variants").and_then(|x| x.as_array()) {
                    for item in arr {
                        if let (Some(name), Some(ps)) = (
                            item.get("name").and_then(|x| x.as_str()),
                            item.get("prompts").and_then(|x| x.as_array()),
                        ) {
                            // Index by normalized name: models often recase labels.
                            variants.insert(
                                name.to_lowercase(),
                                ps.iter()
                                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                    .take(2)
                                    .collect(),
                            );
                        }
                    }
                }
                break;
            }
            Err(e) => {
                db::trace(
                    db,
                    rid,
                    "sam-3.1",
                    &format!("Prompt simplification via {} failed: {}", model, e),
                    None,
                );
            }
        }
    }
    if variants.is_empty() {
        db::trace(
            db,
            rid,
            "sam-3.1",
            "Model returned no usable variants — falling back to head nouns",
            None,
        );
    }
    let frame_count = db::list_frames(db, rid).len() as i64;
    for part in missing {
        // Model variants (case-insensitive) plus a deterministic fallback:
        // the head noun of the original prompt ("screw" from "small black
        // screw") is usually the best single-word prompt.
        let mut vars: Vec<String> = variants
            .get(&part.name.to_lowercase())
            .cloned()
            .unwrap_or_default();
        if let Some(head) = part
            .sam_prompt
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() > 2)
            .last()
        {
            if !vars.iter().any(|v| v.eq_ignore_ascii_case(head)) {
                vars.push(head.to_string());
            }
        }
        vars.truncate(3);
        if vars.is_empty() {
            continue;
        }
        db::trace(
            db,
            rid,
            "sam-3.1",
            &format!(
                "Simplified prompts for \"{}\": {}",
                part.name,
                vars.join(", ")
            ),
            None,
        );
        let ev = part.evidence_frame_seq.unwrap_or(0);
        let mut seqs = vec![ev];
        if ev > 3 {
            seqs.push(ev - 3);
        }
        if ev + 3 < frame_count {
            seqs.push(ev + 3);
        }
        let mut done = false;
        for seq in seqs {
            if done {
                break;
            }
            let Some(fp) = db::frame_path(db, rid, seq) else {
                continue;
            };
            let fp = PathBuf::from(fp);
            for var in vars.iter().take(2) {
                if var.trim().is_empty() || *var == part.sam_prompt {
                    continue;
                }
                match segment_frame(state, db, rid, &part.id, var, seq, &fp).await {
                    Ok(n) if n > 0 => {
                        done = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
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
