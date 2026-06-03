import argparse
import hashlib
import logging
import os
import re
from datetime import date
from pathlib import Path

import matplotlib.pyplot as plt
import numpy as np
import numpy.ma as ma
import pandas as pd
from matplotlib.patches import FancyBboxPatch
from matplotlib.ticker import MaxNLocator

from scripts.logging.custom_logging import setup_logger
from scripts.util.load_env import load_env

# Constants
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DIR = os.path.abspath(os.path.join(SCRIPT_DIR, "../../data/images"))
os.makedirs(OUTPUT_DIR, exist_ok=True)

# Very dark — used in place of pure black for less harsh contrast on slides.
DARK = "#1a1a1a"
MUTED = "#555555"
WHITE = "white"

# Semantic palette. Reuse these instead of hardcoding colors at call sites.
CLOSED = "#006400"           # closed issues/PRs/discussions
OPEN = "#FF8C00"             # open issues/PRs (darkorange)
ANSWERED = "#4C9AFF"         # answered discussions; also "type: feature"
BUG = "#FF4C4C"              # "type: bug"
ENHANCEMENT = "#36B37E"      # "type: enhancement"; also new contributors
TASK = "#FFA500"             # "type: task"
RETURNING_CONTRIBUTOR = "#8E5CE6"
NEW_CONTRIBUTOR = ENHANCEMENT

# Heatmap colormap for contributor activity chart.
HEATMAP_CMAP = "YlOrRd"

KNOWN_BOT_LOGINS = {
    "dependabot",
    "dependabot-preview",
    "renovate",
    "handlerbot",
    "step-security-bot",
    "tronboto",
}


def is_bot_login(login: str) -> bool:
    return (
        isinstance(login, str)
        and (login.endswith("[bot]") or login in KNOWN_BOT_LOGINS)
    )


# Custom label color overrides
COLOR_MAP = {
    "type: bug": BUG,
    "Bug": BUG,
    "type: feature": ANSWERED,
    "Feature": ANSWERED,
    "type: enhancement": ENHANCEMENT,
    "Enhancement": ENHANCEMENT,
    "type: task": TASK,
    "Task": TASK,
    "domain: external docs": "#afab7e",
    "domain: ci": "#d6c720",
    "dependencies": "#1f3f18",
    "domain: deps": "#1f3f18",
    "domain: core": "#b50036",
    "domain: sources": "#2dbcbc",
    "domain: transforms": "#8615bf",
    "domain: sinks": "#ad4f47",
    "created_issues": DARK,
    "closed_issues": CLOSED,
    "created_pull_requests": DARK,
    "closed_pull_requests": CLOSED,
}

# Pretty table-name in titles ("pull_requests" -> "PRs", etc.)
TABLE_PRETTY = {
    "pull_requests": "PRs",
    "issues": "Issues",
    "discussions": "Discussions",
}


def pretty_table(table: str) -> str:
    return TABLE_PRETTY.get(table, table)

# Type overlay lines for the monthly trend chart.
# Each entry: (display_label, color, list_of_possible_column_names)
# The first matching column found in the CSV will be used.
TYPE_OVERLAYS = [
    ("Bugs",         BUG,         ["type: bug",         "Bug"]),
    ("Features",     ANSWERED,    ["type: feature",     "Feature"]),
    ("Enhancements", ENHANCEMENT, ["type: enhancement", "Enhancement"]),
    ("Tasks",        TASK,        ["type: task",        "Task"]),
]


def setup_styles():
    # Base look-and-feel: matplotlib's seaborn whitegrid. Provides white
    # background, soft gray grid, and modern font/color defaults.
    plt.style.use("seaborn-v0_8-whitegrid")

    # Layer our own overrides on top.
    plt.rcParams["font.size"] = 12
    plt.rcParams["axes.titlesize"] = 16
    plt.rcParams["axes.labelsize"] = 12
    plt.rcParams["xtick.labelsize"] = 10
    plt.rcParams["ytick.labelsize"] = 10
    plt.rcParams["savefig.dpi"] = 150
    plt.rcParams["savefig.bbox"] = "tight"
    plt.rcParams["savefig.pad_inches"] = 0.2


def set_axis_labels(ax, xlabel, ylabel):
    if xlabel:
        ax.set_xlabel(xlabel, fontsize=12, fontstyle='italic')
    if ylabel:
        ax.set_ylabel(ylabel, fontsize=12, fontstyle='italic')


def round_bars(ax, radius_frac=0.5):
    """Replace every Rectangle bar in `ax` with a rounded FancyBboxPatch.

    Works for both horizontal and vertical bars — radius is derived from
    the smaller dimension of each bar (so the bar reads as a pill in its
    narrow direction). radius_frac=0.5 produces full rounding; lower
    values produce a softer rectangle. Preserves labels for legend.
    """
    for patch in list(ax.patches):
        if not hasattr(patch, "get_xy"):
            continue
        x, y = patch.get_xy()
        w = patch.get_width()
        h = patch.get_height()
        if w == 0 or h == 0:
            continue
        # boxstyle "round,pad=R,rounding_size=R" expands the bbox by R on
        # every side, so shrink the inner rect by R to keep the final
        # shape the same physical size as the original Rectangle.
        r = min(abs(w), abs(h)) * radius_frac
        color = patch.get_facecolor()
        label = patch.get_label()
        patch.remove()
        new = FancyBboxPatch(
            (x + r, y + r),
            w - 2 * r,
            h - 2 * r,
            boxstyle=f"round,pad={r},rounding_size={r}",
            linewidth=0,
            facecolor=color,
            mutation_aspect=1,
        )
        if label and not label.startswith("_"):
            new.set_label(label)
        ax.add_patch(new)


def parse_args():
    parser = argparse.ArgumentParser(
        description="Generate visual summaries from GitHub issues CSVs."
    )
    parser.add_argument(
        "--input-dir",
        required=True,
        help="Directory containing the summary CSV files",
    )
    parser.add_argument(
        "--start",
        help="Only include data from this YYYY-MM date forward",
    )
    parser.add_argument(
        "--window",
        help="Lookback window relative to today, e.g. 3y or 18m. Computes --start automatically; ignored if --start is also given.",
    )
    parser.add_argument(
        "--exclude-labels",
        type=str,
        help="Comma-separated list of labels to exclude from the various charts",
    )
    parser.add_argument(
        "--env-file",
        type=str,
        help="Path to the .env file to load environment variables from",
    )

    args = parser.parse_args()

    raw = args.exclude_labels
    if raw:
        labels = {lbl.strip() for lbl in raw.split(",") if lbl.strip()}
        args.exclude_labels = labels if labels else None
    else:
        args.exclude_labels = None

    if args.window and not args.start:
        m = re.fullmatch(r"(\d+)(y|m)", args.window.strip().lower())
        if not m:
            parser.error("--window must be like 3y or 18m")
        amount, unit = int(m.group(1)), m.group(2)
        months = amount * 12 if unit == "y" else amount
        today = date.today()
        total_months = today.year * 12 + today.month - 1 - months
        args.start = f"{total_months // 12}-{total_months % 12 + 1:02d}"

    return args


def main():
    setup_logger()
    setup_styles()
    args = parse_args()

    try:
        env = load_env(args.env_file)
    except ValueError as e:
        print(f"Error loading environment variables: {e}")
        return 1

    # source:/transform:/sink: label prefixes only exist in vectordotdev/vector;
    # skip integration-specific charts for every other repo.
    is_vector = env['REPO_OWNER'] == 'vectordotdev' and env['REPO_NAME'] == 'vector'

    table_names = ["issues", "pull_requests"]
    for table in table_names:
        prefix = f"{env['REPO_OWNER']}_{env['REPO_NAME']}_{table}"
        monthly_csv = os.path.join(args.input_dir, f"{prefix}.monthly_summary.csv")
        if os.path.exists(monthly_csv):
            output_path = os.path.join(OUTPUT_DIR, f"{prefix}.monthly_trend.png")
            plot_monthly_summary_basic(monthly_csv, table, output_path, start_date=args.start)

            if is_vector:
                n = 5
                output_path = os.path.join(OUTPUT_DIR, f"{prefix}.integrations.top_{n}.monthly_trend.png")
                plot_integration_trends(monthly_csv,
                                        table,
                                        output_path,
                                        top_n=n,
                                        start_date=args.start,
                                        exclude_labels=args.exclude_labels)

        label_breakdown_csv = os.path.join(args.input_dir, f"{prefix}.label_breakdown.csv")
        if os.path.exists(label_breakdown_csv):
            output_path = os.path.join(OUTPUT_DIR, f"{prefix}.top_labels.png")
            plot_label_breakdown(
                label_breakdown_csv,
                table,
                output_path,
                start_date=args.start,
                exclude_labels=args.exclude_labels
            )

        open_by_label_csv = os.path.join(args.input_dir, f"{prefix}.label_counts.csv")
        if os.path.exists(open_by_label_csv):
            output_path = os.path.join(OUTPUT_DIR, f"{prefix}.label_counts.png")
            plot_label_count(
                open_by_label_csv,
                table,
                output_path,
                start_date=args.start,
                exclude_labels=args.exclude_labels
            )

        if is_vector:
            open_by_label_csv = os.path.join(args.input_dir, f"{prefix}.open_by_label.csv")
            if os.path.exists(open_by_label_csv):
                output_path = os.path.join(OUTPUT_DIR, f"{prefix}.open_closed_total_label_count.png")
                plot_label_state_counts(
                    open_by_label_csv,
                    table,
                    output_path,
                    top_n=30,
                    exclude_labels=args.exclude_labels
                )

        contributor_csv = os.path.join(args.input_dir, f"{prefix}.contributor_monthly.csv")
        if os.path.exists(contributor_csv):
            output_path = os.path.join(OUTPUT_DIR, f"{prefix}.contributors_top10_12m.png")
            plot_contributor_heatmap(contributor_csv, table, output_path)

            output_path = os.path.join(OUTPUT_DIR, f"{prefix}.unique_contributors.png")
            plot_unique_contributors(contributor_csv, table, output_path)

            output_path = os.path.join(OUTPUT_DIR, f"{prefix}.unique_contributors_yearly.png")
            plot_yearly_contributors(contributor_csv, table, output_path)

            # Same data as the yearly chart, also written as a markdown
            # table into trends/{repo}.md between AUTO markers.
            trends_md = Path(SCRIPT_DIR).resolve().parents[1] / "trends" / f"{env['REPO_NAME']}.md"
            update_yearly_contributors_table(contributor_csv, table, trends_md)

    # Discussion trends
    disc_prefix = f"{env['REPO_OWNER']}_{env['REPO_NAME']}_discussions"
    disc_csv = os.path.join(args.input_dir, f"{disc_prefix}.monthly_summary.csv")
    if os.path.exists(disc_csv):
        output_path = os.path.join(OUTPUT_DIR, f"{disc_prefix}.monthly_trend.png")
        plot_discussion_trend(disc_csv, output_path, start_date=args.start)


# Curated categorical palette for auto-assigned label colors. Hand-picked
# from tab10/Dark2/Set1 to keep neighbouring bars clearly distinguishable;
# CSS4_COLORS (the old source) contained many pale pastels that washed out.
_AUTO_PALETTE = [
    "#1f77b4",  # blue
    "#d62728",  # red
    "#2ca02c",  # green
    "#ff7f0e",  # orange
    "#9467bd",  # purple
    "#8c564b",  # brown
    "#e377c2",  # pink
    "#17becf",  # cyan
    "#bcbd22",  # olive
    "#7f7f7f",  # gray
    "#a6761d",  # dark gold
    "#666666",  # dim gray
    "#1b9e77",  # teal
    "#d95f02",  # rust
    "#7570b3",  # indigo
]


def get_label_color(label_name):
    if label_name in COLOR_MAP:
        return COLOR_MAP[label_name]

    seed = int(hashlib.md5(label_name.encode()).hexdigest(), 16)
    return _AUTO_PALETTE[seed % len(_AUTO_PALETTE)]


def plot_monthly_summary_basic(path, table, output_path, start_date=None):
    try:
        df = pd.read_csv(path)
        if start_date:
            df = df[df["month"] >= start_date]
        df["month"] = pd.to_datetime(df["month"])

        plt.figure(figsize=(12, 6))

        created_key = f"created_{table}"
        plt.plot(df["month"], df[created_key], label=f"Created {pretty_table(table)}", color=COLOR_MAP.get(created_key), linewidth=3,
                 marker='o')
        plt.xticks(rotation=45)  # Rotate month labels

        closed_key = f"closed_{table}"
        plt.plot(df["month"], df[closed_key], label=f"Closed {pretty_table(table)}",
                 color=COLOR_MAP.get(closed_key),
                 linewidth=3,
                 marker='o')
        for display_label, color, candidates in TYPE_OVERLAYS:
            matching = [c for c in candidates if c in df.columns]
            if not matching:
                continue
            combined = df[matching].sum(axis=1)
            plt.plot(df["month"], combined,
                     label=display_label, color=color,
                     linewidth=2, linestyle="--")

        plt.title(f"Monthly GitHub Trends ({pretty_table(table)})", fontsize=16)
        ax = plt.gca()
        set_axis_labels(ax, "Month", "Count")

        ax.legend(
            loc="upper left",
            bbox_to_anchor=(1.01, 1),
            borderaxespad=0.,
            frameon=True,
            framealpha=0.5,
            fontsize=10,
        )
        plt.tight_layout()

        plt.savefig(output_path)
        logging.info(f"Saved plot to {output_path}")
        plt.close()
    except Exception as e:
        logging.warning(f"[{table}] Could not generate monthly trend plot: {e}")


def plot_discussion_trend(path, output_path, start_date=None):
    try:
        df = pd.read_csv(path)
        if start_date:
            df = df[df["month"] >= start_date]
        df["month"] = pd.to_datetime(df["month"])

        plt.figure(figsize=(12, 6))

        plt.plot(df["month"], df["created_discussions"],
                 label="Created", color=DARK, linewidth=3, marker='o')
        plt.plot(df["month"], df["closed_discussions"],
                 label="Closed", color=CLOSED, linewidth=3, marker='o')
        plt.plot(df["month"], df["answered_discussions"],
                 label="Answered", color=ANSWERED, linewidth=2, linestyle="--")

        plt.title("Monthly Discussion Trends", fontsize=16)
        ax = plt.gca()
        set_axis_labels(ax, "Month", "Count")

        plt.legend()
        plt.tight_layout()

        plt.savefig(output_path)
        logging.info(f"Saved plot to {output_path}")
        plt.close()
    except Exception as e:
        logging.warning(f"[discussions] Could not generate monthly trend plot: {e}")


def plot_integration_trends(csv_path, table, output_path, start_date=None, exclude_labels=None, top_n=5):
    # Load the CSV data into a DataFrame
    df = pd.read_csv(csv_path)

    if start_date:
        df = df[df["month"] >= start_date]

    numeric_cols = df.select_dtypes(include='number').columns.tolist()

    # Filter out columns that are not count-like
    label_cols = []
    for col in numeric_cols:
        series = df[col].dropna()
        if (series >= 0).all():
            label_cols.append(col)

    if not exclude_labels:
        exclude_labels = set()

    # Build exclusion set
    for non_label in ['month', 'created_issues', 'closed_issues', 'created_pull_requests', 'closed_pull_requests']:
        if non_label in df.columns:
            exclude_labels.add(non_label)

    # Filter label columns
    label_cols = [
        col for col in label_cols
        if col not in exclude_labels
           and (col.startswith("source:") or col.startswith("transform:") or col.startswith("sink:"))
           and df[col].sum() > 0
    ]

    # Top N filtering
    if top_n is not None and top_n > 0:
        top_labels = df[label_cols].sum().nlargest(top_n).index.tolist()
        label_cols = [col for col in label_cols if col in top_labels]

    if not label_cols:
        print("No label count columns found for plotting after filtering. Check the data or parameters.")
        return

    # Create a wider figure to allocate room for the legend
    fig, ax = plt.subplots(figsize=(14, 6))
    df.plot(x='month', y=label_cols, marker='o', ax=ax)

    ax.yaxis.set_major_locator(MaxNLocator(integer=True))
    set_axis_labels(ax, "Month", "Count")
    ax.set_title(f"Integrations Top {top_n} Trend ({pretty_table(table)})", fontsize=16)

    # Legend outside on the right
    ax.legend(
        title="Label",
        loc="center left",
        bbox_to_anchor=(1.0, 0.5),
        fontsize=10,  # Slightly smaller font
        title_fontsize=12,  # Harmonize with body
        labelspacing=0.7,  # Tight vertical spacing
        borderaxespad=0.5,  # Padding between plot and legend
        frameon=True,  # Add a subtle box
        framealpha=0.5,  # Light transparent background
        fancybox=True,  # Rounded corners
        # borderpad=0.8  # Padding inside the legend box
    )

    plt.xticks(rotation=45)
    plt.tight_layout(rect=[0, 0, 0.96, 1])  # Reserve 20% for legend

    plt.savefig(output_path)
    logging.info(f"Saved plot to {output_path}")
    plt.close()


def plot_label_breakdown(path, table, output_path, top_n=20, start_date=None, exclude_labels=None):
    try:
        df = pd.read_csv(path)

        if "month" in df.columns and start_date:
            df = df[df["month"] >= start_date]

        if exclude_labels:
            df = df[~df["label_name"].isin(exclude_labels)]

        df = df.sort_values("count", ascending=False).head(top_n)
        colors = [get_label_color(label) for label in df["label_name"]]

        plt.figure(figsize=(10, 6))
        plt.barh(df["label_name"], df["count"], color=colors)
        ax = plt.gca()
        round_bars(ax)
        plt.title(f"Top {top_n} Labels by Frequency ({pretty_table(table)})", fontsize=16)
        set_axis_labels(ax, "Count", None)
        ax.invert_yaxis()
        plt.tight_layout()

        plt.savefig(output_path)
        logging.info(f"Saved plot to {output_path}")
        plt.close()
    except Exception as e:
        logging.warning(f"[{table}] Could not generate label breakdown plot: {e}")


def plot_label_count(path, table, output_path, top_n=8, start_date=None, exclude_labels=None):
    try:
        df = pd.read_csv(path)
        df["month"] = df["month"].astype(str)

        if start_date:
            df = df[df["month"] >= start_date]

        if exclude_labels:
            df = df[~df["label_name"].isin(exclude_labels)]

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
        round_bars(ax)

        # Axes styling
        ax.set_xticks(np.arange(len(months)))
        ax.set_xticklabels(months, rotation=45)
        set_axis_labels(ax, "Month", "Count")

        ax.set_title(f"Top {top_n} Labels Over Time ({pretty_table(table)})", fontsize=16)

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
        plt.savefig(output_path)
        logging.info(f"Saved plot to {output_path}")
        plt.close()

    except Exception as e:
        logging.warning(f"[{table}] Could not generate label time-series bar chart: {e}")


def plot_label_state_counts(path, table, output_path, top_n, exclude_labels=None):
    try:
        df = pd.read_csv(path)

        if exclude_labels:
            df = df[~df["label_name"].isin(exclude_labels)]

        df = df[df["label_name"].str.startswith(("source:", "transform:", "sink:"))]

        # Add total count column and sort
        df["total"] = df["open_count"] + df["closed_count"]
        df = df.sort_values("total", ascending=False).head(top_n)

        # Plot
        fig, ax = plt.subplots(figsize=(12, 6))
        ax.barh(df["label_name"], df["closed_count"], label="Closed", color=CLOSED)
        ax.barh(df["label_name"], df["open_count"], left=df["closed_count"], label="Open", color=OPEN)

        # Inline "closed / open" labels at the end of each bar
        x_pad = df["total"].max() * 0.01
        for _, row in df.iterrows():
            ax.text(
                row["total"] + x_pad, row["label_name"],
                f"{int(row['closed_count'])} / {int(row['open_count'])}",
                va="center", fontsize=9, color=DARK,
            )
        ax.set_xlim(right=df["total"].max() * 1.12)

        set_axis_labels(ax, "Count", None)

        ax.set_title(f"Top {top_n} Integrations Label Count ({pretty_table(table)})", fontsize=16)
        ax.legend(loc="lower right")
        plt.tight_layout()
        plt.gca().invert_yaxis()  # highest total on top

        plt.savefig(output_path)
        logging.info(f"Saved plot to {output_path}")
        plt.close()
    except Exception as e:
        logging.warning(f"[{table}] Could not generate label count chart: {e}")


def plot_contributor_heatmap(path, table, output_path, top_n=10, window_months=12):
    try:
        df = pd.read_csv(path)
        if df.empty:
            logging.warning(f"[{table}] Contributor CSV is empty: {path}")
            return

        months_all = sorted(df["month"].dropna().unique())
        window = months_all[-window_months:]
        df = df[df["month"].isin(window)]
        if df.empty:
            logging.warning(f"[{table}] No contributor data in last {window_months} months")
            return

        totals = df.groupby("user_login")["count"].sum().sort_values(ascending=False)
        top_users = totals.head(top_n).index.tolist()
        df = df[df["user_login"].isin(top_users)]

        pivot = (
            df.pivot_table(index="user_login", columns="month", values="count", fill_value=0)
              .reindex(index=top_users, columns=window, fill_value=0)
        )

        last_month = window[-1]
        pivot = pivot.assign(_total=totals.reindex(pivot.index).values)
        pivot = pivot.sort_values(by=[last_month, "_total"], ascending=False).drop(columns="_total")

        fig, ax = plt.subplots(figsize=(12, 5))
        # Zero cells masked so the plain axes background shows through
        # instead of the colormap's low end tint.
        cmap = plt.get_cmap(HEATMAP_CMAP).copy()
        cmap.set_bad(color=WHITE)
        masked = ma.masked_where(pivot.values == 0, pivot.values)
        im = ax.imshow(masked, aspect="auto", cmap=cmap)

        ax.set_xticks(np.arange(len(window)))
        ax.set_xticklabels(window, rotation=45, ha="right")
        ax.set_yticks(np.arange(len(pivot.index)))
        ax.set_yticklabels(pivot.index)

        vmax = pivot.values.max() if pivot.values.size else 0
        for i in range(pivot.shape[0]):
            for j in range(pivot.shape[1]):
                v = pivot.values[i, j]
                if v > 0:
                    color = WHITE if v > vmax * 0.6 else DARK
                    ax.text(j, i, int(v), ha="center", va="center", color=color, fontsize=9)

        cbar = fig.colorbar(im, ax=ax)
        cbar.set_label(f"{pretty_table(table)} opened")

        ax.set_title(f"Top {top_n} {pretty_table(table)} contributors (last {window_months} months)", fontsize=16)
        set_axis_labels(ax, "Month", "Contributor")
        ax.grid(False)

        plt.tight_layout()
        plt.savefig(output_path)
        logging.info(f"Saved plot to {output_path}")
        plt.close()
    except Exception as e:
        logging.warning(f"[{table}] Could not generate contributor heatmap: {e}")


def plot_unique_contributors(path, table, output_path, window_months=12):
    try:
        df = pd.read_csv(path)
        if df.empty:
            logging.warning(f"[{table}] Contributor CSV is empty: {path}")
            return

        df = df[~df["user_login"].map(is_bot_login)]
        df = df.dropna(subset=["month", "user_login"])
        if df.empty:
            logging.warning(f"[{table}] No non-bot contributors in {path}")
            return

        # First-ever month a user appears (across the entire history, not the window)
        first_month = df.groupby("user_login")["month"].min()

        months_all = sorted(df["month"].unique())
        window = months_all[-window_months:]
        if not window:
            logging.warning(f"[{table}] No contributor data for unique-contributors plot")
            return

        df_window = df[df["month"].isin(window)].copy()
        df_window["is_new"] = df_window.apply(
            lambda r: first_month[r["user_login"]] == r["month"], axis=1
        )

        # Per-month unique counts split into new vs returning
        per_month = (
            df_window.groupby(["month", "is_new"])["user_login"]
            .nunique()
            .unstack(fill_value=0)
            .reindex(window, fill_value=0)
        )
        new_counts = per_month.get(True, pd.Series(0, index=window))
        returning_counts = per_month.get(False, pd.Series(0, index=window))

        fig, ax = plt.subplots(figsize=(12, 6))

        x = np.arange(len(window))
        ax.bar(x, returning_counts.values, color=RETURNING_CONTRIBUTOR, label="Returning")
        ax.bar(x, new_counts.values, bottom=returning_counts.values,
               color=NEW_CONTRIBUTOR, label="New")

        totals = returning_counts.values + new_counts.values
        for i, t in enumerate(totals):
            if t > 0:
                ax.text(i, t, str(int(t)), ha="center", va="bottom", fontsize=9)

        ax.set_xticks(x)
        ax.set_xticklabels(window, rotation=45, ha="right")
        ax.yaxis.set_major_locator(MaxNLocator(integer=True))
        set_axis_labels(ax, "Month", "Unique contributors")
        ax.set_title(
            f"Unique {pretty_table(table)} contributors (last {len(window)} months)",
            fontsize=16,
        )
        ax.legend(loc="upper left")

        plt.tight_layout()
        plt.savefig(output_path)
        logging.info(f"Saved plot to {output_path}")
        plt.close()
    except Exception as e:
        logging.warning(f"[{table}] Could not generate unique-contributors plot: {e}")


def _yearly_contributor_rows(path):
    """Compute per-year (year, label, total, new, returning, partial) rows
    from contributor_monthly.csv. `partial` is True when the year hasn't
    fully elapsed yet (typically just the current year)."""
    df = pd.read_csv(path)
    if df.empty:
        return []
    df = df[~df["user_login"].map(is_bot_login)]
    df = df.dropna(subset=["month", "user_login"])
    if df.empty:
        return []

    df["month"] = df["month"].astype(str)
    df["year"] = df["month"].str[:4]
    first_year_by_user = df.groupby("user_login")["month"].min().str[:4]

    today_year = pd.Timestamp.utcnow().year
    rows = []
    for year, group in df.groupby("year"):
        users = group["user_login"].unique()
        new_c = sum(first_year_by_user[u] == year for u in users)
        total = len(users)
        months_seen = group["month"].nunique()
        partial = int(year) == today_year and months_seen < 12
        label = f"{year} (YTD, {months_seen}mo)" if partial else f"{year}"
        rows.append((year, label, total, new_c, total - new_c, partial))
    rows.sort(key=lambda r: r[0])
    return rows


def plot_yearly_contributors(path, table, output_path, max_years=6):
    """Horizontal stacked bars: one row per year (last `max_years`),
    split into new (green) vs returning (purple), with totals annotated."""
    try:
        rows = _yearly_contributor_rows(path)
        if not rows:
            return
        rows = rows[-max_years:]

        labels = [r[1] for r in rows]
        new_vals = np.array([r[3] for r in rows])
        ret_vals = np.array([r[4] for r in rows])
        partials = [r[5] for r in rows]
        totals = new_vals + ret_vals

        fig, ax = plt.subplots(figsize=(10, max(3.5, 0.7 * len(rows) + 1.5)))
        y = np.arange(len(labels))
        # Per-row alpha: partial (YTD) years are muted to avoid suggesting
        # they're directly comparable to full years.
        alphas = [0.35 if p else 1.0 for p in partials]
        # Squared edges — rounding stacked segments leaves gaps at the
        # junction.
        for i, a in enumerate(alphas):
            ax.barh(y[i], ret_vals[i], color=RETURNING_CONTRIBUTOR, height=0.55, alpha=a)
            ax.barh(y[i], new_vals[i], left=ret_vals[i], color=NEW_CONTRIBUTOR,
                    height=0.55, alpha=a)

        x_pad = max(totals.max() * 0.015, 0.5)
        for i, (n, r, t, p) in enumerate(zip(new_vals, ret_vals, totals, partials)):
            text_color_inner = WHITE
            text_color_outer = DARK
            if p:
                # Inner labels become dark grey on muted bars so they stay
                # readable against the lighter fill.
                text_color_inner = MUTED
                text_color_outer = MUTED
            if r > 0:
                ax.text(r / 2, i, str(int(r)), ha="center", va="center",
                        color=text_color_inner, fontsize=12, fontweight="bold")
            if n > 0:
                ax.text(r + n / 2, i, str(int(n)), ha="center", va="center",
                        color=text_color_inner, fontsize=12, fontweight="bold")
            ax.text(t + x_pad, i, f"{int(t)} total",
                    va="center", fontsize=12, color=text_color_outer,
                    fontweight="bold",
                    fontstyle="italic" if p else "normal")

        ax.set_yticks(y)
        ax.set_yticklabels(labels, fontsize=12)
        for tick, p in zip(ax.get_yticklabels(), partials):
            if p:
                tick.set_fontstyle("italic")
                tick.set_color(MUTED)
        ax.invert_yaxis()
        ax.set_xlim(right=totals.max() * 1.28)

        ax.set_xticks([])
        ax.xaxis.set_visible(False)
        for spine in ("top", "right", "bottom"):
            ax.spines[spine].set_visible(False)
        ax.grid(False)

        adj = {"pull_requests": "PR", "issues": "Issue", "discussions": "Discussion"}.get(table, table)
        ax.set_title(
            f"Unique {adj} contributors by year",
            fontsize=18, pad=14,
        )
        from matplotlib.patches import Patch
        ax.legend(
            handles=[
                Patch(facecolor=RETURNING_CONTRIBUTOR, label="Returning"),
                Patch(facecolor=NEW_CONTRIBUTOR, label="New"),
            ],
            loc="lower right", frameon=False,
        )

        plt.tight_layout()
        plt.savefig(output_path)
        logging.info(f"Saved plot to {output_path}")
        plt.close()
    except Exception as e:
        logging.warning(f"[{table}] Could not generate yearly contributors chart: {e}")


def update_yearly_contributors_table(path, table, trends_md_path):
    """Compute per-year unique-contributor stats (total / new / returning)
    and rewrite the section in trends/{repo}.md between the markers
    <!-- AUTO:yearly-contributors:start --> and ...:end -->."""
    try:
        rows = _yearly_contributor_rows(path)
        if not rows:
            return

        lines = [
            "| Year | Unique | New | Returning |",
            "|------|--------|-----|-----------|",
        ]
        for _, label, total, new_c, ret_c, _partial in rows:
            lines.append(f"| {label} | {total} | {new_c} | {ret_c} |")
        table_md = "\n".join(lines)

        marker_start = "<!-- AUTO:yearly-contributors:start -->"
        marker_end = "<!-- AUTO:yearly-contributors:end -->"

        if not trends_md_path.exists():
            logging.warning(
                f"[{table}] Trends file not found, skipping yearly table: {trends_md_path}"
            )
            return
        content = trends_md_path.read_text()
        if marker_start not in content or marker_end not in content:
            logging.warning(
                f"[{table}] Markers not found in {trends_md_path}; "
                f"add a section bracketed by {marker_start} / {marker_end}"
            )
            return

        import re
        pattern = re.compile(
            re.escape(marker_start) + r".*?" + re.escape(marker_end),
            re.DOTALL,
        )
        replacement = f"{marker_start}\n\n{table_md}\n\n{marker_end}"
        content = pattern.sub(replacement, content)
        trends_md_path.write_text(content)
        logging.info(f"Updated yearly-contributors table in {trends_md_path}")
    except Exception as e:
        logging.warning(f"[{table}] Could not update yearly-contributors table: {e}")


if __name__ == "__main__":
    main()
