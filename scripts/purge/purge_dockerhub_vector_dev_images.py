import argparse
import os

from scripts.purge.utils import purge_dockerhub_images, get_date_threshold
from scripts.util.load_env import load_env

# ----------------------------
# Configuration
# ----------------------------

DOCKER_REPO = "timberio/vector-dev"

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DIR = os.path.abspath(os.path.join(SCRIPT_DIR, "../../out/purge"))
os.makedirs(OUTPUT_DIR, exist_ok=True)

AUDIT_FILE = os.path.join(OUTPUT_DIR, "vector_dev_dockerhub.jsonl")

# ----------------------------
# Entry Point
# ----------------------------

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Delete old 'nightly' tags from timberio/vector-dev on Docker Hub.")
    parser.add_argument("--older-than",
                        type=int,
                        default=30,
                        help="Delete tags older than this many days (default: 30)")
    parser.add_argument("--env-file",
                        type=str,
                        required=True,
                        help="Path to .env file containing DOCKER_USERNAME and DOCKER_PASSWORD")

    dry_run_group = parser.add_mutually_exclusive_group()
    dry_run_group.add_argument("--dry-run", dest="dry_run", action="store_true", help="Run without deleting (default)")
    dry_run_group.add_argument("--no-dry-run", dest="dry_run", action="store_false", help="Actually delete tags")
    parser.set_defaults(dry_run=True)
    args = parser.parse_args()

    try:
        env = load_env(args.env_file)
    except ValueError as e:
        print(f"Error loading environment variables: {e}")
        exit(1)

    username = env.get("DOCKER_USERNAME")
    password = env.get("DOCKER_PASSWORD")
    if not username or not password:
        print("❌ DOCKER_USERNAME and DOCKER_PASSWORD environment variables are required.")
        exit(1)

    threshold = get_date_threshold(args.older_than)
    purge_dockerhub_images(
        repo=DOCKER_REPO,
        audit_file=AUDIT_FILE,
        threshold=threshold,
        username=username,
        password=password,
        dry_run=args.dry_run,
        tag_filter=lambda tag: True
    )
