"""Record wall and monotonic clock deltas during a manual sleep experiment.

Run with explicit python, redirect stdout to evidence.jsonl, then Ctrl-C.
No recording, power settings, or models are changed by this probe.
"""
import argparse
import json
import time


def sample(previous=None):
    current = {"wall_ns": time.time_ns(), "monotonic_ns": time.monotonic_ns()}
    if previous is not None:
        wall = current["wall_ns"] - previous["wall_ns"]
        monotonic = current["monotonic_ns"] - previous["monotonic_ns"]
        current.update(wall_delta_ns=wall, monotonic_delta_ns=monotonic,
                       wall_minus_monotonic_ns=wall - monotonic)
    return current


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--interval", type=float, default=1.0)
    args = parser.parse_args()
    if not 0 < args.interval < float("inf"):
        parser.error("--interval must be finite and positive")
    previous = None
    try:
        while True:
            previous = sample(previous)
            print(json.dumps(previous), flush=True)
            time.sleep(args.interval)
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
