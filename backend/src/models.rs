use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recording {
    pub id: String,
    pub title: String,
    pub kind: String,   // "upload" | "sample"
    pub status: String, // created | extracting | analyzing | segmenting | ready | awaiting_key | failed
    pub duration_sec: Option<f64>,
    pub frame_count: i64,
    pub created_at: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub seq: i64,
    pub ts_sec: f64,
    pub url: String,
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Part {
    pub id: String,
    pub recording_id: String,
    pub name: String,
    pub sam_prompt: String,
    pub color: Option<String>,
    pub first_seen_sec: Option<f64>,
    pub last_seen_sec: Option<f64>,
    pub evidence_frame_seq: Option<i64>,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub recording_id: String,
    pub order_index: i64,
    pub start_sec: f64,
    pub end_sec: f64,
    pub action: String,
    pub reverse_action: Option<String>,
    pub part_names: Vec<String>,
    pub state_before: Option<String>,
    pub state_after: Option<String>,
    pub evidence_frame_seq: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SamObject {
    pub id: String,
    pub recording_id: String,
    pub part_id: Option<String>,
    pub concept: String,
    pub frame_seq: i64,
    pub sam_frame: i64,
    pub object_id: i64,
    pub x1: i64,
    pub y1: i64,
    pub x2: i64,
    pub y2: i64,
    pub frame_w: i64,
    pub frame_h: i64,
    pub confidence: Option<f64>,
    pub mask_data: Option<String>, // raw SAM mask payload (H,W,enc+payload), decoded in UI when possible
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEvent {
    pub id: i64,
    pub recording_id: String,
    pub ts: String,
    pub stage: String,
    pub message: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Citation {
    #[serde(rename = "type")]
    pub kind: String, // frame | step | part | object
    pub id: String,
    pub frame_seq: Option<i64>,
    pub ts_sec: Option<f64>,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskRequest {
    pub question: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskResponse {
    pub answer: String,
    pub citations: Vec<Citation>,
    pub verified: bool,
    pub insufficient_evidence: bool,
    pub source: String, // "index" | "muse-wonder" | "muse-spark"
}

#[derive(Debug, Clone, Deserialize)]
pub struct RenamePart {
    pub name: String,
}

// ---- Muse Spark structured extraction shapes ----
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedPart {
    pub name: String,
    pub sam_prompt: String,
    pub color: Option<String>,
    pub first_seen_sec: Option<f64>,
    pub last_seen_sec: Option<f64>,
    pub evidence_sec: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedStep {
    pub start_sec: f64,
    pub end_sec: f64,
    pub action: String,
    pub part_names: Vec<String>,
    pub state_before: Option<String>,
    pub state_after: Option<String>,
    pub evidence_sec: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Extraction {
    pub title: Option<String>,
    pub parts: Vec<ExtractedPart>,
    pub steps: Vec<ExtractedStep>,
}
