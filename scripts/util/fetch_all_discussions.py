import argparse
import json
import logging
import os
from datetime import datetime

import requests

from scripts.logging.custom_logging import setup_logger
from scripts.util.load_env import load_github_env_vars

GRAPHQL_URL = "https://api.github.com/graphql"
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DIR = os.path.abspath(os.path.join(SCRIPT_DIR, "../../out/historical/discussions"))
os.makedirs(OUTPUT_DIR, exist_ok=True)


def fetch_discussions(env, limit=100):
    """Fetch GitHub discussions metadata via the GraphQL API."""
    github_token = env["GITHUB_TOKEN"]
    repo_owner = env["REPO_OWNER"]
    repo_name = env["REPO_NAME"]

    # https://docs.github.com/en/graphql/guides/using-the-graphql-api-for-discussions
    query = """
    query($owner: String!, $name: String!, $first: Int!, $after: String) {
      repository(owner: $owner, name: $name) {
        discussions(first: $first, after: $after) {
          pageInfo {
            endCursor
            hasNextPage
          }
          nodes {
            number
            title
            bodyText
            url
            createdAt
            updatedAt
            isAnswered
            locked
            author {
              login
            }
            category {
              name
            }
            comments {
              totalCount
            }
            upvoteCount
          }
        }
      }
    }
    """

    discussions = []
    has_next_page = True
    after = None

    headers = {
        "Authorization": f"Bearer {github_token}",
        "Accept": "application/vnd.github+json"
    }

    while has_next_page:
        variables = {
            "owner": repo_owner,
            "name": repo_name,
            "first": limit,
            "after": after
        }
        response = requests.post(GRAPHQL_URL, json={"query": query, "variables": variables}, headers=headers)

        if response.status_code != 200:
            logging.warning(f"GraphQL request failed: {response.status_code}: {response.text}")
            break

        result = response.json()

        # Check for errors in GraphQL response
        if "errors" in result:
            logging.error(f"GraphQL errors: {result['errors']}")
            break

        # Continue as normal
        data = result.get("data", {}).get("repository", {}).get("discussions", {})

        discussions.extend(data.get("nodes", []))
        has_next_page = data.get("pageInfo", {}).get("hasNextPage", False)
        after = data.get("pageInfo", {}).get("endCursor")

        logging.info(f"Fetched {len(discussions)} discussions so far...")

    return discussions


def write_to_json_file(discussions, repo_owner, repo_name):
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    json_out_file = os.path.join(OUTPUT_DIR, f"{timestamp}_{repo_owner}_{repo_name}_discussions.json")
    logging.info(f"Saving discussions to {json_out_file}...")
    try:
        with open(json_out_file, "w") as f:
            json.dump(discussions, f, indent=4)
        logging.info("Discussions saved successfully.")
    except Exception as e:
        logging.error(f"Error saving discussions: {e}")


def main():
    setup_logger()
    parser = argparse.ArgumentParser(description="Fetch GitHub discussions from a repository.")
    parser.add_argument("--limit", type=int, default=100, help="Number of discussions per page (max 100)")
    args = parser.parse_args()

    try:
        env = load_github_env_vars()
    except ValueError as e:
        print(f"Error loading environment variables: {e}")
        return 1

    try:
        os.makedirs(OUTPUT_DIR, exist_ok=True)
        discussions = fetch_discussions(env, limit=args.limit)
        write_to_json_file(discussions, env["REPO_OWNER"], env["REPO_NAME"])
    except Exception as e:
        print(f"Error fetching discussions: {e}")
        return 1

    return 0


if __name__ == "__main__":
    import sys

    sys.exit(main())
