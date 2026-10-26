#!/usr/bin/env python3
"""Backstep query benchmark — runs labelled questions against a recording's
Finder/Manual endpoints and checks verified citations.

Usage:
  python3 scripts/benchmark.py [base_url] [recording_id]

Requires the backend running. Exits non-zero if any case fails.
Every case checks: (a) verified flag matches expectation, (b) when verified,
at least one citation exists and its label contains the expected text.
"""
import json
import sys
import urllib.request

BASE = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8787"
REC = sys.argv[2] if len(sys.argv) > 2 else "sample-kite-bar"

# (question, expect_verified, expected_citation_label_substring or None)
CASES = [
    ("Where did I put the trim cleat screw?", True, "Trim cleat screw"),
    ("Where is the chicken loop?", True, "Chicken loop"),
    ("Where did I put the quick-release cap?", True, "Quick-release cap"),
    ("Where is the safety line?", True, "Safety line"),
    ("Where is the orange float?", True, "Bar float"),
    ("How does this go back together?", True, "Step"),
    ("How do I reassemble the control bar?", True, "Step"),
    ("What happened to the safety line?", True, "Safety line"),
    ("When was the chicken loop removed?", True, "Chicken loop"),
    ("What was done with the left bar float?", True, "Bar float"),
    ("Where is the red cap?", True, "Quick-release cap"),
    ("Where did the safety line go?", True, "Safety line"),
    ("Which step unloops the chicken loop?", True, "chicken loop"),
    ("Show me the trim cleat screw", True, "Trim cleat screw"),
    ("Where was the screw placed?", True, "Trim cleat screw"),
    ("Where is the left float?", True, "Bar float"),
    ("Is there a purple helmet in the video?", False, None),
    ("Where are my car keys?", False, None),
    ("What is the capital of France?", False, None),
    ("Where did I put the donkey stick?", False, None),
]


def ask(question: str) -> dict:
    req = urllib.request.Request(
        f"{BASE}/api/recordings/{REC}/ask",
        data=json.dumps({"question": question}).encode(),
        headers={"content-type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.load(resp)


def main() -> int:
    passed = 0
    rows = []
    for q, expect_verified, expect_label in CASES:
        try:
            r = ask(q)
        except Exception as e:  # noqa: BLE001
            rows.append((q, "ERROR", str(e)))
            continue
        ok = r.get("verified") == expect_verified
        if ok and expect_verified:
            labels = [c.get("label", "") for c in r.get("citations", [])]
            ok = bool(labels) and (
                expect_label is None
                or any(expect_label.lower() in lb.lower() for lb in labels)
            )
        if ok and not expect_verified:
            ok = bool(r.get("insufficient_evidence"))
        passed += 1 if ok else 0
        rows.append((q, "PASS" if ok else "FAIL", f"verified={r.get('verified')} citations={len(r.get('citations', []))} source={r.get('source')}"))
    for q, status, detail in rows:
        print(f"[{status}] {q}  ({detail})")
    print(f"\n{passed}/{len(CASES)} passed against recording '{REC}' at {BASE}")
    return 0 if passed == len(CASES) else 1


if __name__ == "__main__":
    raise SystemExit(main())
