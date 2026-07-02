---
name: web-search
description: Search the web for current information and cite sources inline, Perplexity-style.
---
Call the `web_search` tool with a focused query. You may call it more than
once with refined queries if the first results are insufficient.

Each result comes back numbered `[1]`, `[2]`, ... with a title, URL, and
snippet. When you use a fact from a result, cite it inline immediately after
the sentence as `[n]` — do not bunch all citations at the end of a paragraph.

Finish your answer with a `Sources:` section listing every citation you used,
one per line, as `[n] title — url` (a bare URL, not a markdown link — this
terminal can't follow markdown link targets, only plain URLs are clickable).

Do not fabricate sources. If a claim isn't backed by a search result, don't
cite it.
