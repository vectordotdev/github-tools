import argparse
import json
import os
from datetime import datetime, timezone, timedelta

import requests

from scripts.purge.utils import get_date_threshold, purge_dockerhub_images
from scripts.util.load_env import load_env

# ----------------------------
# Configuration
# ----------------------------

DOCKER_REPO = "timberio/vector"

GITHUB_ORG = "vectordotdev"
GITHUB_PACKAGE = "vector"
GITHUB_API = f"https://api.github.com/orgs/{GITHUB_ORG}/packages/container/{GITHUB_PACKAGE}/versions"

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DIR = os.path.abspath(os.path.join(SCRIPT_DIR, "../../out/purge"))
os.makedirs(OUTPUT_DIR, exist_ok=True)

GITHUB_AUDIT_FILE = os.path.join(OUTPUT_DIR, "nightly_github.jsonl")
DOCKERHUB_AUDIT_FILE = os.path.join(OUTPUT_DIR, "nightly_dockerhub.jsonl")


# ----------------------------
# Helpers
# ----------------------------

def github_headers(github_token):
    return {
        "Authorization": f"Bearer {github_token}",
        "Accept": "application/vnd.github+json"
    }


def list_github_versions(github_token):
    versions = []
    page = 1
    while True:
        resp = requests.get(f"{GITHUB_API}?per_page=100&page={page}", headers=github_headers(github_token))
        resp.raise_for_status()
        batch = resp.json()
        if not batch:
            break
        versions.extend(batch)
        page += 1
    return versions


def delete_github_version(version_id, github_token):
    resp = requests.delete(f"{GITHUB_API}/{version_id}", headers=github_headers(github_token))
    return resp.status_code == 204


# ----------------------------
# Main Cleanup Logic
# ----------------------------

def clean_github_versions(threshold, github_token, dry_run=True):
    print(f"🔍 Checking GitHub nightly container versions older than {threshold.date()}")

    with open(GITHUB_AUDIT_FILE, "w") as f:
        f.write(json.dumps({"dry_run": dry_run}) + "\n")

        versions = list_github_versions(github_token)
        print(f"ℹ️  Fetched {len(versions)} GitHub versions")

        for version in versions:
            tags = version["metadata"]["container"]["tags"]

            # Skip early if version is newer than threshold
            created_at = version["created_at"]
            created_dt = datetime.fromisoformat(created_at.rstrip("Z")).replace(tzinfo=timezone.utc)
            if created_dt >= threshold:
                continue

            nightly_tags = [t for t in tags if "nightly" in t]
            if nightly_tags:
                print(f"🧹 GitHub version {version['id']} (tags: {tags}, created: {created_dt.date()})")

                if dry_run:
                    for tag in nightly_tags:
                        f.write(json.dumps({
                            "tag": tag,
                            "last_updated": str(created_dt.date())
                        }) + "\n")

                if not dry_run:
                    if delete_github_version(version["id"], github_token):
                        print(f"✅ Deleted GitHub version {version['id']}")
                        for tag in nightly_tags:
                            f.write(json.dumps({
                                "tag": tag,
                                "last_updated": str(created_dt.date())
                            }) + "\n")
                    else:
                        print(f"❌ Failed to delete GitHub version {version['id']}")

    print(f"📄 Wrote audit file: {GITHUB_AUDIT_FILE}")


# ----------------------------
# Entry Point
# ----------------------------

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Delete old nightly versions from GitHub and Docker Hub.")
    parser.add_argument("--older-than",
                        type=int,
                        default=30,
                        help="Delete artifacts older than this many days (default: 30)")
    parser.add_argument("--env-file",
                        type=str,
                        required=True,
                        help="Path to the .env file to load environment variables from")
    parser.add_argument(
        "--target",
        choices=["dockerhub", "github", "all"],
        default="all",
        help="Which platform to clean: 'dockerhub', 'github', or 'all' (default: all)"
    )

    dry_run_group = parser.add_mutually_exclusive_group()
    dry_run_group.add_argument(
        "--dry-run",
        dest="dry_run",
        action="store_true",
        help="Run in dry-run mode (default)."
    )
    dry_run_group.add_argument(
        "--no-dry-run",
        dest="dry_run",
        action="store_false",
        help="Run in destructive mode (actually delete versions)."
    )
    parser.set_defaults(dry_run=True)
    args = parser.parse_args()

    try:
        env = load_env(args.env_file)
    except ValueError as e:
        print(f"Error loading environment variables: {e}")
        exit(1)

    if args.target in ("github", "all"):
        threshold_date = get_date_threshold(args.older_than)
        github_token = env["GITHUB_TOKEN"]
        clean_github_versions(threshold_date, github_token, dry_run=args.dry_run)

    if args.target in ("dockerhub", "all"):
        username = env["DOCKER_USERNAME"]
        password = env["DOCKER_PASSWORD"]
        if not username or not password:
            print("❌ DOCKER_USERNAME and DOCKER_PASSWORD environment variables are required.")
            exit(1)

        purge_dockerhub_images(
            repo=DOCKER_REPO,
            audit_file=DOCKERHUB_AUDIT_FILE,
            threshold=datetime.now(timezone.utc) - timedelta(days=30),
            username=username,
            password=password,
            dry_run=args.dry_run,
            tag_filter=lambda tag: tag.startswith("nightly")
        )
