import os

from dotenv import load_dotenv, find_dotenv

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
ENV_FILE = os.path.abspath(os.path.join(SCRIPT_DIR, "../vector-default.env"))


def load_env(env_file=ENV_FILE) -> dict:
    """
    Load environment variables from a .env file.
    Supports plain values and op:// references (when invoked via `op run --env-file=...`).
    Already-set environment variables (e.g. injected by `op run`) take precedence.

    Args:
        env_file (str): Path to the .env file.

    Returns:
        dict: A dictionary of all key=value pairs from the .env file.

    Raises:
        ValueError: If the .env file is missing or can't be parsed.
    """
    if not os.path.exists(env_file):
        env_file = find_dotenv()
        if not env_file:
            raise ValueError(f"No .env file found at {env_file}")

    success = load_dotenv(env_file, override=True, verbose=True)
    if not success:
        raise ValueError(f"Failed to load .env file at: {env_file}")

    return {k: v for k, v in os.environ.items() if not k.startswith("_")}


# Example usage if running this file directly:
if __name__ == "__main__":
    try:
        env_vars = load_env()
        print(f"Loaded environment variables: {env_vars}")
    except ValueError as e:
        print(f"Error: {e}")
