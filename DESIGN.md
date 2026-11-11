---
name: Forge
description: Type an app idea, watch it pass its checks on a departure board, get a working app.
colors:
  rail-navy: "#0B2545"
  signal-yellow: "#FFC629"
  pass-green: "#1E7A45"
  pass-green-on-board: "#7FE0A4"
  fail-red: "#B3261E"
  fail-red-on-board: "#FF8A80"
  ramp-0: "#F7F9FB"
  ramp-1: "#E8ECF1"
  ramp-2: "#D5DCE5"
  ramp-3: "#AEB8C6"
  ramp-4: "#7D8896"
  ramp-5: "#56606E"
  ramp-6: "#3B4450"
  ramp-7: "#2B323D"
  ramp-8: "#1C2129"
  ramp-9: "#12161C"
typography:
  display:
    fontFamily: "Barlow Condensed, Bahnschrift, DIN Alternate, Avenir Next Condensed, Arial Narrow, system-ui, sans-serif"
    fontSize: "2.25rem"
    fontWeight: 700
    lineHeight: 1.1
  label:
    fontFamily: "Barlow Condensed, Bahnschrift, DIN Alternate, Avenir Next Condensed, Arial Narrow, system-ui, sans-serif"
    fontSize: "0.875rem"
    fontWeight: 700
    lineHeight: 1.2
    letterSpacing: "0.14em"
  body:
    fontFamily: "Segoe UI, system-ui, -apple-system, Helvetica Neue, sans-serif"
    fontSize: "1rem"
    fontWeight: 400
    lineHeight: 1.5
  data:
    fontFamily: "Cascadia Mono, Consolas, SF Mono, ui-monospace, monospace"
    fontSize: "1rem"
    fontWeight: 700
rounded:
  tile: "3px"
  control: "6px"
  panel: "8px"
spacing:
  gutter: "16px"
  group: "24px"
  section: "40px"
components:
  button-primary:
    backgroundColor: "{colors.rail-navy}"
    textColor: "{colors.ramp-0}"
    rounded: "{rounded.control}"
    padding: "12px 20px"
  button-primary-hover:
    backgroundColor: "{colors.ramp-6}"
  button-quiet:
    textColor: "{colors.ramp-8}"
    rounded: "{rounded.control}"
    padding: "12px 20px"
  button-danger:
    backgroundColor: "{colors.fail-red}"
    textColor: "{colors.ramp-0}"
    rounded: "{rounded.control}"
    padding: "12px 20px"
  input:
    backgroundColor: "{colors.ramp-0}"
    textColor: "{colors.ramp-8}"
    rounded: "{rounded.control}"
    padding: "10px 12px"
  board:
    backgroundColor: "{colors.ramp-8}"
    textColor: "{colors.ramp-0}"
    rounded: "{rounded.panel}"
  flap-tile:
    backgroundColor: "{colors.ramp-7}"
    rounded: "{rounded.tile}"
  platform-number:
    backgroundColor: "{colors.signal-yellow}"
    textColor: "{colors.ramp-9}"
    rounded: "{rounded.control}"
---

# Design System: Forge

## Overview

**Creative North Star: "The Departure Board"**

Every build is a departure. The screen people watch most, a build passing its checks, is a station board: one row per check, a time, a plain name, and a status word set in split-flap tiles that flips when it changes. Everything else in the dashboard is a calm, standard working screen that borrows only the board's type, colours and that one move.

The system is restrained and precise. A navy rail carries the navigation; a cool grey ground and white panels hold forms and tables; colour does real jobs only: signal yellow for the current page, numbers and anything in progress; green for passed; red for failed. Density is moderate, with generous separation between groups. Confirmed rejections: no hacker-terminal look, no generic AI SaaS look (gradients, glow, sparkle), nothing slow or heavy.

**Key Characteristics:**
- Status words in flap tiles, only on status, nowhere decorative.
- Numbers lead: builds and apps carry their number in a yellow platform tile.
- One tonal ramp feeds both light and dark themes.
- Flat colour throughout; no gradients, glows, grain or faked material.
- Works offline: one bundled signage font, system fonts for reading text, inline CSS, no external requests.

## Colors

A cool, flat palette: one ramp of blue-greys, a navy rail and a single signal yellow, with green and red reserved for results.

### Primary
- **Rail Navy** (rail-navy): the navigation rail and the primary button in light mode. The page's one committed field of colour.
- **Signal Yellow** (signal-yellow): the current page's underline, platform-number tiles, times on the board, RUNNING flaps, focus rings in dark mode, and text selection.

### Secondary
- **Passed Green** (pass-green, pass-green-on-board): success text on light surfaces, and flap letters for PASSED, DONE and LIVE on the board.
- **Failed Red** (fail-red, fail-red-on-board): error text and the danger button; FAILED and STOPPED flap letters on the board.

### Neutral
- **The Ramp** (ramp-0 to ramp-9): ramp-1 is the light ground, ramp-0 panels and fields, ramp-2 lines, ramp-5 muted text, ramp-8 body text and the board. Dark mode reads the same ramp from the other end: ramp-9 ground, ramp-8 panels, ramp-7 board, ramp-1 text.

### Named Rules
**The One Ramp Rule.** Every neutral comes from ramp-0 to ramp-9. A new grey is a new ramp step or it is not used.

**The Signal Means Now Rule.** Signal yellow marks the current page, a number or something in progress. It never decorates.

## Typography

**Display Font:** Barlow Condensed Bold, bundled in `static/` under the SIL Open Font License, built into the program and served by the dashboard itself, with Bahnschrift, DIN Alternate and Arial Narrow as fallbacks
**Body Font:** Segoe UI with the platform system font as fallback
**Label/Mono Font:** Cascadia Mono, Consolas or SF Mono, for times, durations and examples

**Character:** Station-sign condensed caps for anything a board would print; a plain system sans for everything a person reads.

### Hierarchy
- **Display** (700, 2.25rem, 1.1; 1.75rem on phones): the page heading, including the platform-number tile.
- **Label** (700, 0.875rem, tracked 0.14em, uppercase): section headings and table headers.
- **Body** (400, 1rem, 1.5; 70ch measure): all reading text, including board notes.
- **Data** (700 mono, 1rem): board times; durations and examples in regular weight.

### Named Rules
**The One Size Rule.** Body text is one size. Hierarchy comes from weight, tracked caps and space, not a ladder of sizes.

## Layout

A single centred column, 1120px at most, with a 16px minimum side gutter. The build page splits into the board (flexible) and a 320px side column for examples and the technical record; below 900px the side column moves under the board. Below 640px the board drops its time and duration columns, and the rail wraps into a brand row and a links row. Wide tables scroll sideways inside their own box; the page itself never scrolls horizontally. Spacing steps are 16px inside groups, 24px between groups and 40px above section headings.

## Elevation & Depth

Flat. There are no shadows. Depth comes from tonal layering only: ground, then panel, then the darker board, each one ramp step or more apart, separated by 1px lines from the ramp.

### Named Rules
**The Flat Rule.** No gradients, glows, grain, bevels or offset shadows anywhere.

## Shapes

Small, consistent corners: 3px on flap tiles, 6px on buttons and fields, 8px on panels, tables and the board. Platform-number tiles share the 6px control corner and the small waiting-count badge the 3px tile corner. Borders are 1px ramp lines. Flap tiles carry a 1px hairline across their middle, drawn behind the letter.

## Components

### Buttons
- **Shape:** gently rounded (6px).
- **Primary:** rail navy with ramp-0 text (12px 20px) in light mode; in dark mode ramp-1 with ramp-9 text.
- **Hover / Focus:** hover moves to ramp-6; focus is a 3px outline offset 2px, navy in light mode and signal yellow in dark mode and on the rail.
- **Quiet:** transparent with a ramp-3 border, for secondary actions.
- **Danger:** failed red with ramp-0 text, only for actions that stop or reject.

### Cards / Containers
- **Corner Style:** 8px.
- **Background:** ramp-0 (light) or ramp-8 (dark) on the ground.
- **Shadow Strategy:** none; see Elevation & Depth.
- **Border:** 1px ramp-2 (light) or ramp-7 (dark).
- **Internal Padding:** 24px sides, 24px bottom.

### Inputs / Fields
- **Style:** panel colour with a 1px ramp-3 border and 6px corners; placeholder text in muted ramp-5.
- **Focus:** the shared 3px focus outline.
- **Error:** the message above the form in failed red, with role="alert"; the typed values stay in place.

### Navigation
- **Style:** the navy rail with FORGE in tracked condensed caps, then page links in semibold body type.
- **States:** links rest at ramp-2 and turn ramp-0 on hover; the current page is ramp-0 with a 3px signal-yellow underline. A count of changes waiting for approval sits in a small yellow badge.
- **Mobile:** the brand takes its own row and the links wrap beneath it.

### Departure Board (signature)
A dark table with one row per check: time (mono, signal yellow), the check's name with a one-line note, the status word in flap tiles, and the time taken. Status words: DONE, PASSED, LIVE (green); RUNNING, BUILDING, WAITING, TRIAL (yellow); FAILED, STOPPED (red); WAIT (muted). A row's tiles flip once, letter by letter, only when its state has changed since the person's last refresh; reduced-motion turns the flip off. Small flaps reuse the component for status columns in other tables. The tiles are hidden from screen readers, which get the word once.

### Platform Number (signature)
A build's or app's number in a yellow tile at the start of the heading or name, like a platform number.

## Do's and Don'ts

### Do:
- **Do** use flap tiles only for status words.
- **Do** lead with the number: `Build 8`, the platform tile on headings and app names.
- **Do** take every neutral from the ramp, in both themes.
- **Do** keep plain words first and put logs, test output, scans and diffs behind a "Technical record" disclosure.

### Don't:
- **Don't** use a black screen with green monospace text, or any terminal styling.
- **Don't** use gradients, glows, sparkle icons or glass.
- **Don't** put a small label above a heading; the heading carries its own weight.
- **Don't** load fonts, scripts or images from the internet; anything new is bundled and served locally, so the dashboard works offline.
