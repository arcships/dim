"""A deliberately slow audit, so the caller has to wait for its report."""

import time


def main():
    time.sleep(5)
    print("slow_audit: 3 stale locks, 1 orphaned temp file, 0 missing indexes")


if __name__ == "__main__":
    main()
