"""GitHub API utilities for pagination and rate limiting."""

import time
from typing import Iterator, List, Dict

import requests


API_ROOT = "https://api.github.com"


def gh_headers(token: str) -> Dict[str, str]:
    """Create GitHub API request headers with authentication."""
    return {
        "Authorization": f"Bearer {token}",
        "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "github-tools/1.0",
    }


def backoff_sleep(retry: int):
    """Basic exponential backoff with jitter."""
    base = min(60, 2 ** retry)
    time.sleep(base + 0.1 * retry)


def paged_get(url: str, headers: Dict[str, str], params: Dict[str, str]) -> Iterator[List[Dict]]:
    """
    Generic paginator for GitHub list endpoints.

    Handles:
    - Pagination (100 items per page)
    - Rate limiting with automatic retry
    - Returns iterator of pages (each page is a list of items)

    Args:
        url: The GitHub API endpoint URL
        headers: Request headers (should include auth)
        params: Query parameters (page/per_page will be added automatically)

    Yields:
        List[Dict]: Each page of results as a list of items
    """
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
