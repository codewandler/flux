# Design record: fleet coordinator foundations

**Status:** superseded in product shape by
[native-board-fleet-cli.md](native-board-fleet-cli.md) and flux-roadmap Decision 0010 ·
**Historical epic:** [A-111](../stories/A-111-fleet-coordinator-epic.md)

This design originally established several foundations that shipped and remain valid: WorkBoard as a
write-capable sibling of read-only datasources, MemoryBoard and MarkdownBoard, generated policy-gated
operations, outbound A2A task control and concurrent application delivery.

Its former conclusion—that the coordinator product is only a `coordinator.flux` reference Program
over remote A2A workers—is superseded. Decision 0010 makes a durable local supervisor and complete
`flux fleet` CLI the V1 product. Local native sub-agents are the only V1 workers because the host can
verify their worktrees and exact commits. Planning and execution state are linked by explicit
BoardRefs through the BoardRegistry rather than by one implicit WorkBoard.

The current product design and story order live in
[native-board-fleet-cli.md](native-board-fleet-cli.md), C-239 and A-117. This record remains the
provenance for delivered primitives. A-111 and the agent-fleet-runtime stories now describe later
remote transport, isolation, discovery and authentication work and are not dependencies of V1.
