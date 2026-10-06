---
name: enfour-maintain
description: Maintain Enfour Memory after a verified decision, a fix, or a task handoff. Use this skill to save, correct, forget, link, or export project memory.
---

Use the connected Enfour Memory tools and the project repository scope.
If the scope is unknown, use the `enfour-recall` skill first.
Respect the user's memory preferences. Save supported information in the approved scope.

## Write and review

Recall related records. Inspect a record for its full text and current revision.
Use one stable key for each fact. Use revision zero for a new key.

Write short, direct sentences. Use at most 20 words for each sentence and six sentences for each paragraph.
Use active verbs, approved words, and clear technical nouns.

Give each paragraph one topic. Put a condition before its instruction.
Avoid contractions and prose semicolons. Use headings as noun phrases.

Keep exact errors, commands, paths, numbers, and source quotations unchanged.
Put exact evidence in Markdown code or quotations. Add prose about the evidence and a source reference.
Do not put general prose in code to bypass the checks.

Call `validate_memory` with the new memory. Repair each error at the returned field and byte range.
Review advisory findings in context. Do not apply a suggestion that is not clear automatically.
Check the meaning, negation, quantities, conditions, and uncertainty against the source.
A completed check does not show complete ASD-STE100 compliance.

Call `remember` with the accepted text and the inspected revision.
On a revision conflict, inspect again and review the new evidence.
Verify the returned ID and revision. An error does not show that the memory was saved.
Use an expiry only for information with a known end date. Do not save credentials.

## Relations and removal

Inspect before a call to `forget`. It hides the current memory but keeps its history.
For `relate`, inspect the two endpoints. Supply their current revisions and the relation source.
An endpoint change hides the previous link. Review its meaning before you restore the link.

## Export

Use `graph` for current nodes, edges, sources, and revisions.
Use the JSON HTTP graph API for a frontend that must have parsed data.
Export only the requested scope. Report the result and its scope.
