#!/usr/bin/env python3
"""Per-memory Finder/Manual benchmark.

Resolves recordings by title substring, asks labelled questions, and checks:
  - verified flag matches expectation
  - at least one citation whose label contains the expected substring
  - optionally, the first citation's frame_seq

Usage: python3 scripts/bench_memories.py [base_url]
Exits non-zero if any case fails.
"""
import json
import sys
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8787"

# memory title substring -> [(question, expect_verified, label_substr|None, frame|None)]
SUITES = {
    "brass clock": [
        ("Where did I put the retaining screw?", True, "Retaining Screw", 22),
        ("Where is the brass movement block?", True, "Brass", 33),
        ("How does this go back together?", True, "Step", None),
        ("What did you do with the tweezers?", True, "tweezers", None),
        ("Where are my car keys?", False, None, None),
        ("Where is the hammer?", False, None, None),
    ],
    "desktop computer tower": [
        ("Where is the drive bay screw?", True, "drive bay screw", 5),
        ("Where is the CPU cooling fan?", True, "fan", 22),
        ("How does this go back together?", True, "Step", None),
        ("Which screwdriver was used?", True, "screwdriver", None),
        ("Where is the graphics card?", False, None, None),
        ("Where did I put my phone?", False, None, None),
    ],
    "CPU Cooler and Processor": [
        ("Where did I put the CPU processor?", True, "CPU processor", 18),
        ("Where is the heatsink fan?", True, "heatsink", 2),
        ("How does this go back together?", True, "Step", None),
        ("Where are the RAM sticks?", False, None, None),
    ],
    "kite control bar": [
        ("Where did I put the trim cleat screw?", True, "Trim cleat screw", 5),
        ("Where is the chicken loop?", True, "Chicken loop", None),
        ("How does this go back together?", True, "Step", None),
        ("Where are my car keys?", False, None, None),
        ("What is the capital of France?", False, None, None),
    ],
}


def get(path: str):
    with urllib.request.urlopen(f"{BASE}{path}", timeout=30) as r:
        return json.load(r)


def ask(rec_id: str, question: str) -> dict:
    req = urllib.request.Request(
        f"{BASE}/api/recordings/{rec_id}/ask",
        data=json.dumps({"question": question}).encode(),
        headers={"content-type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.load(resp)


def main() -> int:
    recordings = get("/api/recordings")["recordings"]
    total = passed = 0
    failures = []
    for title_sub, cases in SUITES.items():
        rec = next(
            (r for r in recordings if title_sub.lower() in r["title"].lower()), None
        )
        if rec is None:
            print(f"[SKIP] no recording matching '{title_sub}'")
            continue
        print(f"## {rec['title']} ({rec['status']})")
        for q, expect_verified, label, frame in cases:
            total += 1
            try:
                r = ask(rec["id"], q)
            except Exception as e:  # noqa: BLE001
                print(f"[ERROR] {q}  ({e})")
                failures.append((title_sub, q, f"http error: {e}"))
                continue
            ok = r.get("verified") == expect_verified
            if ok and expect_verified:
                labels = [c.get("label", "") for c in r.get("citations", [])]
                ok = bool(labels) and (
                    label is None
                    or any(label.lower() in lb.lower() for lb in labels)
                )
                if ok and frame is not None:
                    first = (r.get("citations") or [{}])[0]
                    ok = first.get("frame_seq") == frame
            if ok and not expect_verified:
                ok = bool(r.get("insufficient_evidence"))
            passed += 1 if ok else 0
            detail = (
                f"verified={r.get('verified')} "
                f"citations={len(r.get('citations', []))} "
                f"frames={[c.get('frame_seq') for c in r.get('citations', [])]} "
                f"source={r.get('source')}"
            )
            print(f"[{'PASS' if ok else 'FAIL'}] {q}  ({detail})")
            if not ok:
                failures.append((title_sub, q, detail))
    print(f"\n{passed}/{total} passed")
    for title_sub, q, detail in failures:
        print(f"  FAIL [{title_sub}] {q} ({detail})")
    return 0 if passed == total else 1


if __name__ == "__main__":
    raise SystemExit(main())
