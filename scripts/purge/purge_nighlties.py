import argparse
import os
from datetime import datetime, timezone, timedelta

from scripts.purge.utils import get_date_threshold, purge_dockerhub_images, purge_github_versions
from scripts.util.load_env import load_env

# ----------------------------
# Configuration
# ----------------------------

DOCKER_REPO = "timberio/vector"

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DIR = os.path.abspath(os.path.join(SCRIPT_DIR, "../../out/purge"))
os.makedirs(OUTPUT_DIR, exist_ok=True)

GITHUB_AUDIT_FILE = os.path.join(OUTPUT_DIR, "nightly_github.jsonl")
DOCKERHUB_AUDIT_FILE = os.path.join(OUTPUT_DIR, "nightly_dockerhub.jsonl")

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
        purge_github_versions(
            threshold=threshold_date,
            github_token=github_token,
            audit_file=GITHUB_AUDIT_FILE,
            dry_run=args.dry_run
        )

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
