#!/usr/bin/env python3
"""Compare fenced code blocks between docs/best-practices and docs/zh-hant/best-practices.
CommonMark-style fence parsing: a block opens with >=3 backticks/tildes and closes on a
line of the same char with length >= the opener. Usage: fences.py [--sync]"""
import re, sys, glob, os
FENCE = re.compile(r'^( {0,3})(`{3,}|~{3,})(.*)$')
def parse(text):
    lines = text.split('\n'); out = []; spans = []; i = 0
    while i < len(lines):
        m = FENCE.match(lines[i])
        if m:
            ch, n = m.group(2)[0], len(m.group(2)); j = i + 1
            while j < len(lines):
                c = re.match(r'^ {0,3}(%s{%d,})\s*$' % (re.escape(ch), n), lines[j])
                if c: break
                j += 1
            out.append('\n'.join(lines[i:j+1])); spans.append((i, j)); i = j + 1
        else: i += 1
    return lines, out, spans
root = os.path.dirname(os.path.abspath(__file__)) + '/../docs'
sync = '--sync' in sys.argv; bad = 0; pairs = 0
for en in sorted(glob.glob(root + '/best-practices/*.md')):
    name = os.path.basename(en); zh = root + '/zh-hant/best-practices/' + name; pairs += 1
    _, eb, _ = parse(open(en).read()); zl, zb, zs = parse(open(zh).read())
    if sync and len(eb) == len(zb):
        for (a, b), blk in reversed(list(zip(zs, eb))): zl[a:b+1] = blk.split('\n')
        open(zh, 'w').write('\n'.join(zl)); zb = eb
    diffs = sum(x != y for x, y in zip(eb, zb)) if len(eb) == len(zb) else -1
    print(f'{name}: en={len(eb)} zh={len(zb)} diffs={diffs if diffs >= 0 else "count-mismatch"}')
    bad += 1 if diffs else 0
print(f'{pairs} pairs checked, {bad} with differences'); sys.exit(1 if bad else 0)
