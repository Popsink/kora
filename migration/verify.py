#!/usr/bin/env python3
"""
Kora migration verifier — Phase 3 of the Karapace → Kora migration.

Reads an audit snapshot and validates the target Kora instance against it,
checking every schema ID, subject, version, and schema content.

Usage:
    python3 verify.py <kora_url> [--audit PATH] [--quiet]

Environment variables (used as fallback when CLI args are not provided):
    KORA_URL       Kora base URL
    KORA_USER      BasicAuth username
    KORA_PASSWORD  BasicAuth password
    AUDIT_FILE     Audit JSON path (default: latest file in audits/)
"""

import argparse
import json
import os
import sys
import urllib.request
import urllib.error
from pathlib import Path
from typing import Any

AUDITS_DIR = Path(__file__).parent / "audits"
HTTP_TIMEOUT = 30


def find_latest_audit() -> str | None:
    files = sorted(AUDITS_DIR.glob("*.json"), key=lambda p: p.stat().st_mtime, reverse=True)
    return str(files[0]) if files else None


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
        raw = e.read().decode()
        try:
            parsed = json.loads(raw)
        except Exception:
            parsed = raw
        return {"_http_error": e.code, "_body": parsed}
    except urllib.error.URLError as e:
        return {"_http_error": 0, "_body": str(e.reason)}


def is_error(obj: Any) -> bool:
    return isinstance(obj, dict) and "_http_error" in obj


def normalise(schema_text: str) -> str:
    try:
        return json.dumps(json.loads(schema_text), sort_keys=True)
    except Exception:
        return schema_text.strip()


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Verify Kora against a Karapace audit snapshot",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("url", nargs="?", default=os.environ.get("KORA_URL"),
                        help="Kora base URL, e.g. https://kora.example.com (env: KORA_URL)")
    parser.add_argument("--audit", default=os.environ.get("AUDIT_FILE"),
                        help="Audit JSON produced by audit.py (env: AUDIT_FILE, default: latest file in audits/)")
    parser.add_argument("--user", default=os.environ.get("KORA_USER"),
                        help="BasicAuth username (env: KORA_USER)")
    parser.add_argument("--password", default=os.environ.get("KORA_PASSWORD"),
                        help="BasicAuth password (env: KORA_PASSWORD)")
    parser.add_argument("--quiet", "-q", action="store_true",
                        help="Only print failures and the final summary")
    args = parser.parse_args()

    if not args.url:
        parser.error("url is required (or set KORA_URL)")

    if not args.audit:
        args.audit = find_latest_audit()
    if not args.audit:
        parser.error("No audit file found in audits/ — run 'just migrate-audit' first, or set AUDIT_FILE")

    if args.user and args.password:
        configure_auth(args.user, args.password, args.url)
    elif args.user or args.password:
        parser.error("--user and --password must be provided together "
                     "(or set both KORA_USER and KORA_PASSWORD)")

    base = args.url.rstrip("/")

    with open(args.audit) as f:
        audit = json.load(f)

    print(f"Verifying {base} against audit from {audit['summary']['timestamp']}")
    print(f"Expecting {audit['summary']['schema_count']} schema(s) across "
          f"{audit['summary']['subject_count']} subject(s)\n")

    failures: list[str] = []
    checks = 0

    # --- Check 1: every schema ID resolves to the correct content ---
    if not args.quiet:
        print("Checking schema IDs ...")
    for id_str, expected in audit["schemas_by_id"].items():
        schema_id = int(id_str)
        resp = get(base, f"/schemas/ids/{schema_id}")
        checks += 1

        if is_error(resp):
            failures.append(f"  ID {schema_id}: GET /schemas/ids/{schema_id} → {resp['_body']}")
            continue

        got = normalise(resp.get("schema", ""))
        want = normalise(expected["schema_text"])
        if got != want:
            failures.append(
                f"  ID {schema_id}: schema text mismatch\n"
                f"    want: {want[:120]}\n"
                f"    got:  {got[:120]}"
            )
        elif not args.quiet:
            print(f"  [{schema_id:3d}] ok")

    # --- Check 2: every subject/version maps to the right ID and content ---
    if not args.quiet:
        print("\nChecking subject versions ...")
    for subject_data in audit["subjects"]:
        subject = subject_data["name"]
        for ver in subject_data["versions"]:
            version = ver["version"]
            expected_id = ver["schema_id"]
            resp = get(base, f"/subjects/{subject}/versions/{version}")
            checks += 1

            if is_error(resp):
                failures.append(f"  {subject} v{version}: {resp['_body']}")
                continue

            got_id = resp.get("id")
            got_schema = normalise(resp.get("schema", ""))
            want_schema = normalise(ver["schema_text"])

            if got_id != expected_id:
                failures.append(
                    f"  {subject} v{version}: ID mismatch — expected {expected_id}, got {got_id}"
                )
            elif got_schema != want_schema:
                failures.append(f"  {subject} v{version}: schema text mismatch")
            elif not args.quiet:
                print(f"  {subject} v{version} → id={got_id} ok")

    # --- Check 3: subject list matches audit ---
    if not args.quiet:
        print("\nChecking subject list ...")
    kora_subjects = get(base, "/subjects")
    checks += 1
    if is_error(kora_subjects):
        failures.append(f"  GET /subjects failed: {kora_subjects['_body']}")
    else:
        audit_active = {s["name"] for s in audit["subjects"] if not s["deleted"]}
        kora_set = set(kora_subjects)
        missing = audit_active - kora_set
        extra = kora_set - audit_active
        if missing:
            failures.append(f"  Missing subjects in Kora: {sorted(missing)}")
        if extra:
            failures.append(f"  Extra subjects in Kora (not in audit): {sorted(extra)}")
        if not missing and not extra and not args.quiet:
            print(f"  {len(kora_subjects)} subject(s) — matches audit")

    # --- Summary ---
    print(f"\n{'='*60}")
    print(f"Checks run : {checks}")
    print(f"Failures   : {len(failures)}")
    if failures:
        print("\nFAILURES:")
        for f in failures:
            print(f)
        sys.exit(1)
    else:
        print("\nAll checks passed — migration verified successfully.")


if __name__ == "__main__":
    main()
