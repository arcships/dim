# Release pipeline

A deploy starts from a green build on `main`. The pipeline attempts a failed
deploy at most four times in total, waiting 90 seconds between attempts. After
the fourth failure it stops and pages the release engineer.
