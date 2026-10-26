use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::models::*;

pub type Db = Arc<Mutex<Connection>>;

pub fn open(path: &Path) -> anyhow::Result<Db> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        r#"
        PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS recordings(
          id TEXT PRIMARY KEY, title TEXT NOT NULL, kind TEXT NOT NULL,
          status TEXT NOT NULL, duration_sec REAL, frame_count INTEGER NOT NULL DEFAULT 0,
          created_at TEXT NOT NULL, error TEXT, video_path TEXT, content_hash TEXT
        );
        CREATE TABLE IF NOT EXISTS frames(
          recording_id TEXT NOT NULL, seq INTEGER NOT NULL, ts_sec REAL NOT NULL,
          path TEXT NOT NULL, width INTEGER NOT NULL DEFAULT 960, height INTEGER NOT NULL DEFAULT 540,
          PRIMARY KEY(recording_id, seq)
        );
        CREATE TABLE IF NOT EXISTS parts(
          id TEXT PRIMARY KEY, recording_id TEXT NOT NULL, name TEXT NOT NULL,
          sam_prompt TEXT NOT NULL, color TEXT, first_seen_sec REAL, last_seen_sec REAL,
          evidence_frame_seq INTEGER, confirmed INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS steps(
          id TEXT PRIMARY KEY, recording_id TEXT NOT NULL, order_index INTEGER NOT NULL,
          start_sec REAL NOT NULL, end_sec REAL NOT NULL, action TEXT NOT NULL,
          reverse_action TEXT, part_names TEXT NOT NULL DEFAULT '[]',
          state_before TEXT, state_after TEXT, evidence_frame_seq INTEGER
        );
        CREATE TABLE IF NOT EXISTS sam_objects(
          id TEXT PRIMARY KEY, recording_id TEXT NOT NULL, part_id TEXT, concept TEXT NOT NULL,
          frame_seq INTEGER NOT NULL, sam_frame INTEGER NOT NULL, object_id INTEGER NOT NULL,
          x1 INTEGER NOT NULL, y1 INTEGER NOT NULL, x2 INTEGER NOT NULL, y2 INTEGER NOT NULL,
          frame_w INTEGER NOT NULL, frame_h INTEGER NOT NULL, confidence REAL, mask_data TEXT
        );
        CREATE TABLE IF NOT EXISTS trace_events(
          id INTEGER PRIMARY KEY AUTOINCREMENT, recording_id TEXT NOT NULL, ts TEXT NOT NULL,
          stage TEXT NOT NULL, message TEXT NOT NULL, detail TEXT
        );
        CREATE TABLE IF NOT EXISTS response_cache(
          cache_key TEXT PRIMARY KEY, response TEXT NOT NULL, created_at TEXT NOT NULL
        );
        "#,
    )?;
    // Best-effort migrations for databases created before these columns existed.
    for stmt in [
        "ALTER TABLE recordings ADD COLUMN meta_file_id TEXT",
        "ALTER TABLE recordings ADD COLUMN frame_w INTEGER NOT NULL DEFAULT 960",
        "ALTER TABLE recordings ADD COLUMN frame_h INTEGER NOT NULL DEFAULT 540",
    ] {
        let _ = conn.execute(stmt, []);
    }
    Ok(Arc::new(Mutex::new(conn)))
}

pub fn trace(db: &Db, recording_id: &str, stage: &str, message: &str, detail: Option<&str>) {
    if let Ok(c) = db.lock() {
        let _ = c.execute(
            "INSERT INTO trace_events(recording_id, ts, stage, message, detail) VALUES(?1,?2,?3,?4,?5)",
            params![recording_id, chrono::Utc::now().to_rfc3339(), stage, message, detail],
        );
    }
}

pub fn set_status(db: &Db, id: &str, status: &str, error: Option<&str>) {
    if let Ok(c) = db.lock() {
        let _ = c.execute(
            "UPDATE recordings SET status=?1, error=?2 WHERE id=?3",
            params![status, error, id],
        );
    }
}

pub fn list_recordings(db: &Db) -> Vec<Recording> {
    let c = match db.lock() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match c.prepare("SELECT id,title,kind,status,duration_sec,frame_count,created_at,error,meta_file_id,frame_w,frame_h FROM recordings ORDER BY created_at DESC") {
        Ok(s) => s, Err(_) => return vec![],
    };
    let rows = stmt.query_map([], |r| {
        Ok(Recording {
            id: r.get(0)?,
            title: r.get(1)?,
            kind: r.get(2)?,
            status: r.get(3)?,
            duration_sec: r.get(4)?,
            frame_count: r.get(5)?,
            created_at: r.get(6)?,
            error: r.get(7)?,
            meta_file_id: r.get(8)?,
            frame_w: r.get(9)?,
            frame_h: r.get(10)?,
        })
    });
    match rows {
        Ok(it) => it.filter_map(|x| x.ok()).collect(),
        Err(_) => vec![],
    }
}

pub fn get_recording(db: &Db, id: &str) -> Option<Recording> {
    list_recordings(db).into_iter().find(|r| r.id == id)
}

pub fn list_frames(db: &Db, recording_id: &str) -> Vec<Frame> {
    let c = match db.lock() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match c.prepare(
        "SELECT seq,ts_sec,path,width,height FROM frames WHERE recording_id=?1 ORDER BY seq",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let rows = stmt.query_map(params![recording_id], |r| {
        let seq: i64 = r.get(0)?;
        Ok(Frame {
            seq,
            ts_sec: r.get(1)?,
            url: format!("/api/recordings/{}/frames/{}", recording_id, seq),
            width: r.get(3)?,
            height: r.get(4)?,
        })
    });
    match rows {
        Ok(it) => it.filter_map(|x| x.ok()).collect(),
        Err(_) => vec![],
    }
}

pub fn frame_path(db: &Db, recording_id: &str, seq: i64) -> Option<String> {
    let c = db.lock().ok()?;
    c.query_row(
        "SELECT path FROM frames WHERE recording_id=?1 AND seq=?2",
        params![recording_id, seq],
        |r| r.get(0),
    )
    .ok()
}

pub fn frame_seq_at(db: &Db, recording_id: &str, sec: f64) -> Option<i64> {
    let c = db.lock().ok()?;
    c.query_row(
        "SELECT seq FROM frames WHERE recording_id=?1 ORDER BY ABS(ts_sec-?2) LIMIT 1",
        params![recording_id, sec],
        |r| r.get(0),
    )
    .ok()
}

pub fn list_parts(db: &Db, recording_id: &str) -> Vec<Part> {
    let c = match db.lock() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match c.prepare("SELECT id,recording_id,name,sam_prompt,color,first_seen_sec,last_seen_sec,evidence_frame_seq,confirmed FROM parts WHERE recording_id=?1 ORDER BY name") {
        Ok(s) => s, Err(_) => return vec![],
    };
    let rows = stmt.query_map(params![recording_id], |r| {
        Ok(Part {
            id: r.get(0)?,
            recording_id: r.get(1)?,
            name: r.get(2)?,
            sam_prompt: r.get(3)?,
            color: r.get(4)?,
            first_seen_sec: r.get(5)?,
            last_seen_sec: r.get(6)?,
            evidence_frame_seq: r.get(7)?,
            confirmed: r.get::<_, i64>(8)? != 0,
        })
    });
    match rows {
        Ok(it) => it.filter_map(|x| x.ok()).collect(),
        Err(_) => vec![],
    }
}

pub fn list_steps(db: &Db, recording_id: &str) -> Vec<Step> {
    let c = match db.lock() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match c.prepare("SELECT id,recording_id,order_index,start_sec,end_sec,action,reverse_action,part_names,state_before,state_after,evidence_frame_seq FROM steps WHERE recording_id=?1 ORDER BY order_index") {
        Ok(s) => s, Err(_) => return vec![],
    };
    let rows = stmt.query_map(params![recording_id], |r| {
        let names: String = r.get(7)?;
        Ok(Step {
            id: r.get(0)?,
            recording_id: r.get(1)?,
            order_index: r.get(2)?,
            start_sec: r.get(3)?,
            end_sec: r.get(4)?,
            action: r.get(5)?,
            reverse_action: r.get(6)?,
            part_names: serde_json::from_str(&names).unwrap_or_default(),
            state_before: r.get(8)?,
            state_after: r.get(9)?,
            evidence_frame_seq: r.get(10)?,
        })
    });
    match rows {
        Ok(it) => it.filter_map(|x| x.ok()).collect(),
        Err(_) => vec![],
    }
}

pub fn list_objects(db: &Db, recording_id: &str) -> Vec<SamObject> {
    let c = match db.lock() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match c.prepare("SELECT id,recording_id,part_id,concept,frame_seq,sam_frame,object_id,x1,y1,x2,y2,frame_w,frame_h,confidence,mask_data FROM sam_objects WHERE recording_id=?1 ORDER BY frame_seq, object_id") {
        Ok(s) => s, Err(_) => return vec![],
    };
    let rows = stmt.query_map(params![recording_id], |r| {
        Ok(SamObject {
            id: r.get(0)?,
            recording_id: r.get(1)?,
            part_id: r.get(2)?,
            concept: r.get(3)?,
            frame_seq: r.get(4)?,
            sam_frame: r.get(5)?,
            object_id: r.get(6)?,
            x1: r.get(7)?,
            y1: r.get(8)?,
            x2: r.get(9)?,
            y2: r.get(10)?,
            frame_w: r.get(11)?,
            frame_h: r.get(12)?,
            confidence: r.get(13)?,
            mask_data: r.get(14)?,
        })
    });
    match rows {
        Ok(it) => it.filter_map(|x| x.ok()).collect(),
        Err(_) => vec![],
    }
}

pub fn list_trace(db: &Db, recording_id: &str) -> Vec<TraceEvent> {
    let c = match db.lock() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match c.prepare("SELECT id,recording_id,ts,stage,message,detail FROM trace_events WHERE recording_id=?1 ORDER BY id") {
        Ok(s) => s, Err(_) => return vec![],
    };
    let rows = stmt.query_map(params![recording_id], |r| {
        Ok(TraceEvent {
            id: r.get(0)?,
            recording_id: r.get(1)?,
            ts: r.get(2)?,
            stage: r.get(3)?,
            message: r.get(4)?,
            detail: r.get(5)?,
        })
    });
    match rows {
        Ok(it) => it.filter_map(|x| x.ok()).collect(),
        Err(_) => vec![],
    }
}

pub fn cache_get(db: &Db, key: &str) -> Option<String> {
    let c = db.lock().ok()?;
    c.query_row(
        "SELECT response FROM response_cache WHERE cache_key=?1",
        params![key],
        |r| r.get(0),
    )
    .ok()
}

pub fn cache_put(db: &Db, key: &str, response: &str) {
    if let Ok(c) = db.lock() {
        let _ = c.execute(
            "INSERT OR REPLACE INTO response_cache(cache_key,response,created_at) VALUES(?1,?2,?3)",
            params![key, response, chrono::Utc::now().to_rfc3339()],
        );
    }
}
