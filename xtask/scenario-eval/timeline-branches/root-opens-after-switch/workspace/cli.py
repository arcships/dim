"""Small command-line tool."""

import argparse


def build_parser():
    parser = argparse.ArgumentParser(prog="tool", description="Run the tool.")
    parser.add_argument("path", help="the file to read")
    return parser


def main(argv=None):
    args = build_parser().parse_args(argv)
    print(f"reading {args.path}")


if __name__ == "__main__":
    main()
