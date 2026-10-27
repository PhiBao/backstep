"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

type Recording = {
  id: string; title: string; kind: string; status: string;
  duration_sec: number | null; frame_count: number; created_at: string; error: string | null;
};
type Frame = { seq: number; ts_sec: number; url: string; width: number; height: number };
type Part = {
  id: string; name: string; sam_prompt: string; color: string | null;
  first_seen_sec: number | null; last_seen_sec: number | null;
  evidence_frame_seq: number | null; confirmed: boolean;
};
type Step = {
  id: string; order_index: number; start_sec: number; end_sec: number; action: string;
  reverse_action: string | null; part_names: string[];
  state_before: string | null; state_after: string | null; evidence_frame_seq: number | null;
  detail?: {
    tools?: string[]; orientation?: string | null; warnings?: string[];
    part_count?: number | null; uncertain?: boolean; note?: string | null;
  } | null;
};
type SamObject = {
  id: string; part_id: string | null; concept: string; frame_seq: number; object_id: number;
  x1: number; y1: number; x2: number; y2: number; frame_w: number; frame_h: number;
  confidence: number | null;
};
type TraceEvent = { id: number; ts: string; stage: string; message: string; detail: string | null };
type Detail = {
  recording: Recording; frames: Frame[]; parts: Part[]; steps: Step[];
  objects: SamObject[]; trace: TraceEvent[];
};
type ManualStep = {
  manual_order: number; step_id: string; instruction: string; original_action: string;
  part_names: string[]; state_target: string | null; evidence_frame_seq: number | null;
  citation: { type: string; id: string; frame_seq: number | null; ts_sec: number | null; label: string };
};
type Citation = { type: string; id: string; frame_seq: number | null; ts_sec: number | null; label: string };
type AskResponse = {
  answer: string; citations: Citation[]; verified: boolean;
  insufficient_evidence: boolean; source: string;
};

const EXAMPLES = [
  "Where did I put the trim cleat screw?",
  "How does this go back together?",
  "Where is the chicken loop?",
  "What happened to the safety line?",
];

function tc(sec: number | null | undefined): string {
  if (sec === null || sec === undefined || Number.isNaN(sec)) return "--:--";
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

export default function Home() {
  const [health, setHealth] = useState<{ meta_api_configured: boolean } | null>(null);
  const [recordings, setRecordings] = useState<Recording[]>([]);
  const [selectedId, setSelectedId] = useState<string>("");
  const [detail, setDetail] = useState<Detail | null>(null);
  const [manual, setManual] = useState<ManualStep[]>([]);
  const [frameSeq, setFrameSeq] = useState<number>(0);
  const [tab, setTab] = useState<"guide" | "manual" | "finder" | "parts" | "trace">("manual");
  const [guideIdx, setGuideIdx] = useState(0);
  const [guideDone, setGuideDone] = useState<Record<string, boolean>>({});
  const [checkResult, setCheckResult] = useState<{ verdict: string; explanation: string; confidence: number } | null>(null);
  const [checkBusy, setCheckBusy] = useState(false);
  const checkFileRef = useRef<HTMLInputElement>(null);
  const [question, setQuestion] = useState("");
  const [askResult, setAskResult] = useState<AskResponse | null>(null);
  const [asking, setAsking] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [renames, setRenames] = useState<Record<string, string>>({});
  const [pickedFile, setPickedFile] = useState<{ name: string; size: number } | null>(null);
  const [dupTitle, setDupTitle] = useState(false);
  const [tracking, setTracking] = useState<{ partId: string; partName: string } | null>(null);
  const [drag, setDrag] = useState<{ x0: number; y0: number; x1: number; y1: number } | null>(null);
  const [trackBusy, setTrackBusy] = useState(false);
  const [trackMsg, setTrackMsg] = useState<string | null>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const titleRef = useRef<HTMLInputElement>(null);
  const imgRef = useRef<HTMLImageElement>(null);

  // Backend base URL. Empty string = same-origin Next.js proxy (fine for reads
  // and small uploads, but the proxy fails on ~100MB bodies). Set
  // NEXT_PUBLIC_BACKEND_URL=http://127.0.0.1:8787 (dev) or your deployed API
  // so large teardown uploads go straight to the Rust service.
  const BACKEND = process.env.NEXT_PUBLIC_BACKEND_URL || "";
  const api = (p: string) => `${BACKEND}${p}`;

  const MAX_BYTES = 200 * 1024 * 1024;
  const fmtSize = (n: number) =>
    n >= 1024 * 1024 ? `${(n / (1024 * 1024)).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`;

  function onPickFile(e: React.ChangeEvent<HTMLInputElement>) {
    const f = e.target.files?.[0];
    setDupTitle(false);
    if (!f) {
      setPickedFile(null);
      return;
    }
    if (!/\.(mp4|mov)$/i.test(f.name)) {
      setUploadError("Only .mp4 or .mov recordings are supported.");
      setPickedFile(null);
      if (fileRef.current) fileRef.current.value = "";
      return;
    }
    if (f.size > MAX_BYTES) {
      setUploadError(`That file is ${fmtSize(f.size)} — the limit is 200 MB. Trim it or export at a lower bitrate first.`);
      setPickedFile(null);
      if (fileRef.current) fileRef.current.value = "";
      return;
    }
    setUploadError(null);
    setPickedFile({ name: f.name, size: f.size });
    const titleEl = titleRef.current;
    if (titleEl && !titleEl.value.trim()) {
      titleEl.value = f.name.replace(/\.(mp4|mov)$/i, "").replace(/[_-]+/g, " ").trim();
    }
  }

  async function postUpload(title: string): Promise<Recording> {
    const file = fileRef.current?.files?.[0];
    if (!file) throw new Error("Choose a video file first.");
    const fd = new FormData();
    fd.append("file", file);
    if (title) fd.append("title", title);
    const r = await fetch(api("/api/recordings"), { method: "POST", body: fd });
    const text = await r.text();
    let j: { recording?: Recording; error?: string } = {};
    try {
      j = text ? (JSON.parse(text) as typeof j) : {};
    } catch {
      throw new Error(
        r.ok
          ? "The server returned an unreadable response. Please try again."
          : `Upload failed (HTTP ${r.status}). The server may have restarted mid-upload — please try again.`
      );
    }
    if (!r.ok || !j.recording) throw new Error(j.error || `Upload failed (HTTP ${r.status}). Please try again.`);
    return j.recording;
  }

  const loadRecordings = useCallback(async () => {
    try {
      const r = await fetch(api("/api/recordings"));
      const j = await r.json();
      setRecordings(j.recordings || []);
      return (j.recordings || []) as Recording[];
    } catch {
      return [];
    }
  }, []);

  const loadDetail = useCallback(async (id: string) => {
    if (!id) return;
    try {
      const r = await fetch(api(`/api/recordings/${id}`));
      if (!r.ok) return;
      const j = (await r.json()) as Detail;
      setDetail(j);
      const mr = await fetch(api(`/api/recordings/${id}/manual`));
      if (mr.ok) {
        const mj = await mr.json();
        setManual(mj.manual || []);
      }
    } catch {
      /* backend warming up */
    }
  }, []);

  useEffect(() => {
    fetch(api("/health")).then((r) => r.json()).then(setHealth).catch(() => setHealth(null));
    loadRecordings().then((recs) => {
      if (recs.length > 0) setSelectedId(recs[0].id);
    });
  }, [loadRecordings]);

  useEffect(() => {
    if (!selectedId) return;
    setAskResult(null);
    setFrameSeq(0);
    loadDetail(selectedId);
  }, [selectedId, loadDetail]);

  // Poll while a recording is being analysed.
  useEffect(() => {
    const status = detail?.recording.status;
    if (!selectedId || !status) return;
    if (["ready", "awaiting_key", "failed"].includes(status)) return;
    const t = setInterval(() => {
      loadDetail(selectedId);
      loadRecordings();
    }, 2500);
    return () => clearInterval(t);
  }, [detail?.recording.status, selectedId, loadDetail, loadRecordings]);

  const frames = detail?.frames || [];
  const currentFrame = useMemo(
    () => frames.find((f) => f.seq === frameSeq) || frames[0],
    [frames, frameSeq]
  );
  const frameObjects = useMemo(
    () => (detail?.objects || []).filter((o) => currentFrame && o.frame_seq === currentFrame.seq),
    [detail, currentFrame]
  );
  const partName = useCallback(
    (pId: string | null) => detail?.parts.find((p) => p.id === pId)?.name || "",
    [detail]
  );

  function seek(seq: number | null) {
    if (seq === null || seq === undefined) return;
    setFrameSeq(seq);
    document.querySelector(".frame-wrap")?.scrollIntoView({ behavior: "smooth", block: "center" });
  }

  async function onUpload(e: React.FormEvent) {
    e.preventDefault();
    const file = fileRef.current?.files?.[0];
    if (!file) {
      setUploadError("Choose a video file first.");
      return;
    }
    const title = titleRef.current?.value?.trim() || file.name.replace(/\.(mp4|mov)$/i, "");
    const clash = recordings.find((r) => r.title.trim().toLowerCase() === title.toLowerCase());
    if (clash && !dupTitle) {
      setDupTitle(true);
      return;
    }
    setUploading(true);
    setUploadError(null);
    try {
      if (clash && dupTitle) {
        await fetch(api(`/api/recordings/${clash.id}`), { method: "DELETE" });
      }
      const rec = await postUpload(title);
      const recs = await loadRecordings();
      if (!recs.find((r: Recording) => r.id === rec.id)) setRecordings([rec, ...recs]);
      setSelectedId(rec.id);
      if (fileRef.current) fileRef.current.value = "";
      if (titleRef.current) titleRef.current.value = "";
      setPickedFile(null);
      setDupTitle(false);
    } catch (err) {
      setUploadError(err instanceof Error ? err.message : "Upload failed. Please try again.");
    } finally {
      setUploading(false);
    }
  }

  async function onAsk(q?: string) {
    const questionText = (q ?? question).trim();
    if (!questionText || !selectedId) return;
    setAsking(true);
    try {
      const r = await fetch(api(`/api/recordings/${selectedId}/ask`), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ question: questionText }),
      });
      const j = (await r.json()) as AskResponse;
      setAskResult(j);
      if (j.citations.length > 0) seek(j.citations[0].frame_seq);
    } finally {
      setAsking(false);
    }
  }

  function toMedia(e: React.MouseEvent): { x: number; y: number } | null {
    const img = imgRef.current;
    if (!img || !currentFrame) return null;
    const r = img.getBoundingClientRect();
    const nw = img.naturalWidth, nh = img.naturalHeight;
    if (!nw || !nh) return null;
    const scale = Math.min(r.width / nw, r.height / nh);
    const dw = nw * scale, dh = nh * scale;
    const ox = r.left + (r.width - dw) / 2, oy = r.top + (r.height - dh) / 2;
    const dx = e.clientX - ox, dy = e.clientY - oy;
    if (dx < 0 || dy < 0 || dx > dw || dy > dh) return null;
    return {
      x: Math.round((dx / scale) * (currentFrame.width / nw)),
      y: Math.round((dy / scale) * (currentFrame.height / nh)),
    };
  }

  function startTrack(part: Part) {
    setTracking({ partId: part.id, partName: part.name });
    setDrag(null);
    setTrackMsg(null);
    if (part.evidence_frame_seq !== null && part.evidence_frame_seq !== undefined) {
      seek(part.evidence_frame_seq);
    }
  }

  function cancelTrack() {
    setTracking(null);
    setDrag(null);
  }

  async function confirmTrack() {
    if (!tracking || !drag || !currentFrame || !selectedId) return;
    const x1 = Math.max(0, Math.min(drag.x0, drag.x1));
    const y1 = Math.max(0, Math.min(drag.y0, drag.y1));
    const x2 = Math.min(currentFrame.width, Math.max(drag.x0, drag.x1));
    const y2 = Math.min(currentFrame.height, Math.max(drag.y0, drag.y1));
    if (x2 - x1 < 5 || y2 - y1 < 5) {
      setTrackMsg("That box is too small — drag a larger region around the part.");
      return;
    }
    setTrackBusy(true);
    setTrackMsg(null);
    try {
      const r = await fetch(api(`/api/recordings/${selectedId}/track`), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ part_id: tracking.partId, frame_seq: currentFrame.seq, x1, y1, x2, y2 }),
      });
      const j = await r.json();
      if (!r.ok) throw new Error(j.error || "tracking failed");
      const n = (j.objects || []).length;
      setTrackMsg(n > 0 ? `Tracked — SAM locked onto ${n} region${n > 1 ? "s" : ""}.` : (j.message || "No mask for that box."));
      setTracking(null);
      setDrag(null);
      await loadDetail(selectedId);
    } catch (err) {
      setTrackMsg(err instanceof Error ? err.message : "Tracking failed. Please try again.");
    } finally {
      setTrackBusy(false);
    }
  }

  async function onRename(part: Part) {
    const name = (renames[part.id] ?? part.name).trim();
    if (!name || name === part.name) return;
    await fetch(api(`/api/parts/${part.id}`), {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name }),
    });
    loadDetail(selectedId);
  }

  async function onRerun() {
    await fetch(api(`/api/recordings/${selectedId}/rerun`), { method: "POST" });
    loadDetail(selectedId);
  }

  async function onDelete(id: string) {
    await fetch(api(`/api/recordings/${id}`), { method: "DELETE" });
    const recs = await loadRecordings();
    setSelectedId(recs[0]?.id || "");
  }

  useEffect(() => {
    if (!selectedId) return;
    try {
      const raw = localStorage.getItem(`backstep-guide-${selectedId}`);
      setGuideDone(raw ? (JSON.parse(raw) as Record<string, boolean>) : {});
    } catch {
      setGuideDone({});
    }
    setGuideIdx(0);
    setCheckResult(null);
  }, [selectedId]);

  const guideSteps = useMemo(() => {
    const steps = [...(detail?.steps || [])].sort((a, b) => b.order_index - a.order_index);
    return steps;
  }, [detail]);

  function markDone(stepId: string) {
    if (!selectedId) return;
    const next = { ...guideDone, [stepId]: true };
    setGuideDone(next);
    try {
      localStorage.setItem(`backstep-guide-${selectedId}`, JSON.stringify(next));
    } catch { /* private mode */ }
  }

  async function onCheckPhoto(stepId: string, file: File) {
    if (!selectedId) return;
    setCheckBusy(true);
    setCheckResult(null);
    try {
      const fd = new FormData();
      fd.append("step_id", stepId);
      fd.append("file", file);
      const r = await fetch(api(`/api/recordings/${selectedId}/check`), { method: "POST", body: fd });
      const j = await r.json();
      if (!r.ok) throw new Error(j.error || "check failed");
      setCheckResult(j);
      if (j.verdict === "matches") markDone(stepId);
    } catch (err) {
      setCheckResult({ verdict: "unsure", explanation: err instanceof Error ? err.message : "Check failed.", confidence: 0 });
    } finally {
      setCheckBusy(false);
    }
  }

  const rec = detail?.recording;

  return (
    <div className="shell">
      <header className="topbar">
        <div className="mark">
          <div className="mark-badge" aria-hidden>
            <svg width="22" height="22" viewBox="0 0 64 64">
              <rect x="6" y="10" width="52" height="44" rx="5" fill="none" stroke="#FAF7F2" strokeWidth="5" />
              <path d="M42 22 L26 32 L42 42" fill="none" stroke="#FF5A1F" strokeWidth="6" />
            </svg>
          </div>
          <div>
            <div className="wordmark">BACKSTEP</div>
            <div className="tagline">The rewind button for the real world.</div>
          </div>
        </div>
        <div className="topbar-spacer" />
        <span className={`pill ${health?.meta_api_configured ? "on" : "off"}`} title="Meta Model API status">
          {health === null
            ? "backend …"
            : health.meta_api_configured
              ? "Meta Model API · live"
              : "Meta Model API · key not set (sample mode)"}
        </span>
      </header>

      <div className="layout">
        <aside className="rail">
          <section>
            <h2>Memories</h2>
            {recordings.map((r) => (
              <button
                key={r.id}
                className={`rec-card ${r.id === selectedId ? "active" : ""}`}
                onClick={() => setSelectedId(r.id)}
              >
                <div className="rec-title">{r.title}</div>
                <div className="rec-meta">
                  {r.kind === "sample" ? <span className="badge sample">Sample</span> : <span className="badge status">Upload</span>}{" "}
                  <span className="badge status">{r.status}</span>{" "}
                  {r.frame_count} frames · {tc(r.duration_sec)}
                </div>
              </button>
            ))}
            {recordings.length === 0 && <div className="empty">No recordings yet. The backend may still be starting.</div>}
          </section>

          <section>
            <h2>New teardown</h2>
            <form className="upload-box" onSubmit={onUpload}>
              <div style={{ fontSize: 13 }}>
                Film yourself taking something apart (.mp4 / .mov, ≤ 200 MB). Backstep extracts the parts and steps.
              </div>
              <input ref={titleRef} type="text" placeholder="Title (optional — defaults to filename)" aria-label="Recording title" />
              <div className="file-row">
                <button type="button" className="file-btn" onClick={() => fileRef.current?.click()} disabled={uploading}>
                  Choose file
                </button>
                <span className={`file-name ${pickedFile ? "has-file" : ""}`} title={pickedFile?.name || ""}>
                  {pickedFile ? `${pickedFile.name} · ${fmtSize(pickedFile.size)}` : "No file chosen"}
                </span>
              </div>
              <input
                ref={fileRef}
                type="file"
                accept="video/mp4,video/quicktime,.mp4,.mov"
                aria-label="Video file"
                onChange={onPickFile}
                style={{ position: "absolute", width: 1, height: 1, opacity: 0, pointerEvents: "none" }}
                tabIndex={-1}
              />
              <div className="upload-actions">
                <button className="btn small" type="submit" disabled={uploading || !pickedFile}>
                  {uploading ? "Uploading…" : "Build memory"}
                </button>
              </div>
              {dupTitle && (
                <div className="dup-box">
                  A memory with this title already exists. Replace it with the new recording?
                  <div className="row">
                    <button className="btn small" type="submit" disabled={uploading}>
                      {uploading ? "Replacing…" : "Replace it"}
                    </button>
                    <button className="btn ghost small" type="button" onClick={() => setDupTitle(false)} disabled={uploading}>
                      Cancel
                    </button>
                  </div>
                </div>
              )}
              {uploadError && <div style={{ color: "var(--danger)", fontSize: 13, marginTop: 8 }}>{uploadError}</div>}
              {!health?.meta_api_configured && (
                <div style={{ fontSize: 12, color: "var(--ink-soft)", marginTop: 8 }}>
                  Without <span className="mono">MODEL_API_KEY</span> on the server, uploads stop after frame extraction — nothing is faked.
                </div>
              )}
            </form>
          </section>

          {rec && rec.kind !== "sample" && (
            <section>
              <h2>Manage</h2>
              <div style={{ display: "flex", gap: 8 }}>
                <button className="btn ghost small" onClick={onRerun}>Re-run analysis</button>
                <button className="btn ghost small" onClick={() => onDelete(rec.id)}>Delete</button>
              </div>
            </section>
          )}
        </aside>

        <main
          className="stage"
          onKeyDown={(e) => {
            if (e.key === "Escape" && tracking) cancelTrack();
            if (e.key === "Enter" && tracking && drag && (e.target as HTMLElement).tagName !== "INPUT") confirmTrack();
          }}
          tabIndex={-1}
        >
          {!rec ? (
            <div className="empty">Select a memory to begin.</div>
          ) : (
            <>
              <div className="stage-head">
                <h1>{rec.title}</h1>
                <span className="stage-sub mono">
                  {rec.status} · {frames.length} frames · {detail?.parts.length || 0} parts · {detail?.steps.length || 0} steps
                </span>
              </div>

              {rec.kind === "sample" && (
                <div className="notice">
                  <strong>Sample memory (precomputed).</strong> The frames below are labelled diagrams, not real
                  footage — they demonstrate the Manual, Finder, and evidence model. Upload your own teardown for
                  live SAM 3.1 + Muse Spark analysis.
                </div>
              )}
              {rec.status === "awaiting_key" && (
                <div className="notice">
                  Frames are extracted and ready. Set <span className="mono">MODEL_API_KEY</span> on the backend and
                  choose <strong>Re-run analysis</strong> to build the memory with Muse Spark + SAM 3.1.
                </div>
              )}
              {rec.status === "failed" && rec.error && (
                <div className="notice">Analysis failed: {rec.error}</div>
              )}
              {!["ready", "awaiting_key", "failed", "sample"].includes(rec.status) && rec.kind !== "sample" && (
                <div className="notice">Working… current stage: <span className="mono">{rec.status}</span>. This view refreshes automatically.</div>
              )}

              {tracking && (
                <div className="notice" style={{ borderColor: "var(--ink)" }}>
                  <strong>Tracking “{tracking.partName}”.</strong> Drag a box around the part on this frame, then confirm.
                  <span style={{ display: "flex", gap: 8, marginTop: 8, flexWrap: "wrap" }}>
                    <button className="btn small" onClick={confirmTrack} disabled={trackBusy || !drag}>
                      {trackBusy ? "Tracking…" : "Confirm box"}
                    </button>
                    <button className="btn ghost small" onClick={cancelTrack} disabled={trackBusy}>Cancel</button>
                  </span>
                  {trackMsg && <div style={{ marginTop: 8 }}>{trackMsg}</div>}
                </div>
              )}
              {!tracking && trackMsg && (
                <div className="notice">{trackMsg}</div>
              )}
              <div className={`frame-wrap ${tracking ? "tracking" : ""}`}>
                {currentFrame ? (
                  <>
                    {/* eslint-disable-next-line @next/next/no-img-element */}
                    <img ref={imgRef} className="frame" src={`${BACKEND}${currentFrame.url}`} alt={`Frame ${currentFrame.seq} at ${tc(currentFrame.ts_sec)}`} draggable={false} />
                    {tracking && (
                      <div
                        className="drag-layer"
                        onMouseDown={(e) => {
                          const p = toMedia(e);
                          if (p) setDrag({ x0: p.x, y0: p.y, x1: p.x, y1: p.y });
                        }}
                        onMouseMove={(e) => {
                          if (e.buttons !== 1) return;
                          const p = toMedia(e);
                          if (p) setDrag((d) => (d ? { ...d, x1: p.x, y1: p.y } : d));
                        }}
                        onMouseUp={(e) => {
                          const p = toMedia(e);
                          if (p) setDrag((d) => (d ? { ...d, x1: p.x, y1: p.y } : d));
                        }}
                      />
                    )}
                    {frameObjects.map((o) => (
                      <div
                        key={o.id}
                        className="obj-box"
                        style={{
                          left: `${(o.x1 / o.frame_w) * 100}%`,
                          top: `${(o.y1 / o.frame_h) * 100}%`,
                          width: `${((o.x2 - o.x1) / o.frame_w) * 100}%`,
                          height: `${((o.y2 - o.y1) / o.frame_h) * 100}%`,
                        }}
                      >
                        <span className="obj-label">
                          {partName(o.part_id) || o.concept}
                          {o.confidence !== null ? ` · ${o.confidence.toFixed(2)}` : ""}
                        </span>
                      </div>
                    ))}
                    {drag && currentFrame && (
                      <div
                        className="drag-box"
                        style={{
                          left: `${(Math.min(drag.x0, drag.x1) / currentFrame.width) * 100}%`,
                          top: `${(Math.min(drag.y0, drag.y1) / currentFrame.height) * 100}%`,
                          width: `${(Math.abs(drag.x1 - drag.x0) / currentFrame.width) * 100}%`,
                          height: `${(Math.abs(drag.y1 - drag.y0) / currentFrame.height) * 100}%`,
                        }}
                      />
                    )}
                    <span className="timecode">
                      TC {tc(currentFrame.ts_sec)} · F{currentFrame.seq}
                    </span>
                  </>
                ) : (
                  <div className="frame-empty">No frames yet. Upload a recording to build a memory.</div>
                )}
              </div>

              {frames.length > 0 && (
                <div className="timeline">
                  <input
                    type="range" min={0} max={frames.length - 1} value={Math.max(0, frames.findIndex((f) => f.seq === frameSeq))}
                    onChange={(e) => setFrameSeq(frames[Number(e.target.value)].seq)}
                    aria-label="Scrub timeline"
                  />
                  <div className="strip">
                    {frames.map((f) => (
                      <button key={f.seq} className={f.seq === currentFrame?.seq ? "active" : ""} onClick={() => setFrameSeq(f.seq)}>
                        {/* eslint-disable-next-line @next/next/no-img-element */}
                        <img src={`${BACKEND}${f.url}`} alt="" />
                        <span className="tc">{tc(f.ts_sec)}</span>
                      </button>
                    ))}
                  </div>
                </div>
              )}

              <div className="tabs" role="tablist">
                {(["guide", "manual", "finder", "parts", "trace"] as const).map((t) => (
                  <button key={t} role="tab" aria-selected={tab === t} className={`tab ${tab === t ? "active" : ""}`} onClick={() => setTab(t)}>
                    {t === "guide" ? "Guide ▸" : t === "manual" ? "Manual" : t === "finder" ? "Finder" : t === "parts" ? "Parts" : "Trace"}
                  </button>
                ))}
              </div>

              <div className="panel">
                {tab === "guide" && (
                  <>
                    {guideSteps.length === 0 ? (
                      <div className="empty">No steps yet. When the memory is ready, guided reassembly walks you through putting it back together — one step at a time.</div>
                    ) : (
                      (() => {
                        const idx = Math.min(guideIdx, guideSteps.length - 1);
                        const step = guideSteps[idx];
                        const orig = [...(detail?.steps || [])].sort((a, b) => a.order_index - b.order_index);
                        const manualOrder = orig.length - (step?.order_index ?? 0);
                        const d = step?.detail || undefined;
                        const doneCount = guideSteps.filter((st) => guideDone[st.id]).length;
                        return (
                          <div className="guide">
                            <div className="guide-progress">
                              <span className="mono">STEP {idx + 1} / {guideSteps.length}</span>
                              <span className="mono">{doneCount} done</span>
                            </div>
                            <div className="guide-bar"><div style={{ width: `${(doneCount / guideSteps.length) * 100}%` }} /></div>
                            <div className="guide-card">
                              <div className="guide-instruction">
                                {step?.reverse_action || (step ? `Reverse: ${step.action}` : "")}
                              </div>
                              {step && <div className="guide-original">Teardown was: {step.action}</div>}
                              {step?.state_before && <div className="guide-target">Target: {step.state_before}</div>}
                              {d?.uncertain && <div className="guide-warn">⚠ Uncertain — the memory is not sure about this step. Trust your eyes.</div>}
                              {d?.warnings && d.warnings.length > 0 && (
                                <div className="guide-warn">⚠ {d.warnings.join(" · ")}</div>
                              )}
                              {d?.orientation && <div className="guide-note">Orientation: {d.orientation}</div>}
                              {d?.tools && d.tools.length > 0 && (
                                <div className="guide-note">Tools: {d.tools.join(", ")}</div>
                              )}
                              {d?.note && <div className="guide-note">{d.note}</div>}
                              <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 12 }}>
                                <button className="btn ghost" onClick={() => seek(step?.evidence_frame_seq ?? null)}>
                                  Show evidence frame ▸
                                </button>
                                <button className="btn ghost" onClick={() => checkFileRef.current?.click()} disabled={checkBusy || !step}>
                                  {checkBusy ? "Checking…" : "Check my work 📷"}
                                </button>
                                <input
                                  ref={checkFileRef}
                                  type="file"
                                  accept="image/*"
                                  capture="environment"
                                  style={{ position: "absolute", width: 1, height: 1, opacity: 0, pointerEvents: "none" }}
                                  tabIndex={-1}
                                  onChange={(e) => {
                                    const f = e.target.files?.[0];
                                    if (f && step) onCheckPhoto(step.id, f);
                                    e.target.value = "";
                                  }}
                                />
                              </div>
                              {checkResult && (
                                <div className={`guide-verdict ${checkResult.verdict}`}>
                                  <strong>{checkResult.verdict === "matches" ? "✓ Looks right" : checkResult.verdict === "not_yet" ? "Not yet" : "Can't tell"}</strong>
                                  {" — "}{checkResult.explanation}
                                  <span className="mono"> ({checkResult.confidence.toFixed(2)})</span>
                                </div>
                              )}
                              <div className="guide-nav">
                                <button className="btn ghost big" disabled={idx === 0} onClick={() => { setGuideIdx(idx - 1); setCheckResult(null); }}>
                                  ← Back
                                </button>
                                <button
                                  className="btn big"
                                  onClick={() => {
                                    if (step) markDone(step.id);
                                    setCheckResult(null);
                                    if (idx < guideSteps.length - 1) setGuideIdx(idx + 1);
                                  }}
                                >
                                  {idx >= guideSteps.length - 1 ? (guideDone[step?.id || ""] ? "Done ✓" : "Mark done ✓") : "Done — next step →"}
                                </button>
                              </div>
                              <div className="guide-dots">
                                {guideSteps.map((st, i) => (
                                  <button
                                    key={st.id}
                                    className={`guide-dot ${i === idx ? "now" : ""} ${guideDone[st.id] ? "done" : ""}`}
                                    onClick={() => { setGuideIdx(i); setCheckResult(null); seek(st.evidence_frame_seq); }}
                                    aria-label={`Go to step ${i + 1}`}
                                  >
                                    {i + 1}
                                  </button>
                                ))}
                              </div>
                            </div>
                          </div>
                        );
                      })()
                    )}
                  </>
                )}

                {tab === "manual" && (
                  <>
                    {manual.length === 0 ? (
                      <div className="empty">No steps yet. When analysis completes, the reassembly manual appears here — teardown in reverse, each step pinned to its evidence frame.</div>
                    ) : (
                      manual.map((m) => (
                        <button key={m.step_id} className={`step-card ${m.evidence_frame_seq === frameSeq ? "active" : ""}`} onClick={() => seek(m.evidence_frame_seq)}>
                          <span className="step-num">{m.manual_order}</span>
                          <span className="step-body">
                            <span className="step-instruction">{m.instruction}</span>
                            <span className="step-original" style={{ display: "block" }}>Teardown was: {m.original_action}</span>
                            {m.state_target && <span className="step-original" style={{ display: "block" }}>Target state: {m.state_target}</span>}
                            {(() => {
                              const d = detail?.steps.find((st) => st.id === m.step_id)?.detail;
                              if (!d) return null;
                              return (
                                <span className="step-original" style={{ display: "block" }}>
                                  {d.uncertain ? "⚠ uncertain · " : ""}
                                  {d.tools && d.tools.length > 0 ? `tools: ${d.tools.join(", ")} · ` : ""}
                                  {d.orientation ? `orientation: ${d.orientation} · ` : ""}
                                  {d.warnings && d.warnings.length > 0 ? `⚠ ${d.warnings.join("; ")}` : ""}
                                </span>
                              );
                            })()}
                            <span className="step-evidence">▸ evidence frame {m.evidence_frame_seq ?? "—"} · click to view</span>
                          </span>
                        </button>
                      ))
                    )}
                  </>
                )}

                {tab === "finder" && (
                  <>
                    <div className="ask-row">
                      <input
                        value={question}
                        onChange={(e) => setQuestion(e.target.value)}
                        onKeyDown={(e) => { if (e.key === "Enter") onAsk(); }}
                        placeholder="Ask this memory… e.g. Where did I put the screw?"
                        aria-label="Ask the memory"
                      />
                      <button className="btn" onClick={() => onAsk()} disabled={asking}>{asking ? "Thinking…" : "Ask"}</button>
                    </div>
                    <div className="ask-examples">
                      {EXAMPLES.map((ex) => (
                        <button key={ex} className="chip" onClick={() => { setQuestion(ex); onAsk(ex); }}>{ex}</button>
                      ))}
                    </div>
                    {askResult && (
                      <div className="answer">
                        <div className={`verdict ${askResult.verified ? "ok" : "warn"}`}>
                          {askResult.verified
                            ? `✓ Verified against the memory index · source: ${askResult.source}`
                            : "⚠ Insufficient evidence — this is not asserted as fact"}
                        </div>
                        <pre>{askResult.answer}</pre>
                        {askResult.citations.length > 0 && (
                          <div className="citations">
                            {askResult.citations.map((c) => (
                              <button key={c.id} className="cite-btn" onClick={() => seek(c.frame_seq)}>
                                ▸ {c.label} · {tc(c.ts_sec)}
                              </button>
                            ))}
                          </div>
                        )}
                      </div>
                    )}
                    <p style={{ color: "var(--ink-soft)", fontSize: 13 }}>
                      Answers are grounded in this recording only. Every citation is validated against the index before
                      it is shown — an answer with no valid citation is refused, not embellished.
                    </p>
                  </>
                )}

                {tab === "parts" && (
                  <>
                    {(detail?.parts || []).length === 0 ? (
                      <div className="empty">No parts extracted yet.</div>
                    ) : (
                      detail!.parts.map((p) => (
                        <div key={p.id} className="part-row">
                          <div className="grow">
                            <strong>{p.name}</strong>{" "}
                            {p.confirmed && <span className="badge sample">confirmed</span>}
                            <div className="mono" style={{ fontSize: 11, color: "var(--ink-soft)" }}>
                              SAM prompt: “{p.sam_prompt}” · seen {tc(p.first_seen_sec)} → {tc(p.last_seen_sec)}
                            </div>
                            {(() => {
                              const sight = (detail?.objects || []).filter((o) => o.part_id === p.id);
                              const frames = [...new Set(sight.map((o) => o.frame_seq))].sort((a, b) => a - b);
                              if (frames.length === 0) return null;
                              return (
                                <div style={{ marginTop: 6, display: "flex", gap: 5, flexWrap: "wrap", alignItems: "center" }}>
                                  <span className="mono" style={{ fontSize: 11, color: "var(--teal)" }}>sightings:</span>
                                  {frames.map((f) => (
                                    <button key={f} className="cite-btn" onClick={() => seek(f)}>F{f} · {tc(frames.length ? (detail?.frames.find((fr) => fr.seq === f)?.ts_sec ?? null) : null)}</button>
                                  ))}
                                </div>
                              );
                            })()}
                          </div>
                          <input
                            value={renames[p.id] ?? p.name}
                            onChange={(e) => setRenames({ ...renames, [p.id]: e.target.value })}
                            aria-label={`Rename ${p.name}`}
                          />
                          <button className="btn ghost small" onClick={() => onRename(p)}>Rename &amp; confirm</button>
                          <button className="btn ghost small" onClick={() => seek(p.evidence_frame_seq)}>Evidence ▸</button>
                          <button
                            className="btn ghost small"
                            onClick={() => (tracking?.partId === p.id ? cancelTrack() : startTrack(p))}
                            title="Draw a box on the frame so SAM can lock onto this part"
                          >
                            {tracking?.partId === p.id ? "Tracking… ×" : "Track ▣"}
                          </button>
                        </div>
                      ))
                    )}
                    <p style={{ color: "var(--ink-soft)", fontSize: 13 }}>
                      Wrong label? Rename it. Human corrections are recorded in the Trace and make the memory trustworthy.
                    </p>
                  </>
                )}

                {tab === "trace" && (
                  <div className="trace">
                    {(detail?.trace || []).length === 0 ? (
                      <div className="empty">No trace events yet.</div>
                    ) : (
                      detail!.trace.map((t) => (
                        <div key={t.id} className="trace-row">
                          <span className="trace-stage">{t.stage}</span>
                          <span>
                            {t.message}
                            {t.detail ? <span style={{ color: "var(--ink-soft)" }}> — {t.detail}</span> : null}
                          </span>
                        </div>
                      ))
                    )}
                  </div>
                )}
              </div>

              <div className="footer-note">
                Built on Meta Model API — Muse Spark 1.3 reads the recording, SAM 3.1 locates parts, Muse Wonder
                writes the reverse manual. Every answer carries its evidence.{" "}
                {rec.kind === "sample" ? "You are viewing the labelled sample memory." : ""}
              </div>
            </>
          )}
        </main>
      </div>
    </div>
  );
}
