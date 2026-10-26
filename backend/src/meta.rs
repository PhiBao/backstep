use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

use crate::models::Extraction;

#[derive(Clone)]
pub struct MetaClient {
    pub base: String,
    pub key: Option<String>,
    http: reqwest::Client,
}

impl MetaClient {
    pub fn new(base: String, key: Option<String>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            key: key.filter(|k| !k.trim().is_empty()),
            http: reqwest::Client::new(),
        }
    }

    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    fn auth(&self) -> Result<String, anyhow::Error> {
        self.key
            .clone()
            .ok_or_else(|| anyhow::anyhow!("MODEL_API_KEY is not set"))
    }

    async fn post_json(&self, path: &str, body: Value) -> Result<Value, anyhow::Error> {
        let key = self.auth()?;
        let url = format!("{}{}", self.base, path);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&key)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            // Never retry a 400 unchanged (SAM docs); surface the server message.
            anyhow::bail!("Meta API {} {}: {}", status, path, truncate(&text, 800));
        }
        Ok(serde_json::from_str(&text)?)
    }

    pub async fn upload_file(&self, path: &std::path::Path) -> Result<String, anyhow::Error> {
        let key = self.auth()?;
        let bytes = tokio::fs::read(path).await?;
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("recording.mp4")
            .to_string();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str("video/mp4")?;
        let form = reqwest::multipart::Form::new()
            .text("purpose", "user_data")
            .part("file", part);
        let resp = self
            .http
            .post(format!("{}/files", self.base))
            .bearer_auth(&key)
            .multipart(form)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("Meta Files API {}: {}", status, truncate(&text, 800));
        }
        let v: Value = serde_json::from_str(&text)?;
        v.get("id")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                anyhow::anyhow!("Files API response missing id: {}", truncate(&text, 400))
            })
    }

    /// Muse Spark 1.3 video understanding -> structured Extraction (schema-constrained).
    pub async fn spark_extract_video(&self, file_id: &str) -> Result<Extraction, anyhow::Error> {
        let schema = extraction_schema();
        let prompt = "You are analysing a recording of a hands-on teardown / disassembly performed by one person. \
Return structured data only. Identify the distinct physical parts that are removed, detached, or set aside (max 8). \
For each part give: name (human label), sam_prompt (a SHORT concrete noun phrase for an image segmentation model, e.g. \"black metal screw\" — one object, concrete attributes only, no verbs), color if visible, first_seen_sec and last_seen_sec (seconds in the video), evidence_sec (a second where the part is clearly visible). \
Then list the teardown steps in the order performed (max 20). For each step: start_sec, end_sec, action (imperative, one sentence), part_names (names from your parts list), state_before and state_after (short descriptions of the assembly state), evidence_sec (a second where the step's result is clearly visible). \
Use only what is visible in the video. Do not invent parts or steps.";
        let body = json!({
            "model": "muse-spark-1.3",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": prompt},
                    {"type": "input_file", "file_id": file_id}
                ]
            }],
            "text": {"format": {"type": "json_schema", "name": "teardown_extraction", "schema": schema}}
        });
        let v = self.post_json("/responses", body).await?;
        let text = extract_responses_text(&v)
            .ok_or_else(|| anyhow::anyhow!("Spark video response had no output text"))?;
        let parsed: Extraction = serde_json::from_str(strip_fences(&text))?;
        Ok(parsed)
    }

    /// SAM 3.1 image segmentation for one concept on one JPEG frame. Returns raw output_text.
    pub async fn sam_segment_image(
        &self,
        concept: &str,
        jpeg_bytes: &[u8],
    ) -> Result<String, anyhow::Error> {
        let data_url = format!("data:image/jpeg;base64,{}", STANDARD.encode(jpeg_bytes));
        let body = json!({
            "model": "sam-3.1",
            "stream": false,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [
                    {"type": "input_text", "text": concept},
                    {"type": "input_image", "image_url": data_url}
                ]
            }],
            "metadata": {"mask_encoding": "one_bit", "include_confidence": "true"}
        });
        let v = self.post_json("/responses", body).await?;
        Ok(extract_responses_text(&v).unwrap_or_default())
    }

    /// Chat JSON with schema constraint. `model` is tried first; caller handles fallback.
    pub async fn chat_json(
        &self,
        model: &str,
        system: &str,
        user: &str,
        schema_name: &str,
        schema: Value,
    ) -> Result<Value, anyhow::Error> {
        let body = json!({
            "model": model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ],
            "response_format": {"type": "json_schema", "json_schema": {"name": schema_name, "schema": schema}}
        });
        let v = self.post_json("/chat/completions", body).await?;
        let text = v
            .pointer("/choices/0/message/content")
            .and_then(|x| x.as_str())
            .ok_or_else(|| anyhow::anyhow!("chat response had no content"))?;
        Ok(serde_json::from_str(strip_fences(text))?)
    }
}

pub fn extract_responses_text(v: &Value) -> Option<String> {
    if let Some(t) = v.get("output_text").and_then(|x| x.as_str()) {
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let mut buf = String::new();
    if let Some(out) = v.get("output").and_then(|x| x.as_array()) {
        for item in out {
            if let Some(content) = item.get("content").and_then(|x| x.as_array()) {
                for c in content {
                    if let Some(t) = c.get("text").and_then(|x| x.as_str()) {
                        buf.push_str(t);
                    }
                }
            }
        }
    }
    if buf.is_empty() {
        None
    } else {
        Some(buf)
    }
}

fn strip_fences(s: &str) -> &str {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("```json") {
        return rest.strip_suffix("```").unwrap_or(rest).trim();
    }
    if let Some(rest) = t.strip_prefix("```") {
        return rest.strip_suffix("```").unwrap_or(rest).trim();
    }
    t
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

pub fn extraction_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "title": {"type": ["string", "null"]},
            "parts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "name": {"type": "string"},
                        "sam_prompt": {"type": "string"},
                        "color": {"type": ["string", "null"]},
                        "first_seen_sec": {"type": ["number", "null"]},
                        "last_seen_sec": {"type": ["number", "null"]},
                        "evidence_sec": {"type": ["number", "null"]}
                    },
                    "required": ["name", "sam_prompt", "color", "first_seen_sec", "last_seen_sec", "evidence_sec"]
                }
            },
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "start_sec": {"type": "number"},
                        "end_sec": {"type": "number"},
                        "action": {"type": "string"},
                        "part_names": {"type": "array", "items": {"type": "string"}},
                        "state_before": {"type": ["string", "null"]},
                        "state_after": {"type": ["string", "null"]},
                        "evidence_sec": {"type": ["number", "null"]}
                    },
                    "required": ["start_sec", "end_sec", "action", "part_names", "state_before", "state_after", "evidence_sec"]
                }
            }
        },
        "required": ["title", "parts", "steps"]
    })
}
