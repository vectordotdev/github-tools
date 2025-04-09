import subprocess
import os
from datetime import datetime, timedelta
import argparse
import json

# GitHub repository details
REPO_OWNER = "vectordotdev"
REPO_NAME = "vector"

# Define the cutoff date (6 months ago)
CUTOFF_DATE = datetime.now() - timedelta(days=6 * 30)  # Approximation for 6 months

# Fetch pull requests using gh CLI
def fetch_pull_requests():
    try:
        result = subprocess.run(
            ["gh", "pr", "list", "--repo", f"{REPO_OWNER}/{REPO_NAME}", "--state", "open", "--json", "number,title,createdAt,labels"],
            capture_output=True,
            text=True,
            check=True
        )
        return json.loads(result.stdout)
    except subprocess.CalledProcessError as e:
        print(f"Error fetching pull requests: {e.stderr}")
        raise

# Add a comment to a pull request using gh CLI
def add_comment_to_pr(pr_number, comment):
    try:
        subprocess.run(
            ["gh", "pr", "comment", str(pr_number), "--repo", f"{REPO_OWNER}/{REPO_NAME}", "--body", comment],
            check=True
        )
        print(f"Added comment to PR #{pr_number}")
    except subprocess.CalledProcessError as e:
        print(f"Failed to add comment to PR #{pr_number}: {e.stderr}")
        raise

# Close a pull request using gh CLI
def close_pull_request(pr_number):
    try:
        subprocess.run(
            ["gh", "pr", "close", str(pr_number), "--repo", f"{REPO_OWNER}/{REPO_NAME}"],
            check=True
        )
        print(f"Closed PR #{pr_number}")
    except subprocess.CalledProcessError as e:
        print(f"Failed to close PR #{pr_number}: {e.stderr}")
        raise

# Main function
def main(dry_run):
    print("Fetching pull requests...")
    pull_requests = fetch_pull_requests()

    closed_prs = []

    for pr in pull_requests:
        pr_number = pr["number"]
        pr_title = pr["title"]
        created_at = datetime.strptime(pr["createdAt"], "%Y-%m-%dT%H:%M:%SZ")

        if created_at < CUTOFF_DATE:
            labels = [label["name"] for label in pr["labels"]]

            if "meta: awaiting author" in labels:
                print(f"PR #{pr_number} (created at: {created_at}) would be closed.")
                if not dry_run:
                    comment = (
                        "Thank you for your contribution to Vector! To keep the repository tidy and focused, we are closing this PR due to inactivity. "
                        "We greatly appreciate the time and effort you've put into this PR. "
                        "If you'd like to continue working on it, we encourage you to re-open the PR and we would be delighted to review it again. "
                        "Before re-opening, please git merge origin master to resolve any conflicts with origin/master."
                    )
                    add_comment_to_pr(pr_number, comment)
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
