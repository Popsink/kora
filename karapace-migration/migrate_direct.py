#!/usr/bin/env python3
"""
Kora direct-PostgreSQL migration script — for production use when schema IDs
are not contiguous from 1 (i.e. the API-based approach cannot be used).

Reads an audit snapshot and writes directly to Kora's PostgreSQL database,
inserting schema_contents rows with explicit IDs to preserve the original
Karapace IDs exactly, regardless of gaps.

Requirements:
    pip install fastavro psycopg2-binary

Usage:
    python3 migrate_direct.py <db_url> [--audit PATH] [--dry-run]

    db_url format:  postgresql://user:password@host:port/dbname

Pre-flight checks:
  - Kora DB must be empty (no existing schema_contents rows)
  - audit must have zero dedup collisions

Environment variables (used as fallback when CLI args are not provided):
    KORA_DB_URL  PostgreSQL connection URL
    AUDIT_FILE   Audit JSON path (default: latest file in audits/)
"""

import argparse
import hashlib
import json
import os
import sys
from pathlib import Path
from typing import Any

import fastavro.schema as fa_schema
import psycopg2
import psycopg2.extras

AUDITS_DIR = Path(__file__).parent / "audits"


def find_latest_audit() -> str | None:
    files = sorted(AUDITS_DIR.glob("*.json"), key=lambda p: p.stat().st_mtime, reverse=True)
    return str(files[0]) if files else None


# -- Fingerprint helpers --

def compute_fingerprints(schema_text: str, schema_type: str) -> tuple[str, str, str]:
    """
    Returns (canonical_form, fingerprint, raw_fingerprint) matching Kora's logic:
      - raw_fingerprint : SHA-256 hex of the raw schema text
      - canonical_form  : Avro PCF for AVRO; raw text for JSON/PROTOBUF
      - fingerprint     : CRC-64-AVRO (Rabin) of PCF for AVRO; SHA-256 of PCF for others
    """
    raw_fingerprint = hashlib.sha256(schema_text.encode()).hexdigest()

    if schema_type == "AVRO":
        canonical_form = fa_schema.to_parsing_canonical_form(json.loads(schema_text))
        fingerprint = fa_schema.fingerprint(canonical_form, "CRC-64-AVRO")
    else:
        # JSON Schema and Protobuf: canonical = raw text, fingerprint = SHA-256 of raw
        canonical_form = schema_text
        fingerprint = raw_fingerprint

    return canonical_form, fingerprint, raw_fingerprint


# -- Pre-flight --

def preflight(cur: Any, audit: dict) -> None:
    if audit["summary"]["dedup_collision_count"] > 0:
        n = audit["summary"]["dedup_collision_count"]
        sys.exit(f"ERROR: {n} dedup collision(s) in audit — resolve before migrating.")

    cur.execute("SELECT COUNT(*) FROM schema_contents")
    count = cur.fetchone()[0]
    if count > 0:
        sys.exit(
            f"ERROR: schema_contents is not empty ({count} row(s) exist).\n"
            "Migration requires an empty Kora database."
        )


# -- Migration --

def migrate(cur: Any, audit: dict, dry_run: bool) -> None:
    schemas_by_id = {int(k): v for k, v in audit["schemas_by_id"].items()}
    subjects_data = audit["subjects"]

    total_schemas = len(schemas_by_id)
    total_subjects = len(subjects_data)
    total_versions = sum(len(s["versions"]) for s in subjects_data)

    print(f"  Schemas  : {total_schemas}", file=sys.stderr)
    print(f"  Subjects : {total_subjects}", file=sys.stderr)
    print(f"  Versions : {total_versions}", file=sys.stderr)
    print(file=sys.stderr)

    # --- Step 1/5: Insert schema_contents with explicit IDs ---
    print("Step 1/5: Inserting schema_contents ...", file=sys.stderr)
    schema_rows = []
    for schema_id in sorted(schemas_by_id.keys()):
        schema = schemas_by_id[schema_id]
        schema_type = schema["schema_type"]
        schema_text = schema["schema_text"]
        canonical_form, fingerprint, raw_fingerprint = compute_fingerprints(schema_text, schema_type)
        schema_rows.append((schema_id, schema_type, schema_text, canonical_form, fingerprint, raw_fingerprint))

    if not dry_run:
        psycopg2.extras.execute_values(
            cur,
            """INSERT INTO schema_contents (id, schema_type, schema_text, canonical_form, fingerprint, raw_fingerprint)
               VALUES %s
               ON CONFLICT (id) DO NOTHING""",
            schema_rows,
        )
        print(f"  Inserted {len(schema_rows)} schema_contents rows.", file=sys.stderr)
    else:
        print(f"  [dry-run] Would insert {len(schema_rows)} schema_contents rows.", file=sys.stderr)

    # --- Step 2/5: Insert subjects ---
    # deleted flag is preserved from the audit snapshot.
    print("Step 2/5: Inserting subjects ...", file=sys.stderr)
    subject_rows = [(s["name"], s["deleted"]) for s in subjects_data]

    if not dry_run:
        psycopg2.extras.execute_values(
            cur,
            "INSERT INTO subjects (name, deleted) VALUES %s ON CONFLICT (name) DO NOTHING",
            subject_rows,
        )
        print(f"  Inserted {len(subject_rows)} subject rows.", file=sys.stderr)
    else:
        print(f"  [dry-run] Would insert {len(subject_rows)} subject rows.", file=sys.stderr)

    # --- Step 3/5: Insert schema_versions ---
    # Both content_id and deleted flag are preserved from the audit snapshot.
    print("Step 3/5: Inserting schema_versions ...", file=sys.stderr)

    if not dry_run:
        cur.execute("SELECT id, name FROM subjects")
        subject_id_map = {name: sid for sid, name in cur.fetchall()}

        version_rows = []
        for subject in subjects_data:
            sid = subject_id_map[subject["name"]]
            for ver in subject["versions"]:
                version_rows.append((sid, ver["version"], ver["schema_id"], ver["deleted"]))

        psycopg2.extras.execute_values(
            cur,
            """INSERT INTO schema_versions (subject_id, version, content_id, deleted)
               VALUES %s
               ON CONFLICT (subject_id, version) DO NOTHING""",
            version_rows,
        )
        print(f"  Inserted {len(version_rows)} schema_version rows.", file=sys.stderr)
    else:
        print(f"  [dry-run] Would insert {total_versions} schema_version rows.", file=sys.stderr)

    # --- Step 4/5: Insert configs (global + per-subject) ---
    print("Step 4/5: Inserting configs ...", file=sys.stderr)

    source_compat = audit["summary"]["global_config"].get("compatibilityLevel", "NONE")
    per_subject_configs = [
        (s["name"], s["config"])
        for s in subjects_data
        if s["config"] is not None
    ]

    if not dry_run:
        cur.execute(
            "UPDATE config SET compatibility_level = %s WHERE subject IS NULL",
            (source_compat,),
        )
        print(f"  Global compatibility → {source_compat}", file=sys.stderr)

        if per_subject_configs:
            psycopg2.extras.execute_values(
                cur,
                "INSERT INTO config (subject, compatibility_level) VALUES %s ON CONFLICT (subject) DO NOTHING",
                per_subject_configs,
            )
            print(f"  Inserted {len(per_subject_configs)} per-subject config row(s).", file=sys.stderr)
        else:
            print("  No per-subject configs to migrate.", file=sys.stderr)
    else:
        print(f"  [dry-run] Would set global compatibility → {source_compat}", file=sys.stderr)
        if per_subject_configs:
            print(f"  [dry-run] Would insert {len(per_subject_configs)} per-subject config row(s).", file=sys.stderr)
        else:
            print("  [dry-run] No per-subject configs to migrate.", file=sys.stderr)

    # --- Step 5/5: Reset BIGSERIAL sequences ---
    print("Step 5/5: Resetting sequences ...", file=sys.stderr)
    sequences = [
        ("schema_contents_id_seq", "schema_contents"),
        ("subjects_id_seq", "subjects"),
        ("schema_versions_id_seq", "schema_versions"),
    ]

    if not dry_run:
        for seq_name, table_name in sequences:
            cur.execute(f"SELECT setval('{seq_name}', (SELECT COALESCE(MAX(id), 1) FROM {table_name}))")
            new_val = cur.fetchone()[0]
            print(f"  {seq_name} → {new_val}", file=sys.stderr)
    else:
        print("  [dry-run] Would reset sequences.", file=sys.stderr)


# -- Main --

def main() -> None:
    parser = argparse.ArgumentParser(
        description="Migrate Karapace schemas directly into Kora's PostgreSQL database",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "db_url", nargs="?", default=os.environ.get("KORA_DB_URL"),
        help="PostgreSQL connection URL, e.g. postgresql://user:pass@host:5432/dbname (env: KORA_DB_URL)",
    )
    parser.add_argument("--audit", default=os.environ.get("AUDIT_FILE"),
                        help="Audit JSON from audit.py (env: AUDIT_FILE, default: latest file in audits/)")
    parser.add_argument("--dry-run", action="store_true", help="Print actions without writing anything")
    args = parser.parse_args()

    if not args.db_url:
        parser.error("db_url is required (or set KORA_DB_URL)")

    if not args.audit:
        args.audit = find_latest_audit()
    if not args.audit:
        parser.error("No audit file found in audits/ — run 'just migrate-audit' first, or set AUDIT_FILE")

    with open(args.audit) as f:
        audit = json.load(f)

    print(f"Source  : {audit['summary']['source_url']} (audit from {audit['summary']['timestamp']})", file=sys.stderr)
    print(f"Target  : {args.db_url.split('@')[-1]}", file=sys.stderr)  # hide credentials in output
    if args.dry_run:
        print("[DRY RUN — no writes]\n", file=sys.stderr)

    conn = psycopg2.connect(args.db_url)
    conn.autocommit = False
    cur = conn.cursor()

    try:
        if not args.dry_run:
            preflight(cur, audit)

        migrate(cur, audit, args.dry_run)

        if not args.dry_run:
            conn.commit()
            print("\nMigration committed successfully.", file=sys.stderr)
        else:
            conn.rollback()
            print("\nDry run complete — no changes made.", file=sys.stderr)

    except Exception:
        conn.rollback()
        raise
    finally:
        cur.close()
        conn.close()


if __name__ == "__main__":
    main()
