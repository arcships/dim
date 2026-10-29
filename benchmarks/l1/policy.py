"""Path rules that decide `visible` without asking the reviewer.

A dependency bump's diff is a version number: the behaviour change is upstream and
the reviewer has nothing to trace. Running an uncertain model over a certain rule
adds variance without adding information, so these paths short-circuit.

The policy file's own digest is part of the L1 decision identity — editing a rule
must invalidate cached verdicts rather than silently apply to new MRs only.
"""

from __future__ import annotations

import hashlib
import tomllib
from dataclasses import dataclass
from fnmatch import fnmatch
from pathlib import Path

SUPPORTED_SCHEMA_VERSIONS = frozenset({"1"})


class PolicyError(RuntimeError):
    """The policy file is missing, malformed, or of an unsupported version."""


@dataclass(frozen=True)
class Rule:
    id: str
    reason: str
    paths: tuple[str, ...]


@dataclass(frozen=True)
class Policy:
    rules: tuple[Rule, ...]
    digest: str

    def match(self, paths: tuple[str, ...]) -> tuple[str, ...]:
        """Rule ids triggered by these paths, in file order then rule order."""
        hits: list[str] = []
        for path in paths:
            for rule in self.rules:
                if rule.id in hits:
                    continue
                if any(_matches(path, pattern) for pattern in rule.paths):
                    hits.append(rule.id)
        return tuple(hits)


def _matches(path: str, pattern: str) -> bool:
    """Match a repo-relative path against a rule pattern.

    `dir/**` means the directory and everything under it. fnmatch alone would not
    do this: its `*` crosses `/`, so `patches/**` would also match `patchesfoo`.
    """
    if pattern.endswith("/**"):
        prefix = pattern[: -len("/**")]
        return path == prefix or path.startswith(f"{prefix}/")
    return fnmatch(path, pattern)


def load_policy(path: Path) -> Policy:
    try:
        raw = path.read_bytes()
    except OSError as error:
        raise PolicyError(f"cannot read policy at {path}: {error}") from error

    try:
        data = tomllib.loads(raw.decode("utf-8"))
    except (tomllib.TOMLDecodeError, UnicodeDecodeError) as error:
        raise PolicyError(f"policy at {path} is not valid TOML: {error}") from error

    version = str(data.get("schema_version", ""))
    if version not in SUPPORTED_SCHEMA_VERSIONS:
        raise PolicyError(
            f"policy schema_version {version!r} is unsupported; "
            f"expected one of {sorted(SUPPORTED_SCHEMA_VERSIONS)}"
        )

    rules: list[Rule] = []
    for entry in data.get("rule", []):
        rule_id = entry.get("id")
        paths = entry.get("paths")
        if not rule_id or not paths:
            raise PolicyError(f"policy rule is missing id or paths: {entry!r}")
        rules.append(
            Rule(
                id=str(rule_id),
                reason=str(entry.get("reason", "")),
                paths=tuple(str(p) for p in paths),
            )
        )

    if not rules:
        raise PolicyError(f"policy at {path} declares no rules")

    return Policy(rules=tuple(rules), digest=hashlib.sha256(raw).hexdigest())
