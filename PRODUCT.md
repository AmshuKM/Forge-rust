# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

Two audiences, weighted equally:

- Hobbyist developers who clone the repo, run Docker and a local model, and want to turn ideas into small working apps quickly.
- Non-technical people a developer has set Forge up for, who only type ideas and use the resulting apps.

Plain language and simple steps come first; full technical detail (diffs, test output, scan reports) stays one click away.

## Product Purpose

Forge turns an app idea into a working Rust app. The person types what they want, optionally with examples it must get right; Forge has a builder (Claude Code or a local model) write it, tests it in a locked sandbox, and makes it live once every check passes. It then watches the running apps and proposes fixes, which wait for the owner's approval. Success is an app that works on the first visit, built without the person touching code.

## Positioning

The checks are the product. Every app passes unit tests, three security scanners and the owner's own examples inside a sandbox before anyone uses it, and the owner's examples cannot be changed by the AI that builds the app. It runs entirely on the owner's computer, with local models and no subscription required.

## Operating Context

- Runs as a local web dashboard at http://127.0.0.1:8765, started with `cargo run --release -- serve`, often in the background while the person does other things.
- Builds take minutes, so people start one and come back to it.
- Pages: Build an app, build progress, My apps, using an app, Review changes, Health.
- One-time setup in a terminal (`cargo run --release -- setup`) chooses models and the builder.

## Capabilities and Constraints

- Server-rendered HTML from one Rust program; no JavaScript framework.
- Works offline: no fonts, scripts, images or other requests to the internet from the page; the one font is built into the program.
- Must work in light and dark mode and at phone width.
- Terminology: an "app" is a small Rust program in a folder under `apps/`; a "change" or "build" is one attempt to create or modify an app; "checks" or "gates" are tests, security scans and golden cases; "examples" are the owner's own golden cases.
- Only apps the owner asked for go live without a click; everything else waits on Review.

## Brand Commitments

- Name: Forge. No logo, colours or fonts are fixed.
- Character: a workshop that ships. Calm, precise and trustworthy; it shows its checks rather than asking for faith.
- Voice: plain, short sentences; says what happened and what to do next.

## Evidence on Hand

- Working sample apps in this repo: `docqa` (uses a local model) and `slugify` (uses none), both in Rust.
- No testimonials, users, benchmarks or press exist. Do not invent them.

## Product Principles

1. Show the checks. Trust comes from visible evidence that an app passed, not from claims.
2. Plain first, detail on demand. Every page makes sense to a non-developer; the technical record is one click away.
3. The owner decides. Forge never hides what an agent changed or releases outside the owner's rules.
4. Calm while waiting. Long builds report progress honestly without demanding attention.

## Accessibility & Inclusion

No product-specific requirement was established beyond the baseline: keyboard use, visible focus, screen-reader labels and announcements, sufficient contrast in both themes, and no information carried by colour alone.
