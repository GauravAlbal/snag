#!/usr/bin/env python3
"""Minimal downstream consumer for the Snag export stream.

The export stream is the public API for anything that wants to read Snag
observations. This consumer is intentionally small and stdlib-only: it
parses the header, validates every record, checkpoints the last sequence
and hash, and exits nonzero on any violation.

Contract (see ../../docs/STABILITY.md and ../../schemas/export-stream-v1.schema.json):
- Line 1 is the export header (export_kind == "export_header").
- Every later line is a record envelope (export_kind == "record").
- local_sequence is strictly contiguous (+1 per record).
- record_hash / previous_record_hash look like blake3:<64 hex> or 64 zeros.

Usage:
    snag export --output stream.jsonl
    python3 consumer.py --stream stream.jsonl                    # one-shot
    snag export | python3 consumer.py --stream -                 # via pipe
    python3 consumer.py --stream stream.jsonl --checkpoint state.json
    python3 consumer.py --stream more.jsonl --checkpoint state.json \
        --after-sequence 42          # resume a poll loop

The checkpoint file stores the last validated sequence and hash; a polling
loop writes it after each successful pass and resumes with --after-sequence.
"""

import argparse
import json
import re
import sys

HASH_RE = re.compile(r"^(?:blake3:[0-9a-f]{64}|0{64})$")


def validate_hash(field: str, value: str) -> None:
    if not isinstance(value, str) or not HASH_RE.match(value):
        raise ValueError(f"{field}: invalid record hash {value!r}")


def parse_stream(stream, after_sequence: int):
    """Yield (kind, doc) for header and records; enforce the wire contract.

    `after_sequence` skips records with local_sequence <= N (resumable
    polling). The header is always required and always validated.
    """
    line = stream.readline()
    if not line:
        raise ValueError("empty stream: missing export header")
    header = json.loads(line)
    if header.get("export_kind") != "export_header":
        raise ValueError("first line is not an export header")
    if header.get("export_schema_version") != 1:
        raise ValueError(
            f"unsupported export_schema_version {header.get('export_schema_version')}"
        )
    if header.get("minimum_reader_version", 1) > 3:
        raise ValueError(
            f"stream requires reader version {header.get('minimum_reader_version')}; "
            "this consumer supports <= 3"
        )
    validate_hash("previous_checkpoint_hash", header["previous_checkpoint_hash"])
    validate_hash("head_record_hash", header["head_record_hash"])
    yield "header", header

    expected_seq = header["first_sequence"]
    for line_no, line in enumerate(stream, start=2):
        if not line.strip():
            continue  # tolerate a single trailing newline
        rec = json.loads(line)
        if rec.get("export_kind") != "record":
            raise ValueError(f"line {line_no}: expected record envelope")
        if rec.get("record_schema_version") != 1:
            raise ValueError(
                f"line {line_no}: unsupported record_schema_version "
                f"{rec.get('record_schema_version')}"
            )
        seq = rec.get("local_sequence")
        if not isinstance(seq, int) or seq < 1:
            raise ValueError(f"line {line_no}: invalid local_sequence {seq!r}")
        if seq != expected_seq:
            raise ValueError(
                f"line {line_no}: sequence gap — expected {expected_seq}, got {seq}"
            )
        if not isinstance(rec.get("record_id"), str) or not rec["record_id"]:
            raise ValueError(f"line {line_no}: empty or missing record_id")
        validate_hash("previous_record_hash", rec["previous_record_hash"])
        validate_hash("record_hash", rec["record_hash"])
        expected_seq = seq + 1
        if seq > after_sequence:
            yield "record", rec


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--stream", required=True, help="export stream file, or '-' for stdin")
    ap.add_argument("--checkpoint", help="write {last_sequence, last_record_hash} here on success")
    ap.add_argument("--after-sequence", type=int, default=0, help="skip records with local_sequence <= N")
    args = ap.parse_args()

    stream = sys.stdin if args.stream == "-" else open(args.stream, encoding="utf-8")
    last_seq = 0
    last_hash = None
    count = 0
    try:
        for kind, doc in parse_stream(stream, args.after_sequence):
            if kind == "record":
                last_seq = doc["local_sequence"]
                last_hash = doc["record_hash"]
                count += 1
    except (json.JSONDecodeError, KeyError, ValueError) as e:
        print(f"consumer: stream invalid: {e}", file=sys.stderr)
        return 1
    finally:
        if stream is not sys.stdin:
            stream.close()

    if args.checkpoint:
        with open(args.checkpoint, "w", encoding="utf-8") as f:
            json.dump({"last_sequence": last_seq, "last_record_hash": last_hash}, f)
    print(f"consumer: validated {count} records (last sequence {last_seq})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
