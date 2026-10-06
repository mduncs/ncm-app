"""Read a Claude Code transcript the way the Talk tab would: you, the replies to you, and reports."""
import glob, json, os, re, time
from datetime import datetime

def ts(r):
    try:
        return datetime.fromisoformat(r["timestamp"].replace("Z", "+00:00")).timestamp()
    except Exception:
        return None

def text_of(r):
    c = (r.get("message") or {}).get("content")
    if isinstance(c, str):
        return c
    return "\n".join(x.get("text", "") for x in (c or []) if isinstance(x, dict) and x.get("type") == "text")

def load(session):
    files = glob.glob(os.path.expanduser(f"~/.claude/projects/*/{session}.jsonl"))
    if not files:
        return None
    recs = []
    for line in open(files[0], errors="replace"):
        try:
            recs.append(json.loads(line))
        except Exception:
            pass
    by = {r.get("uuid"): r for r in recs if r.get("uuid")}
    memo = {}
    def origin(r):
        chain = []
        while r is not None and len(chain) < 2000:
            u = r.get("uuid")
            if u in memo:
                o = memo[u]; break
            chain.append(u)
            if r.get("type") == "user":
                o = (r.get("origin") or {}).get("kind") or r.get("turnOrigin")
                if o:
                    break
            r = by.get(r.get("parentUuid"))
        else:
            o = None
        for u in chain:
            memo[u] = o
        return o
    events = []
    for r in recs:
        t = r.get("type")
        if t == "user":
            o = (r.get("origin") or {}).get("kind")
            body = text_of(r).strip()
            if o == "human" and body:
                events.append(("you", ts(r), body))
            elif o == "peer":
                m = re.search(r'<agent-message from="([^"]+)">\s*(.*?)\s*</agent-message>', body, re.S)
                if m:
                    events.append(("report", ts(r), m.group(1), m.group(2)))
            elif o in ("task-notification", "task_notification"):
                name = re.search(r'<summary>Agent "([^"]+)"\s*(\w+)', body)
                status = re.search(r"<status>(\w+)</status>", body)
                if name:
                    events.append(("report", ts(r), name.group(1), f"{status.group(1) if status else name.group(2)}"))
        elif t == "assistant":
            body = text_of(r).strip()
            if body and not r.get("isSidechain"):
                events.append(("reply" if origin(r) == "human" else "busywork", ts(r), body))
    return events

def unanswered(events):
    """Seconds since your last message if nothing has answered it yet, else None."""
    last = max((i for i, e in enumerate(events) if e[0] == "you"), default=None)
    if last is None:
        return None
    if any(e[0] == "reply" for e in events[last + 1:]):
        return None
    return time.time() - (events[last][1] or time.time())
