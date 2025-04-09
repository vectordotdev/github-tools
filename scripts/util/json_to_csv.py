import json
import csv
import argparse
import os

def convert_label_summary_to_csv(json_path, csv_path=None):
    # Read label summary JSON
    with open(json_path, "r") as f:
        data = json.load(f)

    # Prepare rows for CSV
    rows = [("Category", "Value", "Count")]
    for category, values in data.items():
        for value, count in values.items():
            rows.append((category, value, count))

    # Determine CSV path if not provided
    if not csv_path:
        base, _ = os.path.splitext(json_path)
        csv_path = base + ".csv"

    # Write to CSV
    with open(csv_path, "w", newline="") as f:
        writer = csv.writer(f)
        writer.writerows(rows)

    print(f"✅ CSV saved to: {csv_path}")


def main():
    parser = argparse.ArgumentParser(description="Convert label_summary.json to CSV for visualization")
    parser.add_argument("json_file", help="Path to label_summary.json")
    parser.add_argument("--csv_file", help="Optional path to output CSV file")

    args = parser.parse_args()
    convert_label_summary_to_csv(args.json_file, args.csv_file)


if __name__ == "__main__":
    main()
