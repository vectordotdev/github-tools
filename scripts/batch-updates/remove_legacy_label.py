#!/usr/bin/env python3
"""
Remove a legacy label (e.g. 'type: bug') from GitHub issues/PRs, optionally
requiring that a 'Type' label (e.g. 'Type: bug') is already present.

Usage examples:

  # Dry-run, see what would change in vectordotdev/vector
  python remove_legacy_label.py \
    --repo vectordotdev/vector \
    --legacy-label "type: bug" \
    --dry-run

  # Actually remove the label from open + closed issues, across two repos
  GITHUB_TOKEN=ghp_... python remove_legacy_label.py \
    --repo vectordotdev/vector --repo vectordotdev/vrl \
    --legacy-label "type: bug" \
    --state all

  # Only remove when a 'Type: bug' label is already present (safety)
  python remove_legacy_label.py \
    --repo vectordotdev/vector \
    --legacy-label "type: bug" \
    --require-type-label \
    --type-prefix "Type" \
    --type-value "bug"

Notes:
- Auth via env var GITHUB_TOKEN or --token flag.
- Works on issues and PRs (GitHub "issues" endpoint returns both).
- Idempotent: skipping items that don't have the legacy label.
"""

import argparse
import os
import re
import sys
import time
from typing import Iterator, List, Dict, Optional, Tuple
from urllib.parse import quote

import requests

API_ROOT = "https://api.github.com"


def gh_headers(token: str) -> Dict[str, str]:
    return {
        "Authorization": f"Bearer {token}",
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "label-cleanup-script/1.0",
    }


def backoff_sleep(retry: int):
    # Basic exponential backoff with jitter
    base = min(60, 2 ** retry)
    time.sleep(base + 0.1 * retry)


def paged_get(url: str, headers: Dict[str, str], params: Dict[str, str]) -> Iterator[List[Dict]]:
    """Generic paginator for GitHub list endpoints."""
    page = 1
    while True:
        params_with_page = {**params, "per_page": "100", "page": str(page)}
        resp = requests.get(url, headers=headers, params=params_with_page)
        if resp.status_code == 403 and "rate limit" in resp.text.lower():
            # Backoff on rate limits
            reset = resp.headers.get("X-RateLimit-Reset")
            if reset and reset.isdigit():
                wait = max(0, int(reset) - int(time.time())) + 1
                print(f"[rate-limit] sleeping {wait}s until reset…", flush=True)
                time.sleep(wait)
                continue
        resp.raise_for_status()
        data = resp.json()
        if not data:
            break
        yield data
        if len(data) < 100:
            break
        page += 1


def iter_issues(repo: str, token: str, state: str, legacy_label: str) -> Iterator[Dict]:
    """
    Iterate issues/PRs that *have* the legacy label.
    state: open | closed | all
    """
    headers = gh_headers(token)
    url = f"{API_ROOT}/repos/{repo}/issues"
    params = {
        "state": state,
        "labels": legacy_label,  # server-side filter: must include legacy label
        "direction": "asc",
        "sort": "created",
    }
    for page in paged_get(url, headers, params):
        for item in page:
            yield item


def has_label(item: Dict, name: str, case_insensitive: bool = False) -> bool:
    for lbl in item.get("labels", []):
        if case_insensitive:
            if lbl["name"].lower() == name.lower():
                return True
        else:
            if lbl["name"] == name:
                return True
    return False


def find_type_label(
        item: Dict,
        type_prefix: str,
        type_value: Optional[str],
) -> Optional[str]:
    """
    Look for a 'Type' label on the item. By default matches things like:
      - 'Type: bug'
      - 'Type/bug'
      - 'Type bug'
      - exact-case-insensitive on both sides

    If type_value is None, *any* Type label qualifies.
    If provided, we match 'Type: {type_value}' (various separators).
    Returns the matched label name if found, else None.
    """
    # Build regex like: r"^Type\s*[:/ ]\s*bug$" (case-insensitive)
    if type_value:
        pattern = rf"^{re.escape(type_prefix)}\s*[:/ ]\s*{re.escape(type_value)}$"
    else:
        pattern = rf"^{re.escape(type_prefix)}\s*[:/ ]\s*\S.+$"

    rx = re.compile(pattern, re.IGNORECASE)

    for lbl in item.get("labels", []):
        if rx.match(lbl["name"]):
            return lbl["name"]
    return None


def remove_label(repo: str, token: str, number: int, label_name: str, dry_run: bool) -> Tuple[bool, str]:
    """
    Remove label from a single issue/PR. Returns (changed, message).
    """
    label_path = quote(label_name, safe="")
    url = f"{API_ROOT}/repos/{repo}/issues/{number}/labels/{label_path}"
    headers = gh_headers(token)

    if dry_run:
        return True, f"[dry-run] would DELETE {url}"

    # Retry a few times on transient failures
    for attempt in range(5):
        resp = requests.delete(url, headers=headers)
        if resp.status_code in (204, 200):
            return True, f"Removed label '{label_name}' from #{number}"
        if resp.status_code == 404:
            return False, f"Label '{label_name}' not present on #{number} (404)"
        if resp.status_code == 403 and "rate limit" in resp.text.lower():
            backoff_sleep(attempt + 1)
            continue
        try:
            body = resp.json()
        except Exception:
            body = {"text": resp.text}
        if 500 <= resp.status_code < 600:
            backoff_sleep(attempt + 1)
            continue
        # Non-retryable
        return False, f"Failed to remove label on #{number}: {resp.status_code} {body}"

    return False, f"Failed to remove label on #{number} after retries"


def main():
    parser = argparse.ArgumentParser(description="Remove legacy labels from GitHub issues/PRs.")
    parser.add_argument(
        "--repo",
        action="append",
        required=True,
        help="Repository in 'owner/name' form. Can be provided multiple times.",
    )
    parser.add_argument(
        "--legacy-label",
        required=True,
        help='The exact legacy label to remove (e.g., "type: bug").',
    )
    parser.add_argument(
        "--state",
        choices=["open", "closed", "all"],
        default="open",
        help="Which items to process (default: open).",
    )
    parser.add_argument(
        "--require-type-label",
        action="store_true",
        help="Only remove the legacy label if a matching 'Type' label is already present.",
    )
    parser.add_argument(
        "--type-prefix",
        default="Type",
        help="Prefix for the new label family (default: 'Type').",
    )
    parser.add_argument(
        "--type-value",
        default="bug",
        help="Value for the 'Type' label (default: 'bug'). Use with --require-type-label.",
    )
    parser.add_argument(
        "--case-insensitive-legacy",
        action="store_true",
        help="Match legacy label name case-insensitively.",
    )
    parser.add_argument(
        "--token",
        default=os.environ.get("GITHUB_TOKEN", ""),
        help="GitHub token (or set GITHUB_TOKEN env var). Needs issues:write scope.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Don't change anything; print what would happen.",
    )
    args = parser.parse_args()

    if not args.token:
        print("Error: provide a GitHub token via --token or GITHUB_TOKEN env var.", file=sys.stderr)
        sys.exit(2)

    total_seen = total_changed = total_skipped = 0

    for repo in args.repo:
        print(f"\n=== Repo: {repo} ===")
        for item in iter_issues(repo, args.token, args.state, args.legacy_label):
            number = item["number"]
            title = item.get("title", "")
            total_seen += 1

            # Double-check presence (defensive)
            if not has_label(item, args.legacy_label, args.case_insensitive_legacy):
                total_skipped += 1
                print(f"#{number}  (no legacy label)  {title}")
                continue

            if args.require_type_label:
                matched = find_type_label(item, args.type_prefix, args.type_value)
                if not matched:
                    total_skipped += 1
                    print(f"#{number}  SKIP (no matching '{args.type_prefix}: {args.type_value}' label)  {title}")
                    continue

            changed, msg = remove_label(repo, args.token, number, args.legacy_label, args.dry_run)
            if changed:
                total_changed += 1
            else:
                total_skipped += 1
            print(f"#{number}  {msg}  {title}")

    print(
        f"\nDone. Items seen={total_seen}, changed={total_changed}, skipped/unchanged={total_skipped}."
    )


if __name__ == "__main__":
    main()
