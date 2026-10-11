# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Stack

The user requested Astro + TypeScript or TanStack Start and a static build.
Astro + TypeScript was selected for this small static landing page. Deployment is
to the user's Cloudflare account at mcp.olinn.is.

## Users

People exploring and installing the MCP servers in the family-mcp repository.
English and Icelandic readers are equally supported, as requested by the user.

## Product Purpose

Introduce the five MCP servers briefly, explain what each makes available, and
provide accurate install commands plus links to GitHub and setup documentation.
Success is finding the relevant server and copying its install command quickly.

## Positioning

One small collection of unofficial local MCP servers for everyday services in
Iceland: Abler, InfoMentor, Inna, Krónan, and Domino’s Iceland.

## Operating Context

Users install a standalone native executable, authenticate locally, and connect
it to their MCP client. macOS and glibc Linux on arm64 and x64 are supported.
The landing page itself does not log in, access accounts, or run an MCP server.

## Capabilities and Constraints

- Brief introduction, five server descriptions, copyable install commands, and
  links to the repository and per-package setup instructions.
- A collapsed, copyable prompt per server that asks a coding agent to install,
  hand sign-in to the user, register the server, and read the pinned README.
- Both English and Icelandic, with a visible language switch.
- Use the published package versions and their pinned installer URLs.
- Abler provides sports schedules; InfoMentor provides school information;
  Inna provides timetables, assignments, grades, attendance and messages.
- Inna's whole-day illness registration and leave applications are an opt-in
  preview requiring explicit approval; live submission remains unverified.
  Sign in with electronic ID or import an existing browser session, including
  a Google account already linked in Inna. Show the electronic-ID comparison
  code exactly, including leading zeros, to the user to match on their phone.
- Krónan provides product and recipe search, shopping-list and basket edits,
  and ordering after explicit approval. Its 0.2.0 write tools are a preview;
  live write tests remain unverified.
- Domino’s is a preview: charging is untested and 3-D Secure continuation is
  not implemented. Preserve this distinction in both languages.
- Credentials stay on the user's machine; returned account data is shared with
  the configured MCP host. Do not claim all data stays local.
- No invented adoption, testimonials, partnerships, or performance claims.

## Evidence on Hand

Repository README, package READMEs, package manifests, and GitHub releases.
Historical published-release snapshot, verified on 2026-09-17: Abler 0.5.2,
InfoMentor 0.6.1, Krónan 0.1.0, and Domino’s 0.1.0 preview.

Current public-release snapshot, verified via GitHub API on 2026-10-02:
Abler 0.5.3 and InfoMentor 0.8.0 stable; Inna 0.1.1, Krónan 0.2.0 and
Domino’s 0.1.0 preview. Inna 0.1.1 was published publicly as a prerelease on
2026-10-02, with UTC parsing and repeated-read hardening. The public English
and Icelandic pages were verified against those version and installer pins.

Inna 0.2.1 was published publicly as a prerelease on 2026-10-03 and replaces
0.1.1 on both pages. It reads several students under one session; switching
between two students is not yet confirmed on a real account, and both pages
say so.

Inna 0.2.2 was published as a full release on 2026-10-03 and replaces 0.2.1
on both pages. It adds a session keep-alive while the server runs.

Inna 0.3.0 was published on 2026-10-03 and replaces 0.2.2 on both pages. It adds
`auth login --google` (a browser sign-in window) and lets electronic-ID login
proceed for accounts with several students. A user confirmed switching between
two students on a real account that day, so the pages no longer call it
unconfirmed. Google's acceptance of the sign-in window is not yet verified live.

Release snapshot, verified via GitHub API on 2026-10-11: Abler 0.7.1 and
InfoMentor 0.9.1 stable; Inna 0.4.1, Krónan 0.3.1 and Domino’s 0.2.0 preview.
All five are published full releases and all five native executables are now
compiled from Rust. Sign-in saves Abler, InfoMentor and Krónan credentials in an
encrypted store, so the agent prompts no longer describe the older session and
token environment variables as the files sign-in saves. Both pages were built
and checked against these version and installer pins.

## Product Principles

- Keep the introduction and path to installation short.
- Write Icelandic directly and idiomatically; avoid literal translations of English marketing copy.
- Keep platform, architecture and runtime details in setup documentation, not on the landing page.
- Make capabilities and preview limitations accurate in both languages.
- Link detailed setup instructions instead of duplicating full documentation.
  The agent prompt carries only the install, sign-in and registration commands
  and the setup-time safety rules; tools, file locations and troubleshooting stay
  in the pinned README it points to.
- Use semantic, responsive HTML with keyboard-accessible controls.
