import json

import requests

from scripts.util.load_env import load_github_env_vars


def fetch_all_labels(env):
    # Load the GitHub token
    github_token = env["GITHUB_TOKEN"]
    repo_owner = env["REPO_OWNER"]
    repo_name = env["REPO_NAME"]

    # GitHub API endpoint for repository labels
    api_url = f"https://api.github.com/repos/{repo_owner}/{repo_name}/labels"
    headers = {"Authorization": f"token {github_token}", "Accept": "application/vnd.github.v3+json"}

    labels = []
    page = 1
    per_page = 100  # Max allowed by GitHub API is 100

    while True:
        print(f"Fetching page {page} of labels...")
        response = requests.get(api_url, headers=headers, params={"per_page": per_page, "page": page})

        if response.status_code != 200:
            raise Exception(f"API request failed: {response.status_code} - {response.text}")

        page_labels = response.json()
        labels.extend(page_labels)

        # Check if there are more pages (GitHub API uses Link header for pagination)
        if not page_labels or len(page_labels) < per_page:
            break

        page += 1

    return labels


def print_labels(labels):
    """
    Print the fetched labels in a readable format.
    """
    print(f"\nTotal labels found: {len(labels)}")
    print("Labels from https://github.com/vectordotdev/vector/labels:")
    for label in labels:
        name = label.get("name", "Unnamed")
        color = label.get("color", "No color")
        description = label.get("description", "No description") or "No description"
        print(f"- Name: {name}, Color: #{color}, Description: {description}")


def save_labels_to_json(labels, filename="vector_labels.json"):
    filtered_labels = [
        {"name": label["name"], "color": label["color"], "description": label.get("description", "No description")}
        for label in labels
    ]
    with open(filename, "w") as f:
        json.dump(filtered_labels, f, indent=4)
    print(f"Labels saved to '{filename}'")


if __name__ == "__main__":
    env = load_github_env_vars()
    all_labels = fetch_all_labels(env)
    print_labels(all_labels)
    save_labels_to_json(all_labels, f"{env['REPO_OWNER']}_{env['REPO_NAME']}_labels.json")
