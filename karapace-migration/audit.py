#!/usr/bin/env python3
"""
Karapace audit script — Phase 0 of the Karapace → Kora migration.

Produces a JSON snapshot of all schemas, subjects, versions, configs, and
a dedup-collision report (cases where two subjects share schema content but
Karapace assigned different IDs, which Kora would collapse into one).

Usage:
    python3 audit.py <karapace_url> [--output PATH] [--workers N]

Environment variables (used as fallback when CLI args are not provided):
    KARAPACE_URL       Karapace base URL
    KARAPACE_USER      BasicAuth username
    KARAPACE_PASSWORD  BasicAuth password
    AUDIT_OUTPUT       Output file path (default: audits/{hostname}-{date}.json)
"""

import argparse
import json
import os
import sys
import urllib.parse
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from pathlib import Path

import urllib.request
import urllib.error
from typing import Any

AUDITS_DIR = Path(__file__).parent / "audits"
HTTP_TIMEOUT = 30


def default_output_path(url: str) -> str:
    hostname = urllib.parse.urlparse(url).hostname or url
    date = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H%M%SZ")
    AUDITS_DIR.mkdir(exist_ok=True)
    return str(AUDITS_DIR / f"{hostname}-{date}.json")


_opener: urllib.request.OpenerDirector = urllib.request.build_opener()


def configure_auth(user: str, password: str, base_url: str) -> None:
    mgr = urllib.request.HTTPPasswordMgrWithDefaultRealm()
    mgr.add_password(None, base_url, user, password)
    handler = urllib.request.HTTPBasicAuthHandler(mgr)
    global _opener
    _opener = urllib.request.build_opener(handler)


def get(base_url: str, path: str) -> Any:
    url = base_url.rstrip("/") + path
    try:
        with _opener.open(url, timeout=HTTP_TIMEOUT) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as e:
        body = e.read().decode()
        try:
            data = json.loads(body)
        except Exception:
            data = body
        return {"_http_error": e.code, "_body": data}
    except urllib.error.URLError as e:
        return {"_http_error": 0, "_body": str(e.reason)}


def is_error(obj: Any) -> bool:
    return isinstance(obj, dict) and "_http_error" in obj


def fetch_subject(base: str, subject: str, deleted_subject_names: set[str]) -> dict:
    """Fetch all data for one subject via HTTP. Safe to call from a worker thread."""
    subj_entry: dict = {
        "name": subject,
        "deleted": subject in deleted_subject_names,
        "config": None,
        "versions": [],
    }

    config_resp = get(base, f"/config/{subject}")
    if not is_error(config_resp):
        subj_entry["config"] = config_resp.get("compatibilityLevel")

    versions_resp = get(base, f"/subjects/{subject}/versions")
    active_versions = versions_resp if not is_error(versions_resp) else []

    versions_deleted_resp = get(base, f"/subjects/{subject}/versions?deleted=true")
    all_versions = versions_deleted_resp if not is_error(versions_deleted_resp) else active_versions

    for version in sorted(all_versions):
        ver_resp = get(base, f"/subjects/{subject}/versions/{version}")
        if is_error(ver_resp):
            print(f"  WARNING: could not fetch {subject} v{version}: {ver_resp}", file=sys.stderr)
            continue

        subj_entry["versions"].append({
            "version": version,
            "schema_id": ver_resp.get("id"),
            "schema_type": ver_resp.get("schemaType", "AVRO"),  # Karapace omits type for AVRO
            "schema_text": ver_resp.get("schema"),
            "references": ver_resp.get("references") or [],
            "deleted": version not in active_versions,
        })

    return subj_entry


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Audit a Karapace schema registry",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("url", nargs="?", default=os.environ.get("KARAPACE_URL"),
                        help="Base URL, e.g. https://karapace.example.com (env: KARAPACE_URL)")
    parser.add_argument("--output", default=os.environ.get("AUDIT_OUTPUT"),
                        help="Output file (env: AUDIT_OUTPUT, default: audits/{hostname}-{date}.json)")
    parser.add_argument("--user", default=os.environ.get("KARAPACE_USER"),
                        help="BasicAuth username (env: KARAPACE_USER)")
    parser.add_argument("--password", default=os.environ.get("KARAPACE_PASSWORD"),
                        help="BasicAuth password (env: KARAPACE_PASSWORD)")
    parser.add_argument("--workers", type=int, default=20,
                        help="Parallel workers for per-subject fetching (default: 20)")
    args = parser.parse_args()

    if not args.url:
        parser.error("url is required (or set KARAPACE_URL)")

    base = args.url.rstrip("/")

    if not args.output:
        args.output = default_output_path(base)

    if args.user and args.password:
        configure_auth(args.user, args.password, base)
    elif args.user or args.password:
        parser.error("--user and --password must be provided together "
                     "(or set both KARAPACE_USER and KARAPACE_PASSWORD)")

    print(f"Auditing {base} ...", file=sys.stderr)

    # --- Global config ---
    global_config = get(base, "/config")
    print(f"  Global config: {global_config}", file=sys.stderr)

    # --- Subject list ---
    active_subjects = get(base, "/subjects")
    deleted_subjects_resp = get(base, "/subjects?deleted=true")

    all_subjects_set = set(active_subjects if not is_error(active_subjects) else [])
    if not is_error(deleted_subjects_resp):
        all_subjects_set.update(deleted_subjects_resp)
    all_subjects = sorted(all_subjects_set)
    deleted_subject_names = all_subjects_set - set(active_subjects if not is_error(active_subjects) else [])

    print(f"  Subjects: {len(all_subjects)} total ({len(deleted_subject_names)} soft-deleted)", file=sys.stderr)
    print(f"  Fetching with {args.workers} workers ...", file=sys.stderr)

    # --- Parallel per-subject fetch ---
    # Workers do all HTTP I/O; main thread merges results in sorted order.
    subject_results: dict[str, dict] = {}
    with ThreadPoolExecutor(max_workers=args.workers) as executor:
        futures = {
            executor.submit(fetch_subject, base, subject, deleted_subject_names): subject
            for subject in all_subjects
        }
        completed = 0
        for future in as_completed(futures):
            subject = futures[future]
            subject_results[subject] = future.result()
            completed += 1
            print(f"  [{completed}/{len(all_subjects)}] {subject}", file=sys.stderr)

    # --- Merge in sorted order (deterministic output) ---
    subjects_data: list[dict] = []
    schemas_by_id: dict[int, dict] = {}
    content_to_ids: dict[str, list[int]] = defaultdict(list)

    for subject in all_subjects:
        subj_entry = subject_results[subject]
        subjects_data.append(subj_entry)

        for ver in subj_entry["versions"]:
            schema_id = ver["schema_id"]
            schema_text = ver["schema_text"]

            if schema_id not in schemas_by_id:
                schemas_by_id[schema_id] = {
                    "id": schema_id,
                    "schema_type": ver["schema_type"],
                    "schema_text": schema_text,
                    "references": ver["references"],
                    "subject_versions": [],
                }
            schemas_by_id[schema_id]["subject_versions"].append({
                "subject": subject,
                "version": ver["version"],
                "deleted": ver["deleted"],
            })

            try:
                canonical = json.dumps(json.loads(schema_text), sort_keys=True)
            except Exception:
                canonical = schema_text  # non-JSON schema (e.g. Protobuf .proto text)
            content_to_ids[canonical].append(schema_id)

    # --- Dedup collision report ---
    collisions = []
    for canonical, ids in content_to_ids.items():
        unique_ids = list(dict.fromkeys(ids))
        if len(unique_ids) > 1:
            collisions.append({"canonical_content": canonical, "ids": unique_ids})

    if collisions:
        print(f"  WARNING: {len(collisions)} dedup collision(s) found — review before migrating!", file=sys.stderr)
    else:
        print("  No dedup collisions found.", file=sys.stderr)

    # --- Summary ---
    has_references = any(ver["references"] for s in subjects_data for ver in s["versions"])
    summary = {
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "source_url": base,
        "subject_count": len(all_subjects),
        "subject_count_active": len(all_subjects) - len(deleted_subject_names),
        "subject_count_deleted": len(deleted_subject_names),
        "schema_count": len(schemas_by_id),
        "version_count": sum(len(s["versions"]) for s in subjects_data),
        "has_references": has_references,
        "dedup_collision_count": len(collisions),
        "global_config": global_config,
    }

    report = {
        "summary": summary,
        "subjects": subjects_data,
        "schemas_by_id": {str(k): v for k, v in sorted(schemas_by_id.items())},
        "dedup_collisions": collisions,
    }

    with open(args.output, "w") as f:
        json.dump(report, f, indent=2)

    print(f"\nAudit complete. Written to {args.output}", file=sys.stderr)
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
