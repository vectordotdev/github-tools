import argparse
import json
import os
from datetime import datetime, timezone, timedelta

import requests

from scripts.util.load_env import load_env

# ----------------------------
# Configuration
# ----------------------------

DOCKER_REPO = "timberio/vector"
DOCKER_TAGS_API = f"https://hub.docker.com/v2/repositories/{DOCKER_REPO}/tags?page_size=100"

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

def get_date_threshold(days_old):
    return datetime.now(timezone.utc) - timedelta(days=days_old)


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


def list_docker_tags():
    tags = []
    page = 1
    while True:
        resp = requests.get(f"{DOCKER_TAGS_API}&page={page}")
        resp.raise_for_status()
        data = resp.json()
        tags.extend(data.get("results", []))
        if not data.get("next"):
            break
        page += 1
    return tags


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

                for tag in nightly_tags:
                    f.write(json.dumps({
                        "tag": tag,
                        "last_updated": str(created_dt.date())
                    }) + "\n")

                if not dry_run:
                    if delete_github_version(version["id"], github_token):
                        print(f"✅ Deleted GitHub version {version['id']}")
                    else:
                        print(f"❌ Failed to delete GitHub version {version['id']}")

    print(f"📄 Wrote audit file: {GITHUB_AUDIT_FILE}")


def clean_dockerhub_tags(threshold, username, password, dry_run=True):
    print(f"🔍 Checking Docker Hub tags older than {threshold.date()}")
    login_resp = requests.post(
        "https://hub.docker.com/v2/users/login/",
        json={"username": username, "password": password}
    )

    if login_resp.status_code != 200:
        print(f"❌ Failed to authenticate with Docker Hub: {login_resp.text}")
        return

    token = login_resp.json()["token"]
    headers = {"Authorization": f"JWT {token}"}

    with open(DOCKERHUB_AUDIT_FILE, "w") as f:
        f.write(json.dumps({"dry_run": dry_run}) + "\n")

        for tag in list_docker_tags():
            name = tag["name"]
            tag_date = datetime.fromisoformat(tag["last_updated"].replace("Z", "+00:00"))

            if name.startswith("nightly") and tag_date < threshold:
                print(f"🧹 Deleting Docker tag {name} (last updated: {tag_date.date()})")
                f.write(json.dumps({"tag": name, "last_updated": str(tag_date.date())}) + "\n")

                if not dry_run:
                    delete_url = f"https://hub.docker.com/v2/repositories/{DOCKER_REPO}/tags/{name}/"
                    delete_resp = requests.delete(delete_url, headers=headers)
                    if delete_resp.status_code == 204:
                        print(f"✅ Deleted Docker tag: {name}")
                    else:
                        print(f"❌ Failed to delete {name}: {delete_resp.status_code} - {delete_resp.text}")

    print(f"📄 Wrote audit file: {DOCKERHUB_AUDIT_FILE}")


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
    parser.add_argument("--dry-run",
                        action="store_true",
                        default=True,
                        help="Run in dry-run mode (default: True). Pass --dry-run false to actually delete.")
    args = parser.parse_args()

    try:
        env = load_env(args.env_file)
    except ValueError as e:
        print(f"Error loading environment variables: {e}")
        exit(1)

    threshold_date = get_date_threshold(args.older_than)
    github_token = env["GITHUB_TOKEN"]
    clean_github_versions(threshold_date, github_token, dry_run=args.dry_run)

    username = env["DOCKER_USERNAME"]
    password = env["DOCKER_PASSWORD"]
    if not username or not password:
        print("❌ DOCKER_USERNAME and DOCKER_PASSWORD environment variables are required.")
        exit(1)

    clean_dockerhub_tags(threshold_date, username, password, dry_run=args.dry_run)
