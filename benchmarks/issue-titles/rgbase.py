"""Reasonable automated rg baseline: extract code-ish terms from the text, rg each, rank files by
#distinct terms matched, then by total matches. Mimics a dev grepping the identifiers in an issue."""
import re, subprocess, sys, collections
STOP = set("the and for with from that this when into have does not are was but you all can use get set new return true false none null self".split())
def terms(text, limit=12):
    t = []
    t += re.findall(r'`([^`\n]{3,60})`', text)                  # backticked spans
    t += re.findall(r'\b[A-Za-z]+(?:_[A-Za-z0-9]+)+\b', text)     # snake_case
    t += re.findall(r'\b[a-z]+[A-Z][A-Za-z0-9]+\b', text)          # camelCase
    t += re.findall(r'\b[A-Z][a-z0-9]+[A-Z][A-Za-z0-9]+\b', text)  # CamelCase
    t += re.findall(r'\b[A-Za-z_]+\.[A-Za-z_]+(?:\.[A-Za-z_]+)*\b', text)  # dotted
    t += re.findall(r'\b[A-Z]{3,}(?:_[A-Z0-9]+)*\b', text)          # CONSTANTS / redis commands
    out = []
    for x in t:
        x = x.strip().strip('()')
        x = re.sub(r'\(.*', '', x)
        if len(x) < 4 or x.lower() in STOP or x in out: continue
        out.append(x)
    return out[:limit]
EXCL=["-g","!**/__tests__/**","-g","!**/test/**","-g","!**/tests/**","-g","!*_test.go","-g","!*test*.js","-g","!**/fixtures/**","-g","!*.md","-g","!*.json","-g","!*.tcl","-g","!**/testdata/**","-g","!**/e2e/**"]
def rank(repo, text, k=3, src_only=False):
    ts = terms(text)
    distinct = collections.Counter(); total = collections.Counter()
    for term in ts:
        try:
            o = subprocess.run(["rg","-c","-F","--no-messages",*(EXCL if src_only else []),term, "."], cwd=repo, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60).stdout
        except subprocess.TimeoutExpired: continue
        for line in o.splitlines():
            f, _, c = line.rpartition(":"); f = f.removeprefix("./")
            distinct[f] += 1; total[f] += int(c)
    files = sorted(distinct, key=lambda f: (-distinct[f], -total[f], len(f)))
    return ts, files[:k]
if __name__ == "__main__":
    ts, fs = rank(sys.argv[1], sys.argv[2])
    print(ts); print(fs)
