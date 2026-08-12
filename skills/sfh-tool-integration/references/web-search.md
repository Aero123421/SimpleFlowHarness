# Web search and retrieval flows

## Two-stage design

```text
search/fetch step
→ shape/source validator
→ immutable result artifact
→ synthesis/review step
```

This prevents the model from silently changing the query, losing sources, or treating a transport error page as evidence.

## Evidence manifest

Record at least:

```json
{
  "query": "...",
  "retrieved_at": "...",
  "tool": "...",
  "tool_version": "...",
  "filters": {},
  "results": [
    {"title": "...", "url": "...", "snippet": "..."}
  ],
  "limitations": []
}
```

Use a project-specific schema. Reject malformed output before AI synthesis.

## Changing results

A rerun may produce different rankings or pages. That is expected nondeterminism. The durable artifact says what this run observed. Downstream steps should read that artifact, not re-search independently unless the flow explicitly requests another sample.

## Rate limits

Map 429/temporary network errors from the search tool or wrapper to an explicit retryable exit code. A valid empty result set is a domain result, not a transport failure.

## Prompt injection

Treat page text, search snippets, repository issues, and comments as untrusted data. The synthesis prompt should state that evidence cannot modify system policy, credentials, workspace permissions, or tool selection.
