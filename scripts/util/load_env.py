import os

from dotenv import load_dotenv, find_dotenv

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))  # Get the directory where this script is located
ENV_FILE = os.path.abspath(os.path.join(SCRIPT_DIR, "../vector-default.env"))


def load_github_env_vars(env_file=ENV_FILE):
    """
    Load and validate GitHub-related environment variables from the given .env file.

    Args:
        env_file (str): Path to the .env file.

    Returns:
        dict: A dictionary containing GITHUB_TOKEN, REPO_OWNER, and REPO_NAME.

    Raises:
        ValueError: If any required environment variables are missing.
    """
    # Load environment variables from the custom .env file
    if not os.path.exists(env_file):
        env_file = find_dotenv()
    assert load_dotenv(env_file, override=True, verbose=True)

    # Read environment variables
    github_token = os.getenv("GITHUB_TOKEN")
    repo_owner = os.getenv("REPO_OWNER")
    repo_name = os.getenv("REPO_NAME")

    # Validate environment variables
    if not github_token:
        raise ValueError("GITHUB_TOKEN not found in .env file. Please set it before running the script.")
    if not repo_owner:
        raise ValueError("REPO_OWNER not found in .env file. Please set it before running the script.")
    if not repo_name:
        raise ValueError("REPO_NAME not found in .env file. Please set it before running the script.")

    # Return the environment variables in a dictionary
    return {
        "GITHUB_TOKEN": github_token,
        "REPO_OWNER": repo_owner,
        "REPO_NAME": repo_name
    }


# Example usage if running this file directly:
if __name__ == "__main__":
    try:
        env_vars = load_github_env_vars()
        print(f"Loaded environment variables: {env_vars}")
    except ValueError as e:
        print(f"Error: {e}")
