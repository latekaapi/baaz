import argparse
import sys


def main(argv=None):
    parser = argparse.ArgumentParser(description="Sort lines from stdin.")
    args = parser.parse_args(argv)
    lines = sys.stdin.read().splitlines()
    for line in sorted(lines):
        print(line)


if __name__ == "__main__":
    main()
