import argparse
import os

from scripts.purge.utils import get_date_threshold, purge_github_untagged_versions
from scripts.util.load_env import load_env

# ----------------------------
# Configuration
# ----------------------------

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DIR = os.path.abspath(os.path.join(SCRIPT_DIR, "../../out/purge"))
os.makedirs(OUTPUT_DIR, exist_ok=True)

GITHUB_AUDIT_FILE = os.path.join(OUTPUT_DIR, "untagged_github.jsonl")

# ----------------------------
# Entry Point
# ----------------------------

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Delete old untagged version from GitHub.")
    parser.add_argument("--older-than",
                        type=int,
                        default=30,
                        help="Delete artifacts older than this many days (default: 30)")
    parser.add_argument("--env-file",
                        type=str,
                        required=True,
                        help="Path to the .env file to load environment variables from")

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

    threshold_date = get_date_threshold(args.older_than)
    github_token = env["GITHUB_TOKEN"]
    purge_github_untagged_versions(
        threshold=threshold_date,
        github_token=github_token,
        audit_file=GITHUB_AUDIT_FILE,
        dry_run=args.dry_run,
    )
