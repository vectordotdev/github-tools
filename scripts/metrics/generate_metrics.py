import json
import argparse
import os
import logging

def main():
    # Parse command-line arguments
    parser = argparse.ArgumentParser(description="Analyze GitHub issues JSON to produce label and monthly status summaries.")
    parser.add_argument("input_json", help="Path to the GitHub issues JSON file")
    args = parser.parse_args()
    input_path = args.input_json

    # Configure logging for info and error messages
    logging.basicConfig(level=logging.INFO, format='%(levelname)s: %(message)s')
    logging.info(f"Reading input JSON file: {input_path}")

    # Verify input file exists
    if not os.path.isfile(input_path):
        logging.error(f"Input file not found: {input_path}")
        return 1

    # Load issues from JSON
    try:
        with open(input_path, 'r') as f:
            issues = json.load(f)
    except json.JSONDecodeError as e:
        logging.error(f"Failed to parse JSON from {input_path}: {e}")
        return 1
    except Exception as e:
        logging.error(f"Error reading file {input_path}: {e}")
        return 1

    if not isinstance(issues, list):
        logging.error(f"JSON content in {input_path} is not a list of issues.")
        return 1

    logging.info(f"Loaded {len(issues)} issues from {input_path}")

    # Compute label summary
    logging.info("Computing label summary statistics")
    label_counts = {}
    for issue in issues:
        labels = issue.get("labels", [])
        if not labels:
            continue  # skip issues with no labels
        for label in labels:
            # Each label could be an object with 'name' or just a string
            if isinstance(label, dict):
                label_name = label.get("name")
            else:
                label_name = str(label)
            if not label_name:
                continue
            label_counts[label_name] = label_counts.get(label_name, 0) + 1

    # Compute monthly status summary
    logging.info("Computing monthly status summary")
    monthly_counts = {}
    for issue in issues:
        created_at = issue.get("created_at")
        state = issue.get("state")
        if not created_at or not state:
            logging.warning("Issue missing created_at or state; skipping.")
            continue
        # Extract year and month from date string (format: "YYYY-MM-DDTHH:MM:SSZ")
        if len(created_at) < 7 or created_at[4] != '-' or created_at[7] != '-':
            logging.warning(f"Unexpected date format for issue: {created_at}")
            continue
        year = created_at[0:4]
        month = created_at[5:7]
        if not (year.isdigit() and month.isdigit()):
            logging.warning(f"Non-numeric year/month in date for issue: {created_at}")
            continue
        # Initialize nested structure and increment count
        monthly_counts.setdefault(year, {})
        monthly_counts[year].setdefault(month, {})
        monthly_counts[year][month][state] = monthly_counts[year][month].get(state, 0) + 1

    # Prepare output file paths in the same directory as input
    input_dir = os.path.dirname(os.path.abspath(input_path)) or '.'
    label_summary_file = os.path.join(input_dir, "label_summary.json")
    monthly_summary_file = os.path.join(input_dir, "monthly_status_summary.json")

    # Write label summary to JSON file
    try:
        with open(label_summary_file, 'w') as f_out:
            json.dump(label_counts, f_out, indent=4)
        logging.info(f"Label summary saved to {label_summary_file}")
    except Exception as e:
        logging.error(f"Failed to write label summary to {label_summary_file}: {e}")
        return 1

    # Sort years and months for a neat output structure
    sorted_monthly_counts = {}
    for year in sorted(monthly_counts.keys()):
        sorted_monthly_counts[year] = {}
        for month in sorted(monthly_counts[year].keys()):
            sorted_monthly_counts[year][month] = monthly_counts[year][month]

    # Write monthly status summary to JSON file
    try:
        with open(monthly_summary_file, 'w') as f_out:
            json.dump(sorted_monthly_counts, f_out, indent=4)
        logging.info(f"Monthly status summary saved to {monthly_summary_file}")
    except Exception as e:
        logging.error(f"Failed to write monthly status summary to {monthly_summary_file}: {e}")
        return 1

    logging.info("Analysis completed successfully.")
    return 0

if __name__ == "__main__":
    exit(main())
