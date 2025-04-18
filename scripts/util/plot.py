import argparse
import logging
import os
import random

import numpy as np
import pandas as pd
import matplotlib.pyplot as plt
import matplotlib.colors as mcolors
import hashlib

from matplotlib.ticker import MaxNLocator

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
    "domain: external docs": "#afab7e",
    "domain: ci": "#d6c720",
    "domain: deps": "#1f3f18",
    "domain: core": "#b50036",
    "domain: sources": "#2dbcbc",
    "domain: transforms": "#8615bf",
    "domain: sinks": "#ad4f47",
    "open_issues":  "#070707",
    "closed_issues": "#27b01c",
    "open_pull_requests":  "#070707",
    "closed_pull_requests": "#27b01c",
}

def main():
    setup_logger()

    parser = argparse.ArgumentParser(description="Generate visual summaries from GitHub issues CSVs.")
    parser.add_argument("--input-dir", required=True, help="Directory containing the summary CSV files")
    parser.add_argument("--start", help="Only include data from this YYYY-MM date forward")
    parser.add_argument(
        "--exclude-labels",
        help="Comma-separated list of labels to exclude from the label time-series chart",
    )
    args = parser.parse_args()

    table_names = ["issues", "pull_requests"]
    for table in table_names:
        monthly_csv = os.path.join(args.input_dir, f"{table}.monthly_summary.csv")
        if os.path.exists(monthly_csv):
            plot_monthly_summary_basic(monthly_csv, table, start_date=args.start)
            plot_integration_trends(monthly_csv, table, start_date=args.start, exclude_labels=args.exclude_labels, top_n=5)

        label_breakdown_csv = os.path.join(args.input_dir, f"{table}.label_breakdown.csv")
        if os.path.exists(label_breakdown_csv):
            plot_label_breakdown(
                label_breakdown_csv,
                table,
                start_date=args.start,
                exclude_labels=args.exclude_labels
            )

        open_by_label_csv = os.path.join(args.input_dir, f"{table}.label_counts.csv")
        if os.path.exists(open_by_label_csv):
            plot_label_count(
                open_by_label_csv,
                table,
                start_date=args.start,
                exclude_labels=args.exclude_labels
            )

        open_by_label_csv = os.path.join(args.input_dir, f"{table}.open_by_label.csv")
        if os.path.exists(os.path.join(args.input_dir, open_by_label_csv)):
            plot_label_state_counts(
                open_by_label_csv,
                table,
                top_n=30,
                exclude_labels=args.exclude_labels
            )


def get_label_color(label_name):
    if label_name in COLOR_MAP:
        return COLOR_MAP[label_name]

    all_colors = list(mcolors.CSS4_COLORS.values())
    seed = int(hashlib.md5(label_name.encode()).hexdigest(), 16)
    random.seed(seed)
    return random.choice(all_colors)

def plot_monthly_summary_basic(path, table, start_date=None):
    try:
        df = pd.read_csv(path)
        if start_date:
            df = df[df["month"] >= start_date]
        df["month"] = pd.to_datetime(df["month"])

        plt.figure(figsize=(12, 6))
        plt.style.use("ggplot")

        open_key = f"open_{table}"
        plt.plot(df["month"], df[open_key], label=f"Open {table}", color=COLOR_MAP.get(open_key), linewidth=4, marker='o')

        closed_key = f"closed_{table}"
        plt.plot(df["month"], df[closed_key], label=f"Closed {table}", color=COLOR_MAP.get(closed_key), linewidth=4, marker='o')
        plt.plot(df["month"], df["type: bug"], label="Bugs", color=COLOR_MAP.get("type: bug"), linewidth=2, linestyle='--')
        plt.plot(df["month"], df["type: feature"], label="Features", color=COLOR_MAP.get("type: feature"), linewidth=2, linestyle='--')
        plt.plot(df["month"], df["type: enhancement"], label="Enhancements", color=COLOR_MAP.get("type: enhancement"), linewidth=2, linestyle='--')

        plt.title(f"Monthly GitHub Trends ({table})", fontsize=16)
        plt.xlabel("Month", fontsize=12)
        plt.ylabel("Count", fontsize=12)
        plt.xticks(rotation=45)
        plt.legend()
        plt.grid(True)
        plt.tight_layout()

        output_path = os.path.join(OUTPUT_DIR, f"{table}.monthly_issues_trend.png")
        plt.savefig(output_path)
        logging.info(f"Saved plot to {output_path}")
        plt.close()
    except Exception as e:
        logging.warning(f"[{table}] Could not generate monthly trend plot: {e}")

def plot_integration_trends(csv_path, table, start_date=None, exclude_labels=None, top_n=None):
    # Load the CSV data into a DataFrame
    df = pd.read_csv(csv_path)

    if start_date:
        df = df[df["month"] >= start_date]

    numeric_cols = df.select_dtypes(
        include='number').columns.tolist()

    # Filter out columns that are not count-like (i.e., not whole numbers or have negative values)
    label_cols = []
    for col in numeric_cols:
        # Check if all values are integer-like and non-negative
        series = df[col].dropna()  # ignore NaNs for the check
        if (series >= 0).all():
            label_cols.append(col)

    # Exclude any specific columns that are not label categories
    exclude_set = set(exclude_labels) if exclude_labels else set()
    # Add common non-label columns to exclude set (if present in DataFrame)
    for non_label in ['month', 'open_issues', 'closed_issues']:
        if non_label in df.columns:
            exclude_set.add(non_label)

    # Filter out the excluded columns from label_cols
    label_cols = [col for col in label_cols
                  if col not in exclude_set
                  and (col.startswith("source:") or col.startswith("transform:") or col.startswith("sink:"))
                  and df[col].sum() > 0]

    # If requested, select only the top N labels by total count (sum over all months)
    if top_n is not None and top_n > 0:
        # Calculate total count for each label and get the top N labels
        top_labels = df[label_cols].sum().nlargest(top_n).index.tolist()
        label_cols = [col for col in label_cols if col in top_labels]

    # Safety check: if no label columns remain, we cannot plot
    if not label_cols:
        raise ValueError("No label count columns found for plotting after filtering. Check the data or parameters.")

    # Create the plot using the filtered label columns
    ax = df.plot(x='month', y=label_cols, marker='o')  # marker='o' for clarity on points (optional)

    # Format the Y-axis to have integer ticks only (appropriate for count data)
    ax.yaxis.set_major_locator(MaxNLocator(
        integer=True))  # force y-axis to use integer tick labels&#8203;:contentReference[oaicite:5]{index=5}
    ax.set_ylabel("Count")  # Y-axis label (counts)
    ax.set_xlabel("Month")  # X-axis label (month)
    ax.set_title(f"Integrations Trend ({table})", fontsize=16)  # Plot title (can be adjusted as needed)
    plt.xticks(rotation=45)  # Rotate month labels if they are long (optional)
    ax.legend(title="Labels", bbox_to_anchor=(1.02, 1), loc="upper left", borderaxespad=0)

    plt.tight_layout()  # Adjust layout to fit labels

    output_path = os.path.join(OUTPUT_DIR, f"{table}.integrations.top_{top_n}.monthly_trend.png")
    plt.savefig(output_path)
    logging.info(f"Saved plot to {output_path}")
    plt.close()

def plot_label_breakdown(path, table, top_n=15, start_date=None, exclude_labels=None):
    try:
        df = pd.read_csv(path)

        if "month" in df.columns and start_date:
            df = df[df["month"] >= start_date]

        if exclude_labels:
            exclude_set = set(label.strip() for label in exclude_labels.split(","))
            df = df[~df["label_name"].isin(exclude_set)]

        df = df.sort_values("count", ascending=False).head(top_n)
        colors = [get_label_color(label) for label in df["label_name"]]

        plt.figure(figsize=(10, 6))
        plt.barh(df["label_name"], df["count"], color=colors)
        plt.title(f"Top Labels by Frequency ({table})", fontsize=16)
        plt.xlabel("Count", fontsize=12)
        plt.gca().invert_yaxis()
        plt.tight_layout()

        output_path = os.path.join(OUTPUT_DIR, f"{table}.top_labels.png")
        plt.savefig(output_path)
        logging.info(f"Saved plot to {output_path}")
        plt.close()
    except Exception as e:
        logging.warning(f"[{table}] Could not generate label breakdown plot: {e}")


def plot_label_count(path, table, top_n=8, start_date=None, exclude_labels=None):
    try:
        df = pd.read_csv(path)
        df["month"] = df["month"].astype(str)

        if start_date:
            df = df[df["month"] >= start_date]

        if exclude_labels:
            exclude_set = set(label.strip() for label in exclude_labels.split(","))
            df = df[~df["label_name"].isin(exclude_set)]

        # Top N labels by total count
        top_labels = (
            df.groupby("label_name")["count"]
            .sum()
            .sort_values(ascending=False)
            .head(top_n)
            .index
            .tolist()
        )
        df = df[df["label_name"].isin(top_labels)]

        # Pivot data
        pivot_df = df.pivot(index="month", columns="label_name", values="count").fillna(0)
        pivot_df = pivot_df[top_labels]  # Ensure consistent column order
        pivot_df = pivot_df.sort_index()

        months = pivot_df.index.tolist()
        n_labels = len(top_labels)
        bar_group_width = 0.8
        bar_width = bar_group_width / n_labels
        offsets = [(j - (n_labels - 1) / 2) * bar_width for j in range(n_labels)]

        colors = {label: get_label_color(label) for label in top_labels}

        fig, ax = plt.subplots(figsize=(14, 7))

        for label in top_labels:
            x_positions = []
            heights = []
            for month_index, month in enumerate(months):
                row = pivot_df.loc[month]
                sorted_labels = row.sort_values().index.tolist()
                if label in sorted_labels:
                    pos = sorted_labels.index(label)
                    x = month_index + offsets[pos]
                    x_positions.append(x)
                    heights.append(row[label])
            ax.bar(x_positions, heights, width=bar_width, label=label, color=colors[label])

        # Axes styling
        ax.set_xticks(np.arange(len(months)))
        ax.set_xticklabels(months, rotation=45)
        ax.set_xlabel("Month")
        ax.set_ylabel("Issue Count")
        ax.set_title(f"Top {top_n} Labels Over Time ({table})", fontsize=16)
        ax.grid(axis='y', linestyle='--', alpha=0.7)

        # Legend sorted by total volume
        label_totals = pivot_df.sum().to_dict()
        labels_sorted = sorted(top_labels, key=lambda lbl: -label_totals.get(lbl, 0))
        handles, labels = ax.get_legend_handles_labels()
        handles_sorted = [handles[labels.index(lbl)] for lbl in labels_sorted]
        ax.legend(
            handles_sorted,
            labels_sorted,
            title="Label",
            bbox_to_anchor=(1.01, 1),
            loc='upper left',
            borderaxespad=0.
        )

        plt.tight_layout()
        output_path = os.path.join(OUTPUT_DIR, f"{table}.label_counts.png")
        plt.savefig(output_path)
        logging.info(f"Saved plot to {output_path}")
        plt.close()

    except Exception as e:
        logging.warning(f"[{table}] Could not generate label time-series bar chart: {e}")


def plot_label_state_counts(path, table, top_n, exclude_labels=None):
    try:
        df = pd.read_csv(path)

        if exclude_labels:
            exclude_set = set(label.strip() for label in exclude_labels.split(","))
            df = df[~df["label_name"].isin(exclude_set)]

        df = df[df["label_name"].str.startswith(("source:", "transform:", "sink:"))]

        # Add total count column and sort
        df["total"] = df["open_count"] + df["closed_count"]
        df = df.sort_values("total", ascending=False).head(top_n)

        # Plot
        fig, ax = plt.subplots(figsize=(12, 6))
        ax.barh(df["label_name"], df["closed_count"], label="Closed", color="black")
        ax.barh(df["label_name"], df["open_count"], left=df["closed_count"], label="Open", color="green")

        ax.set_xlabel("Count")
        ax.set_ylabel("Label")
        ax.set_title(f"Top {top_n} Integrations Label Count ({table})", fontsize=16)
        ax.legend(loc="lower right")
        plt.tight_layout()
        plt.gca().invert_yaxis()  # highest total on top

        output_path = os.path.join(OUTPUT_DIR, f"{table}.open_closed_total_label_count.png")
        plt.savefig(output_path)
        logging.info(f"Saved plot to {output_path}")
        plt.close()
    except Exception as e:
        logging.warning(f"[{table}] Could not generate label count chart: {e}")

if __name__ == "__main__":
    main()
