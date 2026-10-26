# Backstep — Strengthened Build Plan (approved Oct 26, 2026)

## Thesis
Film a teardown once. Backstep builds an object-state memory (parts, order, state)
from that recording, then answers with frame-level evidence:
- **Manual**: "How does this go back together?" → reverse replay of your own hands.
- **Finder**: "Where did I put the tiny black screw?" → last-seen frame + box/mask.

Every claim is a citation. If it wasn't seen, it isn't said.

## Track / theme
Indie Developer track · Build for Your World (Push the Frontier as backup framing).

## Strengthenings vs. discovery memo
1. Scope narrowed: ONE recording, TWO jobs (Manual + Finder). Voice, multi-session
   memory, and live-camera verify are stretch, not MVP.
2. Perception is two-tier:
   - Tier A (must): Muse Spark 1.3 video understanding (Files API) → structured
     steps/parts JSON (schema-constrained). SAM 3.1 image segmentation on each
     part's evidence frame → boxes + masks for evidence display.
   - Tier B (should): SAM 3.1 video tracking for a selected part.
3. Spark proposes SAM noun phrases (SAM 3 Agent pattern) — never hand-written prompts.
4. Deterministic citation validation in Rust: model-proposed citations are checked
   against the index; invalid ones are dropped, answers with no valid citation are
   flagged unverified or refused ("insufficient evidence").
5. Human correction loop: rename/merge parts, confirm steps. Counters SAM's known
   fine-grained weakness and builds trust.
6. Content-hash caching of every model response; ingest is a resumable background
   job with a visible trace. Demo path = retrieval + render only.
7. Muse Wonder is synthesis-only; Muse Spark fallback if Wonder is unavailable.
8. Standard mode only (no training on user content). API key server-side only.

## Architecture
- `backend/` Rust (Axum, Tokio, rusqlite, reqwest): upload, ffmpeg frames,
  Meta Files/Spark/SAM clients, SQLite index, citation-validated ask, SSE/poll jobs.
- `web/` Next.js + TypeScript: timeline-spine workspace (Manual / Finder / Parts /
  Trace), box overlays on frames, evidence seek.
- Storage: local `data/` dir (recordings, frames, cache). SQLite index.

## MVP boundary
Must: ingest → frames → Spark steps → SAM evidence → reverse Manual → Finder with
validated citations → correction loop → trace → sample memory (labelled) → README.
Should: SAM video tracking, accuracy benchmark table (20 queries).
Stretch: voice query (Muse Voice Transcribe), Muse Image step illustrations.
Non-goals: AR guidance, teams/accounts, mobile app, screen memory.

## Proof
- Citation coverage: every shipped answer cites an existing frame/object/step.
- 20-query hand-labelled benchmark with correct/total table in README.
- Real reassembly completed by following the generated manual (record when footage exists).

## Risks
- Fine-grained parts ("tiny black screw"): exemplar/box prompts + correction loop.
- No API key at build start: app runs honestly in labelled sample mode; live
  pipeline activates with MODEL_API_KEY. Nothing mocked is presented as live.
- Long video cost/latency: 1 fps frames (cap 300), max 8 SAM concepts, caching.
