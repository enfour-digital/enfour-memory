---
name: enfour-maintain
description: Maintain Enfour Memory after a verified decision or non-obvious fix, at a task handoff, or when asked to save, correct, forget, link, or export project memory. Skip routine progress and unsupported conclusions.
---

Use the connected Enfour Memory tools and the established repository scope. If the scope is unknown, use `enfour-recall` to establish it first.
Respect the user's memory preferences. Save only reusable, supported information within the authorized scope.

## Save or correct

Recall related records before writing. Inspect a matching record to obtain its full text and current revision.
Use one stable key per fact or decision. Use `expected_revision: 0` for a new key and the inspected revision for an update.

Write a short title, the supported conclusion, and evidence in `source`. Include a file and commit, test result reference, or explicit user statement.
For a decision, include the reason and material tradeoff. For a handoff, include verified state, remaining work, and the next concrete action.
Use an expiry only when the information has a known end date. Exclude credentials, raw transcripts, and speculative fixes.

On a revision conflict, inspect again and reconcile the new evidence before retrying. Confirm the returned ID and revision. An error is not a saved note.

## Forget or link

Inspect before `forget`. Pass the current revision. Explain that forgetting hides current results but retains history.
For `relate`, inspect both endpoints. Supply their current revisions and the source that supports the relationship.
A later endpoint edit hides the old link until it is checked again.

## Export

Use `graph` for current nodes, edges, sources, and revisions. Use the JSON HTTP graph API when a frontend needs parsed data.
Export only the requested scope. History and inactive links are not part of the active graph.

Report the completed change or export and its scope. If memory is unavailable, state that nothing was saved.
