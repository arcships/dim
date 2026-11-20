"""A branch part in Python: it consumes the branch's facts, folding them
through the cutoff of its own read and then live in order, and, after the first
turn ends, writes one marked input to its own branch naming the branch and the
events it has seen. `python_plugin` registers it."""

from rutis import define_plugin

PAGE = 1024


async def apply(ctx, config):
    dispatcher = ctx.use("dispatcher")
    timeline = ctx.use("timeline")
    tree = ctx.use("timelineTree")
    identity = await ctx.use("timelineIdentity").get()
    session, branch = identity["sessionId"], identity["branchId"]
    seen = []
    state = {"written": False}

    def step(event, live):
        key = f"{event['type']}/{event['name']}"
        seen.append(key)
        if key == "input/message" and "py-marker" in str(event["data"]):
            state["written"] = True
        if live and key == "turn/end" and not state["written"]:
            state["written"] = True
            marker = f'<py-marker session="{session}" branch="{branch}" seen="{",".join(seen)}"/>'
            return tree.input(branch, {"blocks": [{"type": "raw", "text": marker}], "mode": "queue"})
        return None

    async def release(event):
        written = step(event, True)
        if written is not None:
            await written

    async def rebuild():
        cutoff = int(await dispatcher.committed())
        cursor = None
        while cutoff > 0:
            query = {
                "scope": "branch",
                "include_publishing": True,
                "through": cutoff,
                "query": {
                    "filter": {
                        "op": "in",
                        "value": {"kind": "field", "path": "kind"},
                        "values": ["input/message", "turn/start", "turn/end"],
                    },
                    "select": {"kind": "events"},
                },
                "page": {"limit": PAGE, "after": cursor},
            }
            page = await timeline.query(query)
            for row in page["result"]["events"]:
                step(row["event"], False)
            cursor = page.get("next")
            if cursor is None:
                break
        return cutoff

    await dispatcher.consume(
        "python-branch-fixture", ["input/message", "turn/start", "turn/end"], rebuild, release
    )


part = define_plugin(
    apply,
    inject=["dispatcher", "timeline", "timelineIdentity", "timelineTree"],
    name="PythonBranchPart",
)
