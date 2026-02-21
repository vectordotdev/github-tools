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

  # Process only issues created after a certain date
  python remove_legacy_label.py \
    --repo vectordotdev/vector \
    --legacy-label "type: bug" \
    --since "2024-01-01" \
    --set-type-field \
    --dry-run

  # Limit processing to first 10 issues (useful for testing)
  python remove_legacy_label.py \
    --repo vectordotdev/vector \
    --legacy-label "type: bug" \
    --limit 10 \
    --set-type-field \
    --dry-run

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
from datetime import datetime
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


def get_type_mapping() -> Dict[str, Dict[str, any]]:
    """
    Define the valid mappings from legacy labels to GitHub type field.
    Legacy label -> Type field with ID and name
    """
    return {
        "type: bug": {"id": 2240170, "name": "Bug", "node_id": "IT_kwDOAQFeYs4AIi6q"},
        "type: feature": {"id": 2240173, "name": "Feature", "node_id": "IT_kwDOAQFeYs4AIi6t"},
        "type: task": {"id": 2240168, "name": "Task", "node_id": "IT_kwDOAQFeYs4AIi6o"}
    }


def set_type_field(repo: str, token: str, number: int, type_info: Dict, dry_run: bool) -> Tuple[bool, str]:
    """
    Set the type field on a single issue/PR. Returns (changed, message).
    """
    url = f"{API_ROOT}/repos/{repo}/issues/{number}"
    headers = gh_headers(token)
    # Set the type field using the type name (as per GitHub REST API docs)
    body = {"type": type_info["name"]}

    if dry_run:
        return True, f"[dry-run] would SET type to '{type_info['name']}'"

    # Retry a few times on transient failures
    for attempt in range(5):
        resp = requests.patch(url, headers=headers, json=body)
        if resp.status_code in (200, 201):
            # Verify the type was actually set
            # Some issues may not support the type field if they're not in the project
            result = resp.json()
            actual_type = result.get("type")
            if actual_type and isinstance(actual_type, dict):
                if actual_type.get("name") == type_info["name"]:
                    return True, f"Set type to '{type_info['name']}'"
                else:
                    return False, f"Type field exists but set to '{actual_type.get('name')}' instead of '{type_info['name']}'"
            else:
                # Type field not set - issue may not be in the project
                return False, f"Type field not supported (issue may not be in project)"
        if resp.status_code == 403 and "rate limit" in resp.text.lower():
            backoff_sleep(attempt + 1)
            continue
        try:
            body_resp = resp.json()
        except Exception:
            body_resp = {"text": resp.text}
        if 500 <= resp.status_code < 600:
            backoff_sleep(attempt + 1)
            continue
        # Non-retryable
        return False, f"Failed to set type: {resp.status_code} {body_resp}"

    return False, f"Failed to set type after retries"


def add_label(repo: str, token: str, number: int, label_name: str, dry_run: bool) -> Tuple[bool, str]:
    """
    Add label to a single issue/PR. Returns (changed, message).
    """
    url = f"{API_ROOT}/repos/{repo}/issues/{number}/labels"
    headers = gh_headers(token)
    body = {"labels": [label_name]}

    if dry_run:
        return True, f"[dry-run] would ADD label '{label_name}'"

    # Retry a few times on transient failures
    for attempt in range(5):
        resp = requests.post(url, headers=headers, json=body)
        if resp.status_code in (200, 201):
            return True, f"Added label '{label_name}' back to #{number}"
        if resp.status_code == 403 and "rate limit" in resp.text.lower():
            backoff_sleep(attempt + 1)
            continue
        try:
            body_resp = resp.json()
        except Exception:
            body_resp = {"text": resp.text}
        if 500 <= resp.status_code < 600:
            backoff_sleep(attempt + 1)
            continue
        # Non-retryable
        return False, f"Failed to add label on #{number}: {resp.status_code} {body_resp}"

    return False, f"Failed to add label on #{number} after retries"


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
    parser = argparse.ArgumentParser(description="Remove legacy type labels and set type field for GitHub issues/PRs.")
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
        "--set-type-field",
        action="store_true",
        help="Set the GitHub type field (Bug/Feature/Task) before removing the legacy label.",
    )
    parser.add_argument(
        "--require-type-field",
        action="store_true",
        help="Only remove the legacy label if the type field is already set (safety check).",
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
    parser.add_argument(
        "--since",
        help="Only process issues created after this date (YYYY-MM-DD format).",
    )
    parser.add_argument(
        "--limit",
        type=int,
        help="Maximum number of issues to process (useful for testing).",
    )
    args = parser.parse_args()

    if not args.token:
        print("Error: provide a GitHub token via --token or GITHUB_TOKEN env var.", file=sys.stderr)
        sys.exit(2)

    # Get valid type mappings
    type_mapping = get_type_mapping()

    # Validate that the legacy label is in our supported mappings
    legacy_label_lower = args.legacy_label.lower() if args.case_insensitive_legacy else args.legacy_label
    valid_legacy_lower = {k.lower(): k for k in type_mapping.keys()}

    if legacy_label_lower not in valid_legacy_lower:
        print(f"Error: '{args.legacy_label}' is not a supported legacy label.", file=sys.stderr)
        print(f"Supported legacy labels: {', '.join(type_mapping.keys())}", file=sys.stderr)
        sys.exit(2)

    # Get the canonical legacy label name and its type info
    canonical_legacy = valid_legacy_lower[legacy_label_lower]
    type_info = type_mapping[canonical_legacy]

    # Parse since date if provided
    since_date = None
    if args.since:
        try:
            since_date = datetime.strptime(args.since, "%Y-%m-%d")
            print(f"Filtering to issues created after {since_date.date()}")
        except ValueError:
            print(f"Error: Invalid date format '{args.since}'. Use YYYY-MM-DD format.", file=sys.stderr)
            sys.exit(2)

    print(f"Migrating '{args.legacy_label}' -> Type field '{type_info['name']}'")
    if args.limit:
        print(f"Processing up to {args.limit} issues")

    total_seen = total_changed = total_skipped = 0
    processed_count = 0

    for repo in args.repo:
        print(f"\n=== Repo: {repo} ===")
        for item in iter_issues(repo, args.token, args.state, args.legacy_label):
            number = item["number"]
            title = item.get("title", "")
            total_seen += 1

            # Check limit
            if args.limit and processed_count >= args.limit:
                print(f"Reached limit of {args.limit} issues. Stopping.")
                break

            # Check date filter
            if since_date:
                created_at = datetime.strptime(item["created_at"], "%Y-%m-%dT%H:%M:%SZ")
                if created_at < since_date:
                    total_skipped += 1
                    print(f"#{number}  SKIP (created {created_at.date()}, before {since_date.date()})  {title}")
                    continue

            # Double-check presence (defensive)
            if not has_label(item, args.legacy_label, args.case_insensitive_legacy):
                total_skipped += 1
                print(f"#{number}  (no legacy label)  {title}")
                continue

            # Check if type field is already set
            current_type = item.get("type")
            has_correct_type = (
                    current_type and
                    isinstance(current_type, dict) and
                    current_type.get("name") == type_info["name"]
            )

            if args.require_type_field and not has_correct_type:
                total_skipped += 1
                print(f"#{number}  SKIP (type field not set to '{type_info['name']}')  {title}")
                continue

            # Increment processed count before making changes
            processed_count += 1

            # Set type field if requested and not already set correctly
            if args.set_type_field and not has_correct_type:
                set_ok, set_msg = set_type_field(repo, args.token, number, type_info, args.dry_run)
                print(f"#{number}  {set_msg}  {title}")

                if not set_ok and not args.dry_run:
                    total_skipped += 1
                    print(f"#{number}  ERROR: Cannot set type field. Keeping legacy label and stopping.")
                    # Exit early when we can't set the type field
                    print(f"\nStopping due to type field error. Please check if issue #{number} is in the GitHub Project.")
                    break

            # Only remove label if type was set successfully (or if we're not setting type)
            if args.set_type_field and not has_correct_type and not set_ok:
                # Don't remove the label if we couldn't set the type
                total_skipped += 1
                print(f"#{number}  SKIP: Not removing label since type couldn't be set")
            else:
                # Remove legacy label
                changed, msg = remove_label(repo, args.token, number, args.legacy_label, args.dry_run)
                if changed:
                    total_changed += 1
                else:
                    total_skipped += 1
                print(f"#{number}  {msg}  {title}")

        # Break outer loop if limit reached
        if args.limit and processed_count >= args.limit:
            break

    print(
        f"\nDone. Items seen={total_seen}, processed={processed_count}, changed={total_changed}, skipped/unchanged={total_skipped}."
    )


if __name__ == "__main__":
    main()
