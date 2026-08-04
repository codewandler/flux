---
id: C-526
title: "Make TUI copy/paste work without hidden terminal choreography"
pillar: Core
status: backlog
priority: P1
epic: tui-polish
areas: [flux-tui]
note: "Dogfood report: text could neither be copied nor pasted in the TUI despite C-105 and C-111 being marked done"
---

# Make TUI copy/paste work without hidden terminal choreography

## Goal

Copying ordinary transcript text and pasting ordinary prompt text must work on first use. The current
implementation has three individually plausible mechanisms—terminal-native selection behind the
undiscoverable Ctrl-T mouse toggle, focused-entry OSC-52 yank, and bracketed-paste events—but a real
dogfood session still presented as “cannot copy/paste text.” Reconcile those paths into one obvious,
tested interaction instead of treating the presence of handlers as proof that clipboard interop
works.

## Acceptance

- [ ] Start the TUI with terminal-native text selection available: mouse capture is off by default,
      the footer states that wheel capture is available with Ctrl-T, and enabling capture makes the
      inverse “Ctrl-T to select/copy” action visible. A persisted user preference or explicit CLI
      option may opt into capture-on startup, but a fresh install does not require knowing a hidden
      chord before it can copy.
- [ ] A PTY-level failing-first test drives startup, mouse-capture enable/disable and teardown and
      proves the emitted crossterm mode sequence matches the visible state. Normal exit, provider
      error, panic/drop cleanup and a failed partial setup restore every mode that Flux successfully
      enabled; the guard does not believe capture is on after a live toggle turned it off.
- [ ] Bracketed paste inserts exactly one copy of arbitrary UTF-8 and multiline text into the active
      composer, queue editor, search fields and denial-reason editor according to their existing key
      precedence. Newlines and leading/trailing whitespace are preserved, control sequences are not
      executed, and terminals that report a paste event plus a key event cannot duplicate content.
- [ ] The help overlay and website TUI guide describe the portable primary flow (select/copy while
      capture is off; terminal paste into the composer), the Ctrl-T wheel-scroll tradeoff, and the
      focused-entry `y`/OSC-52 alternative. They distinguish terminal clipboard shortcuts such as
      Ctrl-Shift-C/V or Cmd-C/V from Flux key bindings instead of claiming bare Ctrl-C/V is portable.
- [ ] Focused-entry yank reports a typed, visible success, size refusal or unsupported/blocked OSC-52
      outcome. It never prints “copied” merely because bytes were written to a terminal that has
      explicitly disabled OSC-52, and its payload remains capped and free of terminal-control
      injection outside the encoded clipboard body.
- [ ] A manual interoperability matrix is recorded for at least one native Linux terminal, tmux and
      an SSH session, covering native selection copy, paste, mouse-wheel opt-in and OSC-52 yank.
      Automated TestBackend/input tests pin the same state transitions and multiline-paste content.
- [ ] Standard workspace build/test/clippy/fmt, `flux-codegate`, embedded-doc regeneration/check and
      website gates are green.

## Progress

- 2026-08-04 — filed from a live report that Flux TUI text could not be copied or pasted. Source
  inspection found bracketed paste and both copy mechanisms, so this story treats the gap as an
  end-to-end usability/interoperability failure rather than filing another isolated clipboard API.

## Notes

- Existing partial contracts: [C-105](C-105-mouse-capture-copy-toggle.md) and
  [C-111](C-111-transcript-entry-focus-yank.md). Their completed implementation remains useful but
  is not sufficient evidence for this live workflow.
- Current seams: `crates/flux-tui/src/terminal_io.rs` enables capture and bracketed paste;
  `crates/flux-tui/src/lib.rs` handles `Event::Paste`, Ctrl-T and OSC-52; `ChatState.mouse_capture`
  drives the footer. Keep one state owner so guard teardown, emitted terminal modes and UI text
  cannot disagree.
