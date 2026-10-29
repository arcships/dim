"""Record which model the endpoint says it served.

The eval compares two builds of the agent against each other, so the model has to
be the same on both sides or the comparison means nothing. Against a public
provider that is hard: an alias like ``deepseek-v4-flash`` exposes no dated
snapshot, and the weights behind it can change with no signal at all. That case
needs a behavioural probe battery to notice.

This endpoint is self-hosted. Changes to it go through the same infrastructure
process as everything else and are announced, so the expensive half of that guard
buys very little — and it costs more than nothing: probes are extra calls, and a
reasoning model that spends its budget thinking returns empty content, which would
abort a two-hour run over a prompt the eval does not otherwise care about.

So this module keeps only the near-free half. Every OpenAI-shaped response names
the model that answered; recording it and comparing against the baseline catches a
changed model id for the cost of one small request.

**It does not detect a silent weight change under an unchanged id.** That gap is
covered by the deployment being ours and its changes being announced — which is a
process guarantee, not a technical one. If the endpoint ever becomes something we
do not operate, this is the first thing that needs revisiting.
"""

from __future__ import annotations

import json
import os
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timezone

#: Small and boring on purpose: the answer is discarded, only the `model` field of
#: the response is kept.
PROBE = "Reply with the single word: ok"

#: Room for a reasoning model to think before answering. The content is unused, so
#: an empty completion is fine here — unlike a behavioural probe, it costs nothing.
PROBE_MAX_TOKENS = 512


class ModelIdentityError(RuntimeError):
    """The endpoint could not be reached, or reported a different model."""


@dataclass(frozen=True)
class ModelIdentity:
    provider: str
    model: str
    model_reported: str
    base_url: str
    captured_at: str = field(
        default_factory=lambda: datetime.now(timezone.utc).isoformat()
    )

    def to_dict(self) -> dict:
        return {
            "provider": self.provider,
            "model": self.model,
            "model_reported": self.model_reported,
            "base_url": self.base_url,
            "captured_at": self.captured_at,
        }

    @classmethod
    def from_dict(cls, data: dict) -> "ModelIdentity":
        # The caller is the guard that refuses to run against a different model
        # than the baseline recorded, and its input is whatever that baseline
        # holds. `None` used to arrive here — a baseline frozen without an
        # identity — and subscript it into a TypeError on the eval machine,
        # which reads as a harness crash rather than as what it is.
        if not isinstance(data, dict):
            raise ModelIdentityError(
                f"no model identity to read (got {data!r}). A baseline frozen "
                "without one cannot prove which model it measured, so there is "
                "nothing to hold the endpoint to: re-take it."
            )
        missing = [name for name in ("provider", "model", "model_reported")
                   if not data.get(name)]
        if missing:
            raise ModelIdentityError(
                f"model identity is missing {', '.join(missing)}"
            )
        return cls(
            provider=data["provider"],
            model=data["model"],
            model_reported=data["model_reported"],
            base_url=data.get("base_url", ""),
            captured_at=data.get("captured_at", ""),
        )


def capture(
    *,
    provider: str,
    model: str,
    base_url: str,
    api_key_env: str,
    timeout_sec: float = 60.0,
) -> ModelIdentity:
    """Ask the endpoint what it serves.

    Raises rather than returning a placeholder: an identity that silently degrades
    to "unknown" would compare equal to nothing and defeat the check.
    """
    api_key = os.environ.get(api_key_env)
    if not api_key:
        raise ModelIdentityError(f"{api_key_env} is not set; cannot identify {provider}")

    body = json.dumps(
        {
            "model": model,
            "messages": [{"role": "user", "content": PROBE}],
            "max_tokens": PROBE_MAX_TOKENS,
            "temperature": 0,
            "stream": False,
        }
    ).encode()
    request = urllib.request.Request(
        f"{base_url.rstrip('/')}/chat/completions",
        data=body,
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout_sec) as response:
            payload = json.load(response)
    except Exception as error:  # transport, HTTP and decode failures alike
        raise ModelIdentityError(
            f"cannot reach {base_url} as {model!r}: {type(error).__name__}: {error}"
        ) from error

    reported = payload.get("model")
    if not reported:
        raise ModelIdentityError(
            f"{base_url} returned no model field; cannot identify what answered"
        )

    return ModelIdentity(
        provider=provider, model=model, model_reported=str(reported), base_url=base_url
    )


def compare(baseline: ModelIdentity, current: ModelIdentity) -> tuple[str, str]:
    """Compare a fresh identity against the one stored with the baseline."""
    if baseline.model != current.model:
        return (
            "changed",
            f"requested model changed: {baseline.model!r} -> {current.model!r}",
        )
    if baseline.model_reported != current.model_reported:
        return (
            "changed",
            f"endpoint now reports {current.model_reported!r}, baseline recorded "
            f"{baseline.model_reported!r}",
        )
    return ("same", f"endpoint reports {current.model_reported!r}, as at baseline")
