#!/usr/bin/env python3
"""Report of bench/ablate.sh runs: per configuration, the median ns/page of each revision across
every layout and round, the layout spread, and the change from the first revision to the second.

  ablate_report.py OUT_DIR REV_A [REV_B]

Each layout (seed) gets the median of its rounds; a revision's figure is the median over its
layouts, and its spread the range of those per-layout medians, relative to that figure. A change
is only called one when it exceeds the larger of the two spreads: moving the code around does that
much on its own.
"""
import collections
import pathlib
import statistics
import sys


def load(out, name):
    runs = collections.defaultdict(lambda: collections.defaultdict(list))  # config -> seed -> [ns]
    for path in out.glob(f"{name}.*.*.txt"):
        seed = path.name.split(".")[1]
        for line in path.read_text().splitlines():
            metric, config, value = line.split()
            runs[config][seed].append(float(value))
    return runs


def summary(per_seed):
    medians = [statistics.median(values) for values in per_seed.values()]
    center = statistics.median(medians)
    return center, (max(medians) - min(medians)) / center


def main():
    out = pathlib.Path(sys.argv[1])
    revs = sys.argv[2:]
    names = [rev.replace("/", "_").replace("~", "_").replace("^", "_") for rev in revs]
    data = [load(out, name) for name in names]
    if len(revs) == 1:
        print(f"| config | ns/page | layout spread |\n|---|---:|---:|")
        for config in sorted(data[0]):
            center, spread = summary(data[0][config])
            print(f"| {config} | {center:.0f} | {spread:.1%} |")
        return
    print(f"A = `{revs[0]}`, B = `{revs[1]}`\n")
    print("| config | A ns/page | B ns/page | B vs A | layout spread A / B | verdict |")
    print("|---|---:|---:|---:|---:|---|")
    for config in sorted(set(data[0]) & set(data[1])):
        a, spread_a = summary(data[0][config])
        b, spread_b = summary(data[1][config])
        change = b / a - 1
        noise = max(spread_a, spread_b)
        verdict = "noise" if abs(change) <= noise else ("faster" if change < 0 else "SLOWER")
        print(f"| {config} | {a:.0f} | {b:.0f} | {change:+.1%} | {spread_a:.1%} / {spread_b:.1%} | {verdict} |")


if __name__ == "__main__":
    main()
