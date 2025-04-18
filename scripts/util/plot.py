import argparse
import logging
import os
import random

import pandas as pd
import matplotlib.pyplot as plt
import matplotlib.colors as mcolors
import hashlib
from scripts.logging.custom_logging import setup_logger


# Constants
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DIR = os.path.abspath(os.path.join(SCRIPT_DIR, "../../out/plot"))
os.makedirs(OUTPUT_DIR, exist_ok=True)

# Custom label color overrides
COLOR_MAP = {
    "type: bug": "#FF4C4C",
    "type: feature": "#4C9AFF",
    "type: enhancement": "#36B37E",
    "sink: aws_s3": "#FFC107",
    "sink: splunk_hec": "#27b01c",
    "domain: external_docs": "#9C27B0",
    "open_issues":  "#FF4C4C",
    "closed_issues": "#27b01c",
    # Add more as needed
}

def main():
    setup_logger()

    # CLI args
    parser = argparse.ArgumentParser(description="Generate visual summaries from GitHub issues CSVs.")
    parser.add_argument("--input-dir", required=True, help="Directory containing the summary CSV files")
    parser.add_argument("--start", help="Only include data from this YYYY-MM date forward")
    parser.add_argument(
        "--exclude-labels",
        help="Comma-separated list of labels to exclude from the label time-series chart",
    )
    args = parser.parse_args()

    monthly_csv = os.path.join(args.input_dir, "monthly_summary.issues.csv")
    label_breakdown = os.path.join(args.input_dir, "label_breakdown.issues.csv")
    label_counts = os.path.join(args.input_dir, "label_counts.issues.csv")

    if os.path.exists(monthly_csv):
        plot_monthly_summary(monthly_csv, start_date=args.start)

    if os.path.exists(label_breakdown):
        plot_label_breakdown(
            label_breakdown,
            start_date=args.start,
            exclude_labels=args.exclude_labels
        )

    if os.path.exists(label_counts):
        plot_label_count(
            label_counts,
            start_date=args.start,
            exclude_labels=args.exclude_labels
        )


def get_label_color(label_name):
    if label_name in COLOR_MAP:
        return COLOR_MAP[label_name]

    # Use a hash to pick a color from the default tableau colors
    all_colors = list(mcolors.CSS4_COLORS.values())  # 140+ distinct named colors
    seed = int(hashlib.md5(label_name.encode()).hexdigest(), 16)
    random.seed(seed)
    return random.choice(all_colors)

# === Plot 1: Monthly Issue Trends ===
def plot_monthly_summary(path, start_date=None):
    try:
        df = pd.read_csv(path)
        if start_date:
            df = df[df["month"] >= start_date]

        plt.figure(figsize=(12, 6))
        plt.style.use("ggplot")

        plt.plot(df["month"], df["open_issues"], label="Open Issues", color=COLOR_MAP.get("open_issues"), linewidth=4, marker='o')
        plt.plot(df["month"], df["closed_issues"], label="Closed Issues", color=COLOR_MAP.get("closed_issues"), linewidth=4, marker='o')
        plt.plot(df["month"], df["bugs"], label="Bugs", color=COLOR_MAP.get("type: bug"), linewidth=2, linestyle='--')
        plt.plot(df["month"], df["features"], label="Features", color=COLOR_MAP.get("type: feature"), linewidth=2, linestyle='--')
        plt.plot(df["month"], df["enhancements"], label="Enhancements", color=COLOR_MAP.get("type: enhancement"), linewidth=2, linestyle='--')

        plt.title("Monthly GitHub Issue Trends", fontsize=16)
        plt.xlabel("Month", fontsize=12)
        plt.ylabel("Number of Issues", fontsize=12)
        plt.xticks(rotation=45)
        plt.legend()
        plt.grid(True)
        plt.tight_layout()

        output_path = os.path.join(OUTPUT_DIR, "monthly_issues_trend.png")
        plt.savefig(output_path)
        logging.info(f"✅ Saved monthly trend plot to {output_path}")
        plt.close()
    except Exception as e:
        logging.warning(f"Could not generate monthly trend plot: {e}")

# === Plot 2: Label Breakdown ===
def plot_label_breakdown(path, top_n=15, start_date=None, exclude_labels=None):
    try:
        df = pd.read_csv(path)

        if "month" in df.columns and start_date:
            df = df[df["month"] >= start_date]

        if exclude_labels:
            exclude_set = set(label.strip() for label in exclude_labels.split(","))
            df = df[~df["label_name"].isin(exclude_set)]

        df = df.sort_values("count", ascending=False).head(top_n)

        # Assign custom colors
        colors = [get_label_color(label) for label in df["label_name"]]

        plt.figure(figsize=(10, 6))
        plt.barh(df["label_name"], df["count"], color=colors)
        plt.title("Top Labels by Frequency", fontsize=16)
        plt.xlabel("Count", fontsize=12)
        plt.gca().invert_yaxis()
        plt.tight_layout()

        output_path = os.path.join(OUTPUT_DIR, "top_labels.png")
        plt.savefig(output_path)
        logging.info(f"✅ Saved label breakdown plot to {output_path}")
        plt.close()
    except Exception as e:
        logging.warning(f"Could not generate label breakdown plot: {e}")

# === Plot 3: Label Time-Series as Bar Chart ===
def plot_label_count(path, top_n=8, start_date=None, exclude_labels=None):
    try:
        df = pd.read_csv(path)
        df["month"] = df["month"].astype(str)

        if start_date:
            df = df[df["month"] >= start_date]

        if exclude_labels:
            exclude_set = set(label.strip() for label in exclude_labels.split(","))
            df = df[~df["label_name"].isin(exclude_set)]

        top_labels = (
            df.groupby("label_name")["count"]
            .sum()
            .sort_values(ascending=False)
            .head(top_n)
            .index
        )
        df = df[df["label_name"].isin(top_labels)]

        pivot_df = df.pivot(index="month", columns="label_name", values="count").fillna(0)
        pivot_df = pivot_df.sort_index()

        colors = [get_label_color(label) for label in pivot_df.columns]

        ax = pivot_df.plot(kind="bar", figsize=(14, 7), width=0.8, color=colors)
        plt.title(f"Top {top_n} Labels Over Time", fontsize=16)
        plt.xlabel("Month", fontsize=12)
        plt.ylabel("Issue Count", fontsize=12)
        plt.xticks(rotation=45)
        plt.legend(title="Label", bbox_to_anchor=(1.05, 1), loc='upper left')
        plt.tight_layout()

        output_path = os.path.join(OUTPUT_DIR, "label_counts.png")
        plt.savefig(output_path)
        logging.info(f"✅ Saved label time-series bar chart to {output_path}")
        plt.close()
    except Exception as e:
        logging.warning(f"Could not generate label time-series bar chart: {e}")

if __name__ == "__main__":
    main()
