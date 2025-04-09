import os
from datetime import datetime, timedelta, UTC
import requests
import json
import semver  # Added semver library

from scripts.util.load_env import load_github_env_vars

# Load environment variables
ENV = load_github_env_vars()
OWNER = ENV.get("REPO_OWNER", "vectordotdev")
REPO = ENV.get("REPO_NAME", "vector")
TOKEN = ENV.get("GITHUB_TOKEN")

# GitHub API URL for branches
API_URL = f"https://api.github.com/repos/{OWNER}/{REPO}/branches"

# Set the headers for authentication
COMMON_HEADERS = {
    "Authorization": f"token {TOKEN}",
    "Accept": "application/vnd.github.v3+json"
}


def is_semver_branch(branch_name):
    """Check if the branch name is a valid SemVer string starting with 'v'."""
    if not branch_name.startswith('v'):
        return False
    try:
        # Remove the 'v' prefix and parse the rest as a SemVer string
        semver.VersionInfo.parse(branch_name[1:])
        return True
    except ValueError:
        return False


def get_last_commit_date(github_token, repo_owner, repo_name, branch_name):
    """Fetch the last commit date for the given branch from GitHub API"""
    url = f"https://api.github.com/repos/{repo_owner}/{repo_name}/commits?sha={branch_name}&per_page=1"
    headers = {"Authorization": f"token {github_token}", "Accept": "application/vnd.github.v3+json"}

    try:
        response = requests.get(url, headers=headers)
        response.raise_for_status()

        commits = response.json()
        if not commits:
            return None

        last_commit_date = commits[0]['commit']['committer']['date']
        return datetime.strptime(last_commit_date, "%Y-%m-%dT%H:%M:%SZ")

    except requests.exceptions.RequestException as e:
        print(f"Error fetching commit date for branch {branch_name}: {str(e)}")
        return None


def check_branch_activity(last_commit_date, activity_limit_years=4):
    """Check if the branch was updated within the activity limit (default 4 years)"""
    if last_commit_date is None:
        return False

    now = datetime.now(UTC)
    activity_limit = timedelta(days=activity_limit_years * 365)
    return (now - last_commit_date) <= activity_limit


def get_branches(page=1, per_page=100):
    """Fetches a list of all branches from the repository with pagination."""
    params = {"page": page, "per_page": per_page}
    try:
        response = requests.get(API_URL, headers=COMMON_HEADERS, params=params)
        response.raise_for_status()

        branches = response.json()
        link_header = response.headers.get("Link", "")
        has_next = "rel=\"next\"" in link_header
        return branches, has_next
    except requests.exceptions.RequestException as e:
        print(f"Error fetching branches (page {page}): {str(e)}")
        return [], False


def delete_branch(branch_name):
    """Deletes the specified branch using the GitHub API."""
    delete_url = f"https://api.github.com/repos/{OWNER}/{REPO}/git/refs/heads/{branch_name}"
    try:
        response = requests.delete(delete_url, headers=COMMON_HEADERS)
        if response.status_code == 204:
            print(f"Successfully deleted branch: {branch_name}")
        else:
            print(f"Failed to delete branch {branch_name}: {response.status_code} - {response.text}")
    except requests.exceptions.RequestException as e:
        print(f"Error deleting branch {branch_name}: {str(e)}")


def main():
    """Main function to list and delete stale branches with pagination."""
    if not all([OWNER, REPO, TOKEN]):
        print("Error: Missing required environment variables (REPO_OWNER, REPO_NAME, GITHUB_TOKEN)")
        return

    page = 1
    per_page = 100
    all_branches = []

    while True:
        branches, has_next = get_branches(page, per_page)
        if not branches:
            break
        all_branches.extend(branches)
        if not has_next:
            break
        page += 1

    if not all_branches:
        print("No branches found or failed to fetch branches.")
        return

    for branch in all_branches:
        branch_name = branch['name']

        # Skip protected branches, main/master, and valid SemVer branches
        if (branch.get('protected', False) or
                branch_name in ['main', 'master'] or
                is_semver_branch(branch_name)):
            print(f"Skipping special branch: {branch_name}")
            continue

        # Check branch activity
        last_commit_date = get_last_commit_date(TOKEN, OWNER, REPO, branch_name)

        if last_commit_date:
            if check_branch_activity(last_commit_date):
                print(f"Keeping active branch: {branch_name} (Last commit: {last_commit_date})")
            else:
                print(f"Deleting stale branch: {branch_name} (Last commit: {last_commit_date})")
                delete_branch(branch_name)
        else:
            print(f"Could not determine activity for branch: {branch_name}")


if __name__ == "__main__":
    main()
