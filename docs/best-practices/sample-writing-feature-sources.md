# Sample: Writing feature sources

> **SAMPLE PAGE — placeholder.** This file exists only to demonstrate that every Markdown file in `docs/best-practices/` is automatically turned into a page on the product site. Replace or delete it once real best-practice guides are written.

## Keep one feature per file

Each `api/{definition}/{version}/{feature}/{feature}.md` describes exactly one HTTP method (GET, POST, PUT or DELETE).

## Use the suggested headings

The compiler understands free text, but these optional headings make intent clearer:

```markdown
# create-order

Uses POST. Requires the auth pipeline.

## Input
- product id
- quantity

## Errors
- Out of stock returns 409.
```

## State security settings in their own sentence

Write things like "This endpoint is public." as a standalone sentence so the reviewer can spot it easily; the validator extracts security items from the lock either way.
