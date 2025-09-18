import json
from datetime import datetime, timezone, timedelta

import requests


def get_date_threshold(days_old):
    return datetime.now(timezone.utc) - timedelta(days=days_old)


def list_docker_tags(repo):
    docker_tags_api = f"https://hub.docker.com/v2/repositories/{repo}/tags?page_size=100"

    tags = []
    page = 1
    while True:
        resp = requests.get(f"{docker_tags_api}&page={page}")
        resp.raise_for_status()
        data = resp.json()
        tags.extend(data.get("results", []))
        if not data.get("next"):
            break
        page += 1
    return tags


def list_tags(repo):
    tags_api = f"https://hub.docker.com/v2/repositories/{repo}/tags?page_size=100"
    tags = []
    page = 1
    while True:
        resp = requests.get(f"{tags_api}&page={page}")
        resp.raise_for_status()
        data = resp.json()
        tags.extend(data.get("results", []))
        if not data.get("next"):
            break
        page += 1
    return tags


def purge_dockerhub_images(repo, audit_file, threshold, username, password, dry_run=True, tag_filter=None):
    """
    Deletes Docker Hub tags for a given repo that are older than the threshold and match the filter.

    Parameters:
        - repo (str): Docker Hub repo in format "namespace/name"
        - audit_file (str): Path to write audit log
        - threshold (datetime): Tags older than this date will be purged
        - username (str): Docker Hub username
        - password (str): Docker Hub password
        - dry_run (bool): If True, don't actually delete tags
        - tag_filter (Callable[[str], bool]): A function that returns True if tag should be considered
    """
    print(f"🔍 Checking Docker Hub tags for '{repo}' older than {threshold.date()}")

    login_resp = requests.post(
        "https://hub.docker.com/v2/users/login/",
        json={"username": username, "password": password}
    )

    if login_resp.status_code != 200:
        print(f"❌ Failed to authenticate with Docker Hub: {login_resp.text}")
        return

    token = login_resp.json()["token"]
    headers = {"Authorization": f"JWT {token}"}

    if tag_filter is None:
        # Default to only "nightly" tags
        tag_filter = lambda tag: tag.startswith("nightly")

    with open(audit_file, "w") as f:
        f.write(json.dumps({"dry_run": dry_run}) + "\n")

        for tag in list_tags(repo):
            name = tag["name"]
            tag_date = datetime.fromisoformat(tag["last_updated"].replace("Z", "+00:00"))

            if tag_date < threshold and tag_filter(name):
                print(f"🧹 Found tag: {name} (last updated: {tag_date.date()})")

                if dry_run:
                    f.write(json.dumps({"tag": name, "last_updated": str(tag_date.date())}) + "\n")
                else:
                    delete_url = f"https://hub.docker.com/v2/repositories/{repo}/tags/{name}/"
                    delete_resp = requests.delete(delete_url, headers=headers)
                    if delete_resp.status_code == 204:
                        print(f"✅ Deleted tag: {name}")
                        f.write(json.dumps({"tag": name, "last_updated": str(tag_date.date())}) + "\n")
                    else:
                        print(f"❌ Failed to delete {name}: {delete_resp.status_code} - {delete_resp.text}")

    print(f"📄 Audit log saved to: {audit_file}")
