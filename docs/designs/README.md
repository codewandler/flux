# Design records

One file per non-trivial design decision (`<slug>.md`), linked from its story's `design:`
frontmatter field. Active designs and shipped-epic records live here side by side on purpose: the
design of a shipped epic stays behind as its record, and amending a shipped design (rather than
replacing it) keeps the story ↔ design link alive. Superseded or point-in-time material moves to
[docs/archive/](../archive/).

To find a design, start from its story (the `design:` field names the file), from the
[roadmap](../roadmap.md) narrative that cites it, or by grepping this directory. There is
deliberately no hand-maintained index here — at 150+ files one rots faster than it helps; the story
frontmatter is the index.
