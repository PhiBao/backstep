use rusqlite::params;
use std::path::PathBuf;

use crate::db::Db;

pub const SAMPLE_ID: &str = "sample-kite-bar";

struct SamplePart {
    name: &'static str,
    prompt: &'static str,
    color: Option<&'static str>,
    first: f64,
    last: f64,
    evidence_seq: i64,
    // box in 960x540 frame
    box_: (i64, i64, i64, i64),
}

const PARTS: &[SamplePart] = &[
    SamplePart {
        name: "Quick-release cap",
        prompt: "red plastic cap",
        color: Some("red"),
        first: 2.0,
        last: 48.0,
        evidence_seq: 1,
        box_: (390, 180, 500, 250),
    },
    SamplePart {
        name: "Chicken loop",
        prompt: "black rubber loop",
        color: Some("black"),
        first: 5.0,
        last: 52.0,
        evidence_seq: 2,
        box_: (360, 260, 540, 360),
    },
    SamplePart {
        name: "Safety line",
        prompt: "red line",
        color: Some("red"),
        first: 8.0,
        last: 55.0,
        evidence_seq: 3,
        box_: (540, 120, 880, 170),
    },
    SamplePart {
        name: "Bar float (left)",
        prompt: "orange float",
        color: Some("orange"),
        first: 12.0,
        last: 44.0,
        evidence_seq: 4,
        box_: (90, 230, 230, 300),
    },
    SamplePart {
        name: "Trim cleat screw",
        prompt: "silver metal screw",
        color: Some("silver"),
        first: 20.0,
        last: 38.0,
        evidence_seq: 5,
        box_: (620, 300, 660, 340),
    },
];

struct SampleStep {
    action: &'static str,
    reverse: &'static str,
    start: f64,
    end: f64,
    parts: &'static str, // JSON array literal
    before: &'static str,
    after: &'static str,
    evidence_seq: i64,
}

const STEPS: &[SampleStep] = &[
    SampleStep {
        action: "Release the quick-release and open the housing",
        reverse: "Close the housing and reset the quick-release last",
        start: 0.0,
        end: 11.0,
        parts: "[\"Quick-release cap\"]",
        before: "Control bar fully assembled",
        after: "Quick-release housing open",
        evidence_seq: 0,
    },
    SampleStep {
        action: "Lift the red quick-release cap clear of the housing",
        reverse: "Seat the red quick-release cap back onto the housing",
        start: 11.0,
        end: 22.0,
        parts: "[\"Quick-release cap\"]",
        before: "Housing open, cap seated",
        after: "Cap removed and placed above the bar",
        evidence_seq: 1,
    },
    SampleStep {
        action: "Unloop the chicken loop from the hook and lay it below the bar",
        reverse: "Loop the chicken loop back onto the hook",
        start: 22.0,
        end: 33.0,
        parts: "[\"Chicken loop\"]",
        before: "Chicken loop hooked",
        after: "Chicken loop free, resting below the bar",
        evidence_seq: 2,
    },
    SampleStep {
        action: "Pull the safety line through and coil it to the right of the bar",
        reverse: "Feed the safety line back through its guide",
        start: 33.0,
        end: 44.0,
        parts: "[\"Safety line\"]",
        before: "Safety line threaded",
        after: "Safety line coiled on the right",
        evidence_seq: 3,
    },
    SampleStep {
        action: "Slide the left bar float off the bar end",
        reverse: "Slide the left bar float back onto the bar end",
        start: 44.0,
        end: 50.0,
        parts: "[\"Bar float (left)\"]",
        before: "Float seated on bar end",
        after: "Float removed, at the left of the bench",
        evidence_seq: 4,
    },
    SampleStep {
        action: "Remove the trim cleat screw and place it in the tray",
        reverse: "Refit the trim cleat screw and tighten it",
        start: 50.0,
        end: 55.0,
        parts: "[\"Trim cleat screw\"]",
        before: "Trim cleat fixed",
        after: "Screw in the tray, cleat loose",
        evidence_seq: 5,
    },
];

pub fn seed_sample(db: &Db, data_dir: &std::path::Path) {
    let exists = {
        let c = match db.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        c.query_row(
            "SELECT COUNT(*) FROM recordings WHERE id=?1",
            params![SAMPLE_ID],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 0
    };
    if exists {
        return;
    }
    let dir = data_dir.join("sample");
    let _ = std::fs::create_dir_all(&dir);
    for seq in 0..=5i64 {
        let p = dir.join(format!("frame-{:04}.svg", seq));
        if !p.exists() {
            let _ = std::fs::write(&p, sample_svg(seq));
        }
    }
    let c = match db.lock() {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = c.execute(
        "INSERT INTO recordings(id,title,kind,status,duration_sec,frame_count,created_at,video_path) VALUES(?1,?2,'sample','ready',55.0,6,?3,NULL)",
        params![SAMPLE_ID, "Sample memory — kite control bar teardown", chrono::Utc::now().to_rfc3339()],
    );
    for seq in 0..=5i64 {
        let p: PathBuf = dir.join(format!("frame-{:04}.svg", seq));
        let _ = c.execute(
            "INSERT INTO frames(recording_id,seq,ts_sec,path,width,height) VALUES(?1,?2,?3,?4,960,540)",
            params![SAMPLE_ID, seq, seq as f64 * 11.0, p.to_string_lossy().to_string()],
        );
    }
    for (i, part) in PARTS.iter().enumerate() {
        let pid = format!("sample-part-{}", i + 1);
        let _ = c.execute(
            "INSERT INTO parts(id,recording_id,name,sam_prompt,color,first_seen_sec,last_seen_sec,evidence_frame_seq,confirmed) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,0)",
            params![pid, SAMPLE_ID, part.name, part.prompt, part.color, part.first, part.last, part.evidence_seq],
        );
        let (x1, y1, x2, y2) = part.box_;
        let _ = c.execute(
            "INSERT INTO sam_objects(id,recording_id,part_id,concept,frame_seq,sam_frame,object_id,x1,y1,x2,y2,frame_w,frame_h,confidence,mask_data) VALUES(?1,?2,?3,?4,?5,?5,0,?6,?7,?8,?9,960,540,0.92,NULL)",
            params![format!("sample-obj-{}", i + 1), SAMPLE_ID, pid, part.prompt, part.evidence_seq, x1, y1, x2, y2],
        );
    }
    for (i, s) in STEPS.iter().enumerate() {
        let _ = c.execute(
            "INSERT INTO steps(id,recording_id,order_index,start_sec,end_sec,action,reverse_action,part_names,state_before,state_after,evidence_frame_seq) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![format!("sample-step-{}", i + 1), SAMPLE_ID, i as i64, s.start, s.end, s.action, s.reverse, s.parts, s.before, s.after, s.evidence_seq],
        );
    }
    let _ = c.execute(
        "INSERT INTO trace_events(recording_id,ts,stage,message,detail) VALUES(?1,?2,'sample','Sample memory loaded','Precomputed example index. Upload your own recording for live Meta Model API analysis.')",
        params![SAMPLE_ID, chrono::Utc::now().to_rfc3339()],
    );
}

fn sample_svg(seq: i64) -> String {
    // Bench diagram: bar across the middle; removed parts drift to labelled zones as seq grows.
    let cap_y = if seq >= 1 { 90 } else { 190 };
    let loop_y = if seq >= 2 { 400 } else { 280 };
    let float_x = if seq >= 4 { 40 } else { 110 };
    let screw_opacity = if seq >= 5 { 1.0 } else { 0.25 };
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="960" height="540" viewBox="0 0 960 540">
<rect width="960" height="540" fill="#EFE9DF"/>
<rect x="40" y="120" width="880" height="300" rx="8" fill="#E2D9C8" stroke="#1A1E1C" stroke-width="2"/>
<text x="56" y="100" font-family="monospace" font-size="20" fill="#1A1E1C">SAMPLE FRAME {seq} — NOT REAL FOOTAGE</text>
<rect x="90" y="240" width="780" height="26" rx="13" fill="#1A1E1C"/>
<rect x="{float_x}" y="228" width="130" height="50" rx="16" fill="#FF7A1A" stroke="#1A1E1C" stroke-width="2"/>
<rect x="730" y="228" width="130" height="50" rx="16" fill="#FF7A1A" stroke="#1A1E1C" stroke-width="2"/>
<rect x="390" y="{cap_y}" width="110" height="60" rx="10" fill="#D43A2F" stroke="#1A1E1C" stroke-width="2"/>
<text x="398" y="{cap_y_text}" font-family="monospace" font-size="15" fill="#fff">QR CAP</text>
<ellipse cx="450" cy="{loop_y}" rx="80" ry="42" fill="none" stroke="#1A1E1C" stroke-width="14"/>
<path d="M540 140 L880 150" stroke="#D43A2F" stroke-width="7"/>
<circle cx="640" cy="320" r="18" fill="#C0C6CC" stroke="#1A1E1C" stroke-width="2" opacity="{screw_opacity}"/>
<text x="56" y="500" font-family="monospace" font-size="16" fill="#5A5148">kite control bar · bench · sample index (precomputed)</text>
</svg>"##,
        seq = seq,
        float_x = float_x,
        cap_y = cap_y,
        cap_y_text = cap_y + 36,
        loop_y = loop_y,
        screw_opacity = screw_opacity
    )
}
