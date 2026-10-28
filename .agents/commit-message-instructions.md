# Git Commit Message Generation

Generate Git commit messages following these rules strictly.

## Format

Use the gitmoji convention, but write **ASCII gitmoji tokens** in the commit subject instead of the emoji character itself. For example, write `:sparkles:` rather than ✨. Use the shortcodes as defined by the gitmoji spec at gitmoji.dev.

Either of the two forms below is acceptable:

```
<ascii-gitmoji> <subject>

<body>
```

or

```
<ascii-gitmoji> <type>(<scope>): <subject>

<body>
```

A blank line must separate the subject line from the body. When there is no body, output the subject line only.

## Core Principle: Summarize, Don't Enumerate

A commit message is a _summary_, not an inventory of changes. Do not walk through the diff file by file or change by change. Instead, identify what the changes have in common and express that shared essence in the simplest possible way — one short, general sentence (and, only if truly needed, a few equally general bullets).

If many changes share a single theme, describe the theme, not the individual changes. "Format codes across components" is better than listing five components. "Tidy up imports" is better than naming each import.

## Subject (required)

- One line only.
- Briefly summarize the main work of this commit — the single most important change, stated in general terms.
- Imperative mood, lowercase start (after the gitmoji token or type prefix), no trailing period.
- Keep it concise: aim for 50 characters or fewer, hard limit 72.
- Pick the gitmoji token that best matches the dominant intent of the change.
- If the second form is used, the type/scope follows Conventional Commits and should be consistent with the chosen gitmoji.

## Classify Before Writing

- Before writing, decide what this commit fundamentally is: a feature, a fix, a refactor, a formatting/style change, a docs change, etc. Let the chosen gitmoji and subject reflect that essential nature.
- Do not inflate a trivial change (formatting, linting, code cleanup, whitespace, import ordering) into a feature or refactor commit. If the commit is essentially formatting, the message should simply say so, e.g. `:art: format codes`.

## Body

A subject that stands on its own is a complete commit message. In most cases the subject is all that's needed, and reaching for a body only makes the message longer without making it clearer.

Add a body only when the subject genuinely cannot carry the important information by itself — for example, when the commit spans unrelated concerns and a single subject would be misleading, or when there is a reason or caveat worth recording that won't fit into one line. When that happens:
- Keep it as short as possible — one general bullet is often enough; never pad to a fixed count.
- Each bullet starts with a capital letter and stays on one short line (<= 72 chars).
- Summarize common themes rather than enumerating. Do not break changes into fine-grained details; drop the trivial and the incidental.
- Never write bullets that merely restate or slightly rephrase the subject (e.g. "Improve readability", "Enhance consistency", "Clean up code") — those add nothing.
- Rank by importance and include only what matters alongside the subject. Minor incidental changes (editorconfig tweaks, typo fixes, formatting, trivial cleanup) are left out entirely when the main work is substantial.
- Omit implementation minutiae, file paths, function names, and diff details unless essential to understanding the change.
- Use `-` for bullets.

## Examples

Formatting/cleanup across many files:
```
:art: format codes
```

A refactor touching several components with a shared goal:
```
:recycle: simplify component structure
```

A bug fix, fully described by the subject:
```
:bug: fix crash when settings file is missing
```

A feature with one related detail worth noting:
```
:sparkles: add dark mode support

- Persist theme choice in user settings
```

A commit spanning two unrelated concerns, where the subject alone would be misleading:
```
:bug: fix login redirect and update README

- Redirect now respects the return URL
- Documentation reflects the new auth flow
```

## Language

- Always write the entire commit message in English.

## Output

- Output only the final commit message. No explanations, no preamble, no markdown code fences.
