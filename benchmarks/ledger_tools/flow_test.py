"""Ledger flow tests.

What these protect is the ledger's value as evidence months later: that every
merge left a verdict, that a run says what it answered for, and that a k=1
screen can never be read as a judgement. The last one is the reason the ledger
has two record types at all.

Run: python3 -m unittest discover -s benchmarks/ledger_tools -p 'flow_test.py'
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from ledger_tools import (  # noqa: E402
    Ledger, LedgerError, MergeRecord, RunRecord, SchemaError, audit, health,
)

SHA_A = "a" * 40
SHA_B = "b" * 40
SHA_C = "c" * 40
HEAD = "d" * 40
BASE = "e" * 40


def merge(sha: str, verdict: str = "visible", trigger: str = "agent",
          **kwargs) -> MergeRecord:
    return MergeRecord(
        merge_sha=sha,
        project_id="dimcode",
        subject="Merge branch 'x' into 'main'",
        merged_at="2026-09-14T10:00:00+08:00",
        l1_verdict=verdict,
        l1_trigger=trigger,
        l1_reason="tool result formatting changed",
        l1_identity="identity-" + sha[:4],
        **kwargs,
    )


def run(run_id: str = "r1", k: int = 3, outcome: str = "pass",
        covers: tuple[str, ...] = (SHA_A,), **kwargs) -> RunRecord:
    fields = {
        "run_type": "checkpoint", "corpus_pin": "pin", "subject_commit": HEAD,
        "window_base": BASE, "window_head": HEAD, "covers": covers,
    }
    # `invalid` is the one outcome that cannot be written without saying why, so
    # the helper supplies the commonest reason rather than making every caller
    # repeat it. Tests about the distinction pass their own.
    if outcome == "invalid":
        fields["invalid_reason"] = "unscoreable"
    fields.update(kwargs)
    return RunRecord(run_id=run_id, k=k, outcome=outcome, **fields)


class LedgerFlowTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.ledger = Ledger(self.root)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    # --- the k rule ------------------------------------------------------

    def test_a_k1_run_cannot_conclude(self) -> None:
        """The prohibition is structural: a daily screen's pass is unconstructable,
        so no reader has to remember the rule."""
        for outcome in ("pass", "fail"):
            with self.assertRaises(SchemaError) as caught:
                run(k=1, outcome=outcome, breaches=("x",) if outcome == "fail" else ()).validate()
            self.assertIn("cannot conclude", str(caught.exception))

    def test_a_k1_run_records_a_screen(self) -> None:
        record = run(k=1, outcome="screen")
        record.validate()
        self.assertFalse(record.conclusive)

    def test_an_invalid_run_must_say_why(self) -> None:
        """`invalid` spans "nothing was measured" and "it was measured and there
        is nothing to compare it to". `measured` cannot be derived without
        knowing which, and the store is append-only, so a record that does not
        say is one nobody can read later."""
        with self.assertRaises(SchemaError):
            RunRecord(run_id="r1", run_type="checkpoint", k=3, corpus_pin="pin",
                      subject_commit=HEAD, window_base=BASE, window_head=HEAD,
                      covers=(SHA_A,), outcome="invalid").validate()
        with self.assertRaises(SchemaError):
            run(outcome="pass", invalid_reason="unscoreable").validate()
        # The evidence is required with the reason that rests on it.
        with self.assertRaises(SchemaError):
            run(outcome="invalid", invalid_reason="l2_failed").validate()
        with self.assertRaises(SchemaError):
            run(outcome="invalid", invalid_reason="settle_failed").validate()
        run(outcome="invalid", invalid_reason="l2_failed", l2_exit_code=1).validate()

    def test_a_baseline_cannot_come_from_a_dirty_harness(self) -> None:
        """The ruler every later comparison is read against cannot be defined by
        code that exists on one machine and in no commit. `--allow-dirty` lets an
        experiment run; the schema is what keeps it out of the reference."""
        with self.assertRaises(SchemaError):
            run(run_type="baseline", window_base=None, covers=(),
                runner_dirty=True).validate()
        run(runner_dirty=True).validate()

    def test_the_new_failure_fields_do_not_move_an_old_digest(self) -> None:
        """They join the digest only when set. An always-present key would change
        the digest of every record already written — the audit re-derives it to
        detect tampering, so each would start reading as edited, and a write-once
        store cannot be rewritten to fix that."""
        for record in (run(outcome="pass"), run(k=1, outcome="screen")):
            for name in ("invalid_reason", "l2_exit_code", "settle_error",
                         "runner_dirty"):
                self.assertNotIn(name, record.to_dict())
        # And a record that has them round-trips through the store unchanged.
        broken = run(outcome="invalid", invalid_reason="l2_failed", l2_exit_code=124)
        self.assertEqual(RunRecord.from_dict(broken.to_dict()).digest, broken.digest)

    def test_a_k1_run_that_could_not_be_scored_records_invalid(self) -> None:
        """`invalid` is not a judgement, so the k rule has no business barring it.

        Barred, the only outcome left for a k=1 run whose job came back unscorable
        was `screen` — which says a measurement exists. The ledger is write-once,
        so every such night would have left a permanent record of a screen that
        never screened anything.
        """
        record = run(k=1, outcome="invalid")
        record.validate()
        self.assertFalse(record.conclusive)

    def test_a_k3_run_may_not_hide_behind_screen(self) -> None:
        """A conclusive run that files itself as a screen discards a result that
        cost hours of machine time."""
        with self.assertRaises(SchemaError):
            run(k=3, outcome="screen").validate()

    def test_conclusive_is_derived_not_declared(self) -> None:
        self.assertFalse(run(k=1, outcome="screen").conclusive)
        self.assertTrue(run(k=3).conclusive)
        self.assertNotIn("conclusive", RunRecord.__dataclass_fields__)

    def test_a_failing_run_must_name_what_it_breached(self) -> None:
        with self.assertRaises(SchemaError):
            run(outcome="fail").validate()
        run(outcome="fail", breaches=("full flip",)).validate()

    # --- merge records ---------------------------------------------------

    def test_a_merge_is_recorded_once(self) -> None:
        self.ledger.append_merge(merge(SHA_A))
        with self.assertRaises(LedgerError):
            self.ledger.append_merge(merge(SHA_A))

    def test_an_undecided_merge_cannot_be_recorded_invisible(self) -> None:
        """Fail-open means the judge never answered. Filing it as invisible would
        turn an outage into a decision to skip."""
        with self.assertRaises(SchemaError):
            merge(SHA_A, verdict="invisible", trigger="fail_open").validate()
        merge(SHA_A, verdict="visible", trigger="fail_open").validate()

    def test_short_shas_are_refused(self) -> None:
        with self.assertRaises(SchemaError):
            merge("abc1234").validate()

    # --- runs and their coverage -----------------------------------------

    def test_a_run_cannot_claim_merges_the_ledger_never_judged(self) -> None:
        self.ledger.append_merge(merge(SHA_A))
        with self.assertRaises(LedgerError) as caught:
            self.ledger.append_run(run(covers=(SHA_A, SHA_B)))
        self.assertIn("no L1 record", str(caught.exception))

    def test_unjudged_commits_are_recorded_rather_than_claimed(self) -> None:
        """A direct push has no MR to judge; it still has to appear, or the run
        looks like it answered for less than it did."""
        self.ledger.append_merge(merge(SHA_A))
        self.ledger.append_run(run(covers=(SHA_A,), unjudged=(SHA_C,)))
        self.assertEqual(self.ledger.runs()[0].unjudged, (SHA_C,))

    def test_a_commit_cannot_be_both_covered_and_unjudged(self) -> None:
        with self.assertRaises(SchemaError):
            run(covers=(SHA_A,), unjudged=(SHA_A,)).validate()

    def test_a_windowed_run_needs_a_base(self) -> None:
        with self.assertRaises(SchemaError):
            run(window_base=None).validate()
        # A baseline answers for one commit, so it has none.
        run(run_type="baseline", window_base=None, covers=()).validate()

    def test_a_run_is_recorded_once(self) -> None:
        self.ledger.append_merge(merge(SHA_A))
        self.ledger.append_run(run())
        with self.assertRaises(LedgerError):
            self.ledger.append_run(run())

    # --- health ----------------------------------------------------------

    def test_only_a_conclusive_pass_clears_a_merge(self) -> None:
        """A clean k=1 screen is not evidence: the merge it covered is still
        waiting to be judged, so it cannot enter the escape denominator."""
        merges = [merge(SHA_A)]
        screens = [run(k=1, outcome="screen", covers=(SHA_A,))]
        report = health(merges, screens, regressed_commits=set())
        self.assertEqual(report.l2_escape.denominator, 0)
        self.assertIsNone(report.l2_escape.rate)

        judged = [run(run_id="r2", k=3, outcome="pass", covers=(SHA_A,))]
        report = health(merges, screens + judged, regressed_commits={SHA_A})
        self.assertEqual((report.l2_escape.numerator, report.l2_escape.denominator),
                         (1, 1))

    def test_an_invisible_merge_inside_a_tested_window_is_not_l1s_fault(self) -> None:
        """L2 runs against the window head, so an invisible merge's code was in the
        build. Blaming L1 for a regression the run did test would make the false
        negative rate measure the wrong thing."""
        merges = [merge(SHA_A, verdict="invisible", trigger="agent"),
                  merge(SHA_B, verdict="visible")]
        runs = [run(covers=(SHA_A, SHA_B))]
        report = health(merges, runs, regressed_commits={SHA_A})
        self.assertEqual(report.l1_false_negative.denominator, 0)

        # With no run covering it, the same merge is squarely L1's call.
        report = health(merges, [], regressed_commits={SHA_A})
        self.assertEqual((report.l1_false_negative.numerator,
                          report.l1_false_negative.denominator), (1, 1))

    def test_a_run_that_measured_nothing_covers_nothing(self) -> None:
        """Coverage is what exempts an invisible merge from L1's account, so a
        run that produced no readable measurement must not grant it.

        All three failure reasons look the same to this metric: harbor failed,
        scoring raised, or the output did not add up. Counting their merges as
        covered credited L1 with coverage that does not exist — a crashed harness
        read as a clean L1.
        """
        merges = [merge(SHA_A, verdict="invisible")]
        for reason in ("l2_failed", "settle_failed", "unscoreable"):
            extra = {"l2_exit_code": 1} if reason == "l2_failed" else {}
            if reason == "settle_failed":
                extra = {"settle_error": "KeyError: 'tasks'"}
            broken = run(outcome="invalid", invalid_reason=reason,
                         covers=(SHA_A,), **extra)
            self.assertFalse(broken.measured, reason)
            report = health(merges, [broken], regressed_commits={SHA_A})
            self.assertEqual(report.l1_false_negative.denominator, 1, reason)

    def test_a_run_with_no_baseline_still_covers_its_merges(self) -> None:
        """The other half of the same rule, and the reason `invalid` had to say
        why it is invalid rather than just that it is.

        `no_reference` means the run measured fine and has nothing to compare
        against. Its window advances, so no later run will ever cover these
        merges again. Treating it like a crash would move every merge from the
        whole pre-baseline period into the denominator, as merges L1 waved
        through untested — which is the opposite of what happened.
        """
        merges = [merge(SHA_A, verdict="invisible")]
        unreferenced = run(outcome="invalid", invalid_reason="no_reference",
                           covers=(SHA_A,))
        self.assertTrue(unreferenced.measured)
        report = health(merges, [unreferenced], regressed_commits={SHA_A})
        self.assertEqual(report.l1_false_negative.denominator, 0)
        # Measured is not cleared: it compared nothing, so nothing passed.
        self.assertEqual(report.l2_escape.denominator, 0)

    def test_an_empty_quarter_reports_no_rate_rather_than_zero(self) -> None:
        report = health([], [], regressed_commits=set())
        for metric in (report.l1_false_negative, report.l1_visible_rate,
                       report.l2_escape):
            self.assertIsNone(metric.rate)

    # --- audit -----------------------------------------------------------

    def test_clean_ledger_has_no_findings(self) -> None:
        self.ledger.append_merge(merge(SHA_A))
        self.ledger.append_run(run())
        self.assertEqual(audit(self.root), [])

    def test_audit_detects_a_tampered_record(self) -> None:
        self.ledger.append_merge(merge(SHA_A))
        path = self.ledger.merge_path(SHA_A)
        data = json.loads(path.read_text())
        data["l1_verdict"] = "invisible"
        path.write_text(json.dumps(data))
        self.assertEqual([f.kind for f in audit(self.root)], ["tampered"])

    def test_audit_reads_each_subtree_with_its_own_schema(self) -> None:
        """Guessing one schema per file would report every run as malformed and
        bury real findings."""
        self.ledger.append_merge(merge(SHA_A))
        self.ledger.append_run(run(k=1, outcome="screen"))
        self.assertEqual(audit(self.root), [])

    def test_audit_detects_a_run_covering_an_unrecorded_merge(self) -> None:
        self.ledger.append_merge(merge(SHA_A))
        self.ledger.append_run(run())
        self.ledger.merge_path(SHA_A).unlink()
        self.assertEqual([f.kind for f in audit(self.root)], ["uncovered"])


if __name__ == "__main__":
    unittest.main()
