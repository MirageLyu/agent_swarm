---
name: research
description: |
  Conducting a focused investigation and producing a decision-grade report:
  scope the question, gather evidence, write a 1-page recommendation.
compatible_roles: [researcher, architect]
---

# Research playbook

Research artifacts exist to **enable a decision**. If your report does not
end with a recommendation a downstream agent can act on, it is half-done.

## Scope before searching

State the question in this format:

> "Should we use X or Y for Z, given constraints A, B, C?"

If the question doesn't fit, refine it. Don't open the browser yet.

## Gather evidence

- **Primary sources first**: official docs, RFCs, source code, benchmark
  scripts you can re-run. Secondary sources (blog posts, forum threads)
  inform but never decide.
- **Date-stamp every fact**: APIs and benchmarks change. A 2022 blog post
  about library X might be wrong today.
- **Capture quotes verbatim** when summarizing, with URL + retrieval date.

## Report shape (1 page max)

```
# <Question>

## Recommendation
<Single paragraph. Be specific.>

## Evidence
- <Fact 1> [source: <url>, retrieved YYYY-MM-DD]
- <Fact 2> [source: ...]
- ...

## Trade-offs
| Option | Pros | Cons |
| ------ | ---- | ---- |
| X      | ...  | ...  |
| Y      | ...  | ...  |

## What I did NOT investigate
<Bullets. Honest about scope limits.>
```

## Anti-patterns

- "I read 30 articles and here is everything I found." Synthesize.
- Recommending the most popular option by default; popularity is one input,
  not the answer.
- Hiding negative evidence about your preferred option. Trust is earned by
  surfacing the cons.
