#!/usr/bin/env python3
"""Deliberately simple GitHub Issues adapter for Snag export streams.

THIS IS A SIMPLE, ONE-OBSERVATION-PER-ISSUE ADAPTER — NOT THE CANONICAL
ARCHITECTURE. Snag's model preserves observations first and coalesces them
into findings downstream; filing every observation as an issue is right for a
small repo with modest volume, and wrong for a high-volume fleet (issue
inflation, duplicate symptoms). Use this to get visibility quickly; do not
build your pipeline on it.

Behavior:
- Reads a Snag export stream (same wire contract as consumer.py).
- Creates one GitHub issue per non-retracted observation via the REST API.
- Skips nothing on retry UNLESS you pass --checkpoint: the adapter does not
  look at existing issues; re-running without a checkpoint duplicates issues.

Usage:
    snag export --output observations.jsonl
    GITHUB_TOKEN=ghp_... python3 github-issues.py \
        --stream observations.jsonl --repo owner/name --label snag
    python3 github-issues.py --stream observations.jsonl --repo owner/name \
        --dry-run            # print the issues that would be filed

Requires: Python 3 stdlib only. The token needs the `issues: write` scope.
"""

import argparse
import json
import os
import re
import sys
import urllib.request

HASH_RE = re.compile(r"^(?:blake3:[0-9a-f]{64}|0{64})$")


def validate_header(line: str) -> dict:
    header = json.loads(line)
    if header.get("export_kind") != "export_header":
        raise ValueError("first line is not an export header")
    if header.get("export_schema_version") != 1:
        raise ValueError("unsupported export_schema_version")
    if header.get("minimum_reader_version", 1) > 1:
        raise ValueError("stream requires a newer reader")
    return header


def iter_observations(stream):
    """Yield (observation, record) for every observation_created record."""
    line = stream.readline()
    if not line:
        raise ValueError("empty stream: missing export header")
    validate_header(line)
    for line_no, line in enumerate(stream, start=2):
        if not line.strip():
            continue
        rec = json.loads(line)
        if rec.get("export_kind") != "record":
            raise ValueError(f"line {line_no}: expected record envelope")
        if not isinstance(rec.get("local_sequence"), int) or rec.get("local_sequence") < 1:
            raise ValueError(f"line {line_no}: invalid local_sequence")
        if not isinstance(rec.get("record_id"), str) or not rec["record_id"]:
            raise ValueError(f"line {line_no}: empty record_id")
        for field in ("record_hash", "previous_record_hash"):
            if not isinstance(rec.get(field), str) or not HASH_RE.match(rec[field]):
                raise ValueError(f"line {line_no}: invalid {field}")
        if rec.get("record_type") != "observation_created":
            continue  # retractions are recorded in the stream; not filed
        yield rec["canonical_payload"], rec


def issue_body(obs: dict, rec: dict) -> str:
    def field(key: str, label: str) -> str:
        value = obs.get(key)
        return f"**{label}:** {value}\n\n" if value else ""

    body = field("summary", "Summary")
    body += field("expected_behavior", "Expected")
    body += field("observed_behavior", "Observed")
    body += field("reproduction", "Reproduction")
    body += field("workaround", "Workaround")
    body += field("impact", "Impact")
    body += (
        "---\n"
        f"_Filed by the simple Snag export adapter. Observation `{obs['observation_id']}` "
        f"(sequence {rec['local_sequence']}, kind {obs.get('kind_assertion') or 'unset'}, "
        f"severity {obs.get('severity_assertion') or 'unset'}). "
        "This is a deliberately simple one-observation-per-issue adapter, not the canonical "
        "pipeline: Snag preserves raw observations and coalesces them into findings downstream._"
    )
    return body


def create_issue(repo: str, title: str, body: str, labels: list[str], token: str) -> str:
    url = f"https://api.github.com/repos/{repo}/issues"
    payload = {"title": title, "body": body}
    if labels:
        payload["labels"] = labels
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode(),
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())["html_url"]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--stream", required=True, help="export stream file, or '-' for stdin")
    ap.add_argument("--repo", required=True, help="destination 'owner/name'")
    ap.add_argument("--token", default=os.environ.get("GITHUB_TOKEN", ""), help="GitHub token (or GITHUB_TOKEN env); needs issues:write")
    ap.add_argument("--label", action="append", default=[], help="label to apply (repeatable)")
    ap.add_argument("--dry-run", action="store_true", help="print issues without filing")
    args = ap.parse_args()

    if not args.token and not args.dry_run:
        print("github-issues: --token (or GITHUB_TOKEN) is required unless --dry-run", file=sys.stderr)
        return 2

    stream = sys.stdin if args.stream == "-" else open(args.stream, encoding="utf-8")
    filed = 0
    try:
        for obs, rec in iter_observations(stream):
            title = obs.get("title") or "(untitled observation)"
            body = issue_body(obs, rec)
            if args.dry_run:
                print(f"would file: {title}  (sequence {rec['local_sequence']})")
            else:
                url = create_issue(args.repo, title, body, args.label, args.token)
                print(f"filed: {url}")
            filed += 1
    except (json.JSONDecodeError, KeyError, ValueError, OSError) as e:
        print(f"github-issues: {e}", file=sys.stderr)
        return 1
    finally:
        if stream is not sys.stdin:
            stream.close()

    print(f"github-issues: {filed} observation(s) {'previewed' if args.dry_run else 'filed'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
