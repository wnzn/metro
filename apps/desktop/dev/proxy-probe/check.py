"""Checks a proxy-probe run (see run.sh).

argv: nsh_output proxy_log "<url> <direct|proxied>"...

Every URL must have exactly one result line. A case is "proxied" when the
proxy's log holds its exact authority (host:port as in the URL, trailing dot
included) and "direct" otherwise; a direct case must not have failed at the
proxy either. The proxy must have seen no authority outside the cases.
"""
import sys
from urllib.parse import urlsplit

out = open(sys.argv[1]).read()
log = open(sys.argv[2]).read().splitlines()
cases = [c.rsplit(" ", 1) for c in sys.argv[3:]]
if "PROBE-BEGIN" not in out:
    print(out[-3000:])
    sys.exit("the probe app did not finish")

lines = [l for l in out.split("PROBE-BEGIN", 1)[1].split("PROBE-END", 1)[0].splitlines() if " => " in l]
results = {}
for l in lines:
    u, r = l.split(" => ", 1)
    results.setdefault(u.strip(), []).append(r.strip())

# What reached the proxy, by exact authority: CONNECT host:port, or an absolute URL's.
seen = []
for l in log:
    parts = l.split()
    if len(parts) >= 2:
        seen.append(parts[1] if parts[0] == "CONNECT" else urlsplit(parts[1]).netloc)

authority = lambda url: url.split("//", 1)[1].split("/", 1)[0]
failed = 0
if len(lines) != len(cases) or any(len(results.get(u, [])) != 1 for u, _ in cases):
    print(f"FAIL expected exactly one result per URL, got {len(lines)} result lines for {len(cases)} URLs")
    failed += 1
for url, want in cases:
    hits = seen.count(authority(url))
    got = "proxied" if hits else "direct"
    result = (results.get(url) or ["(no result)"])[0]
    ok = got == want and hits <= 1 and (want == "proxied" or "CONNECT proxy failed" not in result)
    failed += not ok
    print(f"{'ok  ' if ok else 'FAIL'} {url:42} expected {want:8} got {got:8} | {result[:90]}")
unexpected = [a for a in seen if a not in [authority(u) for u, _ in cases]]
if unexpected:
    print("FAIL the proxy saw authorities outside the cases:", unexpected)
    failed += 1
print(f"\n{len(cases)} cases, {failed} failure(s)")
sys.exit(1 if failed else 0)
