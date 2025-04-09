import argparse
import json
import os
from dotenv import load_dotenv
import requests
import pandas as pd
from datetime import datetime

from scripts.util.load_env import load_github_env_vars

# Load environment variables from custom .env file
ENV = load_github_env_vars()

# Validate environment variables
if not ENV["GITHUB_TOKEN"]:
    raise ValueError("GITHUB_TOKEN not found in .env file. Please set it before running the script.")
if not ENV["REPO_OWNER"]:
    raise ValueError("REPO_OWNER not found in .env file. Please set it before running the script.")
if not ENV["REPO_NAME"]:
    raise ValueError("REPO_NAME not found in .env file. Please set it before running the script.")

# Constants
API_BASE_URL = "https://api.github.com/repos"
BATCH_SIZE = 100  # Max allowed by GitHub API is 100

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DIR = os.path.abspath(os.path.join(SCRIPT_DIR, "../github_issues_metrics-out"))

# Ensure output directory exists
if not os.path.exists(OUTPUT_DIR):
    os.makedirs(OUTPUT_DIR)


# Generate timestamp for filenames
TIMESTAMP = datetime.now().strftime("%Y%m%d_%H%M%S")
ISSUES_JSON_FILE = os.path.join(OUTPUT_DIR, f"{TIMESTAMP}_issues.json")

def fetch_issues(env, include_closed=False):
    github_token = env["GITHUB_TOKEN"]
    repo_owner = env["REPO_OWNER"]
    repo_name = env["REPO_NAME"]

    state = "all" if include_closed else "open"
    issues = []
    page = 1
    while True:
        print(f"Fetching page {page} (batch size: {BATCH_SIZE}, state: {state})...")
        response = requests.get(
            f"{API_BASE_URL}/{repo_owner}/{repo_name}/issues?state={state}&per_page={BATCH_SIZE}&page={page}",
            headers={"Authorization": f"token {github_token}"}
        )
        if response.status_code != 200:
            raise Exception(f"API request failed: {response.status_code} - {response.text}")
        data = response.json()
        issues.extend(data)
        print(f"Page {page} fetched. Total issues collected: {len(issues)}")
        if not data:
            print("No more issues to fetch.")
            break
        page += 1

    # Save raw issues to JSON file
    print(f"Saving raw issues with URLs to {ISSUES_JSON_FILE}...")
    with open(ISSUES_JSON_FILE, "w") as f:
        json.dump(issues, f, indent=4)
    print(f"Raw issues (including URLs) saved successfully to {ISSUES_JSON_FILE}.")

    return issues
