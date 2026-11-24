"""Login command."""

import argparse

from auth.token import issue


def build_parser():
    parser = argparse.ArgumentParser(prog="login", description="Log a user in.")
    parser.add_argument("user", help="the user to log in")
    return parser


def login(user):
    """Issues the user's token."""
    return issue(ttl=60)


def main(argv=None):
    args = build_parser().parse_args(argv)
    token = login(args.user)
    print(f"logged in {args.user}; token valid for {token['ttl']}s")


if __name__ == "__main__":
    main()
