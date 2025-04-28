import argparse
import json
import logging
import os

import requests

from scripts.logging.custom_logging import setup_logger
from scripts.util.load_env import load_github_env_vars

# Constants
API_BASE_URL = "https://api.github.com/repos"
BATCH_SIZE = 100  # Max issues per page (GitHub API maximum is 100)
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DIR = os.path.abspath(os.path.join(SCRIPT_DIR, "../../out/historical/issues"))
os.makedirs(OUTPUT_DIR, exist_ok=True)


def fetch_issues(env, include_closed=False):
    """Fetch all issues from the GitHub API (open by default, or all if include_closed).
    Logs warnings and errors but always returns collected issues, even on partial failure."""

    github_token = env["GITHUB_TOKEN"]
    repo_owner = env["REPO_OWNER"]
    repo_name = env["REPO_NAME"]

    state = "all" if include_closed else "open"
    issues = []
    page = 1

    while True:
        logging.info(f"Fetching page {page} (batch size: {BATCH_SIZE}, state: {state})...")
        try:
            response = requests.get(
                f"{API_BASE_URL}/{repo_owner}/{repo_name}/issues",
                params={"state": state, "per_page": BATCH_SIZE, "page": page},
                headers={"Authorization": f"token {github_token}"}
            )
            if response.status_code != 200:
                logging.warning(f"API request failed on page {page} - Status {response.status_code}: {response.text}")
                break

            try:
                data = response.json()
            except json.JSONDecodeError:
                logging.warning(f"Failed to decode JSON on page {page}. Response: {response.text}")
                break

            if not data:
                logging.info("No more issues to fetch.")
                break

            issues.extend(data)
            logging.info(f"Page {page} fetched. Total issues collected: {len(issues)}")

            if len(data) < BATCH_SIZE:
                logging.info("Reached the last page of issues.")
                break

            page += 1

        except Exception as e:
            logging.error(f"Unexpected failure on page {page}: {e}")
            break

    return issues


def write_to_json_file(issues, repo_owner, repo_name):
    json_out_file = os.path.join(OUTPUT_DIR, f"{repo_owner}_{repo_name}_issues.json")
    logging.info(f"Saving raw issues with URLs to {json_out_file}...")
    try:
        with open(json_out_file, "w") as f:
            json.dump(issues, f, indent=4)
        logging.info(f"Issues saved to {json_out_file}")
    except Exception as e:
        logging.error(f"Error saving issues JSON file: {e}")


def main():
    setup_logger()
    
    parser = argparse.ArgumentParser(description="Fetch GitHub issues from a repository.")
    parser.add_argument(
        "--include-closed",
        action="store_true",
        default=True,
        help="Include closed issues as well as open issues in the fetch."
    )
    parser.add_argument(
        "--env-file",
        type=str,
        help="Path to the .env file to load environment variables from",
    )
    args = parser.parse_args()

    # Load environment variables from .env and validate them
    try:
        env = load_github_env_vars(args.env_file)  # expects GITHUB_TOKEN, REPO_OWNER, REPO_NAME to be set
    except ValueError as e:
        print(f"Error loading environment variables: {e}")
        return 1

    # Fetch issues using the GitHub API
    try:
        os.makedirs(OUTPUT_DIR, exist_ok=True)
        issues = fetch_issues(env, include_closed=args.include_closed)
        write_to_json_file(issues,
                           repo_owner=env['REPO_OWNER'],
                           repo_name=env['REPO_NAME'])
    except Exception as e:
        print(f"Error fetching issues: {e}")
        return 1

    return 0  # success


if __name__ == "__main__":
    import sys

    sys.exit(main())
