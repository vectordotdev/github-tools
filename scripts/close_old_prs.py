import requests
import os
from datetime import datetime, timedelta
import argparse

# GitHub repository details
REPO_OWNER = "vectordotdev"
REPO_NAME = "vector"

# GitHub personal access token (read from environment variable)
GITHUB_TOKEN = os.getenv("GITHUB_TOKEN")
print(GITHUB_TOKEN[-10:])
if not GITHUB_TOKEN:
    raise EnvironmentError("GITHUB_TOKEN environment variable is not set.")

# Headers for authentication
HEADERS = {
    "Authorization": f"Bearer {GITHUB_TOKEN}",
    "Accept": "application/vnd.github+json",
}

# Define the cutoff date (6 months ago)
CUTOFF_DATE = datetime.now() - timedelta(days=6 * 30)  # Approximation for 6 months

# GitHub API endpoint for listing pull requests
PULLS_URL = f"https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/pulls"

COMMENT = (
    "Thank you for your contribution to Vector! To keep the repository tidy and focused, we are closing this PR due to inactivity. "
    "We greatly appreciate the time and effort you've put into this PR."
    "If you'd like to continue working on it, we encourage you to re-open the PR and we would be delighted to review it again. "
    "Before re-opening, please use `git merge origin master` to resolve any conflicts with origin/master."
)

# Fetch pull requests
def fetch_pull_requests():
    params = {
        "state": "open",
        "per_page": 100
    }
    response = requests.get(PULLS_URL, headers=HEADERS, params=params)
    response.raise_for_status()
    return response.json()

# Add a comment to a pull request
def add_comment_to_pr(pr_number, comment):
    url = f"https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/issues/{pr_number}/comments"
    data = {"body": comment}
    response = requests.post(url, headers=HEADERS, json=data)
    if response.status_code != 200:
        print(f"Request Headers: {response.headers}")
        print(f"Request Body: {response.json()}")
        print(f"Failed to close PR #{pr_number}. Response status: {response.status_code}")
    response.raise_for_status()
    print(f"Added comment to PR #{pr_number}")

# Close a pull request
def close_pull_request(pr_number):
    url = f"https://api.github.com/repos/{REPO_OWNER}/{REPO_NAME}/pulls/{pr_number}"
    data = {"state": "closed"}
    response = requests.patch(url, headers=HEADERS, json=data)
    response.raise_for_status()
    print(f"Closed PR #{pr_number}")

# Main function
def main(dry_run):
    print("Fetching pull requests...")
    pull_requests = fetch_pull_requests()

    closed_prs = []

    for pr in pull_requests:
        pr_number = pr["number"]
        pr_title = pr["title"]
        created_at = datetime.strptime(pr["created_at"], "%Y-%m-%dT%H:%M:%SZ")

        if created_at < CUTOFF_DATE:
            labels = [label["name"] for label in pr["labels"]]

            if "meta: awaiting author" in labels:
                print(f"PR #{pr_number} (created at: {created_at}) to be closed.")
                if not dry_run:
                    add_comment_to_pr(pr_number, COMMENT)
                    close_pull_request(pr_number)
                closed_prs.append({"number": pr_number, "title": pr_title, "created_at": created_at})

    print("\nReport:")
    print(f"Total PRs that would be closed: {len(closed_prs)}" if dry_run else f"Total PRs closed: {len(closed_prs)}")
    for pr in closed_prs:
        print(f"- PR #{pr['number']}: {pr['title']} (Created at: {pr['created_at']})")

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Close inactive GitHub pull requests.")
    parser.add_argument("--dry-run", action="store_true", help="If set, do not modify any PRs, only print what would be done.")
    args = parser.parse_args()

    main(dry_run=args.dry_run)
