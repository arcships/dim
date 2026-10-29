"""The L1 review agent: stdin carries prompt + diff, stdout carries verdicts.

This is the `command` half of ReviewerConfig. Everything around it already
existed — build_input frames the diff, parse_output validates coverage — but
nothing actually sent the prompt to a model, so L1 could never reach a verdict
on files the path rules do not claim.

Deliberately a plain OpenAI-shaped call rather than a `dim exec` invocation: the
judge must not be the agent under test. Running the eval's own CLI here would
make L1's behaviour move whenever the thing being measured moves.

Usage (as a ReviewerConfig.command):

    python3 -m l1.agent_reviewer

Reads DIM_EVAL_BASE_URL, DIM_EVAL_JUDGE_MODEL and the key named by
DIM_EVAL_API_KEY_ENV. Writes one JSON object per changed file, then a summary.
"""

from __future__ import annotations

import json
import os
import sys
import time
import urllib.error
import urllib.request

#: DeepSeek-V4.1-Flash. It replaced glm-5.3 on 2026-09-30: glm spent its whole
#: output budget reasoning on large merges and returned no verdict at all, while
#: this model re-judged the same window agreeing with glm on every file glm had
#: judged, at about four times the throughput.
DEFAULT_BASE_URL = "https://api.deepseek.com/v1"
DEFAULT_MODEL = "deepseek-flash"
DEFAULT_KEY_ENV = "DEEPSEEK_API_KEY"

#: Judging traces data flow through a diff it has in full; `low` did that as well
#: as glm did, and `high` spent up to twice the reasoning on the same merges.
REASONING_EFFORT = "low"

#: Reasoning counts against this, and how much a merge needs varies widely from
#: one run to the next — the same diff at the same effort has taken 7k tokens and
#: exhausted 16k. At `low` the median merge needs about 1k, but the largest in the
#: first smoke window took 18.8k. The cap is billed by use, so headroom is free.
MAX_TOKENS = 64000

#: One request's worst case: MAX_TOKENS at the ~200 tokens/s this model produces
#: is about 320 s. The 300 s this replaced sat just under the ~320 s glm took to
#: exhaust its own cap, which is why that failure was reported as a timeout.
REQUEST_TIMEOUT_SEC = 480.0

#: A verdict is one short request. Retrying it is far cheaper than losing the L1
#: decision, and the endpoint drops connections often enough to matter — two
#: trials in the current baseline died this way milliseconds apart.
MAX_ATTEMPTS = 3
RETRY_BACKOFF_SEC = 2.0

#: How long the caller must allow before killing this process: every attempt
#: running to its timeout, the backoff between them, and a minute for the ssh
#: hop of a remote judge. Anything shorter kills a judge that was still within
#: its own retries — the 600 s that ended the first smoke run's large merges.
DEADLINE_SEC = (MAX_ATTEMPTS * REQUEST_TIMEOUT_SEC
                + RETRY_BACKOFF_SEC * sum(range(1, MAX_ATTEMPTS)) + 60.0)

#: Judging is a tracing task, not a generative one; sampling variance here shows
#: up as a trigger decision that flips between runs on an unchanged diff.
TEMPERATURE = 0.0


class JudgeError(RuntimeError):
    pass


def _endpoint(base_url: str) -> str:
    base = base_url.rstrip("/")
    if not base.endswith("/v1"):
        base = f"{base}/v1"
    return f"{base}/chat/completions"


def call_model(content: str, base_url: str, model: str, api_key: str) -> str:
    payload = json.dumps({
        "model": model,
        "messages": [{"role": "user", "content": content}],
        "max_tokens": MAX_TOKENS,
        "temperature": TEMPERATURE,
        "reasoning_effort": REASONING_EFFORT,
    }).encode()

    last_error: Exception | None = None
    for attempt in range(1, MAX_ATTEMPTS + 1):
        request = urllib.request.Request(
            _endpoint(base_url),
            data=payload,
            headers={
                "Content-Type": "application/json",
                "Authorization": f"Bearer {api_key}",
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=REQUEST_TIMEOUT_SEC) as response:
                body = json.loads(response.read().decode())
            choices = body.get("choices") or []
            if not choices:
                raise JudgeError(f"endpoint returned no choices: {str(body)[:300]}")
            if choices[0].get("finish_reason") == "length":
                # Cut off at the cap, usually mid-reasoning with no content at all.
                # Said here because downstream it would only surface as missing
                # verdicts, or as a timeout when the cap takes longer than that.
                raise JudgeError(
                    f"output cut off at max_tokens={MAX_TOKENS} "
                    f"(usage: {body.get('usage')})"
                )
            text = (choices[0].get("message") or {}).get("content") or ""
            if not text.strip():
                # Empty without hitting the cap is still a real failure here: an
                # empty body means no verdict for any file.
                raise JudgeError("endpoint returned empty content")
            return text
        except urllib.error.HTTPError as error:
            # The body of a 4xx is where the endpoint says what was wrong with the
            # request — too many tokens, a rejected parameter. `str(HTTPError)` is
            # just "HTTP Error 400: ", so discarding the body throws away the
            # diagnosis at exactly the moment it is needed.
            try:
                detail = error.read().decode(errors="replace").strip()[:500]
            except Exception:  # the body is best-effort; the status still matters
                detail = ""
            last_error = JudgeError(
                f"HTTP {error.code} {error.reason}" + (f": {detail}" if detail else "")
            )
        except (urllib.error.URLError, TimeoutError, JudgeError, OSError) as error:
            last_error = error

        if attempt < MAX_ATTEMPTS:
            sys.stderr.write(f"[judge] attempt {attempt} failed: {last_error}; retrying\n")
            time.sleep(RETRY_BACKOFF_SEC * attempt)
    raise JudgeError(f"judge failed after {MAX_ATTEMPTS} attempts: {last_error}")


def main() -> int:
    content = sys.stdin.read()
    if not content.strip():
        sys.stderr.write("no input on stdin\n")
        return 2

    base_url = os.environ.get("DIM_EVAL_BASE_URL", DEFAULT_BASE_URL)
    model = os.environ.get("DIM_EVAL_JUDGE_MODEL", DEFAULT_MODEL)
    key_var = os.environ.get("DIM_EVAL_API_KEY_ENV", DEFAULT_KEY_ENV)
    api_key = os.environ.get(key_var, "")
    if not api_key:
        sys.stderr.write(f"{key_var} is empty\n")
        return 2

    try:
        text = call_model(content, base_url, model, api_key)
    except JudgeError as error:
        sys.stderr.write(f"{error}\n")
        return 1

    # Emitted verbatim. parse_output already ignores prose and code fences and
    # validates coverage against git's file list, so reshaping the model's output
    # here would only hide a malformed verdict that the caller is built to catch.
    sys.stdout.write(text if text.endswith("\n") else text + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
