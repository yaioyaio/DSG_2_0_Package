#!/usr/bin/env python3
"""Read a finalized result using the supervisor's independently retained archive pin."""
import argparse
import hashlib
import json
import os
import re
import stat

DIGEST = re.compile(r"[0-9a-f]{64}")
FULL_ID = re.compile(r"[a-z0-9]{25}")
NATIVE = {"containerd.db", "history.db", "metadata_v2.db", "snapshots/metadata.db", "solver.db"}
LIMIT = 64 * 1024


def require(condition):
    if not condition:
        raise ValueError("preserve")


def unique(pairs):
    result = dict(pairs)
    require(len(result) == len(pairs))
    return result


def is_integer(value, expected):
    return type(value) is int and value == expected


def inspect_archive(path, pin, custody, record):
    require(DIGEST.fullmatch(pin) and DIGEST.fullmatch(custody) and FULL_ID.fullmatch(record))
    fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as archive:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and 0 < info.st_size <= LIMIT)
        data = archive.read(LIMIT + 1)
        require(len(data) == info.st_size and hashlib.sha256(data).hexdigest() == pin)
    result = json.loads(data, object_pairs_hook=unique)
    require(result["custody_sha256"] == custody and result["record_id"] == record)
    purpose = result["purpose"]
    require(purpose in ("candidate", "controller_rehearsal") and is_integer(result["guest_response_code"], 0))
    require(is_integer(result["exact_mutation_calls"], 1) and is_integer(result["receipt_recovery_mutation_calls"], 0))
    attempt = result["attempt"]
    applied, recovered = attempt["application"], attempt["recovered"]
    receipt = recovered["receipt"]
    require(is_integer(applied["code"], 0) and is_integer(recovered["code"], 0))
    canonical = json.dumps(receipt, sort_keys=True, separators=(",", ":")).encode()
    require(json.dumps(applied["receipt"], sort_keys=True, separators=(",", ":")).encode() == canonical)
    require(receipt.keys() == {"Request", "LogicalRecords", "PhysicalBytes"})
    require(is_integer(receipt["LogicalRecords"], 1) and receipt["PhysicalBytes"] is None)
    request = receipt["Request"]
    digests = {"Project", "Generation", "MetadataDigest", "Origin", "Intent", "Approval"}
    require(request.keys() == digests | {"RecordID", "Worker", "ApprovedAtMS", "ExpiresAtMS"})
    require(request["RecordID"] == record and FULL_ID.fullmatch(request["Worker"]))
    require(all(DIGEST.fullmatch(request[key]) for key in digests))
    start, end = request["ApprovedAtMS"], request["ExpiresAtMS"]
    require(type(start) is int and type(end) is int and start > 0 and 0 < end - start <= 30 * 60_000)
    post = attempt["postconditions"]
    require(post["byte_identical_after_receipt_only"] is True and post["same_read_only_mount_after_recovery"] is True)
    require(is_integer(post["database_files_checked"], len(NATIVE)))
    require(sorted(post["database_files_changed"]) == sorted(NATIVE - {"history.db"}))
    hashes = post["completion_store_hashes"]
    require(DIGEST.fullmatch(hashes["authority"]) and DIGEST.fullmatch(hashes["journal"]))
    require(hashes["native"].keys() == NATIVE and all(DIGEST.fullmatch(v) for v in hashes["native"].values()))
    reply = {"status": "archived_settled_receipt", "record_id": record,
             "receipt_sha256": hashlib.sha256(canonical).hexdigest(), "mutation_calls": 0}
    if purpose == "controller_rehearsal":
        reply["purpose"] = purpose
    return reply


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True)
    parser.add_argument("--archive-sha256", required=True)
    parser.add_argument("--custody-sha256", required=True)
    parser.add_argument("--record-id", required=True)
    args = parser.parse_args()
    try:
        result = inspect_archive(args.archive, args.archive_sha256, args.custody_sha256, args.record_id)
    except (KeyError, TypeError, ValueError, OSError, AttributeError, RecursionError):
        print(json.dumps({"status": "preserve", "mutation_calls": 0}))
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
